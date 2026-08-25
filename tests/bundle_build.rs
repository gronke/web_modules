//! `--bundle` inside the build pipeline, bound as specs: entries keep their URLs,
//! inlined modules leave the tree, the import map disappears, survivors are
//! rewritten, and the guards fire. Offline — fixtures use relative imports only;
//! the vendored (bare-import) path is network-gated at the end.
#![cfg(feature = "bundle")]

use std::path::{Path, PathBuf};

use web_modules::build::{build, BuildOptions, Output, Processors};

/// A `BuildOptions` over one root with bundling on and the given entries.
fn opts<'a>(root: &'a PathBuf, out: &'a Path, entries: &'a [PathBuf]) -> BuildOptions<'a> {
    let mut processors = Processors::default();
    processors.bundle = true;
    processors.bundle_entries = entries.to_vec();
    BuildOptions {
        specs: &[],
        roots: std::slice::from_ref(root),
        out,
        mount: "/web_modules",
        html: "<!doctype html>{importmap}<script type=module src=./app.js></script>",
        template: None,
        processors,
        output: Output::default(),
    }
}

fn write(root: &Path, name: &str, content: &str) {
    std::fs::create_dir_all(root).unwrap();
    std::fs::write(root.join(name), content).unwrap();
}

#[test]
fn bundle_inlines_relative_imports_and_prunes_the_tree() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(
        &root,
        "app.js",
        "import { util } from \"./util.js\";\nconsole.log(util);\n",
    );
    write(
        &root,
        "util.js",
        "export const util = \"utility_marker\";\n",
    );
    let out = dir.path().join("out");
    build(&opts(&root, &out, &[])).unwrap();

    let app = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(app.contains("utility_marker"), "inlined; got {app}");
    assert!(!app.contains("./util.js"), "no import left; got {app}");
    assert!(
        !out.join("util.js").exists(),
        "the inlined module is pruned"
    );
    assert!(
        !out.join("importmap.json").exists(),
        "no import map in a bundled dist"
    );
    assert!(!out.join("web_modules").exists());
}

#[test]
fn two_entries_share_a_chunk_and_unreferenced_files_survive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(
        &root,
        "app.js",
        "import { s } from \"./shared.js\";\nconsole.log(\"app\", s);\n",
    );
    write(
        &root,
        "admin.js",
        "import { s } from \"./shared.js\";\nconsole.log(\"admin\", s);\n",
    );
    write(&root, "shared.js", "export const s = \"shared_marker\";\n");
    write(
        &root,
        "standalone.js",
        "console.log(\"standalone survives\");\n",
    );
    let out = dir.path().join("out");
    let entries = [PathBuf::from("app.js"), PathBuf::from("admin.js")];
    build(&opts(&root, &out, &entries)).unwrap();

    assert!(out.join("app.js").exists() && out.join("admin.js").exists());
    assert!(
        !out.join("shared.js").exists(),
        "shared module folded into a chunk"
    );
    let chunks: Vec<_> = std::fs::read_dir(out.join("chunks"))
        .expect("chunks/ emitted")
        .filter_map(|e| e.ok())
        .collect();
    assert!(!chunks.is_empty(), "content-hashed shared chunk written");
    assert!(
        out.join("standalone.js").exists(),
        "a URL-referenced (unimported) file survives at its URL"
    );
}

#[test]
fn synthesized_html_has_no_importmap_when_bundling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(&root, "app.js", "console.log(1);\n");
    let out = dir.path().join("out");
    build(&opts(&root, &out, &[])).unwrap();
    let html = std::fs::read_to_string(out.join("index.html")).unwrap();
    assert!(
        !html.contains("importmap"),
        "every bare import is inlined, the page needs no map; got {html}"
    );
    assert!(
        html.contains("./app.js"),
        "the entry reference stands; got {html}"
    );
}

#[test]
fn missing_entry_names_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(&root, "app.js", "console.log(1);\n");
    let out = dir.path().join("out");
    let entries = [PathBuf::from("nope.js")];
    let err = build(&opts(&root, &out, &entries)).unwrap_err().to_string();
    assert!(
        err.contains("bundle entry") && err.contains("--bundle-entry"),
        "got {err}"
    );
}

