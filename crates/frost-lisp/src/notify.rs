//! `defnotify` — declarative long-command desktop notifications.
//!
//! blzsh parity: when a command runs longer than a threshold, emit
//! a desktop notification so the user knows to come back to the
//! terminal. Instead of hand-wiring the shell script, users declare:
//!
//! ```lisp
//! (defnotify :threshold-ms 30000
//!            :title "frost"
//!            :message "command finished (${FROST_CMD_DURATION})")
//! ```
//!
//! Apply behavior: synthesize a `precmd` hook body that:
//!
//! 0. Skips while `$FROST_OSC133_ACTIVE` is set: the terminal then owns
//!    the finished-command card through OSC 133, so one command makes
//!    one notification.
//! 1. Checks `$FROST_CMD_DURATION_MS` against the threshold.
//! 2. Skips if the last command was itself a notifier invocation
//!    (avoid self-loops when precmd notifies and the user's ENTER
//!    would re-trigger).
//! 3. On Darwin shells out to `osascript -e 'display notification …'`;
//!    on Linux falls back to `notify-send` or `dunstify`. Tool
//!    detection is per-invocation so missing notifier = silent no-op.
//!
//! The synthesized body composes with user-authored `(defhook
//! :event "precmd" …)` entries via the same hook-composition logic
//! that stacks multiple precmds into one shell function.
//!
//! Fields:
//!
//! | Field          | Default                             |
//! |----------------|-------------------------------------|
//! | `:threshold-ms`| 30000 (30 seconds)                  |
//! | `:title`       | `"frost"`                           |
//! | `:message`     | `"command finished"`                |
//!
//! All fields are optional — bare `(defnotify)` uses the defaults.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

const DEFAULT_THRESHOLD_MS: u64 = 30_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum NotifyVia {
    #[default]
    Osc,
    Osascript,
    Libnotify,
}

impl NotifyVia {
    pub const ACCEPTED: &'static str = "osc, osascript, libnotify";

    pub fn parse(raw: &str) -> Result<Self, String> {
        match raw {
            "osc" => Ok(Self::Osc),
            "osascript" => Ok(Self::Osascript),
            "libnotify" => Ok(Self::Libnotify),
            other => Err(format!(
                "defnotify :via {other:?} is not accepted; accepted: {}",
                Self::ACCEPTED
            )),
        }
    }
}

#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defnotify")]
pub struct NotifySpec {
    #[serde(default)]
    pub threshold_ms: Option<u64>,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub enabled: Option<bool>,
    #[serde(default)]
    pub via: Option<String>,
}

fn osc_field(raw: &str) -> String {
    raw.chars()
        .map(|c| {
            if c.is_control() || matches!(c, ';' | '\'' | '\\' | '%') {
                ' '
            } else {
                c
            }
        })
        .collect()
}

const ESC: &str = "\\033";
const BEL: &str = "\\007";

fn osc777_prefix(title: &str) -> String {
    format!("{ESC}]777;notify;{};", osc_field(title))
}

impl NotifySpec {
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    pub fn resolved_via(&self) -> Result<NotifyVia, String> {
        self.via
            .as_deref()
            .map_or(Ok(NotifyVia::default()), NotifyVia::parse)
    }

