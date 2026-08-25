//! Rewrite vendored JavaScript in place — the `web_modules/` half of whole-tree
//! minification.
//!
//! Extraction ships packages byte-for-byte; this pass walks the vendored tree after
//! pruning and sends every `.js`/`.mjs` through the crate's one rewrite pass
//! ([`crate::typescript`]'s parse → `oxc_minifier` → codegen, with an optional source
//! map). Third-party trees ship oddities, so a file that cannot be read or parsed is
//! left as shipped, aloud — the build must not break on someone else's file.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::module_graph::is_emitted_js;
use crate::static_files::build_warning;
use crate::typescript::{append_source_map_comment, rewrite_js_capturing, RewriteOptions};
use crate::Result;

/// Rewrite every vendored `.js`/`.mjs` under `dir`, returning how many files were
/// rewritten. Symlinks are skipped — a marked output contains none this tool wrote.
pub(crate) fn rewrite_vendor_tree(dir: &Path, options: RewriteOptions) -> Result<usize> {
    if !dir.is_dir() {
        return Ok(0);
    }
    let mut count = 0;
    for entry in WalkDir::new(dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| !e.path_is_symlink())
    {
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let ext = path.extension().and_then(|e| e.to_str()).unwrap_or("");
        if !is_emitted_js(ext) {
            continue;
        }
        let rel = path.strip_prefix(dir).unwrap_or(path);
        let Ok(source) = std::fs::read_to_string(path) else {
            build_warning(&format!(
                "web-modules: web_modules/{}: not UTF-8 text; left as shipped",
                rel.display()
            ));
            continue;
        };
        let out = match rewrite_js_capturing(&source, path, rel, options) {
            Ok(out) => out,
            Err(e) => {
                build_warning(&format!(
                    "web-modules: web_modules/{}: left as shipped ({e})",
                    rel.display()
                ));
                continue;
            }
        };
        write_rewritten(path, out.code, out.map.map(|m| m.json))?;
        count += 1;
    }
    Ok(count)
}

/// Replace a vendored file with its rewritten form. The staged tree seeds
/// `web_modules/` as hardlinks into the previous output, so nothing may write
/// through an existing directory entry: stale sidecars are unlinked (safe — the
/// retired tree keeps its own link), the code is written to a sibling and renamed
/// over (the `gzip_file` discipline), and a fresh map is a new file.
fn write_rewritten(path: &Path, mut code: String, map: Option<String>) -> Result<()> {
    remove_if_present(&sibling(path, ".gz"))?;
    let map_path = sibling(path, ".map");
    remove_if_present(&map_path)?;

    if let Some(json) = map {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default();
        append_source_map_comment(&mut code, &format!("{name}.map"));
        std::fs::write(&map_path, json)?;
    }
    let tmp = sibling(path, ".tmp");
    std::fs::write(&tmp, code)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

/// `<path><suffix>` as a sibling path (`lit.js` → `lit.js.map`).
fn sibling(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

fn remove_if_present(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(e.into()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIFY: RewriteOptions = RewriteOptions {
        minify: true,
        source_map: false,
    };

    #[test]
    fn rewrites_js_and_leaves_other_files() {
        let dir = tempfile::tempdir().unwrap();
        let js = dir.path().join("pkg/index.js");
        std::fs::create_dir_all(js.parent().unwrap()).unwrap();
        std::fs::write(&js, "export const answer =\n  6 * 7;\n").unwrap();
        std::fs::write(dir.path().join("pkg/style.css"), "a { color: red }\n").unwrap();

        let count = rewrite_vendor_tree(dir.path(), MINIFY).unwrap();
        assert_eq!(count, 1);
        let out = std::fs::read_to_string(&js).unwrap();
        assert!(out.contains("42"), "constant folded; got {out}");
        assert!(out.matches('\n').count() <= 1, "single line; got {out:?}");
        assert_eq!(
            std::fs::read_to_string(dir.path().join("pkg/style.css")).unwrap(),
            "a { color: red }\n",
            "non-JS untouched"
        );
    }

    #[test]
    fn junk_js_is_left_as_shipped() {
        let dir = tempfile::tempdir().unwrap();
        let js = dir.path().join("weird.js");
        std::fs::write(&js, "this is not { javascript").unwrap();
        assert_eq!(rewrite_vendor_tree(dir.path(), MINIFY).unwrap(), 0);
        assert_eq!(
            std::fs::read_to_string(&js).unwrap(),
            "this is not { javascript",
            "unparseable third-party bytes ship as they came"
        );
    }

    #[test]
    fn hardlinked_seed_survives_the_rewrite() {
        // The staged tree hardlinks the previous output's files; a rewrite must go
        // through rename, never through the shared inode.
        let dir = tempfile::tempdir().unwrap();
        let retired = dir.path().join("retired.js");
        std::fs::write(&retired, "export const long_name = 1 + 1;\n").unwrap();
        let staged = dir.path().join("stage/pkg.js");
        std::fs::create_dir_all(staged.parent().unwrap()).unwrap();
        std::fs::hard_link(&retired, &staged).unwrap();

        rewrite_vendor_tree(&dir.path().join("stage"), MINIFY).unwrap();
        assert_eq!(
            std::fs::read_to_string(&retired).unwrap(),
            "export const long_name = 1 + 1;\n",
            "the retired tree is untouched"
        );
        assert!(
            std::fs::read_to_string(&staged).unwrap().contains('2'),
            "the staged copy is rewritten"
        );
    }

    #[test]
    fn stale_sidecars_are_unlinked_and_a_fresh_map_written() {
        let dir = tempfile::tempdir().unwrap();
        let js = dir.path().join("lib.js");
        std::fs::write(&js, "export const x = 1;\n").unwrap();
        std::fs::write(dir.path().join("lib.js.gz"), "stale").unwrap();
        std::fs::write(dir.path().join("lib.js.map"), "stale").unwrap();

        rewrite_vendor_tree(
            dir.path(),
            RewriteOptions {
                minify: true,
                source_map: true,
            },
        )
        .unwrap();
        assert!(
            !dir.path().join("lib.js.gz").exists(),
            "a sidecar describing the old bytes is gone"
        );
        let map = std::fs::read_to_string(dir.path().join("lib.js.map")).unwrap();
        assert!(map.contains("\"lib.js\""), "fresh map for the new bytes");
        assert!(std::fs::read_to_string(&js)
            .unwrap()
            .ends_with("//# sourceMappingURL=lib.js.map\n"));
    }
}
