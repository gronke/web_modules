//! Regression suite for the selectable symlink modes.
//!
//! The default (`Follow`) is pinned by `tests/security.rs` and `tests/traversal.rs`,
//! which pass unedited — this file covers what each *other* mode changes and, just as
//! important, what it must not change: `follow-unsafe` relaxes only the containment
//! refusal (request-path traversal, the reject list, and source-hiding all stay);
//! `redirect`/`move` answer with the symlink's own content as the `Location` and
//! never open the target, while a build skips the link with a warning.
//!
//! Needs the `dev` feature (for [`Frontend::dev`]); symlink fixtures are Unix-only.
#![cfg(all(feature = "dev", unix))]

use std::path::Path;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;
use web_modules::build::{build, BuildOptions, Output, Processors};
use web_modules::{Frontend, SymlinkMode};

/// GET `uri` against a clone of `router`; return `(status, location, body)`.
async fn get(router: Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let res = router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let location = res
        .headers()
        .get(header::LOCATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, location, body)
}

fn write(path: &Path, content: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn link(target: impl AsRef<Path>, at: &Path) {
    std::os::unix::fs::symlink(target, at).unwrap();
}

/// A tree with an out-of-root secret reachable through `web/exposed.txt`, a rejected
/// dotfile, and a symlinked-out TypeScript source.
fn escape_fixture(tmp: &Path) -> std::path::PathBuf {
    write(&tmp.join("outside/secret.txt"), "out-of-root");
    write(&tmp.join("outside/app.ts"), "export const fromOutside = 1;");
    let web = tmp.join("web");
    write(&web.join("index.html"), "<x>");
    write(&web.join(".env"), "S=1");
    link(tmp.join("outside/secret.txt"), &web.join("exposed.txt"));
    link(tmp.join("outside/app.ts"), &web.join("app.ts"));
    web
}

#[tokio::test]
async fn follow_unsafe_serves_the_escape_but_keeps_every_other_guard() {
    let tmp = tempfile::tempdir().unwrap();
    let web = escape_fixture(tmp.path());

    // Static router and dev router make the same decisions.
    let static_router = Frontend::dir(&web)
        .symlinks(SymlinkMode::FollowUnsafe)
        .router();
    let dev_router = Frontend::new()
        .source(&web)
        .symlinks(SymlinkMode::FollowUnsafe)
        .dev();
    for router in [static_router, dev_router] {
        let (status, _, body) = get(router.clone(), "/exposed.txt").await;
        assert_eq!(status, StatusCode::OK, "the mode's contract");
        assert_eq!(body, b"out-of-root");

        // Only the containment refusal relaxed: traversal forms stay refused …
        for uri in ["/../outside/secret.txt", "/%2e%2e/outside/secret.txt"] {
            let (status, _, body) = get(router.clone(), uri).await;
            assert_ne!(status, StatusCode::OK, "{uri} must not serve");
            assert!(
                !String::from_utf8_lossy(&body).contains("out-of-root"),
                "{uri} must not leak"
            );
        }
        // … the reject list stays …
        let (status, _, _) = get(router.clone(), "/.env").await;
        assert_eq!(
            status,
            StatusCode::NOT_FOUND,
            "rejected name stays rejected"
        );
        // … and sources stay hidden.
        let (status, _, _) = get(router.clone(), "/app.ts").await;
        assert_eq!(status, StatusCode::NOT_FOUND, "raw source stays hidden");
    }

    // The symlinked-out source still compiles to its target (dev only).
    let dev_router = Frontend::new()
        .source(&web)
        .symlinks(SymlinkMode::FollowUnsafe)
        .dev();
    let (status, _, body) = get(dev_router, "/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(String::from_utf8_lossy(&body).contains("fromOutside"));
}

#[cfg(feature = "symlink-move")]
#[tokio::test]
async fn redirect_answers_with_the_link_content_and_never_the_target() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    write(&web.join("real.txt"), "data");
    write(&web.join("theme/site.css"), "b{}");
    link("real.txt", &web.join("link.txt"));
    link("theme", &web.join("styles"));
    link("missing.js", &web.join("dangling.js"));
    link(".env", &web.join("config.js"));
    write(&web.join(".env"), "S=1");

    let static_router = Frontend::dir(&web).symlinks(SymlinkMode::Redirect).router();
    let dev_router = Frontend::new()
        .source(&web)
        .symlinks(SymlinkMode::Redirect)
        .dev();
    for router in [static_router, dev_router] {
        // A file link: 307, the link content verbatim, an empty body.
        let (status, location, body) = get(router.clone(), "/link.txt").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(location, "real.txt");
        assert!(body.is_empty(), "a redirect discloses nothing");

        // A directory link on the way: the remaining components join the target.
        let (status, location, _) = get(router.clone(), "/styles/site.css").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(location, "theme/site.css");

        // A dangling link redirects too — the target is the client's problem.
        let (status, location, _) = get(router.clone(), "/dangling.js").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(location, "missing.js");

        // A redirect to a rejected target is inert: the follow-up request 404s.
        let (status, location, _) = get(router.clone(), "/config.js").await;
        assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
        assert_eq!(location, ".env");
        let (status, _, _) = get(router.clone(), "/.env").await;
        assert_eq!(status, StatusCode::NOT_FOUND);

        // Plain files keep the full guard chain.
        let (status, _, body) = get(router.clone(), "/real.txt").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, b"data");
    }
}

