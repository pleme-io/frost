//! The `kill` builtin — send signals to processes.
//!
//! Usage:
//! - `kill [-signal] pid...`
//! - `kill -s signal pid...`
//! - `kill -l` — list signal names
//! - `kill -l signum` — print name for signal number

use crate::{Builtin, ShellEnvironment};

pub struct Kill;

/// Map of signal names to their numeric values.
/// Uses the standard POSIX signal numbers (platform-dependent but
/// consistent on macOS and Linux for these core signals).
const SIGNALS: &[(&str, i32)] = &[
    ("HUP", 1),
    ("INT", 2),
    ("QUIT", 3),
    ("ILL", 4),
    ("TRAP", 5),
    ("ABRT", 6),
    ("BUS", 7),
    ("FPE", 8),
    ("KILL", 9),
    ("USR1", 10),
    ("SEGV", 11),
    ("USR2", 12),
    ("PIPE", 13),
    ("ALRM", 14),
    ("TERM", 15),
    ("CHLD", 17),
    ("CONT", 18),
    ("STOP", 19),
    ("TSTP", 20),
    ("TTIN", 21),
    ("TTOU", 22),
    ("URG", 23),
    ("XCPU", 24),
    ("XFSZ", 25),
    ("VTALRM", 26),
    ("PROF", 27),
    ("WINCH", 28),
    ("IO", 29),
    ("SYS", 31),
];

/// Resolve a signal name (with or without "SIG" prefix) to its number.
fn signal_name_to_num(name: &str) -> Option<i32> {
    let upper = name.to_ascii_uppercase();
    let stripped = upper.strip_prefix("SIG").unwrap_or(&upper);
    SIGNALS.iter().find(|(n, _)| *n == stripped).map(|(_, v)| *v)
}

/// Resolve a signal number to its name.
fn signal_num_to_name(num: i32) -> Option<&'static str> {
    SIGNALS.iter().find(|(_, v)| *v == num).map(|(n, _)| *n)
}

/// Parse a signal specifier which may be a name or number.
fn parse_signal(spec: &str) -> Option<i32> {
    // Try as a number first
    if let Ok(n) = spec.parse::<i32>() {
        return Some(n);
    }
    signal_name_to_num(spec)
}

impl Builtin for Kill {
    fn name(&self) -> &str {
        "kill"
    }

