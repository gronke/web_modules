//! `npm://` symlink targets: source files from an installed dependency.
//!
//! A symlink whose target is an `npm://<package>/<subpath>` URL is not a filesystem
//! link but a reference into `node_modules`: the build resolves it through
//! [`npm_utils::resolve`], then writes the file(s) at the link's own location — a single
//! file, or, with a trailing `/`, a whole directory subtree — and the dev server
//! resolves the same reference to serve it. It is a package reference resolved within
//! the resolved package, so the symlink mode never applies: the reference is followed in
//! every mode, and only ever into the package it names.

use std::path::{Path, PathBuf};

use crate::{Error, Result};

/// The URL scheme that marks a symlink target as a `node_modules` package reference.
pub(crate) const SCHEME: &str = "npm://";

/// If `path` is a symlink whose target is an `npm://…` URL, returns that target string.
/// A symlink to an ordinary path, or a non-symlink, is `None`.
pub(crate) fn link_target(path: &Path) -> Option<String> {
    if !std::fs::symlink_metadata(path)
        .ok()?
        .file_type()
        .is_symlink()
    {
        return None;
    }
    let target = std::fs::read_link(path).ok()?;
    let target = target.to_str()?;
    target.starts_with(SCHEME).then(|| target.to_owned())
}

/// A parsed `npm://<package>/<subpath>` reference.
pub(crate) struct Reference {
    pub package: String,
    pub subpath: String,
    /// A trailing slash mounts the whole directory rather than a single file.
    pub directory: bool,
}

/// Parse `npm://<package>/<subpath>`; the package may be scoped (`@scope/name`). `None`
/// for a target that is not a well-formed `npm://` reference.
pub(crate) fn parse(target: &str) -> Option<Reference> {
    let rest = target.strip_prefix(SCHEME)?;
    let directory = rest.ends_with('/');
    let rest = rest.trim_end_matches('/');
    let (package, subpath) = match rest.strip_prefix('@') {
        // Scoped: the package spans two segments, `@scope/name`.
        Some(scoped) => {
            let mut parts = scoped.splitn(3, '/');
            let scope = parts.next().filter(|s| !s.is_empty())?;
            let name = parts.next().filter(|s| !s.is_empty())?;
            (format!("@{scope}/{name}"), parts.next().unwrap_or(""))
        }
        None => {
            let mut parts = rest.splitn(2, '/');
            let name = parts.next().filter(|s| !s.is_empty())?;
            (name.to_owned(), parts.next().unwrap_or(""))
        }
    };
    Some(Reference {
        package,
        subpath: subpath.to_owned(),
        directory,
    })
}

/// Resolve a reference against the `node_modules` reachable from `from_dir`, returning
/// what to emit as `(subpath-beneath-the-link, source file on disk)` pairs: one entry
/// with an empty subpath for a file reference, or every file beneath the directory for a
/// directory mount (sorted for deterministic output).
pub(crate) fn resolve(from_dir: &Path, reference: &Reference) -> Result<Vec<(PathBuf, PathBuf)>> {
    if reference.directory {
        let dir = npm_utils::resolve::package_dir(from_dir, &reference.package)
            .map_err(|e| Error::Build(format!("npm://{}: {e}", reference.package)))?;
        // Resolve the mounted directory through the package's canonical location and
        // require it to stay inside — the subpath is attacker-influenced, so an
        // in-package symlink must not redirect the mount outside the module.
        let package_root = dir
            .canonicalize()
            .map_err(|e| Error::Build(format!("npm://{}: {e}", reference.package)))?;
        let base = if reference.subpath.is_empty() {
            package_root.clone()
        } else {
            let joined = npm_utils::path_safety::safe_join(&dir, &reference.subpath)
                .map_err(|e| Error::Build(format!("npm://{}: {e}", reference.package)))?;
            let real = joined.canonicalize().map_err(|e| {
                Error::Build(format!(
                    "npm://{}/{}: {e}",
                    reference.package, reference.subpath
                ))
            })?;
            if !real.starts_with(&package_root) {
                return Err(Error::Build(format!(
                    "npm://{}/{}: resolves outside the package",
                    reference.package, reference.subpath
                )));
            }
            real
        };
        if !base.is_dir() {
            return Err(Error::Build(format!(
                "npm://{}/{}: not a directory in the package",
                reference.package, reference.subpath
            )));
        }
        let mut files = Vec::new();
        // `follow_links` defaults to false, so a symlink inside the mount is neither
        // descended nor (being a symlink, not a file) emitted — only the real files
        // physically under the contained base ship.
        for entry in walkdir::WalkDir::new(&base).sort_by_file_name() {
            let entry =
                entry.map_err(|e| Error::Build(format!("npm://{}: {e}", reference.package)))?;
            if entry.file_type().is_file() {
                let rel = entry
                    .path()
                    .strip_prefix(&base)
                    .map_err(|e| Error::Build(e.to_string()))?;
                files.push((rel.to_path_buf(), entry.path().to_path_buf()));
            }
        }
        Ok(files)
    } else {
        let file =
            npm_utils::resolve::package_file(from_dir, &reference.package, &reference.subpath)
                .map_err(|e| Error::Build(format!("npm://{}: {e}", reference.package)))?;
        Ok(vec![(PathBuf::new(), file)])
    }
}

