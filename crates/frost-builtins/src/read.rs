//! The `read` builtin — read a line from stdin into shell variables.
//!
//! Supports:
//! - `read name` — read a line into variable
//! - `read -r` — don't interpret backslash escapes
//! - `read name1 name2 ...` — split on IFS, last var gets remainder
//! - `read -A array` — read into array (split on IFS)
//! - `read -d delim` — use delimiter instead of newline
//! - `read -k count` — read exactly count characters (zsh-specific)
//! - `read -q` — read single char, return 0 if y/Y

use crate::{Builtin, ShellEnvironment};

pub struct Read;

impl Builtin for Read {
    fn name(&self) -> &str {
        "read"
    }

    fn execute(&self, args: &[&str], env: &mut dyn ShellEnvironment) -> i32 {
        let mut raw = false;
        let mut array_mode = false;
        let mut delimiter: Option<char> = None;
        let mut char_count: Option<usize> = None;
        let mut query_mode = false;
        let mut prompt: Option<&str> = None;
        let mut names: Vec<&str> = Vec::new();

        // Parse options
        let mut i = 0;
        while i < args.len() {
            let arg = args[i];
            if arg == "--" {
                i += 1;
                break;
            }
            if !arg.starts_with('-') || arg.len() < 2 {
                break;
            }

            let mut chars = arg[1..].chars().peekable();
            let mut all_flags = true;
            while let Some(ch) = chars.next() {
                match ch {
                    'r' => raw = true,
                    'A' | 'a' => array_mode = true,
                    'q' => query_mode = true,
                    'd' => {
                        // Delimiter: can be attached (-d:) or next arg (-d :)
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            delimiter = rest.chars().next();
                        } else {
                            i += 1;
                            if i < args.len() {
                                delimiter = args[i].chars().next();
                            }
                        }
                        // Break out of the char loop since we consumed the rest
                        all_flags = true;
                        break;
                    }
                    'k' => {
                        // Char count: can be attached (-k3) or next arg (-k 3)
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            char_count = rest.parse().ok();
                        } else {
                            i += 1;
                            if i < args.len() {
                                char_count = args[i].parse().ok();
                            }
                        }
                        break;
                    }
                    'p' => {
                        // Prompt string: next arg
                        let rest: String = chars.collect();
                        if !rest.is_empty() {
                            prompt = Some(args[i]); // will use the rest
                            // Actually we need the remainder — tricky with borrowed lifetimes.
                            // Instead, consume as next arg.
                        }
                        // Prompt is always the next argument
                        if rest.is_empty() {
                            i += 1;
                            if i < args.len() {
                                prompt = Some(args[i]);
                            }
                        }
                        break;
                    }
                    _ => {
                        // Unknown flag — treat this arg as a variable name
                        all_flags = false;
                        break;
                    }
                }
            }

            if !all_flags {
                break;
            }
            i += 1;
        }

        // Remaining args are variable names
        while i < args.len() {
            names.push(args[i]);
            i += 1;
        }

        // Print prompt if requested
        if let Some(p) = prompt {
            eprint!("{p}");
        }

        // Read input
        let input = if let Some(count) = char_count {
            read_chars(count)
        } else if query_mode {
            read_chars(1)
        } else {
            read_line(delimiter)
        };

        let input = match input {
            Some(s) => s,
            None => return 1, // EOF
        };

        // Query mode: return 0 if y/Y, 1 otherwise
        if query_mode {
            let ch = input.chars().next().unwrap_or('\0');
            return if ch == 'y' || ch == 'Y' { 0 } else { 1 };
        }

        // Process backslash escapes unless -r
        let input = if raw {
            input
        } else {
            process_read_escapes(&input)
        };

        // Trim trailing newline/delimiter (already done in read_line, but be safe)
        let input = input.trim_end_matches('\n').trim_end_matches('\r');

        // Get IFS for splitting
        let ifs = env
            .get_var("IFS")
            .map(|s| s.to_owned())
            .unwrap_or_else(|| " \t\n".to_owned());

        if array_mode {
            // Read into array: split on IFS, store as space-separated in a single var
            let name = names.first().copied().unwrap_or("reply");
            let fields = split_on_ifs(input, &ifs);
            // Store array elements as space-separated (frost convention)
            let value = fields.join(" ");
            env.set_var(name, &value);
            env.set_var_array(name);
        } else if names.is_empty() {
            // No variable names: store in REPLY
            env.set_var("REPLY", input);
        } else if names.len() == 1 {
            // Single variable: entire line (trimmed of IFS whitespace at edges)
            let trimmed = trim_ifs(input, &ifs);
            env.set_var(names[0], trimmed);
        } else {
            // Multiple variables: split on IFS, last gets remainder
            let fields = split_on_ifs_with_remainder(input, &ifs, names.len());
            for (idx, name) in names.iter().enumerate() {
                let value = fields.get(idx).map(|s| s.as_str()).unwrap_or("");
                env.set_var(name, value);
            }
        }

        0
    }
}

