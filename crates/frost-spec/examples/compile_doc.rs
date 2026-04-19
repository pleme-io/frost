//! Demo: compile a mixed Lisp document into typed frost specs.
//!
//! Run: `cargo run -p frost-spec --example compile_doc`

use frost_spec::{BuiltinSpec, OptionSpec};

fn main() {
    frost_spec::register_all();

    let builtins_src = r#"
        (defbuiltin :name "true"  :exit-code 0)
        (defbuiltin :name "false" :exit-code 1)
        (defbuiltin :name ":"     :exit-code 0)
        (defbuiltin :name "["     :aliases ("test") :min-args 1)
    "#;

    let options_src = r#"
        (defoption :name "nullglob"     :default #f :category "glob")
        (defoption :name "extendedglob" :aliases ("extended_glob")
                                         :default #f :category "glob")
        (defoption :name "noclobber"    :default #f :category "redirect")
    "#;

    let builtins: Vec<BuiltinSpec> = tatara_lisp::compile_typed(builtins_src).unwrap();
    let options: Vec<OptionSpec> = tatara_lisp::compile_typed(options_src).unwrap();

    println!("=== registered keywords ===");
    for kw in tatara_lisp::domain::registered_keywords() {
        println!("  {kw}");
    }

    println!("\n=== {} builtins compiled from Lisp ===", builtins.len());
    for b in &builtins {
        let exit = b
            .exit_code
            .map(|c| format!("exit={c}"))
            .unwrap_or_else(|| "logic".into());
        let aliases = if b.aliases.is_empty() {
            String::new()
        } else {
            format!(" (aliases: {})", b.aliases.join(", "))
        };
        println!("  {:<8} {}{aliases}", b.name, exit);
    }

    println!("\n=== {} options compiled from Lisp ===", options.len());
    for o in &options {
        let aliases = if o.aliases.is_empty() {
            String::new()
        } else {
            format!(" (aliases: {})", o.aliases.join(", "))
        };
        println!(
            "  {:<14} default={:<5} category={}{aliases}",
            o.name, o.default, o.category
        );
    }

    println!("\n=== serde JSON surface (same types, no extra code) ===");
    println!(
        "{}",
        serde_json::to_string_pretty(&builtins[3]).unwrap()
    );
}
