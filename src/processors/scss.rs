//! SCSS → CSS compilation via [`grass`] (pure Rust, no Node/dart-sass).
//!
//! [`compile_directory`] mirrors the [`super::typescript`] convention: walk a
//! source tree, skip `_`-prefixed partials, and emit a sibling `.css` for each
//! `.scss`. `load_paths` lets `@use`/`@import` reach vendored stylesheets (e.g. a
//! vendored Bootstrap under `web_modules/bootstrap/scss`).

use std::fs::{create_dir_all, write};
use std::path::Path;

use grass::{Options, OutputStyle};
use walkdir::WalkDir;

use crate::{Error, Result};

fn options<'a>(load_paths: &'a [&'a Path]) -> Options<'a> {
    let mut opts = Options::default().style(OutputStyle::Compressed);
    for path in load_paths {
        opts = opts.load_path(path);
    }
    opts
}

/// Compile a single SCSS string to compressed CSS.
pub fn compile_str(input: &str, load_paths: &[&Path]) -> Result<String> {
    grass::from_string(input.to_string(), &options(load_paths))
        .map_err(|e| Error::Scss(e.to_string()))
}

/// Compile a single `.scss` file to CSS.
pub fn compile_file(path: &Path, load_paths: &[&Path]) -> Result<String> {
    grass::from_path(path, &options(load_paths)).map_err(|e| Error::Scss(e.to_string()))
}

/// Compile every `.scss` under `src_dir` (skipping `_` partials) into a mirrored
/// `.css` under `out_dir`. Returns the number of files written.
pub fn compile_directory(src_dir: &Path, out_dir: &Path, load_paths: &[&Path]) -> Result<usize> {
    let opts = options(load_paths);
    let mut count = 0;
    for entry in WalkDir::new(src_dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| {
            e.path()
                .extension()
                .and_then(|ext| ext.to_str())
                .is_some_and(|ext| ext.eq_ignore_ascii_case("scss"))
        })
    {
        let path = entry.path();
        if path
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with('_'))
        {
            continue;
        }
        let rel = path
            .strip_prefix(src_dir)
            .map_err(|e| Error::Scss(e.to_string()))?;
        let out = out_dir.join(rel).with_extension("css");
        if let Some(parent) = out.parent() {
            create_dir_all(parent)?;
        }
        let css = grass::from_path(path, &opts).map_err(|e| Error::Scss(e.to_string()))?;
        write(&out, css)?;
        count += 1;
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn compiles_and_compresses() {
        let css = compile_str("$c: red; a { color: $c; b { color: $c; } }", &[]).unwrap();
        assert!(css.contains("color:red"));
        assert!(!css.contains('\n'), "compressed output is single-line");
    }

    #[test]
    fn directory_skips_partials() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        create_dir_all(&src).unwrap();
        write(src.join("_vars.scss"), "$c: blue;").unwrap();
        write(src.join("app.scss"), "@use 'vars'; a { color: vars.$c; }").unwrap();
        let n = compile_directory(&src, &out, &[]).unwrap();
        assert_eq!(n, 1);
        assert!(out.join("app.css").exists());
        assert!(!out.join("_vars.css").exists());
    }
}
