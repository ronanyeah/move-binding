use move_binding::move_codegen::{LocalCodegen, MoveCodegen};

#[test]
fn test_generate_package_summaries() {
    let base_path = "crate";
    let alias = "large";

    let c1 = {
        let package = "0x7daa0be72e7c857f40f8aefa74ac835dccfa4e25ec40a92665e54249d93d3892";
        let generated_code = MoveCodegen::expand(
            move_binding::SuiNetwork::Mainnet,
            &package,
            &alias,
            &base_path,
        )
        .unwrap();

        prettyplease::unparse(&syn::parse2(generated_code).unwrap())
    };

    let c2 = {
        let pkg_path = std::path::Path::new("../package_summaries");
        let summary = move_binding::package_summaries_parser::parse_summaries(pkg_path).unwrap();
        let stream = LocalCodegen::expand(&summary, &alias, &base_path, Some(2)).unwrap();

        prettyplease::unparse(&syn::parse2(stream).unwrap())
    };

    assert_eq!(c1, c2);
}
