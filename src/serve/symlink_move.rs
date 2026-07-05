//! The redirect symlink modes (feature `symlink-move`, default-on): serving answers
//! `307 Temporary Redirect` / `308 Permanent Redirect` with the symlink's own content
//! as the `Location`, and never opens anything through a link.
//!
//! This is the crate's own special sauce on top of plain symlink following, so it is
//! strippable: `--no-default-features` yields a build in which a symlink can never
//! become a redirect, while `Follow` / `FollowUnsafe` (in [`super::serving`]) are
//! always available. The build side has no redirect to express — the preflight skips
//! links under these modes with a warning.

use std::path::Path;

use super::serving::{contained_file, Resolved};

/// Resolve `relative` under `root` for the redirect modes: walk the request's
/// components with `symlink_metadata`, and the first symlink on the chain (a file,
/// or a directory on the way) answers with its content as the redirect; when no
/// component is a link, [`contained_file`] decides — plain files keep the identical
/// guard chain the other modes use.
pub(crate) fn resolve(root: &Path, relative: &str) -> Option<Resolved> {
    // Request paths are URL-shaped (`/`-separated; `has_traversal` already rejected
    // `..`), so the component split mirrors the request exactly.
    let components: Vec<&str> = relative.split('/').filter(|c| !c.is_empty()).collect();
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let meta = std::fs::symlink_metadata(&current).ok()?;
        if meta.file_type().is_symlink() {
            let target = std::fs::read_link(&current).ok()?;
            let suffix = components[index + 1..].join("/");
            return location_value(&target, &suffix).map(Resolved::Redirect);
        }
    }
    contained_file(root, relative).map(Resolved::File)
}

/// The `Location` a symlink stands for: the link content taken **literally** (an
/// absolute target becomes a site-absolute URL path; a relative target is a relative
/// reference the client resolves against the request URL), with the request's
/// remaining components appended when the link named a directory on the way. Refuses
/// content that cannot be a safe header value — empty, non-UTF-8, or any control
/// byte (CR/LF response splitting). The target is never opened; whether it exists is
/// the client's follow-up request to find out.
pub(crate) fn location_value(target: &Path, suffix: &str) -> Option<String> {
    let mut location = target.to_str()?.to_string();
    if !suffix.is_empty() {
        if !location.ends_with('/') {
            location.push('/');
        }
        location.push_str(suffix);
    }
    if location.is_empty() || location.bytes().any(|b| b < 0x20 || b == 0x7f) {
        return None;
    }
    Some(location)
}

/// The `307`/`308` a symlink resolves to: status plus `Location`, **empty body** —
/// the target is never read, so a redirect cannot disclose content. Built through
/// the fallible `HeaderValue` path (`axum::response::Redirect` would panic on an
/// invalid value); [`location_value`] pre-sanitizes, this is the second net.
pub(crate) fn redirect_response(
    location: &str,
    permanent: bool,
) -> Option<axum::response::Response> {
    use axum::http::{header, HeaderValue, StatusCode};
    use axum::response::IntoResponse;
    let value = HeaderValue::try_from(location).ok()?;
    let status = if permanent {
        StatusCode::PERMANENT_REDIRECT
    } else {
        StatusCode::TEMPORARY_REDIRECT
    };
    Some((status, [(header::LOCATION, value)]).into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn resolve_redirects_through_a_directory_link_with_the_suffix_joined() {
        use std::os::unix::fs::symlink;
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(root.join("theme")).unwrap();
        std::fs::write(root.join("theme/site.css"), b"x").unwrap();
        symlink(Path::new("theme"), root.join("styles")).unwrap();

        match resolve(&root, "styles/site.css") {
            Some(Resolved::Redirect(location)) => assert_eq!(location, "theme/site.css"),
            other => panic!("expected a redirect, got {:?}", other.is_some()),
        }
        // Plain files keep the full contained guard chain.
        match resolve(&root, "theme/site.css") {
            Some(Resolved::File(real)) => assert!(real.ends_with("theme/site.css")),
            other => panic!("expected the plain file, got {:?}", other.is_some()),
        }
    }

    #[test]
    fn location_value_joins_and_sanitizes() {
        assert_eq!(
            location_value(Path::new("theme"), "sub/site.css").as_deref(),
            Some("theme/sub/site.css")
        );
        assert_eq!(
            location_value(Path::new("theme/"), "site.css").as_deref(),
            Some("theme/site.css"),
            "no doubled slash"
        );
        assert_eq!(
            location_value(Path::new("/etc/passwd"), "").as_deref(),
            Some("/etc/passwd"),
            "absolute targets are literal URL paths - never opened"
        );
        // Header-injection guard: control bytes kill the redirect entirely.
        assert!(location_value(Path::new("x\r\nSet-Cookie: a=b"), "").is_none());
        assert!(location_value(Path::new(""), "").is_none());
    }

    #[test]
    fn redirect_response_sets_status_and_location_with_an_empty_body() {
        let temporary = redirect_response("target.js", false).unwrap();
        assert_eq!(temporary.status(), 307);
        assert_eq!(temporary.headers()["location"], "target.js");
        let permanent = redirect_response("target.js", true).unwrap();
        assert_eq!(permanent.status(), 308);
        // An invalid header value is refused, not panicked on.
        assert!(redirect_response("bad\nvalue", true).is_none());
    }
}
