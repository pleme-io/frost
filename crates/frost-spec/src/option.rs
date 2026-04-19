//! Declarative spec for a zsh shell option (setopt).

use serde::{Deserialize, Serialize};
use tatara_lisp::DeriveTataraDomain;

/// A `setopt`/`unsetopt` option — name, default, category, aliases.
///
/// zsh options have irregular authoring (mixed case, underscores,
/// abbreviations: `extendedglob` and `extended_glob` are the same
/// option). Declaring the full set in Lisp once, instead of scattering
/// `match` arms, means the aliases, defaults, and category are one
/// source of truth for parser, executor, completion, and docs.
///
/// ```lisp
/// (defoption :name "nullglob"     :default #f :category "glob")
/// (defoption :name "extendedglob"
///            :aliases ("extended_glob")
///            :default #f
///            :category "glob")
/// ```
#[derive(DeriveTataraDomain, Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(rename_all = "camelCase")]
#[tatara(keyword = "defoption")]
pub struct OptionSpec {
    pub name: String,

    #[serde(default)]
    pub aliases: Vec<String>,

    pub default: bool,

    pub category: String,

    pub deprecated: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_simple_option() {
        let specs: Vec<OptionSpec> = tatara_lisp::compile_typed(
            r#"(defoption :name "nullglob" :default #f :category "glob")"#,
        )
        .unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "nullglob");
        assert!(!specs[0].default);
        assert_eq!(specs[0].category, "glob");
    }

    #[test]
    fn parses_option_with_aliases() {
        let specs: Vec<OptionSpec> = tatara_lisp::compile_typed(
            r#"(defoption :name "extendedglob"
                        :aliases ("extended_glob")
                        :default #f
                        :category "glob")"#,
        )
        .unwrap();
        assert_eq!(specs[0].aliases, vec!["extended_glob".to_string()]);
    }
}
