//! Trap infrastructure — signal and pseudo-signal handlers.
//!
//! Mirrors zsh's trap system: shell commands can be registered to run
//! when a signal is received or when pseudo-events (EXIT, DEBUG, ERR,
//! ZERR) fire. The [`TrapTable`] stores the mapping and provides
//! helpers for signal name/number translation.

use std::collections::HashMap;

use nix::sys::signal::Signal;

// ── Trap action ──────────────────────────────────────────────────────

/// What to do when a trapped signal or pseudo-signal fires.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrapAction {
    /// Execute this shell command string.
    Command(String),
    /// Reset to the default OS behavior.
    Default,
    /// Ignore the signal entirely.
    Ignore,
}

// ── Pseudo-signals ───────────────────────────────────────────────────

/// Zsh pseudo-signals that have no real Unix signal number.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PseudoSignal {
    /// Fires when the shell exits.
    Exit,
    /// Fires before every command (when enabled).
    Debug,
    /// Fires when a command returns non-zero (bash-style ERR).
    Err,
    /// Fires when a command returns non-zero (zsh-style ZERR).
    Zerr,
}

impl PseudoSignal {
    /// Display name for listing.
    pub fn name(self) -> &'static str {
        match self {
            Self::Exit => "EXIT",
            Self::Debug => "DEBUG",
            Self::Err => "ERR",
            Self::Zerr => "ZERR",
        }
    }

    /// Try to parse a pseudo-signal from its name.
    pub fn from_name(name: &str) -> Option<Self> {
        match name {
            "EXIT" => Some(Self::Exit),
            "DEBUG" => Some(Self::Debug),
            "ERR" => Some(Self::Err),
            "ZERR" => Some(Self::Zerr),
            _ => None,
        }
    }
}

// ── Signal name/number mapping ───────────────────────────────────────

/// All signals we support, using `nix::sys::signal::Signal` for
/// platform-correct numbering (macOS and Linux differ).
const SIGNAL_TABLE: &[(Signal, &str)] = &[
    (Signal::SIGHUP, "HUP"),
    (Signal::SIGINT, "INT"),
    (Signal::SIGQUIT, "QUIT"),
    (Signal::SIGILL, "ILL"),
    (Signal::SIGTRAP, "TRAP"),
    (Signal::SIGABRT, "ABRT"),
    (Signal::SIGBUS, "BUS"),
    (Signal::SIGFPE, "FPE"),
    (Signal::SIGKILL, "KILL"),
    (Signal::SIGUSR1, "USR1"),
    (Signal::SIGSEGV, "SEGV"),
    (Signal::SIGUSR2, "USR2"),
    (Signal::SIGPIPE, "PIPE"),
    (Signal::SIGALRM, "ALRM"),
    (Signal::SIGTERM, "TERM"),
    (Signal::SIGCHLD, "CHLD"),
    (Signal::SIGCONT, "CONT"),
    (Signal::SIGSTOP, "STOP"),
    (Signal::SIGTSTP, "TSTP"),
    (Signal::SIGTTIN, "TTIN"),
    (Signal::SIGTTOU, "TTOU"),
    (Signal::SIGURG, "URG"),
    (Signal::SIGXCPU, "XCPU"),
    (Signal::SIGXFSZ, "XFSZ"),
    (Signal::SIGVTALRM, "VTALRM"),
    (Signal::SIGPROF, "PROF"),
    (Signal::SIGWINCH, "WINCH"),
    (Signal::SIGIO, "IO"),
    (Signal::SIGSYS, "SYS"),
];

/// Convert a signal name (e.g. "INT", "SIGINT") to its platform-native
/// number. Case-insensitive for the name portion.
pub fn signal_name_to_number(name: &str) -> Option<i32> {
    // Strip optional "SIG" prefix
    let stripped = name
        .strip_prefix("SIG")
        .or_else(|| name.strip_prefix("sig"))
        .unwrap_or(name);
    let upper = stripped.to_ascii_uppercase();

    // Check pseudo-signals first — they have no number
    if PseudoSignal::from_name(&upper).is_some() {
        return None;
    }

    // Signal 0 is special (used by `kill -0`)
    if upper == "0" || upper == "EXIT" {
        return Some(0);
    }

    // Try direct numeric parse
    if let Ok(n) = name.parse::<i32>() {
        return Some(n);
    }

    SIGNAL_TABLE
        .iter()
        .find(|(_, n)| *n == upper)
        .map(|(sig, _)| *sig as i32)
}

/// Convert a platform signal number to its canonical short name
/// (e.g. 2 → "INT"). Returns "UNKNOWN" for unrecognized numbers.
pub fn signal_number_to_name(num: i32) -> &'static str {
    if num == 0 {
        return "EXIT";
    }
    SIGNAL_TABLE
        .iter()
        .find(|(sig, _)| *sig as i32 == num)
        .map(|(_, name)| *name)
        .unwrap_or("UNKNOWN")
}

