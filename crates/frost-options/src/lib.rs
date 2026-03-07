//! Zsh shell option management.

use std::collections::HashSet;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ShellOption {
    AutoCd,
    AutoPushd,
    BgNice,
    ClobberRestrict,
    ExtendedGlob,
    ExtendedHistory,
    GlobDots,
    HistExpireDupsFirst,
    HistFindNoDups,
    HistIgnoreDups,
    HistIgnoreSpace,
    HistReduceBlanks,
    HistSaveByCopy,
    HistVerify,
    Interactive,
    InteractiveComments,
    Login,
    Monitor,
    Notify,
    NoClobber,
    NoMatch,
    Prompt,
    PromptSubst,
    PushDIgnoreDups,
    ShareHistory,
    ShGlob,
    VerboseHistory,
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
}

impl Default for Options {
    fn default() -> Self {
        let enabled = HashSet::from([
            ShellOption::BgNice,
            ShellOption::ExtendedGlob,
            ShellOption::Interactive,
            ShellOption::InteractiveComments,
            ShellOption::Monitor,
            ShellOption::Notify,
            ShellOption::NoMatch,
            ShellOption::Prompt,
            ShellOption::PromptSubst,
            ShellOption::HistIgnoreDups,
            ShellOption::HistReduceBlanks,
            ShellOption::HistSaveByCopy,
            ShellOption::ShareHistory,
        ]);
        Self { enabled }
    }
}
