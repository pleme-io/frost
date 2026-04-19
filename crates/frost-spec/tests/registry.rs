//! Cross-domain registry integration: multiple frost domains in one document.

use frost_spec::{BuiltinSpec, OptionSpec};

#[test]
fn both_domains_register_and_resolve() {
    frost_spec::register_all();

    let keywords = tatara_lisp::domain::registered_keywords();
    assert!(keywords.contains(&"defbuiltin"), "missing defbuiltin: {keywords:?}");
    assert!(keywords.contains(&"defoption"), "missing defoption: {keywords:?}");
}

#[test]
fn mixed_document_compiles_each_domain() {
    let src = r#"
        (defbuiltin :name "true"  :exit-code 0)
        (defbuiltin :name "false" :exit-code 1)
    "#;
    let specs: Vec<BuiltinSpec> = tatara_lisp::compile_typed(src).unwrap();
    assert_eq!(specs.len(), 2);

    let src = r#"
        (defoption :name "nullglob"     :default #f :category "glob")
        (defoption :name "extendedglob" :default #f :category "glob")
    "#;
    let opts: Vec<OptionSpec> = tatara_lisp::compile_typed(src).unwrap();
    assert_eq!(opts.len(), 2);
}