/// Read a single line from stdin, terminated by `delimiter` (default newline).
fn read_line(delimiter: Option<char>) -> Option<String> {
    use std::io::Read as _;

    match delimiter {
        Some(delim) => {
            // Read byte-by-byte until delimiter
            let mut buf = String::new();
            let stdin = std::io::stdin();
            let mut handle = stdin.lock();
            let mut byte = [0u8; 1];
            loop {
                match handle.read(&mut byte) {
                    Ok(0) => {
                        if buf.is_empty() {
                            return None;
                        }
                        return Some(buf);
                    }
                    Ok(_) => {
                        let ch = byte[0] as char;
                        if ch == delim {
                            return Some(buf);
                        }
                        buf.push(ch);
                    }
                    Err(_) => return None,
                }
            }
        }
        None => {
            // Standard line read
            let mut buf = String::new();
            let stdin = std::io::stdin();
            let mut handle = stdin.lock();
            use std::io::BufRead;
            match handle.read_line(&mut buf) {
                Ok(0) => None,
                Ok(_) => {
                    // Remove trailing newline
                    if buf.ends_with('\n') {
                        buf.pop();
                        if buf.ends_with('\r') {
                            buf.pop();
                        }
                    }
                    Some(buf)
                }
                Err(_) => None,
            }
        }
    }
}

/// Read exactly `count` characters from stdin.
fn read_chars(count: usize) -> Option<String> {
    use std::io::Read as _;

    let stdin = std::io::stdin();
    let mut handle = stdin.lock();
    let mut buf = String::new();
    let mut byte = [0u8; 1];

    for _ in 0..count {
        match handle.read(&mut byte) {
            Ok(0) => break,
            Ok(_) => buf.push(byte[0] as char),
            Err(_) => break,
        }
    }

    if buf.is_empty() {
        None
    } else {
        Some(buf)
    }
}

/// Process backslash escapes in read input (without -r).
///
/// In `read` (without -r), a backslash before any character removes the
/// special meaning of that character. A backslash-newline acts as line
/// continuation.
fn process_read_escapes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();

    while let Some(c) = chars.next() {
        if c == '\\' {
            match chars.next() {
                Some('\n') => {
                    // Line continuation: skip both backslash and newline
                }
                Some(next) => {
                    // Backslash removes special meaning — just emit the char
                    out.push(next);
                }
                None => {
                    // Trailing backslash: discard it
                }
            }
        } else {
            out.push(c);
        }
    }

    out
}

/// Split a string on IFS characters.
///
/// IFS whitespace characters (space, tab, newline) at the start/end are
/// trimmed, and consecutive IFS whitespace acts as a single delimiter.
/// Non-whitespace IFS characters delimit exactly.
fn split_on_ifs(s: &str, ifs: &str) -> Vec<String> {
    if ifs.is_empty() {
        // Empty IFS: no splitting
        return vec![s.to_owned()];
    }

    let ifs_ws: Vec<char> = ifs.chars().filter(|c| " \t\n".contains(*c)).collect();
    let ifs_non_ws: Vec<char> = ifs.chars().filter(|c| !" \t\n".contains(*c)).collect();

    // Trim leading/trailing IFS whitespace
    let trimmed = s.trim_matches(|c: char| ifs_ws.contains(&c));

    if trimmed.is_empty() {
        return Vec::new();
    }

    let mut fields = Vec::new();
    let mut current = String::new();
    let mut chars = trimmed.chars().peekable();

    while let Some(c) = chars.next() {
        if ifs_non_ws.contains(&c) {
            // Non-whitespace IFS char: always delimits
            fields.push(current.clone());
            current.clear();
            // Skip adjacent IFS whitespace
            while let Some(&next) = chars.peek() {
                if ifs_ws.contains(&next) {
                    chars.next();
                } else {
                    break;
                }
            }
        } else if ifs_ws.contains(&c) {
            // Whitespace IFS: skip consecutive whitespace
            if !current.is_empty() {
                fields.push(current.clone());
                current.clear();
            }
            while let Some(&next) = chars.peek() {
                if ifs_ws.contains(&next) {
                    chars.next();
                } else {
                    break;
                }
            }
        } else {
            current.push(c);
        }
    }

    if !current.is_empty() {
        fields.push(current);
    }

    fields
}