/// Resolve a dev-server request against the source tree: if `relative` under `root` (or
/// one of its ancestors) is an `npm://` symlink, return the real `node_modules` file it
/// maps to. `None` when no `npm://` link lies on the path. The static server never needs
/// this — it serves a built tree where the files are already real.
#[cfg(feature = "axum")]
pub(crate) fn serve_target(root: &Path, relative: &Path) -> Option<PathBuf> {
    let mut prefix = root.to_path_buf();
    let mut components = relative.components();
    while let Some(component) = components.next() {
        prefix.push(component.as_os_str());
        let Some(target) = link_target(&prefix) else {
            continue;
        };
        let reference = parse(&target)?;
        let from_dir = std::fs::canonicalize(prefix.parent()?).ok()?;
        let remainder = components.as_path();
        let file = if reference.directory || !remainder.as_os_str().is_empty() {
            // A directory mount, or a file reached through one: the package directory,
            // plus the reference's own subpath, plus the request's remaining components —
            // then resolved through the package's canonical location and refused if it
            // escapes, so an in-package symlink cannot serve a file outside the module.
            let package_root = npm_utils::resolve::package_dir(&from_dir, &reference.package)
                .ok()?
                .canonicalize()
                .ok()?;
            let mut file = package_root.clone();
            if !reference.subpath.is_empty() {
                file = npm_utils::path_safety::safe_join(&file, &reference.subpath).ok()?;
            }
            if !remainder.as_os_str().is_empty() {
                file = npm_utils::path_safety::safe_join(&file, remainder.to_str()?).ok()?;
            }
            let real = file.canonicalize().ok()?;
            if !real.starts_with(&package_root) {
                return None;
            }
            real
        } else {
            npm_utils::resolve::package_file(&from_dir, &reference.package, &reference.subpath)
                .ok()?
        };
        return file.is_file().then_some(file);
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_file_and_directory_references() {
        let file = parse("npm://bootstrap-icons/icons/eye.svg").unwrap();
        assert_eq!(file.package, "bootstrap-icons");
        assert_eq!(file.subpath, "icons/eye.svg");
        assert!(!file.directory);

        let dir = parse("npm://bootstrap-icons/icons/").unwrap();
        assert_eq!(dir.package, "bootstrap-icons");
        assert_eq!(dir.subpath, "icons");
        assert!(dir.directory);

        let scoped = parse("npm://@scope/pkg/a/b.svg").unwrap();
        assert_eq!(scoped.package, "@scope/pkg");
        assert_eq!(scoped.subpath, "a/b.svg");

        assert!(parse("./icons/eye.svg").is_none());
        assert!(parse("npm://").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn directory_mount_skips_an_escaping_symlink() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let icons = tmp.path().join("node_modules/pkg/icons");
        std::fs::create_dir_all(&icons).unwrap();
        std::fs::write(
            tmp.path().join("node_modules/pkg/package.json"),
            r#"{"name":"pkg"}"#,
        )
        .unwrap();
        std::fs::write(icons.join("real.svg"), "<svg/>").unwrap();
        std::fs::write(tmp.path().join("secret.txt"), "secret").unwrap();
        symlink(tmp.path().join("secret.txt"), icons.join("leak.svg")).unwrap();
        let web = tmp.path().join("web");
        std::fs::create_dir_all(&web).unwrap();

        let reference = parse("npm://pkg/icons/").unwrap();
        let out = resolve(&web, &reference).unwrap();
        // The escaping symlink is skipped; only the real in-package file ships.
        let names: Vec<_> = out
            .iter()
            .map(|(rel, _)| rel.to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["real.svg"]);
    }

    #[cfg(all(unix, feature = "axum"))]
    #[test]
    fn resolves_and_serves_a_file_reference() {
        use std::os::unix::fs::symlink;
        use tempfile::tempdir;
        let tmp = tempdir().unwrap();
        let icons = tmp.path().join("node_modules/bootstrap-icons/icons");
        std::fs::create_dir_all(&icons).unwrap();
        std::fs::write(
            tmp.path().join("node_modules/bootstrap-icons/package.json"),
            r#"{"name":"bootstrap-icons"}"#,
        )
        .unwrap();
        std::fs::write(icons.join("eye.svg"), "<svg/>").unwrap();
        let bi = tmp.path().join("web/icons/bi");
        std::fs::create_dir_all(&bi).unwrap();
        symlink("npm://bootstrap-icons/icons/eye.svg", bi.join("eye.svg")).unwrap();

        // Build side: resolve → the real file, with an empty per-file subpath.
        let reference = parse("npm://bootstrap-icons/icons/eye.svg").unwrap();
        let out = resolve(&bi.canonicalize().unwrap(), &reference).unwrap();
        assert_eq!(out.len(), 1);
        assert!(out[0].0.as_os_str().is_empty());
        assert_eq!(std::fs::read_to_string(&out[0].1).unwrap(), "<svg/>");

        // Dev side: a request reaching the link serves the same bytes.
        let web = tmp.path().join("web");
        let served = serve_target(&web, Path::new("icons/bi/eye.svg")).unwrap();
        assert_eq!(std::fs::read_to_string(served).unwrap(), "<svg/>");
        // A path with no npm:// link on it is not this resolver's concern.
        assert!(serve_target(&web, Path::new("icons/bi")).is_none());
    }
}
