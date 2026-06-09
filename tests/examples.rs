//! The examples *are* the fixtures: these run the real example assembly over the tracked
//! sources and assert the artifacts. Network-gated (vendoring downloads from npm), so
//! `#[ignore]`d — run with `--include-ignored`. Needs `typescript` + `scss` (on under
//! `--all-features`).
#![cfg(all(feature = "typescript", feature = "scss"))]

use std::path::{Path, PathBuf};

use web_modules::build::{build, BuildOptions};
use web_modules::importmap::Importmap;
use web_modules::tsconfig::tsconfig_paths;
use web_modules::typescript::compile_str;
use web_modules::vendor::{read_package_json, specs_from_package_json, vendor, PackageSpec};
use web_modules::Mount;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

#[test]
#[ignore = "network: vendors npm packages"]
fn compose_assembly_co_generates_consistent_artifacts() {
    let ex = examples();
    let compose_web = ex.join("compose/web");

    // Same shape as examples/compose/src/main.rs, but vendoring into a temp dir.
    let (mut specs, sibling_mounts) = read_package_json(&compose_web.join("package.json")).unwrap();
    specs.push(PackageSpec::npm("d3", "^7").no_imports());
    specs.push(PackageSpec::npm("bootstrap", "^5").no_imports());

    let tmp = tempfile::tempdir().unwrap();
    let vendor_root = tmp.path().join("web_modules");
    let vendored = vendor(&vendor_root, "/web_modules", &specs).unwrap();

    let mut mounts = sibling_mounts;
    mounts.push(Mount::root(&compose_web));

    // Co-generated from the one mount set.
    let mut importmap = vendored;
    importmap.extend(Importmap::from_mounts(&mounts));
    let tsconfig = tsconfig_paths(&mounts, &ex);

    // Vendored runtime: d3 UMD bundle + Bootstrap SCSS source are staged.
    assert!(vendor_root.join("d3/dist/d3.min.js").is_file());
    assert!(vendor_root.join("bootstrap/scss/bootstrap.scss").is_file());

    // The import map resolves the registry dep and both sibling components by name.
    assert!(importmap.resolves("lit"));
    assert!(importmap.resolves("counter/counter.js"));
    assert!(importmap.resolves("chart/chart.js"));

    // tsconfig carries the same component specifiers (the drift guard, end to end).
    let paths = tsconfig.as_object().unwrap();
    assert!(paths.contains_key("counter/*"));
    assert!(paths.contains_key("chart/*"));

    // The glue compiles, keeping its by-name imports for the browser to resolve.
    let app_ts = std::fs::read_to_string(compose_web.join("app.ts")).unwrap();
    let app_js = compile_str(&app_ts, &compose_web.join("app.ts")).unwrap();
    assert!(app_js.contains("counter/counter.js"));
    assert!(app_js.contains("chart/chart.js"));
}

#[test]
#[ignore = "network: vendors npm packages"]
fn lit_element_bake_emits_components_and_inlines_importmap() {
    let web = examples().join("lit-element/web");
    let tmp = tempfile::tempdir().unwrap();
    let out = tmp.path().join("dist");

    // Mirror examples/lit-element/build.rs: package.json deps + two programmatic tweaks.
    let mut specs = specs_from_package_json(&web.join("package.json")).unwrap();
    specs.push(PackageSpec::npm("@popperjs/core", "^2").imports([
        ("@popperjs/core", "dist/esm/index.js"),
        ("@popperjs/core/", "dist/esm/"),
    ]));
    specs.push(PackageSpec::npm("@webcomponents/webcomponentsjs", "^2").no_imports());

    build(&BuildOptions {
        specs: &specs,
        src: &web,
        out: &out,
        mount: "/web_modules",
        html: "<!doctype html><html><head>{importmap}<script type=\"module\" src=\"/app.js\"></script></head><body><counter-card></counter-card></body></html>",
        template: None,
        output: Default::default(),
    })
    .unwrap();

    // The split produced both the standalone entry and the reusable component.
    assert!(out.join("app.js").is_file());
    assert!(out.join("counter.js").is_file());
    // app.js imports the reusable component; counter.js carries no Bootstrap JS.
    let app_js = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(app_js.contains("./counter.js"));
    let counter_js = std::fs::read_to_string(out.join("counter.js")).unwrap();
    assert!(counter_js.contains("counter-tick"));
    assert!(!counter_js.contains("bootstrap"));

    // The import map is both inlined into index.html and emitted standalone, resolving
    // the bare specifiers the sources import.
    let index = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(index.contains("type=\"importmap\""));
    let map = Importmap::from_json_file(&out.join("importmap.json")).unwrap();
    assert!(map.resolves("lit") && map.resolves("bootstrap"));
}

// Unlike its network-gated siblings, the `embedded` example vendors nothing, so this runs
// offline (no `#[ignore]`). It pins the *output-optimization* wiring the example turns on:
// baking the same sources with `Output::optimized()` instead of the default yields smaller
// JS and writes real `.gz` sidecars. gzip *serving* under `Accept-Encoding` is covered by
// `tests/output.rs`; here we only assert the bake-level result over the tracked sources.
#[test]
#[cfg(all(feature = "minify", feature = "compress"))]
fn embedded_bake_minifies_and_gzips() {
    use web_modules::build::Output;

    let web = examples().join("embedded/web");
    let html = "<!doctype html>{importmap}<link rel=stylesheet href=/styles.css>\
                <script type=module src=/app.js></script>";

    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    let optimized = tmp.path().join("optimized");

    build(&BuildOptions {
        specs: &[],
        src: &web,
        out: &plain,
        mount: "/web_modules",
        html,
        template: None,
        output: Output::default(), // both off
    })
    .unwrap();
    build(&BuildOptions {
        specs: &[],
        src: &web,
        out: &optimized,
        mount: "/web_modules",
        html,
        template: None,
        output: Output::optimized(), // minify + gzip
    })
    .unwrap();

    // (1) Minification shrank the emitted JS (pretty codegen vs. minified — a wide margin,
    //     so this is robust to oxc version drift, unlike a "string X is gone" check).
    let plain_js = std::fs::metadata(plain.join("app.js")).unwrap().len();
    let min_js = std::fs::metadata(optimized.join("app.js")).unwrap().len();
    assert!(
        min_js < plain_js,
        "minified app.js ({min_js} B) should be smaller than plain ({plain_js} B)"
    );

    // (2) gzip sidecars were written for the servable assets...
    assert!(optimized.join("app.js.gz").is_file(), "app.js.gz sidecar");
    assert!(
        optimized.join("styles.css.gz").is_file(),
        "styles.css.gz sidecar"
    );

    // (3) ...and the sidecar is a real gzip stream (magic bytes 1f 8b). Asserting the
    //     gzip magic rather than "smaller than the original" stays correct even when the
    //     asset is tiny enough that gzip's ~18-byte framing exceeds the savings.
    let gz = std::fs::read(optimized.join("app.js.gz")).unwrap();
    assert_eq!(&gz[..2], &[0x1f, 0x8b], "app.js.gz is a gzip stream");

    // (4) The import map is inlined into index.html (empty here — this example vendors
    //     nothing — but the tag is still emitted).
    let index = std::fs::read_to_string(optimized.join("index.html")).unwrap();
    assert!(index.contains("type=\"importmap\""));
}
