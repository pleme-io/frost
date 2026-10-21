use std::io::Write;

const ESC: &str = "\x1b";
const ST: &str = "\x1b\\";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mark<'a> {
    PromptStart,
    InputStart,
    OutputStart { command: &'a str },
    CommandEnd { exit: i32 },
}

impl Mark<'_> {
    pub fn render(&self) -> String {
        match self {
            Mark::PromptStart => format!("{ESC}]133;A{ST}"),
            Mark::InputStart => format!("{ESC}]133;B{ST}"),
            Mark::OutputStart { command } => {
                format!("{ESC}]133;C;cmdline_url={}{ST}", percent_encode(command))
            }
            Mark::CommandEnd { exit } => format!("{ESC}]133;D;{exit}{ST}"),
        }
    }
}

pub fn percent_encode(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for b in raw.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

pub const DISABLE_ENV: &str = "FROST_OSC133";

pub fn env_disables(value: Option<&str>) -> bool {
    matches!(value, Some("0" | "off" | "false" | "no"))
}

pub fn active(option_on: bool, stdout_is_tty: bool, env_value: Option<&str>) -> bool {
    option_on && stdout_is_tty && !env_disables(env_value)
}

pub fn emit(mark: &Mark<'_>) {
    let mut out = std::io::stdout().lock();
    let _ = out.write_all(mark.render().as_bytes());
    let _ = out.flush();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn prompt_start_bytes() {
        assert_eq!(Mark::PromptStart.render().as_bytes(), b"\x1b]133;A\x1b\\");
    }

    #[test]
    fn input_start_bytes() {
        assert_eq!(Mark::InputStart.render().as_bytes(), b"\x1b]133;B\x1b\\");
    }

    #[test]
    fn output_start_carries_the_percent_encoded_command() {
        assert_eq!(
            Mark::OutputStart {
                command: "cargo test --lib"
            }
            .render()
            .as_bytes(),
            b"\x1b]133;C;cmdline_url=cargo%20test%20--lib\x1b\\"
        );
    }

    #[test]
    fn output_start_encodes_separators_and_utf8() {
        assert_eq!(
            Mark::OutputStart {
                command: "a;b\x07é"
            }
            .render(),
            "\x1b]133;C;cmdline_url=a%3Bb%07%C3%A9\x1b\\"
        );
    }

    #[test]
    fn command_end_bytes() {
        assert_eq!(
            Mark::CommandEnd { exit: 0 }.render().as_bytes(),
            b"\x1b]133;D;0\x1b\\"
        );
        assert_eq!(
            Mark::CommandEnd { exit: 130 }.render().as_bytes(),
            b"\x1b]133;D;130\x1b\\"
        );
    }

    #[test]
    fn activation_needs_option_tty_and_no_env_veto() {
        assert!(active(true, true, None));
        assert!(!active(false, true, None));
        assert!(!active(true, false, None));
        assert!(!active(true, true, Some("0")));
        assert!(!active(true, true, Some("off")));
        assert!(active(true, true, Some("1")));
    }
}
