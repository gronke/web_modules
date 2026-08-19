//! Walking a directory tree that came from outside the project — an extracted archive, a
//! dependency, a path a user named.
//!
//! Such a tree can carry symbolic links, and a link is a way out of it: a link to `/etc`
//! or a link back to an ancestor turns a walk into a traversal or a loop. Every walk here
//! resolves the root once, refuses to follow links, and yields paths **relative** to that
//! root, so a caller cannot accidentally rejoin an escaped path onto a different base.

use std::path::{Path, PathBuf};

use walkdir::WalkDir;

use crate::{Error, Result};

/// Every file under `root`, recursively, as paths relative to `root`.
///
/// Symbolic links are neither followed nor reported, which is what keeps the result inside
/// `root`: a link is the only entry a filesystem walk can reach outside the tree it started
/// in. Sorted, so callers see a stable order.
pub fn files_within(root: &Path) -> Result<Vec<PathBuf>> {
    let real_root = root
        .canonicalize()
        .map_err(|e| Error::Vendor(format!("{}: {e}", root.display())))?;
    let mut out = Vec::new();
    for entry in WalkDir::new(&real_root).follow_links(false) {
        let entry = entry.map_err(|e| Error::Vendor(e.to_string()))?;
        if entry.path_is_symlink() || !entry.file_type().is_file() {
            continue;
        }
        // Relative to the resolved root, so the path carries no `..` and no root prefix a
        // caller could join onto the wrong base. An entry that does not strip is outside
        // the tree and is dropped rather than reported.
        if let Ok(rel) = entry.path().strip_prefix(&real_root) {
            out.push(rel.to_path_buf());
        }
    }
    out.sort();
    Ok(out)
}

/// Whether `path` resolves inside `root`, both canonicalized first so that a link or a `..`
/// component is compared as what it reaches rather than as what it reads.
///
/// A path that does not exist cannot be resolved, and so is not contained.
pub fn contains(root: &Path, path: &Path) -> bool {
    match (root.canonicalize(), path.canonicalize()) {
        (Ok(root), Ok(path)) => path.starts_with(root),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn files_within_yields_relative_paths_in_order() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("a/b")).unwrap();
        std::fs::write(dir.path().join("a/b/deep.ts"), "").unwrap();
        std::fs::write(dir.path().join("top.ts"), "").unwrap();
        assert_eq!(
            files_within(dir.path()).unwrap(),
            vec![PathBuf::from("a/b/deep.ts"), PathBuf::from("top.ts")]
        );
    }

    /// A link is how a walk leaves the tree it was pointed at, so an archive that carries
    /// one must not widen what a walk reads.
    #[test]
    #[cfg(unix)]
    fn a_link_out_of_the_tree_is_not_walked() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.ts"), "").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("own.ts"), "").unwrap();
        std::os::unix::fs::symlink(outside.path(), dir.path().join("escape")).unwrap();
        std::os::unix::fs::symlink(
            outside.path().join("secret.ts"),
            dir.path().join("secret.ts"),
        )
        .unwrap();

        assert_eq!(
            files_within(dir.path()).unwrap(),
            vec![PathBuf::from("own.ts")],
            "neither the linked directory nor the linked file is reported"
        );
    }

    /// A link back to an ancestor makes a following walk recurse forever.
    #[test]
    #[cfg(unix)]
    fn a_link_to_an_ancestor_terminates() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/one.ts"), "").unwrap();
        std::os::unix::fs::symlink(dir.path(), dir.path().join("sub/loop")).unwrap();
        assert_eq!(
            files_within(dir.path()).unwrap(),
            vec![PathBuf::from("sub/one.ts")]
        );
    }

    #[test]
    #[cfg(unix)]
    fn containment_compares_what_a_path_reaches() {
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("f"), "").unwrap();
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("own"), "").unwrap();
        std::os::unix::fs::symlink(outside.path().join("f"), dir.path().join("link")).unwrap();

        assert!(contains(dir.path(), &dir.path().join("own")));
        assert!(
            !contains(dir.path(), &dir.path().join("link")),
            "the link reads as inside but reaches outside"
        );
        assert!(!contains(dir.path(), &dir.path().join("../etc")));
        assert!(
            !contains(dir.path(), &dir.path().join("missing")),
            "an unresolvable path is not contained"
        );
    }
}
