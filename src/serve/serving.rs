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

/// How a request resolved against a root under a [`SymlinkMode`](crate::SymlinkMode):
/// a real servable file, or — in the redirect modes (feature `symlink-move`) — the
/// redirect a symlink stands for.
pub(crate) enum Resolved {
    File(PathBuf),
    /// A sanitized, header-safe `Location` value (the symlink's own content).
    #[cfg(feature = "symlink-move")]
    Redirect(String),
}

/// [`contained_file`], under a symlink mode.
///
/// `Follow` is exactly `contained_file`. `FollowUnsafe` resolves wherever the link
/// points — only the containment refusal is dropped; a dangling or missing path is
/// still `None`. `Redirect`/`Move` never open anything through a link: the request's
/// components are walked with `symlink_metadata`, and the first symlink on the chain
/// (a file, or a directory on the way) answers with its content as the redirect;
/// when no component is a link, `contained_file` decides — plain files keep the
/// identical guard chain in every mode.
pub(crate) fn resolve_file(
    root: &Path,
    relative: &str,
    mode: crate::SymlinkMode,
) -> Option<Resolved> {
    match mode {
        crate::SymlinkMode::Follow => contained_file(root, relative).map(Resolved::File),
        crate::SymlinkMode::FollowUnsafe => {
            let real = root.join(relative).canonicalize().ok()?;
            real.is_file().then_some(Resolved::File(real))
        }
        #[cfg(feature = "symlink-move")]
        crate::SymlinkMode::Redirect | crate::SymlinkMode::Move => {
            super::symlink_move::resolve(root, relative)
        }
    }
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
/// output. One list with the static-copy stage, so build and dev hide the same set.
use crate::static_files::SOURCE_EXTENSIONS;

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
    fn has_traversal_is_lexical_and_leaves_encoded_forms_to_layer_two() {
        // Layer 1 only sees real path components. The request string is never percent-decoded,
        // so an encoded `..` or `/` and a `%00` are ordinary single segments that pass here; the
        // resolved `contained_file` (canonicalize + `starts_with`) is what rejects them when they
        // don't name a real in-root file.
        assert!(!has_traversal("%2e%2e/secret")); // encoded ".." — a literal segment, not a parent
        assert!(!has_traversal("a%2f..%2fb")); // encoded "/" — one segment, no traversal
        assert!(!has_traversal("app.js%00.ts")); // literal "%00", not a NUL byte
    }

    #[cfg(unix)]
    #[test]
    fn has_traversal_does_not_treat_backslash_as_a_separator_on_unix() {
        // On Unix `\` is an ordinary filename character, so a backslash "traversal" is a single
        // Normal component Layer 1 passes; `contained_file` keeps it in-root. On Windows the same
        // string's `\` are separators and Layer 1 flags the `..`.
        assert!(!has_traversal("..\\..\\secret"));
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

    #[cfg(unix)]
    #[test]
    fn resolve_file_modes_diverge_on_an_escaping_link() {
        use crate::SymlinkMode;
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(tmp.path().join("outside.js"), b"x").unwrap();
        symlink(tmp.path().join("outside.js"), root.join("link.js")).unwrap();

        // Follow: refused, exactly `contained_file`.
        assert!(resolve_file(&root, "link.js", SymlinkMode::Follow).is_none());
        // FollowUnsafe: the link resolves and serves.
        match resolve_file(&root, "link.js", SymlinkMode::FollowUnsafe) {
            Some(Resolved::File(real)) => assert!(real.ends_with("outside.js")),
            other => panic!("expected the escaped file, got {:?}", other.is_some()),
        }
        // Redirect: the link content is the Location; nothing is opened.
        #[cfg(feature = "symlink-move")]
        match resolve_file(&root, "link.js", SymlinkMode::Redirect) {
            Some(Resolved::Redirect(location)) => {
                assert_eq!(
                    location,
                    tmp.path().join("outside.js").display().to_string()
                );
            }
            other => panic!("expected a redirect, got {:?}", other.is_some()),
        }
    }

    #[cfg(unix)]
    #[test]
    fn resolve_file_handles_dangling_links_per_mode() {
        use crate::SymlinkMode;
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        symlink(Path::new("missing.js"), root.join("dangling.js")).unwrap();

        // Follow / FollowUnsafe: nothing resolves → 404.
        assert!(resolve_file(&root, "dangling.js", SymlinkMode::Follow).is_none());
        assert!(resolve_file(&root, "dangling.js", SymlinkMode::FollowUnsafe).is_none());
        // Redirect: the Location need not exist — the client finds out.
        #[cfg(feature = "symlink-move")]
        match resolve_file(&root, "dangling.js", SymlinkMode::Move) {
            Some(Resolved::Redirect(location)) => assert_eq!(location, "missing.js"),
            other => panic!("expected a redirect, got {:?}", other.is_some()),
        }
    }
}
