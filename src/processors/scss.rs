//! SCSS → CSS compilation via [`grass`] (pure Rust, no Node/dart-sass).
//!
//! [`compile_directory`] mirrors the [`super::typescript`] convention: walk a
//! source tree, skip `_`-prefixed partials, and emit a sibling `.css` for each
//! `.scss`. `load_paths` lets `@use`/`@import` reach vendored stylesheets (e.g. a
//! vendored Bootstrap under `web_modules/bootstrap/scss`).
//!
//! `@use`/`@import` resolution is sandboxed by [`SandboxFs`]: `grass` resolves an
//! import against the importing file's own directory first, then the load paths,
//! following `..` and symlinks like any lookup, so without a containment check a
//! source stylesheet could `@import "../../../../secret.scss"` and inline a file
//! from outside the tree into the compiled CSS (which the dev server would then
//! serve). The sandbox confines every probe and read to the source roots and their
//! load paths — the SCSS counterpart of the serving layer's `contained_file`.

use std::fs::{create_dir_all, write};
use std::io;
use std::path::{Path, PathBuf};

use grass::{Fs, Options, OutputStyle};
use walkdir::WalkDir;

use crate::{Error, Result};

/// A [`grass::Fs`] that confines every `@use`/`@import` probe and read to an allowlist of
/// canonicalized directories, so SCSS resolution cannot climb out of the source roots and their
/// load paths. The containment mirrors the serving layer's `contained_file`: canonicalize the
/// probed path and require it to stay under one of the allowed `roots`.
#[derive(Debug)]
struct SandboxFs {
    roots: Vec<PathBuf>,
}

impl SandboxFs {
    /// Confine access to `roots`. Each is canonicalized once here; a root that does not exist is
    /// dropped, since it can never contain a file and so never widens the allowlist.
    fn new(roots: &[&Path]) -> Self {
        Self {
            roots: roots.iter().filter_map(|p| p.canonicalize().ok()).collect(),
        }
    }

    /// The real location of `path` if it resolves inside an allowed root, else `None`. A path that
    /// does not resolve — a probe for a candidate that isn't on disk — is not contained, matching
    /// how a missing file reads on the default [`grass::StdFs`].
    fn contained(&self, path: &Path) -> Option<PathBuf> {
        let real = path.canonicalize().ok()?;
        self.roots
            .iter()
            .any(|root| real.starts_with(root))
            .then_some(real)
    }
}

impl Fs for SandboxFs {
    fn is_file(&self, path: &Path) -> bool {
        self.contained(path).is_some_and(|real| real.is_file())
    }

    fn is_dir(&self, path: &Path) -> bool {
        self.contained(path).is_some_and(|real| real.is_dir())
    }

    fn read(&self, path: &Path) -> io::Result<Vec<u8>> {
        match self.contained(path) {
            Some(real) => std::fs::read(real),
            None => Err(io::Error::new(
                io::ErrorKind::NotFound,
                format!("SCSS import {path:?} escapes the source roots"),
            )),
        }
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }
}

/// The directory `grass` resolves a file's relative imports against — its parent, or the current
/// directory for a bare filename. Kept in the sandbox allowlist so the entry file (which
/// [`grass::from_path`] reads through the [`Fs`]) and its sibling imports stay reachable.
fn entry_dir(path: &Path) -> PathBuf {
    match path.parent() {
        Some(parent) if !parent.as_os_str().is_empty() => parent.to_path_buf(),
        _ => PathBuf::from("."),
    }
}

fn options<'a>(fs: &'a dyn Fs, load_paths: &[&Path]) -> Options<'a> {
    let mut opts = Options::default().style(OutputStyle::Compressed).fs(fs);
    for path in load_paths {
        opts = opts.load_path(path);
    }
    opts
}

/// Compile a single SCSS string to compressed CSS. Imports resolve within `load_paths` only (a
/// string has no source directory of its own).
pub fn compile_str(input: &str, load_paths: &[&Path]) -> Result<String> {
    let sandbox = SandboxFs::new(load_paths);
    grass::from_string(input.to_string(), &options(&sandbox, load_paths))
        .map_err(|e| Error::Scss(e.to_string()))
}

/// Compile a single `.scss` file to CSS. Imports resolve within `load_paths` and the file's own
/// directory, and cannot escape them.
pub fn compile_file(path: &Path, load_paths: &[&Path]) -> Result<String> {
    let entry = entry_dir(path);
    let mut roots = load_paths.to_vec();
    roots.push(entry.as_path());
    let sandbox = SandboxFs::new(&roots);
    grass::from_path(path, &options(&sandbox, load_paths)).map_err(|e| Error::Scss(e.to_string()))
}

