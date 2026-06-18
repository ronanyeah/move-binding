use move_binding::move_codegen::LocalCodegen;
use summary_parser::parse_summaries;

#[test]
fn local_codegen_large_package() {
    let pkg_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/package_summaries");
    let summary = parse_summaries(&pkg_path).unwrap();
    let stream = LocalCodegen::expand(&summary, "large", "crate", Some(2)).unwrap();
    let rendered = prettyplease::unparse(&syn::parse2(stream).unwrap());

    // `Drop` references `Coin<T>` and `Balance<T>`; `Coin`'s fields transitively
    // reference `Balance`, so the stub generator must walk to fixpoint.
    assert!(rendered.contains("pub struct Coin<"), "Coin stub missing");
    assert!(
        rendered.contains("pub struct Balance<"),
        "Balance stub missing — transitive fixpoint walk regressed?"
    );
    assert!(rendered.contains("pub struct Table<"), "Table stub missing");

    // create_fun's param-name hack — should use the real Move names rather than p0/p1.
    assert!(
        rendered.contains("root: Arg<Vec<u8>>"),
        "expected real parameter name `root`, got generic placeholder"
    );
    assert!(rendered.contains("wallet_count: Arg<u32>"));

    // Locks in the full output. Regenerate intentional changes with `cargo insta accept`.
    insta::assert_snapshot!("local_codegen__large", rendered);
}
