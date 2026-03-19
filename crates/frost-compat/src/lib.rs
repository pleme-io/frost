//! Zsh compatibility test framework for frost.
//!
//! Parses `.ztst` test files from the zsh test suite and runs them
//! against the frost binary, reporting pass/fail/crash status.

pub mod runner;
pub mod ztst;

pub use runner::{Summary, TestResult, TestStatus, run_test_file};
pub use ztst::{TestFile, TestCase, parse_ztst};
