//! Copy non-source static files into an output tree.
//!
//! The counterpart to the [`typescript`](crate::typescript)/[`scss`](crate::scss)
//! processors: everything they *don't* transform (images, fonts, JSON, `.well-known`,
//! …) is copied across verbatim, so `build` (and any custom build script) ends up with
//! a complete output directory.

use std::path::Path;

use walkdir::WalkDir;

use crate::module_graph::{imports_from_source, ModuleNode};
use crate::Result;

/// Copy files from `src` to `out` (preserving structure), skipping things a build step
/// produces or ignores: `.ts`/`.tsx`/`.mts`/`.scss`/`.tera` sources, `_`-prefixed partials,
/// and any path the [`reject`](crate::reject) list excludes (config / secrets / source).
/// Returns the number of files copied.
pub fn copy_static(src: &Path, out: &Path, reject: &crate::reject::Reject) -> Result<usize> {
    Ok(copy_static_capturing(src, out, reject)?.0)
}

/// Like [`copy_static`], but also returns a [`ModuleNode`] for every copied `.js`/`.mjs`
/// file — its output-relative path and the specifiers it imports — so a hand-written
/// module copied verbatim contributes to the build's module graph the same way
/// transformed files do, and its imports are vendored / resolution-checked without a
/// separate scan of the output tree. The `usize` is the total number of files copied
/// (all types), matching [`copy_static`].
pub(crate) fn copy_static_capturing(
    src: &Path,
    out: &Path,
    reject: &crate::reject::Reject,
) -> Result<(usize, Vec<ModuleNode>)> {
    let mut count = 0;
    let mut nodes = Vec::new();
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        // WalkDir yields paths under `src`, so the strip is infallible.
        let rel = path.strip_prefix(src).expect("walkdir entry is under src");
        // Reject list: never publish config / secret / source-code files into the output.
        if reject.rejects_path(rel) {
            crate::reject::warn_rejected(&rel.display().to_string());
            continue;
        }
        let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
        let ext = path.extension().and_then(|x| x.to_str()).unwrap_or("");
        if name.starts_with('_')
            || ["ts", "tsx", "mts", "scss", "tera"]
                .iter()
                .any(|e| ext.eq_ignore_ascii_case(e))
        {
            continue;
        }
        let dest = out.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        // Every copied `.js`/`.mjs` records a graph node — the record marks the WRITE
        // (replacing any earlier record for the same output path), independent of
        // whether the bytes are readable as a module. An `.mjs` that fails to parse is
        // a build error (it is unambiguously a module and the browser would fail on
        // it); a `.js` falls back to the classic-script goal inside
        // `imports_from_source`, and a file that defies both goals — or is not UTF-8 —
        // contributes an empty import set plus a warning, because an empty set from a
        // parse failure means "unknown", not "imports nothing". The copy itself is
        // byte-for-byte either way.
        if ["js", "mjs"].iter().any(|e| ext.eq_ignore_ascii_case(e)) {
            let module_only = ext.eq_ignore_ascii_case("mjs");
            let (imports, unanalyzable) = match std::fs::read_to_string(path) {
                Ok(source) => {
                    let read = imports_from_source(&source, module_only).map_err(|reason| {
                        crate::Error::Build(format!("web-modules: {}: {reason}", rel.display()))
                    })?;
                    let reason =
                        (!read.parsed).then_some("does not parse as a module or classic script");
                    (read.imports, reason)
                }
                Err(_) => (Vec::new(), Some("is not UTF-8 text")),
            };
            if let Some(reason) = unanalyzable {
                build_warning(&format!(
                    "web-modules: {}: {reason}; its imports are not validated",
                    rel.display()
                ));
            }
            nodes.push(ModuleNode {
                path: rel.to_path_buf(),
                imports,
            });
        }
        std::fs::copy(path, &dest)?;
        count += 1;
    }
    Ok((count, nodes))
}