// ── Trap table ───────────────────────────────────────────────────────

/// Central registry of signal and pseudo-signal trap actions.
#[derive(Debug, Clone)]
pub struct TrapTable {
    /// Real Unix signal traps, keyed by signal number.
    traps: HashMap<i32, TrapAction>,
    /// Pseudo-signal traps (EXIT, DEBUG, ERR, ZERR).
    pseudo_traps: HashMap<PseudoSignal, TrapAction>,
}

impl TrapTable {
    /// Create an empty trap table (all signals at default behavior).
    pub fn new() -> Self {
        Self {
            traps: HashMap::new(),
            pseudo_traps: HashMap::new(),
        }
    }

    /// Register a trap action for a real signal number.
    pub fn set(&mut self, signal: i32, action: TrapAction) {
        self.traps.insert(signal, action);
    }

    /// Look up the trap action for a real signal number.
    pub fn get(&self, signal: i32) -> Option<&TrapAction> {
        self.traps.get(&signal)
    }

    /// Remove the trap for a real signal, restoring default behavior.
    pub fn remove(&mut self, signal: i32) {
        self.traps.remove(&signal);
    }

    /// Register a trap action for a pseudo-signal.
    pub fn set_pseudo(&mut self, sig: PseudoSignal, action: TrapAction) {
        self.pseudo_traps.insert(sig, action);
    }

    /// Look up the trap action for a pseudo-signal.
    pub fn get_pseudo(&self, sig: &PseudoSignal) -> Option<&TrapAction> {
        self.pseudo_traps.get(sig)
    }

    /// List all registered traps as `(name, action)` pairs.
    ///
    /// Pseudo-signal traps are listed first, then real signal traps
    /// sorted by signal number.
    pub fn list(&self) -> Vec<(String, &TrapAction)> {
        let mut result = Vec::new();

        // Pseudo-signals first (deterministic order)
        for pseudo in &[
            PseudoSignal::Exit,
            PseudoSignal::Debug,
            PseudoSignal::Err,
            PseudoSignal::Zerr,
        ] {
            if let Some(action) = self.pseudo_traps.get(pseudo) {
                result.push((pseudo.name().to_owned(), action));
            }
        }

        // Real signals sorted by number
        let mut sig_entries: Vec<_> = self.traps.iter().collect();
        sig_entries.sort_by_key(|(num, _)| *num);
        for (num, action) in sig_entries {
            let name = signal_number_to_name(*num);
            result.push((name.to_owned(), action));
        }

        result
    }
}

