//! Declarative spec for a frost builtin.

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// A builtin described by its invariant metadata — no logic.
///
/// Trivial builtins (`true`, `false`, `:`) are fully expressed by this
/// spec. Complex builtins (loops, `typeset`, `test`) still live in
/// `frost-builtins` as procedural code; the spec captures their flags,
/// aliases, and arity constraints, which is enough to drive help text,
/// completion, and CLI shape without duplication.
///
/// ```lisp
/// (defbuiltin :name "true"  :exit-code 0)
/// (defbuiltin :name "false" :exit-code 1)
/// (defbuiltin :name "[" :aliases ("test") :min-args 1)
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defbuiltin")]
pub struct BuiltinSpec {
    pub name: String,

    #[serde(default)]
    pub aliases: Vec<String>,

    /// Fixed exit code for trivial builtins; None means the builtin has logic.
    pub exit_code: Option<i32>,

    /// Minimum positional argument count (excluding flags).
    pub min_args: Option<u32>,

    /// Maximum positional argument count; None = unbounded.
    pub max_args: Option<u32>,

    /// If set, emit this deprecation message when the builtin is called.
    pub deprecated: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_trivial_builtin() {
        let specs: Vec<BuiltinSpec> =
            tatara_lisp::compile_typed(r#"(defbuiltin :name "true" :exit-code 0)"#).unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "true");
        assert_eq!(specs[0].exit_code, Some(0));
        assert!(specs[0].aliases.is_empty());
    }

    #[test]
    fn parses_builtin_with_aliases() {
        let specs: Vec<BuiltinSpec> = tatara_lisp::compile_typed(
            r#"(defbuiltin :name "[" :aliases ("test") :min-args 1)"#,
        )
        .unwrap();
        assert_eq!(specs[0].name, "[");
        assert_eq!(specs[0].aliases, vec!["test".to_string()]);
        assert_eq!(specs[0].min_args, Some(1));
        assert_eq!(specs[0].exit_code, None);
    }

    #[test]
    fn parses_multiple_defbuiltins() {
        let src = r#"
            (defbuiltin :name "true"  :exit-code 0)
            (defbuiltin :name "false" :exit-code 1)
            (defbuiltin :name ":"     :exit-code 0)
        "#;
        let specs: Vec<BuiltinSpec> = tatara_lisp::compile_typed(src).unwrap();
        assert_eq!(specs.len(), 3);
        assert_eq!(specs[0].name, "true");
        assert_eq!(specs[1].name, "false");
        assert_eq!(specs[2].name, ":");
    }
}