/// Emit a build warning: as a `cargo:warning` directive when running inside a build
/// script (cargo sets `OUT_DIR`, and a build script's stderr is hidden unless it
/// fails), else straight to stderr.
pub(crate) fn build_warning(msg: &str) {
    if std::env::var_os("OUT_DIR").is_some() {
        println!("cargo:warning={msg}");
    } else {
        eprintln!("{msg}");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn copies_static_skips_sources_and_partials() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(src.join("sub")).unwrap();
        std::fs::write(src.join("logo.svg"), b"<svg/>").unwrap();
        std::fs::write(src.join("app.ts"), b"export {}").unwrap();
        std::fs::write(src.join("_partial.scss"), b"$x:1;").unwrap();
        std::fs::write(src.join("sub/data.json"), b"{}").unwrap();

        let n = copy_static(&src, &out, &crate::reject::Reject::none()).unwrap();
        assert_eq!(n, 2);
        assert!(out.join("logo.svg").exists());
        assert!(out.join("sub/data.json").exists());
        assert!(!out.join("app.ts").exists());
        assert!(!out.join("_partial.scss").exists());
    }

    #[test]
    fn copies_static_skips_case_variant_source_extensions() {
        // A source authored with an upper-cased extension is still a source — it must
        // not be copied into the output (else it ships raw), matching the serve path's
        // case-insensitive guard.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("App.SCSS"), b"$x:1;").unwrap();
        std::fs::write(src.join("Main.TS"), b"export {}").unwrap();
        std::fs::write(src.join("Logo.SVG"), b"<svg/>").unwrap();

        let n = copy_static(&src, &out, &crate::reject::Reject::none()).unwrap();
        assert_eq!(n, 1, "only the non-source file is copied");
        assert!(out.join("Logo.SVG").exists());
        assert!(
            !out.join("App.SCSS").exists(),
            "upper-cased .SCSS must be skipped"
        );
        assert!(
            !out.join("Main.TS").exists(),
            "upper-cased .TS must be skipped"
        );
    }

    #[test]
    fn copies_non_utf8_js_byte_for_byte_with_empty_record() {
        // A `.js` that isn't UTF-8 can't be a well-formed ES module — it still copies
        // unchanged (fs::copy, not a decode/re-encode round trip) and still records a
        // graph node with no imports: the node marks the WRITE, so it replaces any
        // earlier record a shadowed same-path file left behind.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        let bytes: &[u8] = &[0xFF, 0xFE, b'v', b'a', b'r', 0x80, 0x00, b';'];
        std::fs::write(src.join("blob.js"), bytes).unwrap();

        let (count, nodes) =
            copy_static_capturing(&src, &out, &crate::reject::Reject::none()).unwrap();
        assert_eq!(count, 1);
        assert_eq!(nodes.len(), 1, "the write is recorded");
        assert!(
            nodes[0].imports.is_empty(),
            "non-UTF-8 bytes contribute no imports; got {:?}",
            nodes[0].imports
        );
        assert_eq!(
            std::fs::read(out.join("blob.js")).unwrap(),
            bytes,
            "the copy is byte-for-byte"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn broken_mjs_fails_the_copy_with_the_path() {
        // An `.mjs` is unambiguously a module; if it does not parse, the browser will
        // fail on it, so the build fails first — naming the file.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("worker.mjs"), "var await = 1;").unwrap();

        let err = copy_static_capturing(&src, &out, &crate::reject::Reject::none()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("worker.mjs") && msg.contains("does not parse as an ES module"),
            "got: {msg}"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn copies_unparsable_js_with_empty_imports() {
        // Broken syntax still copies unchanged; the graph records the file with no
        // imports instead of a partial set from a recovered AST.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        let js = "import { broken from \"lit\";";
        std::fs::write(src.join("broken.js"), js).unwrap();

        let (count, nodes) =
            copy_static_capturing(&src, &out, &crate::reject::Reject::none()).unwrap();
        assert_eq!(count, 1);
        assert_eq!(std::fs::read_to_string(out.join("broken.js")).unwrap(), js);
        assert_eq!(nodes.len(), 1);
        assert!(
            nodes[0].imports.is_empty(),
            "no partial imports from a recovered AST; got {:?}",
            nodes[0].imports
        );
    }

    #[test]
    fn copy_static_drops_rejected_paths() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(src.join(".git")).unwrap();
        std::fs::write(src.join("index.html"), b"<x>").unwrap();
        std::fs::write(src.join("package.json"), b"{}").unwrap();
        std::fs::write(src.join(".env"), b"SECRET=1").unwrap();
        std::fs::write(src.join(".git/config"), b"[core]").unwrap();

        // The default (all-presets) reject list drops the manifest, the dotfile, and the .git dir.
        let n = copy_static(&src, &out, &crate::reject::Reject::all()).unwrap();
        assert_eq!(n, 1, "only index.html survives");
        assert!(out.join("index.html").exists());
        assert!(!out.join("package.json").exists());
        assert!(!out.join(".env").exists());
        assert!(!out.join(".git/config").exists());
    }
}
