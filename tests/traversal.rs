//! Path-traversal regression suite for the public [`Frontend`] routers.
//!
//! Companion to `tests/security.rs`: that file pins the headline promises, this one widens the
//! net to the crafted-request forms an attacker actually reaches for — percent-encoded `..` and
//! `/`, backslash separators, `%00`, trailing-dot / trailing-space name folding, bypass ladders
//! (`....//`, `..;/`), traversal *through* the allow-listed `.well-known`, a symlinked directory,
//! and the embedded (`include_dir!`) root — plus the SCSS side, where an `@import` must not climb
//! out of the source tree and inline a file the dev server would then serve.
//!
//! The request path is never percent-decoded on the way in, so every encoded form stays a literal
//! single segment (`contained_file`'s canonicalize rejects it when it names no in-root file); the
//! un-encoded `..` forms are caught lexically by `has_traversal`. Both routers are driven through
//! `tower::ServiceExt::oneshot`, the same path a real client hits.
//!
//! Needs the `dev` feature (for [`Frontend::dev`] and on-the-fly SCSS); run under `--features full`.
#![cfg(feature = "dev")]

use std::path::Path;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use include_dir::{include_dir, Dir};
use tower::ServiceExt;
use web_modules::Frontend;

/// GET `uri` against a clone of `router`; return `(status, content-type, body)`.
async fn get(router: Router, uri: &str) -> (StatusCode, String, Vec<u8>) {
    let res = router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = res.status();
    let content_type = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("")
        .to_string();
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    (status, content_type, body)
}

fn write(path: &Path, content: impl AsRef<[u8]>) {
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, content).unwrap();
}

fn contains(body: &[u8], needle: &str) -> bool {
    String::from_utf8_lossy(body).contains(needle)
}

/// The crafted request forms this suite fires at a root; none may resolve to the out-of-root
/// secret. The encoded variants stay literal single segments (the path is not decoded); the
/// un-encoded `..` variants are rejected lexically.
const TRAVERSAL_URIS: &[&str] = &[
    "/../secret.txt",
    "/a/../../secret.txt",
    "/%2e%2e/secret.txt",        // encoded ".."
    "/a%2f..%2f..%2fsecret.txt", // encoded "/"
    "/..%5c..%5csecret.txt",     // encoded "\" — a Windows separator
    "/....//secret.txt",         // a naive single "../" strip would leave a traversal
    "/..;/secret.txt",           // a ";"-suffixed bypass form
    "/%00secret.txt",            // leading NUL
    "/secret.txt%00.js",         // NUL before an allowed extension
];

#[tokio::test]
async fn static_router_refuses_every_traversal_form() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(&root.join("app.js"), b"console.log('in-root');");
    write(&tmp.path().join("secret.txt"), b"TOPSECRET");

    let app = Frontend::dir(&root).router();
    // Control: the in-root file really is served, so a non-200 elsewhere means "refused", not "empty".
    let (status, _, body) = get(app.clone(), "/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(contains(&body, "in-root"));

    for uri in TRAVERSAL_URIS {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} must not 200");
        assert!(!contains(&body, "TOPSECRET"), "{uri} leaked the secret");
    }
}

#[tokio::test]
async fn dev_router_refuses_every_traversal_form() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(&root.join("app.js"), b"console.log('in-root');");
    write(&tmp.path().join("secret.txt"), b"TOPSECRET");

    let app = Frontend::dir(&root).dev();
    for uri in TRAVERSAL_URIS {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} must not 200");
        assert!(!contains(&body, "TOPSECRET"), "{uri} leaked the secret");
    }
}

#[tokio::test]
async fn well_known_is_reachable_but_not_a_traversal_ladder() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(
        &root.join(".well-known/security.txt"),
        b"contact: mailto:x@y",
    );
    write(&tmp.path().join("secret.txt"), b"TOPSECRET");

    let app = Frontend::dir(&root).router();
    // The allow-listed directory itself is reachable.
    let (status, _, body) = get(app.clone(), "/.well-known/security.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert!(contains(&body, "contact:"));

    // But it is not a rung to climb out on.
    for uri in ["/.well-known/../secret.txt", "/.well-known/..%2fsecret.txt"] {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} must not 200");
        assert!(
            !contains(&body, "TOPSECRET"),
            "{uri} leaked via .well-known"
        );
    }
}

