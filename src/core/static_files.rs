//! Copy non-source static files into an output tree.
//!
//! The counterpart to the [`typescript`](crate::typescript)/[`scss`](crate::scss)
//! processors: everything they *don't* transform (images, fonts, JSON, `.well-known`,
//! …) is copied across verbatim, so `build` (and any custom build script) ends up with
//! a complete output directory.

use std::path::Path;

use walkdir::WalkDir;

use crate::Result;

/// Copy files from `src` to `out` (preserving structure), skipping things a build
/// step produces or ignores: `.ts`/`.tsx`/`.mts`/`.scss`/`.tera` sources and
/// `_`-prefixed partials. Returns the number of files copied.
pub fn copy_static(src: &Path, out: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in WalkDir::new(src).into_iter().filter_map(|e| e.ok()) {
        let path = entry.path();
        if !path.is_file() {
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
        // WalkDir yields paths under `src`, so the strip is infallible.
        let rel = path.strip_prefix(src).expect("walkdir entry is under src");
        let dest = out.join(rel);
        if let Some(parent) = dest.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::copy(path, &dest)?;
        count += 1;
    }
    Ok(count)
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

        let n = copy_static(&src, &out).unwrap();
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

        let n = copy_static(&src, &out).unwrap();
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
}
