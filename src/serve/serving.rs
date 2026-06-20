//! Shared serving primitives: URL-prefix matching, path containment, and content
//! types, used by both the static ([`server`](crate::server)) and live
//! ([`dev`](crate::dev)) routers.
//!
//! [`contained_file`] is the **sandbox boundary**: a request can never resolve to a
//! file outside a known root, even via `..` or a symlink. (The seed of the planned
//! processor sandbox, where known roots are the allowlist.)

use std::path::{Component, Path, PathBuf};

/// `requested` (no leading `/`) relative to `prefix` (surrounding `/` ignored;
/// `""`/`"/"` is the site root and matches everything), or `None` if it doesn't fall
/// under the prefix.
pub(crate) fn relative_under(prefix: &str, requested: &str) -> Option<String> {
    let prefix = prefix.trim_matches('/');
    if prefix.is_empty() {
        return Some(requested.to_string());
    }
    if requested == prefix {
        return Some(String::new());
    }
    requested
        .strip_prefix(&format!("{prefix}/"))
        .map(str::to_string)
}

/// Layer 1 (lexical): reject a request path that could traverse out of a root (a
/// `..` component, an absolute root, or a drive prefix) before it touches the FS.
pub(crate) fn has_traversal(path: &str) -> bool {
    Path::new(path).components().any(|c| {
        matches!(
            c,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    })
}

/// Layer 2 (resolved): join `relative` onto `root` and return it only as an existing
/// file that stays inside `root` once `.`/`..` and symlinks are resolved, so even a
/// symlink inside a root cannot point outside it.
pub(crate) fn contained_file(root: &Path, relative: &str) -> Option<PathBuf> {
    let real = root.join(relative).canonicalize().ok()?;
    let real_root = root.canonicalize().ok()?;
    (real.is_file() && real.starts_with(&real_root)).then_some(real)
}

/// MIME type from a path's extension, defaulting to `application/octet-stream`.
pub(crate) fn content_type(path: &str) -> String {
    mime_guess::from_path(path)
        .first_or_octet_stream()
        .to_string()
}

/// Extensions of *source* files the toolchain compiles rather than serves: `.ts`/
/// `.tsx`/`.mts` → `.js`, `.scss` → `.css`, `.tera` → its rendered target. Such
/// originals are kept out of HTTP responses; the client only ever gets the compiled
/// output.
const SOURCE_EXTENSIONS: [&str; 5] = ["ts", "tsx", "mts", "scss", "tera"];

/// Whether `path`'s extension marks it as a [source file](SOURCE_EXTENSIONS), matched
/// **case-insensitively**. Case matters for safety: on a case-insensitive filesystem
/// (macOS, Windows) a request for `app.SCSS` resolves to the on-disk `app.scss`, so a
/// case-sensitive guard would hand back the raw source.
pub(crate) fn is_source_file(path: &str) -> bool {
    has_source_extension(Path::new(path))
}

/// Like [`is_source_file`], but on a resolved [`Path`]. Apply this to the path a request
/// actually resolved to (after `canonicalize`), so OS-level name folding the request
/// string didn't reveal (case folding, or a Windows trailing `.`/space) can't smuggle a
/// source past the guard.
pub(crate) fn has_source_extension(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|ext| {
            SOURCE_EXTENSIONS
                .iter()
                .any(|s| ext.eq_ignore_ascii_case(s))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_under_root_and_nested() {
        assert_eq!(relative_under("/", "app.js").as_deref(), Some("app.js"));
        assert_eq!(
            relative_under("/modules/contacts/", "modules/contacts/list.js").as_deref(),
            Some("list.js")
        );
        // Exact prefix hit (a directory request normalises to index.html upstream).
        assert_eq!(relative_under("/ui/", "ui").as_deref(), Some(""));
        assert_eq!(relative_under("/ui/", "other/x.js"), None);
    }

    #[test]
    fn has_traversal_flags_escapes_only() {
        assert!(has_traversal("../secret"));
        assert!(has_traversal("a/../../b"));
        assert!(has_traversal("/etc/passwd"));
        assert!(!has_traversal("app.js"));
        assert!(!has_traversal("sub/app.js"));
        assert!(!has_traversal("index.html"));
    }

    #[test]
    fn is_source_file_flags_compiled_inputs() {
        assert!(is_source_file("app.ts"));
        assert!(is_source_file("a/b.scss"));
        assert!(is_source_file("index.html.tera"));
        assert!(!is_source_file("app.js"));
        assert!(!is_source_file("app.css"));
        assert!(!is_source_file("index.html"));
        assert!(!is_source_file("logo.svg"));
    }

    #[test]
    fn is_source_file_is_case_insensitive() {
        // On a case-insensitive FS (macOS, Windows) `app.SCSS` opens `app.scss`, so the
        // guard must flag case variants too — else the raw source leaks.
        assert!(is_source_file("app.SCSS"));
        assert!(is_source_file("App.Scss"));
        assert!(is_source_file("main.TS"));
        assert!(is_source_file("x.TSX"));
        assert!(is_source_file("y.MTS"));
        assert!(is_source_file("page.html.TERA"));
    }

    #[test]
    fn has_source_extension_checks_resolved_paths() {
        // Same rule applied to a resolved (canonicalised) path — what the disk-backed
        // routers test on the file they actually opened.
        assert!(has_source_extension(Path::new("/abs/web/app.scss")));
        assert!(has_source_extension(Path::new("/abs/web/app.SCSS")));
        assert!(has_source_extension(Path::new("main.TS")));
        assert!(!has_source_extension(Path::new("/abs/web/app.css")));
        assert!(!has_source_extension(Path::new("/abs/web/app.js")));
        // A `.gz` extension is not itself a source; the gz path is de-gzipped by the
        // caller (via `file_stem`) before this check.
        assert!(!has_source_extension(Path::new("/abs/web/app.css.gz")));
        assert!(!has_source_extension(Path::new("/abs/web/noext")));
    }

    #[test]
    fn contained_file_keeps_inside_rejects_outside() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("app.js"), b"x").unwrap();
        std::fs::write(tmp.path().join("secret"), b"s").unwrap();
        assert!(contained_file(&root, "app.js").is_some());
        assert!(contained_file(&root, "../secret").is_none());
        assert!(contained_file(&root, "nope.js").is_none());
    }

    #[cfg(unix)]
    #[test]
    fn contained_file_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(tmp.path().join("outside.js"), b"x").unwrap();
        symlink(tmp.path().join("outside.js"), root.join("link.js")).unwrap();
        // Reachable through the root, but resolves outside it → rejected.
        assert!(contained_file(&root, "link.js").is_none());
    }
}