/// Compile every `.scss` under `src_dir` (skipping `_` partials) into a mirrored
/// `.css` under `out_dir`. Returns the number of files written.
pub fn compile_directory(src_dir: &Path, out_dir: &Path, load_paths: &[&Path]) -> Result<usize> {
    // Every entry file lives under `src_dir`, so one sandbox covering the load paths plus
    // `src_dir` keeps each file and its in-tree imports reachable while refusing escapes.
    let mut roots = load_paths.to_vec();
    roots.push(src_dir);
    let sandbox = SandboxFs::new(&roots);
    let opts = options(&sandbox, load_paths);
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

/// The SCSS stage as a pipeline step: claims `.scss` (minus `_` partials) for a
/// mirrored `.css`.
pub(crate) struct ScssStep {
    load_paths: Vec<PathBuf>,
}

impl ScssStep {
    pub(crate) fn new(load_paths: Vec<PathBuf>) -> Self {
        Self { load_paths }
    }
}

impl crate::build::steps::Preflight for ScssStep {
    fn name(&self) -> &'static str {
        "SCSS compile"
    }

    fn rank(&self) -> crate::build::steps::Rank {
        crate::build::steps::Rank::Transform
    }

    fn claim(&self, rel: &Path) -> Option<crate::build::steps::Claim> {
        let name = rel.file_name()?.to_str()?;
        let ext = rel.extension()?.to_str()?;
        if !ext.eq_ignore_ascii_case("scss") || name.starts_with('_') {
            return None;
        }
        Some(crate::build::steps::Claim {
            out_rel: rel.with_extension("css"),
            tiebreak: 0,
        })
    }
}

impl crate::build::steps::Step for ScssStep {
    fn emit(
        &self,
        _cx: &crate::build::steps::EmitCx<'_>,
        src: &Path,
        _rel: &Path,
        dest: &Path,
    ) -> Result<crate::build::steps::Emitted> {
        let paths: Vec<&Path> = self.load_paths.iter().map(PathBuf::as_path).collect();
        let css = compile_file(src, &paths)?;
        write(dest, css)?;
        Ok(crate::build::steps::Emitted::default())
    }
}

/// Feature-specific `--scss-*` flags, paired with the `--scss` / `--no-scss` toggle in
/// [`ScssArgs`].
#[cfg(feature = "cli")]
#[derive(clap::Args, Clone, Debug, Default)]
pub struct ScssConfig {
    /// Extra SCSS `@use`/`@import` load path(s), on top of the source roots (repeatable).
    #[arg(long = "scss-load-path", value_name = "DIR")]
    pub load_paths: Vec<std::path::PathBuf>,
}

#[cfg(feature = "cli")]
crate::cli_config::feature_args!(ScssArgs, scss, "scss", no_scss, "no-scss", ScssConfig);

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

    #[test]
    fn import_within_the_tree_still_resolves() {
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        create_dir_all(&src).unwrap();
        write(src.join("_vars.scss"), "$c: blue;").unwrap();
        write(src.join("app.scss"), "@use 'vars'; a { color: vars.$c; }").unwrap();
        let css = compile_file(&src.join("app.scss"), &[]).unwrap();
        assert!(css.contains("color:blue"));
    }

    #[test]
    fn import_through_a_load_path_still_resolves() {
        // A vendored stylesheet reached via an explicit load path stays allowed.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let vendor = tmp.path().join("vendor");
        create_dir_all(&src).unwrap();
        create_dir_all(&vendor).unwrap();
        write(vendor.join("_theme.scss"), "$c: green;").unwrap();
        write(src.join("app.scss"), "@use 'theme'; a { color: theme.$c; }").unwrap();
        let css = compile_file(&src.join("app.scss"), &[vendor.as_path()]).unwrap();
        assert!(css.contains("color:green"));
    }

    #[test]
    fn import_climbing_out_of_the_tree_is_refused() {
        // A valid partial sits just outside the source tree, so only containment — not a parse
        // error — can be what stops it from being inlined into the compiled CSS.
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        create_dir_all(&src).unwrap();
        write(tmp.path().join("_secret.scss"), "$leak: red;").unwrap();
        write(src.join("app.scss"), "@import '../secret';").unwrap();
        // `grass` reports the escaping import as an unfindable stylesheet rather than reading it.
        let err = compile_file(&src.join("app.scss"), &[]).unwrap_err();
        assert!(
            matches!(err, Error::Scss(_)),
            "expected an SCSS error, got {err:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn import_through_a_symlink_escaping_the_tree_is_refused() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let src = tmp.path().join("src");
        let outside = tmp.path().join("outside");
        create_dir_all(&src).unwrap();
        create_dir_all(&outside).unwrap();
        write(outside.join("_theme.scss"), "$c: red;").unwrap();
        // A partial that appears to live in the tree is really a symlink pointing out of it.
        symlink(outside.join("_theme.scss"), src.join("_theme.scss")).unwrap();
        write(src.join("app.scss"), "@use 'theme';").unwrap();
        let err = compile_file(&src.join("app.scss"), &[]).unwrap_err();
        assert!(
            matches!(err, Error::Scss(_)),
            "expected an SCSS error, got {err:?}"
        );
    }
}
