//! Copy non-source static files into an output tree.
//!
//! The counterpart to the [`typescript`](crate::typescript)/[`scss`](crate::scss)
//! processors: everything they *don't* transform (images, fonts, JSON, `.well-known`,
//! …) is copied across verbatim, so `build` (and any custom build script) ends up with
//! a complete output directory.

use std::path::Path;

use walkdir::WalkDir;

use crate::module_graph::{imports_for_emitted_js, is_emitted_js};
use crate::Result;

/// Extensions the source processors consume. Never shipped raw — even when the
/// matching processor is disabled, a `.scss` or `.ts` stays unshipped rather than
/// leaking source into the output. The dev server's request filter uses the same list.
pub(crate) const SOURCE_EXTENSIONS: [&str; 5] = ["ts", "tsx", "mts", "scss", "tera"];

/// Copy files from `src` to `out` (preserving structure), skipping things a build step
/// produces or ignores: `.ts`/`.tsx`/`.mts`/`.scss`/`.tera` sources, `_`-prefixed partials,
/// and any path the [`reject`](crate::reject) list excludes (config / secrets / source).
/// Symlinks are skipped entirely — file or directory; the pipeline's preflight, not
/// this standalone helper, honors [`SymlinkMode`](crate::SymlinkMode). Returns the
/// number of files copied.
pub fn copy_static(src: &Path, out: &Path, reject: &crate::reject::Reject) -> Result<usize> {
    let step = StaticStep::new(reject.clone());
    let mut count = 0;
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        // The link check comes first: `is_file` stats *through* a file link, and a
        // copy would read the target's content.
        if entry.path_is_symlink() || !path.is_file() {
            continue;
        }
        // WalkDir yields paths under `src`, so the strip is infallible.
        let rel = path.strip_prefix(src).expect("walkdir entry is under src");
        if step.claims_source(rel).is_none() {
            continue;
        }
        let dest = out.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(path, &dest)?;
        count += 1;
    }
    Ok(count)
}

/// The static-copy stage as a pipeline step: claims everything that is not a source
/// file or a `_` partial (the preflight drops reject-listed targets centrally, and
/// reports them), and copies it byte-for-byte. A copied `.js`/`.mjs` is read for the
/// module graph on the way — the record marks the WRITE, independent of whether the
/// bytes are readable as a module.
pub(crate) struct StaticStep {
    reject: crate::reject::Reject,
}

/// The shape of a static-copy candidate: not a `_` partial, not a processor source.
fn static_candidate(rel: &Path) -> Option<()> {
    let name = rel.file_name()?.to_str()?;
    let ext = rel.extension().and_then(|x| x.to_str()).unwrap_or("");
    if name.starts_with('_')
        || SOURCE_EXTENSIONS
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
    {
        return None;
    }
    Some(())
}

impl StaticStep {
    pub(crate) fn new(reject: crate::reject::Reject) -> Self {
        Self { reject }
    }

    /// The standalone [`copy_static`] claim rule: [`static_candidate`] plus the reject
    /// list, which the walk enforces itself here — outside the pipeline there is no
    /// preflight to do it centrally.
    fn claims_source(&self, rel: &Path) -> Option<()> {
        static_candidate(rel)?;
        // Reject list: never publish config / secret / source-code files into the output.
        if self.reject.rejects_path(rel) {
            crate::reject::warn_rejected(&rel.display().to_string());
            return None;
        }
        Some(())
    }
}

impl crate::build::steps::Preflight for StaticStep {
    fn name(&self) -> &'static str {
        "static copy"
    }

    fn rank(&self) -> crate::build::steps::Rank {
        crate::build::steps::Rank::Static
    }

    fn claim(&self, rel: &Path) -> Option<crate::build::steps::Claim> {
        static_candidate(rel)?;
        Some(crate::build::steps::Claim {
            out_rel: rel.to_path_buf(),
            tiebreak: 0,
        })
    }
}

impl crate::build::steps::Step for StaticStep {
    /// Copy byte-for-byte; read a `.js`/`.mjs` for the graph first, through the shared
    /// [`imports_for_emitted_js`]: an `.mjs` that fails to parse is a build error, and
    /// a file nothing parsed warns that its imports are not validated. A file that is
    /// not UTF-8 text cannot be read for imports at all, so it contributes an empty
    /// import set plus the same style of warning — an empty set from an unread file
    /// means "unknown", not "imports nothing".
    fn emit(
        &self,
        _cx: &crate::build::steps::EmitCx<'_>,
        src: &Path,
        rel: &Path,
        dest: &Path,
    ) -> Result<crate::build::steps::Emitted> {
        let ext = rel.extension().and_then(|x| x.to_str()).unwrap_or("");
        let imports = if !is_emitted_js(ext) {
            None
        } else {
            match std::fs::read_to_string(src) {
                Ok(source) => imports_for_emitted_js(&source, ext, rel)?,
                Err(_) => {
                    build_warning(&format!(
                        "web-modules: {}: is not UTF-8 text; its imports are not validated",
                        rel.display()
                    ));
                    Some(Vec::new())
                }
            }
        };
        std::fs::copy(src, dest)?;
        Ok(crate::build::steps::Emitted { imports })
    }
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