#[tokio::test]
async fn dev_router_hides_scss_source_under_fold_and_bypass_forms() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(
        &root.join("app.scss"),
        b"// SCSS-SOURCE-SECRET\na { color: red; }",
    );

    let app = Frontend::dir(&root).dev();
    // The compiled target is served and carries none of the source.
    let (status, ct, body) = get(app.clone(), "/app.css").await;
    assert_eq!(status, StatusCode::OK);
    assert!(ct.contains("css"));
    assert!(!contains(&body, "SCSS-SOURCE-SECRET"));

    // The raw source is never served — not directly, nor via a case / trailing-dot / trailing-space
    // / encoded-dot fold that a case-insensitive or Windows filesystem might resolve to `app.scss`.
    for uri in [
        "/app.scss",
        "/app.SCSS",
        "/app.scss.",
        "/app.scss%20",
        "/app%2escss",
    ] {
        let (status, _, body) = get(app.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} served source");
        assert!(
            !contains(&body, "SCSS-SOURCE-SECRET"),
            "{uri} leaked source"
        );
    }
}

#[cfg(unix)]
#[tokio::test]
async fn static_router_refuses_a_symlinked_directory_escape() {
    use std::os::unix::fs::symlink;
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(&root.join("app.js"), b"ok");
    let outside = tmp.path().join("outside");
    write(&outside.join("secret.txt"), b"TOPSECRET");
    // A *directory* symlink inside the root that points out of it (the tested symlink cases in
    // `tests/security.rs` are file symlinks).
    symlink(&outside, root.join("link")).unwrap();

    let app = Frontend::dir(&root).router();
    let (status, _, body) = get(app.clone(), "/link/secret.txt").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!contains(&body, "TOPSECRET"));
}

/// A small tree baked into the test binary, standing in for a production embedded frontend.
static EMBEDDED: Dir = include_dir!("$CARGO_MANIFEST_DIR/tests/embedded_fixture");

#[tokio::test]
async fn embedded_root_refuses_traversal_and_hides_sources() {
    let app = Frontend::embedded(&EMBEDDED).router();
    // A baked asset is served.
    let (status, _, body) = get(app.clone(), "/app.js").await;
    assert_eq!(status, StatusCode::OK);
    assert!(contains(&body, "embedded-app-ok"));

    // A baked *source* file is hidden, exactly as on a filesystem root.
    let (status, _, body) = get(app.clone(), "/app.scss").await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert!(!contains(&body, "EMBEDDED-SCSS-SECRET"));

    // Traversal out of the in-memory tree is refused (the embedded branch has only the lexical
    // `has_traversal` guard — there is no filesystem to canonicalize against).
    for uri in ["/../Cargo.toml", "/..%2fCargo.toml", "/%2e%2e/Cargo.toml"] {
        let (status, _, _) = get(app.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} escaped the embedded root");
    }
}

#[tokio::test]
async fn dev_router_refuses_an_scss_import_escaping_the_root() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    // A valid partial with a recognisable body, just outside the served tree.
    write(
        &tmp.path().join("_secret.scss"),
        b"/* SCSS-LEAK */ $c: #abcdef;",
    );
    // A normal stylesheet that must keep compiling and serving.
    write(&root.join("ok.scss"), b"a { color: red; }");
    // The hostile source tries to climb out and inline the secret.
    write(
        &root.join("app.scss"),
        b"@import '../secret'; a { color: $c; }",
    );

    let app = Frontend::dir(&root).dev();

    let (status, ct, body) = get(app.clone(), "/ok.css").await;
    assert_eq!(status, StatusCode::OK, "a normal stylesheet still compiles");
    assert!(ct.contains("css"));
    assert!(contains(&body, "red"));

    let (status, _, body) = get(app.clone(), "/app.css").await;
    assert_ne!(
        status,
        StatusCode::OK,
        "an @import escaping the root must not compile to 200"
    );
    assert!(
        !contains(&body, "SCSS-LEAK"),
        "the out-of-root partial's body leaked"
    );
    assert!(!contains(&body, "abcdef"), "the out-of-root value leaked");
}
