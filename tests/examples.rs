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
use web_modules::vendor::{
    keep_browser_assets, keep_sources, read_package_json, source_specs_from_package_json,
    specs_from_package_json, vendor, PackageSpec,
};
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

/// A dependency that publishes only TypeScript, compiled by vendoring into the layout its
/// own `tsconfig.json` declares. The assertion that matters is that what lands in
/// `web_modules/` is browser-ready: compiled `.js` where the manifest points, no `.ts` at all.
#[test]
#[ignore = "network: fetches a git archive and vendors npm packages"]
fn esptool_git_compiles_a_source_dependency_into_the_vendor_tree() {
    let ex = examples();
    let web = ex.join("esptool-git/web");
    let package_json = web.join("package.json");

    // Same shape as examples/esptool-git/src/main.rs, but vendoring into a temp dir.
    let tmp = tempfile::tempdir().unwrap();
    let vendor_root = tmp.path().join("web_modules");
    let (mut specs, mut mounts) = read_package_json(&package_json).unwrap();

    // The source dependency is not in the plain vend list; it comes from its own reader.
    assert!(!specs.iter().any(|s| s.name() == "fork-esptool-js"));
    specs.extend(source_specs_from_package_json(&package_json).unwrap());

    let importmap = vendor(&vendor_root, "/web_modules", &specs).unwrap();
    mounts.push(Mount::root(&web));

    // Compiled into `lib/`, which is what this package's tsconfig `outDir` names and where
    // its `main`/`module` already point — so no entry had to be guessed.
    let pkg = vendor_root.join("esptool-js");
    assert!(pkg.join("lib/index.js").is_file(), "compiled entry");
    assert!(pkg.join("lib/esploader.js").is_file());
    assert!(
        pkg.join("lib/targets/stub_flasher/stub_flasher_32s3.json")
            .is_file(),
        "assets a dynamic import reaches are copied beside the compiled output"
    );
    assert!(
        !pkg.join("src").exists(),
        "the TypeScript was an intermediate"
    );
    assert_eq!(
        walk_ts(&pkg).len(),
        0,
        "nothing uncompiled may ship: {:?}",
        walk_ts(&pkg)
    );

    // The bare specifier is the dependency key, not the repository it came from.
    assert!(importmap.resolves("esptool-js"));
    assert!(!importmap.resolves("fork-esptool-js"));
    // And the prebuilt registry deps it imports are there too.
    assert!(importmap.resolves("pako"));
    assert!(importmap.resolves("atob-lite"));

    // `keep_sources` is what let `src/` through to be compiled at all.
    assert_eq!(keep_browser_assets("src/esploader.ts"), None);
    assert!(keep_sources("src/esploader.ts").is_some());

    // The app compiles, keeping the bare import for the browser to resolve.
    let app_ts = std::fs::read_to_string(web.join("app.ts")).unwrap();
    let app_js = compile_str(&app_ts, &web.join("app.ts")).unwrap();
    assert!(app_js.contains("esptool-js"));
}

/// Every TypeScript source under `dir`, for the "nothing uncompiled ships" assertion.
///
/// Walked with the crate's own helper: a vendored tree came out of an archive, and a
/// symbolic link in one is a way out of the directory being checked.
fn walk_ts(dir: &Path) -> Vec<PathBuf> {
    web_modules::walk::files_within(dir)
        .unwrap()
        .into_iter()
        .filter(|rel| {
            matches!(
                rel.extension().and_then(|e| e.to_str()),
                Some("ts" | "tsx" | "mts" | "cts")
            )
        })
        .collect()
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
        roots: std::slice::from_ref(&web),
        out: &out,
        mount: "/web_modules",
        html: "<!doctype html><html><head>{importmap}<script type=\"module\" src=\"/app.js\"></script></head><body><counter-card></counter-card></body></html>",
        template: None,
        processors: Default::default(),
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
// baking the same sources with the optimized policy set instead of the defaults yields
// smaller JS, real `.gz` sidecars, a linked source map, and the legal banner collected
// into `app.js.LEGAL.txt`. gzip *serving* under `Accept-Encoding` is covered by
// `tests/output.rs`; here we only assert the bake-level result over the tracked sources.
#[test]
#[cfg(all(feature = "minify", feature = "compress"))]
fn embedded_bake_minifies_gzips_and_writes_sidecars() {
    use web_modules::build::{Output, Processors};
    use web_modules::Comments;

    let web = examples().join("embedded/web");
    let html = "<!doctype html>{importmap}<link rel=stylesheet href=/styles.css>\
                <script type=module src=/app.js></script>";

    let tmp = tempfile::tempdir().unwrap();
    let plain = tmp.path().join("plain");
    let optimized = tmp.path().join("optimized");

    build(&BuildOptions {
        specs: &[],
        roots: std::slice::from_ref(&web),
        out: &plain,
        mount: "/web_modules",
        html,
        template: None,
        processors: Default::default(),
        output: Output::default(), // both off
    })
    .unwrap();
    // Mirror examples/embedded/build.rs: the optimized bake also emits source maps and
    // collects legal comments.
    let mut processors = Processors::default();
    processors.sourcemap = true;
    build(&BuildOptions {
        specs: &[],
        roots: std::slice::from_ref(&web),
        out: &optimized,
        mount: "/web_modules",
        html,
        template: None,
        processors,
        output: Output::optimized().comments(Comments::Collect),
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

    // (5) The optimized bake linked a source map beside the minified file, and gzip
    //     covers it like any other servable asset; the plain bake emitted none.
    assert!(optimized.join("app.js.map").is_file(), "app.js.map sidecar");
    let min_src = std::fs::read_to_string(optimized.join("app.js")).unwrap();
    assert!(min_src.contains("sourceMappingURL=app.js.map"));
    assert!(
        optimized.join("app.js.map.gz").is_file(),
        "app.js.map.gz sidecar"
    );
    assert!(
        !plain.join("app.js.map").exists(),
        "plain bake writes no map"
    );

    // (6) `Comments::Collect` moved the legal banner into the LEGAL.txt sidecar: the
    //     minified file carries the pointer, not the banner.
    let legal = std::fs::read_to_string(optimized.join("app.js.LEGAL.txt")).unwrap();
    assert!(
        legal.contains("@license"),
        "banner landed in app.js.LEGAL.txt"
    );
    assert!(
        min_src.contains("app.js.LEGAL.txt"),
        "pointer comment in app.js"
    );
    assert!(
        !min_src.contains("embedded example"),
        "banner text left app.js"
    );

    // (7) The plain bake (comments unset → Keep) still ships the banner inline and no
    //     sidecar, the contrast that makes (6) meaningful.
    let plain_src = std::fs::read_to_string(plain.join("app.js")).unwrap();
    assert!(plain_src.contains("embedded example"));
    assert!(!plain.join("app.js.LEGAL.txt").exists());
}