    fn execute(&self, args: &[&str], _env: &mut dyn ShellEnvironment) -> i32 {
        if args.is_empty() {
            eprintln!("kill: usage: kill [-s sigspec | -sigspec] pid ...");
            return 2;
        }

        let mut signal: i32 = 15; // default SIGTERM
        let mut pids: Vec<&str> = Vec::new();
        let mut i = 0;
        let mut list_mode = false;
        let mut list_arg: Option<&str> = None;

        while i < args.len() {
            let arg = args[i];
            if arg == "-l" || arg == "-L" {
                list_mode = true;
                // Optional argument: signal number to convert to name
                if i + 1 < args.len() {
                    list_arg = Some(args[i + 1]);
                    i += 2;
                } else {
                    i += 1;
                }
            } else if arg == "-s" {
                // -s signal
                if i + 1 >= args.len() {
                    eprintln!("kill: -s requires an argument");
                    return 2;
                }
                match parse_signal(args[i + 1]) {
                    Some(s) => signal = s,
                    None => {
                        eprintln!("kill: {}: invalid signal specification", args[i + 1]);
                        return 2;
                    }
                }
                i += 2;
            } else if arg.starts_with('-') && arg.len() > 1 && arg != "--" {
                // -SIGNAL or -signum
                match parse_signal(&arg[1..]) {
                    Some(s) => signal = s,
                    None => {
                        eprintln!("kill: {}: invalid signal specification", &arg[1..]);
                        return 2;
                    }
                }
                i += 1;
            } else if arg == "--" {
                // End of options, rest are PIDs
                pids.extend_from_slice(&args[i + 1..]);
                break;
            } else {
                pids.push(arg);
                i += 1;
            }
        }

        // List mode: print signal names
        if list_mode {
            if let Some(arg) = list_arg {
                // Convert signal number to name
                if let Ok(num) = arg.parse::<i32>() {
                    // Handle exit-status encoding: signals > 128 mean
                    // the process was killed by signal (status - 128)
                    let effective = if num > 128 { num - 128 } else { num };
                    match signal_num_to_name(effective) {
                        Some(name) => {
                            println!("{name}");
                            return 0;
                        }
                        None => {
                            eprintln!("kill: {num}: invalid signal number");
                            return 1;
                        }
                    }
                } else {
                    // It's a name — print its number
                    match signal_name_to_num(arg) {
                        Some(num) => {
                            println!("{num}");
                            return 0;
                        }
                        None => {
                            eprintln!("kill: {arg}: invalid signal specification");
                            return 1;
                        }
                    }
                }
            }

            // Print all signals
            for (name, num) in SIGNALS {
                println!("{num:2}) SIG{name}");
            }
            return 0;
        }

        if pids.is_empty() {
            eprintln!("kill: usage: kill [-s sigspec | -sigspec] pid ...");
            return 2;
        }

        // Send the signal to each PID
        let mut status = 0;
        for pid_str in &pids {
            let pid: i32 = match pid_str.parse() {
                Ok(p) => p,
                Err(_) => {
                    eprintln!("kill: {pid_str}: arguments must be process or job IDs");
                    status = 1;
                    continue;
                }
            };

            // Use libc::kill directly for portability — the nix crate's
            // Signal enum doesn't cover arbitrary signal numbers.
            #[allow(unsafe_code)]
            let ret = unsafe { libc::kill(pid, signal) };
            if ret != 0 {
                let err = std::io::Error::last_os_error();
                eprintln!("kill: ({pid}) - {err}");
                status = 1;
            }
        }

        status
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ShellEnvironment;
    use std::collections::HashMap;

    struct MockEnv {
        vars: HashMap<String, String>,
        status: i32,
    }

    impl MockEnv {
        fn new() -> Self {
            Self {
                vars: HashMap::new(),
                status: 0,
            }
        }
    }

    impl ShellEnvironment for MockEnv {
        fn get_var(&self, name: &str) -> Option<&str> {
            self.vars.get(name).map(|s| s.as_str())
        }
        fn set_var(&mut self, name: &str, value: &str) {
            self.vars.insert(name.into(), value.into());
        }
        fn export_var(&mut self, _name: &str) {}
        fn unset_var(&mut self, name: &str) {
            self.vars.remove(name);
        }
        fn exit_status(&self) -> i32 {
            self.status
        }
        fn set_exit_status(&mut self, status: i32) {
            self.status = status;
        }
        fn chdir(&mut self, _path: &str) -> Result<(), String> {
            Ok(())
        }
        fn home_dir(&self) -> Option<&str> {
            None
        }
    }

    #[test]
    fn signal_name_lookup() {
        assert_eq!(signal_name_to_num("TERM"), Some(15));
        assert_eq!(signal_name_to_num("SIGTERM"), Some(15));
        assert_eq!(signal_name_to_num("term"), Some(15));
        assert_eq!(signal_name_to_num("HUP"), Some(1));
        assert_eq!(signal_name_to_num("KILL"), Some(9));
        assert_eq!(signal_name_to_num("BOGUS"), None);
    }

    #[test]
    fn signal_num_lookup() {
        assert_eq!(signal_num_to_name(15), Some("TERM"));
        assert_eq!(signal_num_to_name(9), Some("KILL"));
        assert_eq!(signal_num_to_name(1), Some("HUP"));
        assert_eq!(signal_num_to_name(999), None);
    }

    #[test]
    fn parse_signal_by_number() {
        assert_eq!(parse_signal("9"), Some(9));
        assert_eq!(parse_signal("15"), Some(15));
    }

    #[test]
    fn parse_signal_by_name() {
        assert_eq!(parse_signal("TERM"), Some(15));
        assert_eq!(parse_signal("SIGKILL"), Some(9));
    }

    #[test]
    fn kill_no_args() {
        let mut env = MockEnv::new();
        let kill = Kill;
        assert_eq!(kill.execute(&[], &mut env), 2);
    }

    #[test]
    fn kill_invalid_pid() {
        let mut env = MockEnv::new();
        let kill = Kill;
        assert_eq!(kill.execute(&["notapid"], &mut env), 1);
    }

    #[test]
    fn kill_signal_0_self() {
        // Signal 0 checks if process exists (should succeed for own PID)
        let mut env = MockEnv::new();
        let kill = Kill;
        let pid = std::process::id().to_string();
        assert_eq!(kill.execute(&["-0", &pid], &mut env), 0);
    }

    #[test]
    fn kill_list_signal_name() {
        // kill -l 15 should print TERM
        let mut env = MockEnv::new();
        let kill = Kill;
        let status = kill.execute(&["-l", "15"], &mut env);
        assert_eq!(status, 0);
    }

    #[test]
    fn kill_list_exit_status_encoding() {
        // kill -l 143 (128+15) should print TERM
        let mut env = MockEnv::new();
        let kill = Kill;
        let status = kill.execute(&["-l", "143"], &mut env);
        assert_eq!(status, 0);
    }
}
