//! Zsh shell option management.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellOption {
    // Changing Directories
    AutoCd,
    AutoPushd,
    CdSilent,
    ChaseDots,
    ChaseLinks,
    PushDIgnoreDups,
    // Expansion & Globbing
    BadPattern,
    BareGlobQual,
    BraceCcl,
    CaseGlob,
    CaseMatch,
    ExtendedGlob,
    Glob,
    GlobDots,
    GlobSubst,
    KshGlob,
    MarkDirs,
    NoMatch,
    NullGlob,
    NumericGlobSort,
    RcExpandParam,
    // I/O
    Aliases,
    Clobber,
    ClobberRestrict,
    InteractiveComments,
    MultIos,
    NoClobber,
    PathDirs,
    RcQuotes,
    RmStarSilent,
    ShortLoops,
    // Completion
    AlwaysToEnd,
    AutoList,
    AutoMenu,
    AutoParamSlash,
    MenuComplete,
    // History
    AppendHistory,
    ExtendedHistory,
    HistExpireDupsFirst,
    HistFindNoDups,
    HistIgnoreAllDups,
    HistIgnoreDups,
    HistIgnoreSpace,
    HistReduceBlanks,
    HistSaveByCopy,
    HistVerify,
    ShareHistory,
    VerboseHistory,
    // Job Control
    BgNice,
    CheckJobs,
    Hup,
    Monitor,
    Notify,
    // Scripts/Functions
    CBasics,
    CPrecedences,
    DebugBeforeCmd,
    ErrExit,
    ErrReturn,
    Exec,
    FunctionArgZero,
    LocalLoops,
    LocalOptions,
    LocalPatterns,
    LocalTraps,
    MultiFuncDef,
    PipeFail,
    Verbose,
    Xtrace,
    // Shell Emulation
    BashRematch,
    BsdEcho,
    KshArrays,
    KshAutoload,
    KshZeroSubscript,
    PosixAliases,
    PosixBuiltins,
    PosixIdentifiers,
    PosixStrings,
    PosixTraps,
    ShFileExpansion,
    ShGlob,
    ShNullCmd,
    ShOptionLetters,
    ShWordSplit,
    // Prompting
    Prompt,
    PromptBang,
    PromptCr,
    PromptPercent,
    PromptSubst,
    // Shell State
    Interactive,
    Login,
    Privileged,
    // ZLE
    Beep,
    Emacs,
    Vi,
}

#[derive(Debug, Clone)]
pub struct Options {
    enabled: HashSet<ShellOption>,
}

impl Options {
    pub fn set(&mut self, opt: ShellOption) {
        self.enabled.insert(opt);
    }

    pub fn unset(&mut self, opt: ShellOption) {
        self.enabled.remove(&opt);
    }

    pub fn is_set(&self, opt: ShellOption) -> bool {
        self.enabled.contains(&opt)
    }