#[cfg(feature = "typescript")]
#[test]
fn minify_and_comments_apply_through_rolldown_and_to_survivors() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(
        &root,
        "app.js",
        "/*! (c) bundled */\nimport { util } from \"./util.js\";\nconsole.log(util);\n",
    );
    write(&root, "util.js", "export const util = 6 * 7;\n");
    write(
        &root,
        "standalone.js",
        "// survivor note\nexport const answer =\n  1 + 1;\n",
    );
    let out = dir.path().join("out");
    let mut o = opts(&root, &out, &[]);
    o.output = Output::new(true, false);
    build(&o).unwrap();

    let app = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(
        app.contains("42"),
        "inlined and folded by rolldown; got {app}"
    );
    assert!(
        app.contains("(c) bundled"),
        "legal comment kept inline; got {app}"
    );
    let survivor = std::fs::read_to_string(out.join("standalone.js")).unwrap();
    assert!(
        survivor.contains('2') && !survivor.contains("survivor note"),
        "the non-bundled survivor got the deferred rewrite; got {survivor}"
    );
    assert!(
        survivor.matches('\n').count() <= 1,
        "minified; got {survivor:?}"
    );
}

#[test]
fn sourcemaps_cover_bundle_outputs() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(
        &root,
        "app.js",
        "import { util } from \"./util.js\";\nconsole.log(util);\n",
    );
    write(&root, "util.js", "export const util = 1;\n");
    let out = dir.path().join("out");
    let mut o = opts(&root, &out, &[]);
    o.processors.sourcemap = true;
    build(&o).unwrap();

    assert!(
        out.join("app.js.map").exists(),
        "rolldown wrote the entry's map"
    );
    let app = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(
        app.contains("sourceMappingURL=app.js.map"),
        "and linked it; got {app}"
    );
    assert!(
        !out.join("util.js.map").exists(),
        "the inlined module's map went with it"
    );
}

#[test]
fn chunks_prefix_is_reserved_while_bundling() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(&root, "app.js", "console.log(1);\n");
    std::fs::create_dir_all(root.join("chunks")).unwrap();
    std::fs::write(root.join("chunks/mine.js"), "console.log(2);\n").unwrap();
    let out = dir.path().join("out");
    let err = build(&opts(&root, &out, &[])).unwrap_err().to_string();
    assert!(
        err.contains("reserved") && err.contains("chunks"),
        "got {err}"
    );
}

#[test]
fn bundling_works_inside_an_ambient_tokio_runtime() {
    // The CLI's `main` is `#[tokio::main]`: the sync `build()` (and rolldown's own
    // sync wrapper inside it) must not `block_on` the caller's runtime thread.
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(
        &root,
        "app.js",
        "import { util } from \"./util.js\";\nconsole.log(util);\n",
    );
    write(&root, "util.js", "export const util = \"nested_rt\";\n");
    let out = dir.path().join("out");
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            build(&opts(&root, &out, &[])).unwrap();
        });
    assert!(std::fs::read_to_string(out.join("app.js"))
        .unwrap()
        .contains("nested_rt"));
}

#[test]
#[ignore = "network: downloads lit from the npm registry"]
fn bundling_inlines_vendored_bare_imports_and_guards_orphans() {
    use web_modules::vendor::PackageSpec;
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path().join("web");
    write(
        &root,
        "app.js",
        "import { html } from \"lit\";\nconsole.log(html`x`);\n",
    );
    write(
        &root,
        "extra.js",
        "import { css } from \"lit\";\nconsole.log(css);\n",
    );
    let out = dir.path().join("out");
    let specs = [PackageSpec::npm("lit", "^3")];

    // `extra.js` imports lit by bare specifier but is no entry: with the import map
    // gone it would break in the browser, so the build refuses and names it.
    let mut o = opts(&root, &out, &[]);
    o.specs = &specs;
    let err = build(&o).unwrap_err().to_string();
    assert!(
        err.contains("extra.js") && err.contains("--bundle-entry"),
        "got {err}"
    );

    // As a second entry it bundles too, and the dist is self-contained.
    let entries = [PathBuf::from("app.js"), PathBuf::from("extra.js")];
    let mut o = opts(&root, &out, &entries);
    o.specs = &specs;
    build(&o).unwrap();
    assert!(!out.join("web_modules").exists(), "vendored tree consumed");
    assert!(!out.join("importmap.json").exists());
    let app = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(
        !app.contains("from\"lit\"") && !app.contains("from \"lit\""),
        "no bare import survives; got a bundle of {} bytes",
        app.len()
    );
}