#[cfg(feature = "symlink-move")]
#[tokio::test]
async fn move_answers_permanently_and_symlinked_sources_stay_dark() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    write(&web.join("real.txt"), "data");
    link("real.txt", &web.join("link.txt"));
    write(&tmp.path().join("outside/app.ts"), "export const x = 1;");
    link(tmp.path().join("outside/app.ts"), &web.join("app.ts"));

    let router = Frontend::dir(&web).symlinks(SymlinkMode::Move).router();
    let (status, location, _) = get(router, "/link.txt").await;
    assert_eq!(status, StatusCode::PERMANENT_REDIRECT);
    assert_eq!(location, "real.txt");

    // A symlinked compile candidate is skipped — 404, and no Location leaks the
    // hidden source's target.
    let dev_router = Frontend::new()
        .source(&web)
        .symlinks(SymlinkMode::Move)
        .dev();
    let (status, location, _) = get(dev_router, "/app.js").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(location.is_empty(), "no redirect for a hidden source");
}

fn build_with(web: &Path, out: &Path, mode: SymlinkMode) -> web_modules::Result<()> {
    let mut processors = Processors::default();
    processors.symlinks = mode;
    build(&BuildOptions {
        specs: &[],
        roots: std::slice::from_ref(&web.to_path_buf()),
        out,
        mount: "/web_modules",
        html: "<!doctype html>{importmap}",
        template: None,
        processors,
        output: Output::default(),
    })
}

#[test]
fn build_follow_unsafe_publishes_the_escape() {
    let tmp = tempfile::tempdir().unwrap();
    let web = escape_fixture(tmp.path());
    let out = tmp.path().join("out");
    build_with(&web, &out, SymlinkMode::FollowUnsafe).unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("exposed.txt")).unwrap(),
        "out-of-root",
        "the mode's contract: the escaping target ships"
    );
    assert!(
        out.join("app.js").exists(),
        "the symlinked-out source compiles"
    );
}

#[cfg(feature = "symlink-move")]
#[test]
fn build_redirect_skips_symlinks_and_ships_the_rest() {
    let tmp = tempfile::tempdir().unwrap();
    let web = tmp.path().join("web");
    write(&web.join("real.txt"), "data");
    link("real.txt", &web.join("link.txt"));

    let out = tmp.path().join("out");
    build_with(&web, &out, SymlinkMode::Redirect).unwrap();
    assert!(
        !out.join("link.txt").exists(),
        "a static build cannot express a redirect"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("real.txt")).unwrap(),
        "data"
    );
}

#[test]
fn build_default_mode_still_refuses_the_escape() {
    // The suite's own sanity line; the full default-mode promises live in
    // tests/security.rs and tests/traversal.rs, unedited.
    let tmp = tempfile::tempdir().unwrap();
    let web = escape_fixture(tmp.path());
    let out = tmp.path().join("out");
    let err = build_with(&web, &out, SymlinkMode::Follow)
        .unwrap_err()
        .to_string();
    assert!(err.contains("resolve outside the source root"), "got {err}");
}