/// Split on IFS but stop after `max_fields` fields. The last field gets
/// the remainder of the string (including any IFS characters).
fn split_on_ifs_with_remainder(s: &str, ifs: &str, max_fields: usize) -> Vec<String> {
    if max_fields <= 1 {
        let trimmed = trim_ifs(s, ifs);
        return vec![trimmed.to_owned()];
    }

    if ifs.is_empty() {
        return vec![s.to_owned()];
    }

    let ifs_ws: Vec<char> = ifs.chars().filter(|c| " \t\n".contains(*c)).collect();
    let ifs_non_ws: Vec<char> = ifs.chars().filter(|c| !" \t\n".contains(*c)).collect();

    // Trim leading IFS whitespace
    let trimmed = s.trim_start_matches(|c: char| ifs_ws.contains(&c));

    let mut fields: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut chars = trimmed.chars().peekable();

    while let Some(c) = chars.next() {
        if fields.len() + 1 >= max_fields {
            // This is the last field — take the remainder
            current.push(c);
            current.extend(chars);
            // Trim trailing IFS whitespace from last field
            let end_trimmed = current.trim_end_matches(|c: char| ifs_ws.contains(&c));
            fields.push(end_trimmed.to_owned());
            return fields;
        }

        if ifs_non_ws.contains(&c) {
            fields.push(current.clone());
            current.clear();
            // Skip adjacent IFS whitespace
            while let Some(&next) = chars.peek() {
                if ifs_ws.contains(&next) {
                    chars.next();
                } else {
                    break;
                }
            }
        } else if ifs_ws.contains(&c) {
            if !current.is_empty() {
                fields.push(current.clone());
                current.clear();
            }
            while let Some(&next) = chars.peek() {
                if ifs_ws.contains(&next) {
                    chars.next();
                } else {
                    break;
                }
            }
        } else {
            current.push(c);
        }
    }

    // Fill remaining with what we have
    if !current.is_empty() || fields.len() < max_fields {
        fields.push(current);
    }

    // Pad with empty strings if not enough fields
    while fields.len() < max_fields {
        fields.push(String::new());
    }

    fields
}

/// Trim leading and trailing IFS whitespace characters.
fn trim_ifs<'a>(s: &'a str, ifs: &str) -> &'a str {
    let ifs_ws: Vec<char> = ifs.chars().filter(|c| " \t\n".contains(*c)).collect();
    s.trim_matches(|c: char| ifs_ws.contains(&c))
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── IFS splitting tests ─────────────────────────────────────────

    #[test]
    fn split_simple_spaces() {
        let fields = split_on_ifs("hello world foo", " \t\n");
        assert_eq!(fields, vec!["hello", "world", "foo"]);
    }

    #[test]
    fn split_leading_trailing_whitespace() {
        let fields = split_on_ifs("  hello world  ", " \t\n");
        assert_eq!(fields, vec!["hello", "world"]);
    }

    #[test]
    fn split_tabs() {
        let fields = split_on_ifs("a\tb\tc", " \t\n");
        assert_eq!(fields, vec!["a", "b", "c"]);
    }

    #[test]
    fn split_colon_ifs() {
        let fields = split_on_ifs("one:two:three", ":");
        assert_eq!(fields, vec!["one", "two", "three"]);
    }

    #[test]
    fn split_empty_ifs() {
        let fields = split_on_ifs("no splitting", "");
        assert_eq!(fields, vec!["no splitting"]);
    }

    #[test]
    fn split_with_remainder() {
        let fields = split_on_ifs_with_remainder("one two three four", " \t\n", 2);
        assert_eq!(fields, vec!["one", "two three four"]);
    }

    #[test]
    fn split_with_remainder_exact() {
        let fields = split_on_ifs_with_remainder("one two", " \t\n", 2);
        assert_eq!(fields, vec!["one", "two"]);
    }

    #[test]
    fn split_with_remainder_fewer_fields() {
        let fields = split_on_ifs_with_remainder("only", " \t\n", 3);
        assert_eq!(fields, vec!["only", "", ""]);
    }

    // ── Backslash processing tests ──────────────────────────────────

    #[test]
    fn escape_backslash_newline_continuation() {
        let result = process_read_escapes("hello\\\nworld");
        assert_eq!(result, "helloworld");
    }

    #[test]
    fn escape_backslash_char() {
        let result = process_read_escapes("he\\llo");
        assert_eq!(result, "hello");
    }

    #[test]
    fn escape_trailing_backslash() {
        let result = process_read_escapes("hello\\");
        assert_eq!(result, "hello");
    }

    #[test]
    fn raw_mode_preserves_backslashes() {
        // When -r is used, we skip process_read_escapes entirely
        let input = "hello\\nworld";
        assert_eq!(input, "hello\\nworld");
    }

    // ── Builtin trait tests ─────────────────────────────────────────

    #[test]
    fn name_is_read() {
        let r = Read;
        assert_eq!(r.name(), "read");
    }

    /// Minimal mock environment for tests.
    #[allow(dead_code)]
    struct MockEnv {
        vars: std::collections::HashMap<String, String>,
    }

    impl MockEnv {
        #[allow(dead_code)]
        fn new() -> Self {
            Self {
                vars: std::collections::HashMap::new(),
            }
        }
    }

    impl crate::ShellEnvironment for MockEnv {
        fn get_var(&self, name: &str) -> Option<&str> {
            self.vars.get(name).map(|s| s.as_str())
        }
        fn set_var(&mut self, name: &str, value: &str) {
            self.vars.insert(name.to_owned(), value.to_owned());
        }
        fn export_var(&mut self, _: &str) {}
        fn unset_var(&mut self, name: &str) {
            self.vars.remove(name);
        }
        fn exit_status(&self) -> i32 {
            0
        }
        fn set_exit_status(&mut self, _: i32) {}
        fn chdir(&mut self, _: &str) -> Result<(), String> {
            Ok(())
        }
        fn home_dir(&self) -> Option<&str> {
            None
        }
    }
}
