#![cfg(feature = "bundle")]
//! Integration tests for [`web_modules::bundle::bundle_split`] — multi-entry chunked bundling
//! with preserved entry URLs, import-map-aware resolution, and externals. Pure local files;
//! no network, so unlike `tests/bundle.rs` these run un-ignored.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use web_modules::bundle::{bundle_split, SplitBundleOptions};
use web_modules::importmap::Importmap;

/// Build a miniature `dist/` URL space:
///
/// - `elements/app/a.js` — imports a relative shared module, an import-map specifier
///   (`app/util.js`), and the external `lit`.
/// - `elements/app/b.js` — imports the same shared module (forces a shared chunk).
/// - `elements/lib/shared.js` / `elements/app/util.js` — bundled internals with markers.
/// - `web_modules/lit/index.js` — must stay external (never inlined).
fn write_fixture(root: &Path) {
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    write(
        "elements/app/a.js",
        r#"import { shared } from '../lib/shared.js';
import { util } from 'app/util.js';
import { html } from 'lit';
export const a = shared + util + html.length;
"#,
    );
    write(
        "elements/app/b.js",
        r#"import { shared } from '../lib/shared.js';
export const b = shared + 1;
"#,
    );
    // Markers are live, non-foldable values: comments get stripped, and pure
    // literals get constant-folded and inlined per entry (which would also
    // dissolve the shared chunk this fixture exists to produce). Reading
    // through globalThis keeps the strings opaque and the module shared.
    write(
        "elements/lib/shared.js",
        "export const shared = (globalThis.__m ?? 'MARKER_SHARED_IMPL').length;\n",
    );
    write(
        "elements/app/util.js",
        "export const util = (globalThis.__m ?? 'MARKER_UTIL_IMPL').length;\n",
    );
    write(
        "web_modules/lit/index.js",
        "export const html = 'MARKER_LIT_IMPL';\n",
    );
}

fn importmap() -> Importmap {
    let mut map = Importmap::new();
    map.insert("app/", "/elements/app/")
        .insert("lit", "/web_modules/lit/index.js")
        .insert("web_modules/", "/web_modules/");
    map
}

/// Run the split bundle and return every emitted `.js` as (relative path → content).
fn run_split(root: &Path, out: &Path) -> BTreeMap<String, String> {
    let entries = [
        PathBuf::from("elements/app/a.js"),
        PathBuf::from("elements/app/b.js"),
    ];
    let map = importmap();
    let report = bundle_split(&SplitBundleOptions {
        entries: &entries,
        root,
        out_dir: out,
        importmap: Some(&map),
        external: &["lit".into(), "web_modules/".into()],
        chunk_filenames: "chunks/[name]-[hash].js",
        minify: false,
    })
    .unwrap();

    // The report lists exactly the folded source files: the two entries, the
    // shared module, and the import-map-resolved util — never the external.
    let folded: Vec<String> = report
        .bundled_modules
        .iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect();
    assert!(
        folded.contains(&"elements/lib/shared.js".to_string()),
        "{folded:?}"
    );
    assert!(
        folded.contains(&"elements/app/util.js".to_string()),
        "{folded:?}"
    );
    assert!(
        !folded.iter().any(|f| f.contains("web_modules")),
        "externals must not be reported as bundled: {folded:?}"
    );

    let mut files = BTreeMap::new();
    for entry in walkdir::WalkDir::new(out)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.file_type().is_file())
        .filter(|e| e.path().extension().is_some_and(|x| x == "js"))
    {
        let rel = entry
            .path()
            .strip_prefix(out)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        files.insert(rel, std::fs::read_to_string(entry.path()).unwrap());
    }
    files
}

#[test]
fn entries_keep_their_urls_and_share_chunks() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist");
    let out = tmp.path().join("out");
    write_fixture(&root);

    let files = run_split(&root, &out);

    // URL contract: both entries exist at their exact relative paths.
    assert!(files.contains_key("elements/app/a.js"), "{files:#?}");
    assert!(files.contains_key("elements/app/b.js"), "{files:#?}");

    // The shared module is folded exactly once across the whole output —
    // one chunk, no duplicate evaluation.
    let shared_copies: usize = files
        .values()
        .map(|c| c.matches("MARKER_SHARED_IMPL").count())
        .sum();
    assert_eq!(
        shared_copies, 1,
        "shared module must exist exactly once; output: {files:#?}"
    );

    // Shared internals land under the chunk template's directory.
    assert!(
        files.keys().any(|k| k.starts_with("chunks/")),
        "expected a shared chunk under chunks/, got: {:?}",
        files.keys().collect::<Vec<_>>()
    );
}

#[test]
fn externals_stay_external() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist");
    let out = tmp.path().join("out");
    write_fixture(&root);

    let files = run_split(&root, &out);

    // lit's implementation is never inlined; the import survives as a bare
    // specifier for the browser's import map.
    let lit_inlined: usize = files
        .values()
        .map(|c| c.matches("MARKER_LIT_IMPL").count())
        .sum();
    assert_eq!(lit_inlined, 0, "external module must not be inlined");
    let all = files.values().cloned().collect::<String>();
    assert!(
        all.contains("\"lit\"") || all.contains("'lit'"),
        "the external `lit` import must survive verbatim"
    );
}

