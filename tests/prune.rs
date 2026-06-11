//! The import-graph prune, exercised end-to-end through the real `build()` + vendoring.
//! (The algorithm itself is unit-tested with synthetic fixtures in `src/build/pipeline.rs`;
//! this proves the opt-in path works against an actually-vendored tree.) Network-gated —
//! `#[ignore]`d, run with `--include-ignored`. Needs `typescript` + `scss` (on under
//! `--all-features` / `--features full`).
#![cfg(all(feature = "typescript", feature = "scss"))]

use web_modules::build::{build, BuildOptions, Output};
use web_modules::vendor::PackageSpec;

#[test]
#[ignore = "network: vendors npm packages"]
fn build_with_prune_drops_the_unimported_package() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("web");
    std::fs::create_dir_all(&src).unwrap();
    // The app imports only `mitt`; `nanoid` is vendored but nothing imports it.
    std::fs::write(
        src.join("app.ts"),
        "import mitt from \"mitt\";\nexport const bus = mitt();\n",
    )
    .unwrap();
    let out = tmp.path().join("dist");

    build(&BuildOptions {
        specs: &[
            PackageSpec::npm("mitt", "^3"),
            PackageSpec::npm("nanoid", "^5"),
        ],
        src: &src,
        out: &out,
        mount: "/web_modules",
        html: "<!doctype html>{importmap}<script type=\"module\" src=\"/app.js\"></script>",
        template: None,
        output: Output::default().with_prune_unused(true),
    })
    .unwrap();

    // `mitt` is reachable from the app → kept; `nanoid` is unreachable → pruned from both
    // the vendored tree on disk and the emitted import map.
    assert!(
        out.join("web_modules/mitt").is_dir(),
        "imported package kept on disk"
    );
    assert!(
        !out.join("web_modules/nanoid").exists(),
        "unimported package pruned from disk"
    );
    let importmap = std::fs::read_to_string(out.join("importmap.json")).unwrap();
    assert!(
        importmap.contains("mitt"),
        "kept package stays in the import map: {importmap}"
    );
    assert!(
        !importmap.contains("nanoid"),
        "pruned package removed from the import map: {importmap}"
    );
}