impl Default for TrapTable {
    fn default() -> Self {
        Self::new()
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    // ── signal name ↔ number ─────────────────────────────────────

    #[test]
    fn name_to_number_basic() {
        assert_eq!(signal_name_to_number("INT"), Some(Signal::SIGINT as i32));
        assert_eq!(signal_name_to_number("HUP"), Some(Signal::SIGHUP as i32));
        assert_eq!(signal_name_to_number("TERM"), Some(Signal::SIGTERM as i32));
        assert_eq!(signal_name_to_number("KILL"), Some(Signal::SIGKILL as i32));
    }

    #[test]
    fn name_to_number_with_sig_prefix() {
        assert_eq!(signal_name_to_number("SIGINT"), Some(Signal::SIGINT as i32));
        assert_eq!(
            signal_name_to_number("SIGTERM"),
            Some(Signal::SIGTERM as i32)
        );
    }

    #[test]
    fn name_to_number_case_insensitive() {
        assert_eq!(signal_name_to_number("int"), Some(Signal::SIGINT as i32));
        assert_eq!(
            signal_name_to_number("sigterm"),
            Some(Signal::SIGTERM as i32)
        );
    }

    #[test]
    fn name_to_number_numeric_string() {
        assert_eq!(signal_name_to_number("2"), Some(2));
        assert_eq!(signal_name_to_number("15"), Some(15));
        assert_eq!(signal_name_to_number("0"), Some(0));
    }

    #[test]
    fn name_to_number_unknown() {
        assert_eq!(signal_name_to_number("BOGUS"), None);
    }

    #[test]
    fn number_to_name_basic() {
        assert_eq!(signal_number_to_name(Signal::SIGINT as i32), "INT");
        assert_eq!(signal_number_to_name(Signal::SIGHUP as i32), "HUP");
        assert_eq!(signal_number_to_name(Signal::SIGTERM as i32), "TERM");
        assert_eq!(signal_number_to_name(Signal::SIGKILL as i32), "KILL");
    }

    #[test]
    fn number_to_name_zero() {
        assert_eq!(signal_number_to_name(0), "EXIT");
    }

    #[test]
    fn number_to_name_unknown() {
        assert_eq!(signal_number_to_name(999), "UNKNOWN");
    }

    #[test]
    fn roundtrip_all_signals() {
        for (sig, name) in SIGNAL_TABLE {
            let num = *sig as i32;
            assert_eq!(
                signal_name_to_number(name),
                Some(num),
                "name_to_number({name}) failed"
            );
            assert_eq!(
                signal_number_to_name(num),
                *name,
                "number_to_name({num}) failed"
            );
        }
    }

    // ── pseudo-signal parsing ────────────────────────────────────

    #[test]
    fn pseudo_signal_from_name() {
        assert_eq!(PseudoSignal::from_name("EXIT"), Some(PseudoSignal::Exit));
        assert_eq!(PseudoSignal::from_name("DEBUG"), Some(PseudoSignal::Debug));
        assert_eq!(PseudoSignal::from_name("ERR"), Some(PseudoSignal::Err));
        assert_eq!(PseudoSignal::from_name("ZERR"), Some(PseudoSignal::Zerr));
        assert_eq!(PseudoSignal::from_name("INT"), None);
    }

    #[test]
    fn pseudo_signal_names() {
        assert_eq!(PseudoSignal::Exit.name(), "EXIT");
        assert_eq!(PseudoSignal::Debug.name(), "DEBUG");
        assert_eq!(PseudoSignal::Err.name(), "ERR");
        assert_eq!(PseudoSignal::Zerr.name(), "ZERR");
    }

    // ── TrapTable set/get/remove ─────────────────────────────────

    #[test]
    fn set_and_get_signal_trap() {
        let mut table = TrapTable::new();
        let sigint = Signal::SIGINT as i32;

        table.set(sigint, TrapAction::Command("echo caught".into()));
        assert_eq!(
            table.get(sigint),
            Some(&TrapAction::Command("echo caught".into()))
        );
    }

    #[test]
    fn get_unset_signal_returns_none() {
        let table = TrapTable::new();
        assert_eq!(table.get(Signal::SIGINT as i32), None);
    }

    #[test]
    fn remove_signal_trap() {
        let mut table = TrapTable::new();
        let sigterm = Signal::SIGTERM as i32;

        table.set(sigterm, TrapAction::Ignore);
        assert!(table.get(sigterm).is_some());

        table.remove(sigterm);
        assert_eq!(table.get(sigterm), None);
    }

    #[test]
    fn overwrite_signal_trap() {
        let mut table = TrapTable::new();
        let sighup = Signal::SIGHUP as i32;

        table.set(sighup, TrapAction::Ignore);
        table.set(sighup, TrapAction::Default);
        assert_eq!(table.get(sighup), Some(&TrapAction::Default));
    }

    // ── TrapTable pseudo-signals ─────────────────────────────────

    #[test]
    fn set_and_get_pseudo_trap() {
        let mut table = TrapTable::new();
        table.set_pseudo(PseudoSignal::Exit, TrapAction::Command("cleanup".into()));
        assert_eq!(
            table.get_pseudo(&PseudoSignal::Exit),
            Some(&TrapAction::Command("cleanup".into()))
        );
    }

    #[test]
    fn get_unset_pseudo_returns_none() {
        let table = TrapTable::new();
        assert_eq!(table.get_pseudo(&PseudoSignal::Debug), None);
    }

    // ── TrapTable list ───────────────────────────────────────────

    #[test]
    fn list_empty_table() {
        let table = TrapTable::new();
        assert!(table.list().is_empty());
    }

    #[test]
    fn list_mixed_traps() {
        let mut table = TrapTable::new();
        table.set_pseudo(PseudoSignal::Exit, TrapAction::Command("bye".into()));
        table.set(Signal::SIGINT as i32, TrapAction::Ignore);
        table.set(Signal::SIGTERM as i32, TrapAction::Command("term".into()));

        let listed = table.list();
        assert_eq!(listed.len(), 3);

        // Pseudo-signals come first
        assert_eq!(listed[0].0, "EXIT");
        assert_eq!(listed[0].1, &TrapAction::Command("bye".into()));

        // Real signals sorted by number — INT < TERM
        assert_eq!(listed[1].0, "INT");
        assert_eq!(listed[1].1, &TrapAction::Ignore);
        assert_eq!(listed[2].0, "TERM");
        assert_eq!(listed[2].1, &TrapAction::Command("term".into()));
    }

    #[test]
    fn list_pseudo_order_is_deterministic() {
        let mut table = TrapTable::new();
        // Insert in reverse order — output should still be EXIT, DEBUG, ERR, ZERR
        table.set_pseudo(PseudoSignal::Zerr, TrapAction::Default);
        table.set_pseudo(PseudoSignal::Err, TrapAction::Default);
        table.set_pseudo(PseudoSignal::Debug, TrapAction::Default);
        table.set_pseudo(PseudoSignal::Exit, TrapAction::Default);

        let listed = table.list();
        let names: Vec<_> = listed.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names, vec!["EXIT", "DEBUG", "ERR", "ZERR"]);
    }

    // ── Default trait ────────────────────────────────────────────

    #[test]
    fn default_table_is_empty() {
        let table = TrapTable::default();
        assert!(table.list().is_empty());
    }
}