    #[cfg(unix)]
    #[test]
    fn copy_static_skips_symlinks_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(src.join("theme")).unwrap();
        std::fs::write(dir.path().join("outside.txt"), b"secret").unwrap();
        std::fs::write(src.join("theme/site.css"), b"b{}").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside.txt"), src.join("linked.txt")).unwrap();
        std::os::unix::fs::symlink(src.join("theme"), src.join("styles")).unwrap();

        let n = copy_static(&src, &out, &crate::reject::Reject::none()).unwrap();
        assert_eq!(n, 1, "only the real file is copied");
        assert!(out.join("theme/site.css").exists());
        assert!(
            !out.join("linked.txt").exists(),
            "a file link is never read through"
        );
        assert!(
            !out.join("styles").exists(),
            "a directory link is not descended"
        );
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

    /// Run [`StaticStep::emit`] for one file, `src/<rel>` → `out/<rel>`.
    fn emit_one(src: &Path, rel: &str, out: &Path) -> crate::Result<crate::build::steps::Emitted> {
        use crate::build::steps::Step;
        let step = StaticStep::new(crate::reject::Reject::none());
        let map = crate::importmap::Importmap::new();
        let cx = crate::build::steps::EmitCx { importmap: &map };
        std::fs::create_dir_all(out).unwrap();
        step.emit(&cx, &src.join(rel), Path::new(rel), &out.join(rel))
    }

    #[test]
    fn step_claims_everything_but_sources_and_partials() {
        use crate::build::steps::Preflight;
        let step = StaticStep::new(crate::reject::Reject::all());
        assert!(step.claim(Path::new("page.html")).is_some());
        assert!(step.claim(Path::new("app.js")).is_some());
        assert_eq!(
            step.claim(Path::new("sub/img.png")).unwrap().out_rel,
            Path::new("sub/img.png"),
            "a static claim targets its own path"
        );
        for source in [
            "app.ts",
            "app.TSX",
            "mod.mts",
            "style.scss",
            "page.html.tera",
        ] {
            assert!(
                step.claim(Path::new(source)).is_none(),
                "{source} is a processor's input, never copied raw"
            );
        }
        assert!(step.claim(Path::new("_partial.html")).is_none());
        // A reject-listed path still claims by shape: the preflight drops it centrally
        // (and reports the drop), so the decision is one rule for every step.
        assert!(step.claim(Path::new(".env")).is_some());
        assert!(
            step.claims_source(Path::new(".env")).is_none(),
            "the standalone copy_static rule enforces the reject list itself"
        );
    }

    #[test]
    fn step_copies_non_utf8_js_byte_for_byte_with_empty_record() {
        // A `.js` that isn't UTF-8 can't be a well-formed ES module — it still copies
        // unchanged (fs::copy, not a decode/re-encode round trip) and still records
        // its write with no imports.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        let bytes: &[u8] = &[0xFF, 0xFE, b'v', b'a', b'r', 0x80, 0x00, b';'];
        std::fs::write(src.join("blob.js"), bytes).unwrap();

        let emitted = emit_one(&src, "blob.js", &out).unwrap();
        assert_eq!(
            emitted.imports.as_deref().map(<[_]>::len),
            Some(0),
            "the write is recorded, with no imports"
        );
        assert_eq!(
            std::fs::read(out.join("blob.js")).unwrap(),
            bytes,
            "the copy is byte-for-byte"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn step_fails_a_broken_mjs_with_the_path() {
        // An `.mjs` is unambiguously a module; if it does not parse, the browser will
        // fail on it, so the build fails first — naming the file.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        std::fs::write(src.join("worker.mjs"), "var await = 1;").unwrap();

        let err = emit_one(&src, "worker.mjs", &out).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("worker.mjs") && msg.contains("does not parse as an ES module"),
            "got: {msg}"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn step_copies_unparsable_js_with_empty_imports() {
        // Broken syntax still copies unchanged; the graph records the file with no
        // imports instead of a partial set from a recovered AST.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        std::fs::create_dir_all(&src).unwrap();
        let js = "import { broken from \"lit\";";
        std::fs::write(src.join("broken.js"), js).unwrap();

        let emitted = emit_one(&src, "broken.js", &out).unwrap();
        assert_eq!(std::fs::read_to_string(out.join("broken.js")).unwrap(), js);
        assert_eq!(
            emitted.imports.as_deref().map(<[_]>::len),
            Some(0),
            "no partial imports from a recovered AST; got {:?}",
            emitted.imports
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