#[test]
fn dynamic_imports_split_and_unanalyzable_ones_survive() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist");
    let out = tmp.path().join("out");
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    // A router-like entry: one analyzable dynamic import (also an entry — the
    // section pattern) and one template-literal import only the browser can
    // resolve at runtime.
    write(
        "elements/app/router.js",
        r#"export async function load(name) {
    const fixed = await import('./section/root.js');
    const dynamic = await import(`./section/${name}.js`);
    return [fixed, dynamic];
}
"#,
    );
    write(
        "elements/app/section/root.js",
        "export const section = (globalThis.__m ?? 'MARKER_ROOT_SECTION').length;\n",
    );

    let entries = [
        PathBuf::from("elements/app/router.js"),
        PathBuf::from("elements/app/section/root.js"),
    ];
    let map = importmap();
    bundle_split(&SplitBundleOptions {
        entries: &entries,
        root: &root,
        out_dir: &out,
        importmap: Some(&map),
        external: &["lit".into(), "web_modules/".into()],
        chunk_filenames: "chunks/[name]-[hash].js",
        minify: false,
    })
    .unwrap();

    let router = std::fs::read_to_string(out.join("elements/app/router.js")).unwrap();
    let section = std::fs::read_to_string(out.join("elements/app/section/root.js")).unwrap();

    // The analyzable dynamic import points at the section's preserved URL
    // (either verbatim or rewritten to the equivalent relative entry path) —
    // and never inlines it.
    assert!(
        router.contains("import("),
        "dynamic imports must stay dynamic: {router}"
    );
    assert!(
        !router.contains("MARKER_ROOT_SECTION"),
        "dynamically imported entry must not be inlined into the importer"
    );
    assert!(
        section.contains("MARKER_ROOT_SECTION"),
        "the section entry keeps its own implementation at its URL"
    );

    // The template-literal import is unanalyzable and must survive verbatim
    // for the browser + import map to resolve at runtime.
    assert!(
        router.contains("./section/${"),
        "template-literal dynamic import must survive verbatim: {router}"
    );
}

#[test]
fn relative_imports_of_external_locations_stay_external() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist");
    let out = tmp.path().join("out");
    let write = |rel: &str, content: &str| {
        let path = root.join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, content).unwrap();
    };
    // An entry reaching an external file through a RELATIVE import — the
    // specifier form bypasses list matching, so externality must hold by
    // resolved location or config.js gets folded (evaluated twice next to
    // the still-served original) and reported for pruning.
    write(
        "elements/app/page.js",
        r#"import '../../config.js';
export const page = (globalThis.__m ?? 'MARKER_PAGE_IMPL').length;
"#,
    );
    write("config.js", "globalThis.__config = 'MARKER_CONFIG_IMPL';\n");

    let entries = [PathBuf::from("elements/app/page.js")];
    let map = importmap();
    let report = bundle_split(&SplitBundleOptions {
        entries: &entries,
        root: &root,
        out_dir: &out,
        importmap: Some(&map),
        external: &["lit".into(), "web_modules/".into(), "/config.js".into()],
        chunk_filenames: "chunks/[name]-[hash].js",
        minify: false,
    })
    .unwrap();

    let page = std::fs::read_to_string(out.join("elements/app/page.js")).unwrap();
    assert!(
        !page.contains("MARKER_CONFIG_IMPL"),
        "external location must not be folded: {page}"
    );
    assert!(
        page.contains("../../config.js"),
        "the relative import must be re-relativized to the emitted file: {page}"
    );
    assert!(
        !report
            .bundled_modules
            .iter()
            .any(|p| p.ends_with("config.js")),
        "external location must not be reported for pruning"
    );
}

#[test]
fn importmap_specifiers_resolve_into_the_bundle() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist");
    let out = tmp.path().join("out");
    write_fixture(&root);

    let files = run_split(&root, &out);

    // `app/util.js` resolved through the import map to a root file and was bundled.
    let util_copies: usize = files
        .values()
        .map(|c| c.matches("MARKER_UTIL_IMPL").count())
        .sum();
    assert_eq!(util_copies, 1, "import-map specifier must be bundled once");
    let all = files.values().cloned().collect::<String>();
    assert!(
        !all.contains("'app/util.js'") && !all.contains("\"app/util.js\""),
        "bundled import-map specifier must not survive as a bare import"
    );
}

#[test]
fn split_refuses_a_relative_import_escaping_root() {
    // An entry importing `../../secret.js` climbs out of `root`; the build must fail
    // rather than fold the outside file into the published tree.
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("dist");
    let path = root.join("app/entry.js");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, "import '../../secret.js';\nexport const x = 1;\n").unwrap();
    std::fs::write(
        tmp.path().join("secret.js"),
        "export const leaked = 'TOPSECRET';\n",
    )
    .unwrap();
    let entries = [PathBuf::from("app/entry.js")];
    let result = bundle_split(&SplitBundleOptions {
        entries: &entries,
        root: &root,
        out_dir: &tmp.path().join("out"),
        importmap: None,
        external: &[],
        chunk_filenames: "chunks/[name]-[hash].js",
        minify: false,
    });
    let err = match result {
        Ok(_) => panic!("an escaping import must fail the build"),
        Err(e) => e,
    };
    assert!(err.to_string().contains("outside the bundle root"), "{err}");
}