    /// Look up an option by its normalized name.
    pub fn from_name(name: &str) -> Option<ShellOption> {
        let normalized = normalize_option_name(name);

        // Handle "no" prefix
        let (negated, base) = if let Some(rest) = normalized.strip_prefix("no") {
            (true, rest.to_string())
        } else {
            (false, normalized)
        };

        let opt = match base.as_str() {
            "autocd" => ShellOption::AutoCd,
            "autopushd" => ShellOption::AutoPushd,
            "cdsilent" => ShellOption::CdSilent,
            "chasedots" => ShellOption::ChaseDots,
            "chaselinks" => ShellOption::ChaseLinks,
            "pushdignoredups" => ShellOption::PushDIgnoreDups,
            "badpattern" => ShellOption::BadPattern,
            "bareglobqual" => ShellOption::BareGlobQual,
            "braceccl" => ShellOption::BraceCcl,
            "caseglob" => ShellOption::CaseGlob,
            "casematch" => ShellOption::CaseMatch,
            "extendedglob" => ShellOption::ExtendedGlob,
            "glob" => ShellOption::Glob,
            "globdots" => ShellOption::GlobDots,
            "globsubst" => ShellOption::GlobSubst,
            "kshglob" => ShellOption::KshGlob,
            "markdirs" => ShellOption::MarkDirs,
            "match" if negated => return Some(ShellOption::NoMatch),
            "nomatch" => ShellOption::NoMatch,
            "nullglob" => ShellOption::NullGlob,
            "numericglobsort" => ShellOption::NumericGlobSort,
            "rcexpandparam" => ShellOption::RcExpandParam,
            "aliases" => ShellOption::Aliases,
            "clobber" if negated => return Some(ShellOption::NoClobber),
            "clobber" => ShellOption::Clobber,
            "clobberrestrict" => ShellOption::ClobberRestrict,
            "interactivecomments" => ShellOption::InteractiveComments,
            "multios" => ShellOption::MultIos,
            "noclobber" => ShellOption::NoClobber,
            "pathdirs" => ShellOption::PathDirs,
            "rcquotes" => ShellOption::RcQuotes,
            "rmstarsilent" => ShellOption::RmStarSilent,
            "shortloops" => ShellOption::ShortLoops,
            "alwaystoend" => ShellOption::AlwaysToEnd,
            "autolist" => ShellOption::AutoList,
            "automenu" => ShellOption::AutoMenu,
            "autoparamslash" => ShellOption::AutoParamSlash,
            "menucomplete" => ShellOption::MenuComplete,
            "appendhistory" => ShellOption::AppendHistory,
            "extendedhistory" => ShellOption::ExtendedHistory,
            "histexpiredupsfirst" => ShellOption::HistExpireDupsFirst,
            "histfindnodups" => ShellOption::HistFindNoDups,
            "histignorealldups" => ShellOption::HistIgnoreAllDups,
            "histignoredups" => ShellOption::HistIgnoreDups,
            "histignorespace" => ShellOption::HistIgnoreSpace,
            "histreduceblanks" => ShellOption::HistReduceBlanks,
            "histsavebycopy" => ShellOption::HistSaveByCopy,
            "histverify" => ShellOption::HistVerify,
            "sharehistory" => ShellOption::ShareHistory,
            "bgnice" => ShellOption::BgNice,
            "checkjobs" => ShellOption::CheckJobs,
            "hup" => ShellOption::Hup,
            "monitor" => ShellOption::Monitor,
            "notify" => ShellOption::Notify,
            "cbasics" => ShellOption::CBasics,
            "cprecedences" => ShellOption::CPrecedences,
            "debugbeforecmd" => ShellOption::DebugBeforeCmd,
            "errexit" => ShellOption::ErrExit,
            "errreturn" => ShellOption::ErrReturn,
            "exec" => ShellOption::Exec,
            "functionargzero" => ShellOption::FunctionArgZero,
            "localloops" => ShellOption::LocalLoops,
            "localoptions" => ShellOption::LocalOptions,
            "localpatterns" => ShellOption::LocalPatterns,
            "localtraps" => ShellOption::LocalTraps,
            "multifuncdef" => ShellOption::MultiFuncDef,
            "pipefail" => ShellOption::PipeFail,
            "verbose" => ShellOption::Verbose,
            "xtrace" => ShellOption::Xtrace,
            "bashrematch" => ShellOption::BashRematch,
            "bsdecho" => ShellOption::BsdEcho,
            "ksharrays" => ShellOption::KshArrays,
            "kshautoload" => ShellOption::KshAutoload,
            "kshzerosubscript" => ShellOption::KshZeroSubscript,
            "posixaliases" => ShellOption::PosixAliases,
            "posixbuiltins" => ShellOption::PosixBuiltins,
            "posixidentifiers" => ShellOption::PosixIdentifiers,
            "posixstrings" => ShellOption::PosixStrings,
            "posixtraps" => ShellOption::PosixTraps,
            "shfileexpansion" => ShellOption::ShFileExpansion,
            "shglob" => ShellOption::ShGlob,
            "shnullcmd" => ShellOption::ShNullCmd,
            "shoptionletters" => ShellOption::ShOptionLetters,
            "shwordsplit" => ShellOption::ShWordSplit,
            "prompt" => ShellOption::Prompt,
            "promptbang" => ShellOption::PromptBang,
            "promptcr" => ShellOption::PromptCr,
            "promptpercent" => ShellOption::PromptPercent,
            "promptsubst" => ShellOption::PromptSubst,
            "interactive" => ShellOption::Interactive,
            "login" => ShellOption::Login,
            "privileged" => ShellOption::Privileged,
            "beep" => ShellOption::Beep,
            "emacs" => ShellOption::Emacs,
            "vi" => ShellOption::Vi,
            _ => return None,
        };

        if negated {
            // "noglob" → Some(Glob) but the caller should UNSET it
            // We just return the option; caller decides set/unset based on context
            Some(opt)
        } else {
            Some(opt)
        }
    }

    /// Check if a name represents a negated option (starts with "no").
    pub fn is_negated(name: &str) -> bool {
        let normalized = normalize_option_name(name);
        normalized.starts_with("no")
            && Self::from_name(&normalized[2..]).is_some()
    }
}

/// Normalize an option name: remove underscores, lowercase.
pub fn normalize_option_name(name: &str) -> String {
    name.chars()
        .filter(|c| *c != '_')
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl Default for Options {
    fn default() -> Self {
        let enabled = HashSet::from([
            ShellOption::BgNice,
            ShellOption::Clobber,
            ShellOption::Exec,
            ShellOption::ExtendedGlob,
            ShellOption::Glob,
            ShellOption::Interactive,
            ShellOption::InteractiveComments,
            ShellOption::Monitor,
            ShellOption::Notify,
            ShellOption::NoMatch,
            ShellOption::Prompt,
            ShellOption::PromptSubst,
            ShellOption::PromptCr,
            ShellOption::PromptPercent,
            ShellOption::HistIgnoreDups,
            ShellOption::HistReduceBlanks,
            ShellOption::HistSaveByCopy,
            ShellOption::ShareHistory,
            ShellOption::Aliases,
            ShellOption::FunctionArgZero,
            ShellOption::MultIos,
            ShellOption::ShortLoops,
            ShellOption::CaseMatch,
            ShellOption::Hup,
            ShellOption::BareGlobQual,
        ]);
        Self { enabled }
    }
}
