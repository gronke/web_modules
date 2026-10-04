//! `Importmap::resolve` against the table the lit-element e2e checks in the browsers.

use web_modules::importmap::Importmap;

const TABLE: &str = include_str!("importmap_parity.json");

#[test]
fn resolve_answers_every_case_of_the_table() {
    let map = Importmap::from_json_str(TABLE, "importmap_parity.json").unwrap();
    let table: serde_json::Value = serde_json::from_str(TABLE).unwrap();
    let mismatches: Vec<String> = table["cases"]
        .as_array()
        .unwrap()
        .iter()
        .filter_map(|case| {
            let specifier = case["specifier"].as_str().unwrap();
            let want = case["address"].as_str();
            let got = map.resolve(specifier);
            (got.as_deref() != want).then(|| {
                format!(
                    "{specifier:?}: want {want:?}, got {got:?} ({})",
                    case["why"]
                )
            })
        })
        .collect();
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
}

#[cfg(feature = "typescript")]
#[test]
#[ignore = "network: vendors lit"]
fn the_build_refuses_an_import_the_browser_refuses() {
    use web_modules::build::{build, BuildOptions, Output, Processors};
    use web_modules::vendor::PackageSpec;

    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(
        root.join("app.js"),
        "import \"lit/../lit-html/lit-html.js\";\n",
    )
    .unwrap();
    let err = build(&BuildOptions {
        specs: &[PackageSpec::npm("lit", "^3")],
        roots: std::slice::from_ref(&root),
        out: &dir.path().join("dist"),
        mount: "/web_modules",
        html: "<!doctype html>{importmap}",
        template: None,
        processors: Processors::default(),
        output: Output::default(),
    })
    .unwrap_err()
    .to_string();
    assert!(
        err.contains("app.js: import \"lit/../lit-html/lit-html.js\""),
        "{err}"
    );
}