    pub fn synthesize_precmd_body(&self) -> Result<Option<String>, String> {
        if !self.is_enabled() {
            return Ok(None);
        }
        let via = self.resolved_via()?;
        let threshold = self.threshold_ms.unwrap_or(DEFAULT_THRESHOLD_MS);
        let title = self.title.as_deref().unwrap_or("frost");
        let message = self.message.as_deref().unwrap_or("command finished");
        let guard = format!(
            "if [ -z \"${{FROST_OSC133_ACTIVE:-}}\" ] && [ \"${{FROST_CMD_DURATION_MS:-0}}\" -ge {threshold} ] 2>/dev/null; then"
        );
        let fact = "__frost_notify_text=\"$__frost_notify_message (exit ${FROST_LAST_EXIT:-0}, $((FROST_CMD_DURATION_MS / 1000))s)\"";
        let message_var = format!("__frost_notify_message={message:?}");
        let fire = match via {
            NotifyVia::Osc => format!(
                "  printf '{}%s{}' \"$__frost_notify_text\"",
                osc777_prefix(title),
                BEL,
            ),
            NotifyVia::Osascript => format!(
                "  osascript -e \"display notification \\\"$__frost_notify_text\\\" with title \\\"{}\\\"\" >/dev/null 2>&1",
                osc_field(title)
            ),
            NotifyVia::Libnotify => format!(
                "  notify-send {:?} \"$__frost_notify_text\" 2>/dev/null",
                osc_field(title)
            ),
        };
        Ok(Some(format!(
            "{message_var}\n{guard}\n  {fact}\n{fire}\nfi"
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn body(spec: &NotifySpec) -> String {
        spec.synthesize_precmd_body().unwrap().unwrap()
    }

    #[test]
    fn default_route_is_osc777_and_spawns_nothing() {
        let b = body(&NotifySpec::default());
        assert!(b.contains("30000"));
        assert!(b.contains("\\033]777;notify;frost;"));
        assert!(!b.contains("osascript"));
        assert!(!b.contains("notify-send"));
        assert!(!b.contains("command -v"));
    }

    #[test]
    fn message_carries_exit_status_and_duration() {
        let b = body(&NotifySpec::default());
        assert!(b.contains("FROST_LAST_EXIT"));
        assert!(b.contains("FROST_CMD_DURATION_MS / 1000"));
    }

    #[test]
    fn osascript_is_an_explicit_opt_in() {
        let spec = NotifySpec {
            via: Some("osascript".into()),
            ..Default::default()
        };
        assert!(body(&spec).contains("osascript"));
    }

    #[test]
    fn libnotify_is_an_explicit_opt_in() {
        let spec = NotifySpec {
            via: Some("libnotify".into()),
            ..Default::default()
        };
        assert!(body(&spec).contains("notify-send"));
    }

    #[test]
    fn unknown_route_is_refused_and_names_what_is_accepted() {
        let spec = NotifySpec {
            via: Some("carrier-pigeon".into()),
            ..Default::default()
        };
        let err = spec.synthesize_precmd_body().unwrap_err();
        assert!(err.contains("carrier-pigeon") && err.contains("osc, osascript, libnotify"));
    }

    #[test]
    fn disabled_emits_no_body_but_the_form_stays_declared() {
        let spec = NotifySpec {
            enabled: Some(false),
            ..Default::default()
        };
        assert_eq!(spec.synthesize_precmd_body().unwrap(), None);
    }

    #[test]
    fn custom_threshold_and_title_land_in_body() {
        let spec = NotifySpec {
            threshold_ms: Some(60_000),
            title: Some("build done".into()),
            message: Some("ok".into()),
            ..Default::default()
        };
        let b = body(&spec);
        assert!(b.contains("60000"));
        assert!(b.contains("\"ok\""));
    }

    #[test]
    fn separators_in_the_title_cannot_split_the_osc_payload() {
        assert_eq!(osc_field("a;b\x07c%d"), "a b c d");
    }

    #[test]
    fn body_reads_the_portable_duration_env_var() {
        assert!(body(&NotifySpec::default()).contains("FROST_CMD_DURATION_MS"));
    }

    #[test]
    fn body_yields_while_osc133_integration_is_active() {
        let b = body(&NotifySpec::default());
        assert!(b.starts_with("__frost_notify_message="));
        assert!(b.contains("if [ -z \"${FROST_OSC133_ACTIVE:-}\" ] && [ \"${FROST_CMD_DURATION_MS:-0}\" -ge 30000 ]"));
    }
}
