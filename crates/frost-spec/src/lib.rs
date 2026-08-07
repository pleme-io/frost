//! Typed domain specs for frost, authored as Lisp via tatara.
//!
//! The shell has several domains that are pure typed configuration:
//! builtins, shell options, ZLE widgets, parameter flags. Each can be
//! declaratively authored in Lisp and compiled into a Rust value via
//! `#[derive(TataraDomain)]`.
//!
//! ```lisp
//! (defbuiltin :name "true"  :exit-code 0)
//! (defbuiltin :name "false" :exit-code 1)
//! (defbuiltin :name "["     :aliases ("test"))
//!
//! (defoption :name "nullglob"     :default #f :category "glob")
//! (defoption :name "extendedglob" :default #f :category "glob")
//! ```
//!
//! ## What this POC proves
//!
//! 1. `#[derive(DeriveTataraDomain)]` from `tatara-lisp` compiles and
//!    builds in the frost workspace alongside existing crates.
//! 2. Lisp forms round-trip into typed Rust values with zero hand-written
//!    parser code.
//! 3. The registry dispatches keywords (`defbuiltin`, `defoption`) to
//!    the right domain type.
//! 4. Multiple domains coexist in one registry — the building block for
//!    composing shell configuration from a single Lisp document.
//!
//! Future work: migrate trivial builtins (true/false/:/echo) to be
//! spec-driven, then widgets, then parameter flags. The lexer, parser
//! AST, and executor control flow stay procedural (anti-pattern to
//! tataraize those).

pub mod builtin;
pub mod option;

pub use builtin::BuiltinSpec;
pub use option::OptionSpec;

/// Register all frost domain specs in the global tatara registry.
/// Call once at process startup.
///
/// A refusal means some other type in the linked binary already claims
/// `defbuiltin` or `defoption`. The incumbent keeps the keyword, so
/// frost's handler is simply never reached — and discarding the refusal
/// here would make that silent, which is precisely the defect
/// `register`'s `Result` was introduced to kill. Propagated instead: the
/// caller decides, and the error names both the incumbent type and the
/// challenger.
///
/// # Errors
/// Returns the first [`KeywordCollision`](tatara_lisp::KeywordCollision)
/// if either keyword is already held by a different type.
pub fn register_all() -> Result<(), tatara_lisp::KeywordCollision> {
    tatara_lisp::domain::register::<BuiltinSpec>()?;
    tatara_lisp::domain::register::<OptionSpec>()
}
