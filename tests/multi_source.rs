//! Multi-source behavior: several roots overlaid at one prefix, several mounts at
//! distinct prefixes, and symlinks that reach from one source into another.
//!
//! The adversarial half pins that a name collision between sources is never
//! undefined: without `skip_duplicates` the build refuses and names every claimant;
//! with it, exactly one deterministic winner ships and the loser's bytes appear
//! nowhere in the output.
//! Symlinks crossing sources keep the same discipline per mode: `Follow` refuses the
//! escape at build time and serving falls through to the next source, `FollowUnsafe`
//! makes the link collide like a regular file, and the redirect modes take the link
//! out of the claim set entirely, so the other source's file ships.
//!
//! Needs the `dev` feature (for [`Frontend`]); symlink fixtures are Unix-only.
#![cfg(all(feature = "dev", unix))]

use std::path::{Path, PathBuf};

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
    std::fs::create_dir_all(at.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(target, at).unwrap();
}

fn build_with(
    roots: &[PathBuf],
    out: &Path,
    mode: SymlinkMode,
    skip_duplicates: bool,
) -> web_modules::Result<()> {
    let mut processors = Processors::default();
    processors.symlinks = mode;
    processors.skip_duplicates = skip_duplicates;
    build(&BuildOptions {
        specs: &[],
        roots,
        out,
        mount: "/web_modules",
        html: "<!doctype html>{importmap}",
        template: None,
        processors,
        output: Output::default(),
    })
}

/// Whether any file under `dir` contains `needle` — the contamination probe: a
/// collision loser's bytes must not surface anywhere in the output, under any name.
fn tree_contains(dir: &Path, needle: &str) -> bool {
    for entry in std::fs::read_dir(dir).unwrap().filter_map(|e| e.ok()) {
        let path = entry.path();
        let meta = std::fs::symlink_metadata(&path).unwrap();
        if meta.is_dir() {
            if tree_contains(&path, needle) {
                return true;
            }
        } else if meta.is_file() {
            let bytes = std::fs::read(&path).unwrap();
            if String::from_utf8_lossy(&bytes).contains(needle) {
                return true;
            }
        }
    }
    false
}

#[test]
fn build_refuses_a_cross_root_collision_and_names_both_claimants() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a.join("app.txt"), "FROM-A");
    write(&b.join("app.txt"), "FROM-B");

    let err = build_with(
        &[a.clone(), b.clone()],
        &tmp.path().join("out"),
        SymlinkMode::Follow,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("claimed by more than one source"), "got {err}");
    for claimant in [a.join("app.txt"), b.join("app.txt")] {
        assert!(
            err.contains(&claimant.display().to_string()),
            "every claimant is named; got {err}"
        );
    }
    assert!(err.contains("wins with --skip-duplicates"), "got {err}");
}

#[test]
fn skip_duplicates_ships_exactly_the_first_roots_bytes() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a.join("app.txt"), "FROM-A");
    write(&b.join("app.txt"), "FROM-B");

    let out = tmp.path().join("out");
    build_with(&[a, b], &out, SymlinkMode::Follow, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("app.txt")).unwrap(),
        "FROM-A"
    );
    assert!(
        !tree_contains(&out, "FROM-B"),
        "the loser's bytes appear nowhere in the output"
    );
}

#[test]
fn a_later_root_cannot_hijack_an_earlier_roots_target() {
    // Precedence is root order first, rank second: a later root's literal `app.js`
    // must not shadow the earlier root's transformed `app.ts`, even though a literal
    // outranks a transform within one root.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a.join("app.ts"), "export const origin = \"FROM-A-TS\";");
    write(
        &b.join("app.js"),
        "export const origin = \"FROM-B-LITERAL\";",
    );

    let out = tmp.path().join("out");
    build_with(&[a, b], &out, SymlinkMode::Follow, true).unwrap();
    let shipped = std::fs::read_to_string(out.join("app.js")).unwrap();
    assert!(shipped.contains("FROM-A-TS"), "got {shipped}");
    assert!(
        !tree_contains(&out, "FROM-B-LITERAL"),
        "the hijack attempt appears nowhere in the output"
    );
}

#[tokio::test]
async fn prefix_mounts_stay_isolated() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a.join("x.txt"), "A-PUBLIC");
    write(&b.join("x.txt"), "B-PUBLIC");
    write(&b.join("private.txt"), "B-PRIVATE");

    let router = Frontend::new()
        .mount_dir("/a", &a)
        .mount_dir("/b", &b)
        .dev();

    // Each mount answers under its own prefix with its own bytes.
    let (status, _, body) = get(router.clone(), "/a/x.txt").await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"A-PUBLIC".as_slice())
    );
    let (status, _, body) = get(router.clone(), "/b/x.txt").await;
    assert_eq!(
        (status, body.as_slice()),
        (StatusCode::OK, b"B-PUBLIC".as_slice())
    );

    // A name that exists only in the other mount is not reachable here, not even
    // through traversal forms.
    for uri in [
        "/a/private.txt",
        "/a/../b/private.txt",
        "/a/%2e%2e/b/private.txt",
    ] {
        let (status, _, body) = get(router.clone(), uri).await;
        assert_ne!(status, StatusCode::OK, "{uri} must not serve");
        assert!(
            !String::from_utf8_lossy(&body).contains("B-PRIVATE"),
            "{uri} must not leak across mounts"
        );
    }
}

#[test]
fn follow_refuses_a_cross_source_link_at_build() {
    // Containment is per root: a link into a sibling root escapes its own root and
    // fails the build, even though the target legitimately ships via that other root.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&b.join("real.txt"), "REAL");
    link(b.join("real.txt"), &a.join("data.txt"));

    let err = build_with(&[a, b], &tmp.path().join("out"), SymlinkMode::Follow, false)
        .unwrap_err()
        .to_string();
    assert!(err.contains("resolve outside the source root"), "got {err}");
}

#[tokio::test]
async fn a_refused_link_falls_through_to_the_next_source_when_serving() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&tmp.path().join("outside/secret.txt"), "OUT-OF-ROOTS");
    link(tmp.path().join("outside/secret.txt"), &a.join("shared.txt"));
    write(&b.join("shared.txt"), "FROM-B");

    // Follow: the first source's escaping link is refused and cannot produce the
    // target, so resolution falls through to the next source — never to the escape.
    let router = Frontend::new()
        .source(&a)
        .source(&b)
        .symlinks(SymlinkMode::Follow)
        .dev();
    let (status, _, body) = get(router, "/shared.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"FROM-B", "the refusal falls through, not the escape");

    // FollowUnsafe: the first source's link now can produce the target, so first
    // source wins and the escape is served — the mode's contract.
    let router = Frontend::new()
        .source(&a)
        .source(&b)
        .symlinks(SymlinkMode::FollowUnsafe)
        .dev();
    let (status, _, body) = get(router, "/shared.txt").await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body, b"OUT-OF-ROOTS");
}

#[test]
fn follow_unsafe_makes_a_cross_source_link_collide_like_a_file() {
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&b.join("real.txt"), "REAL");
    write(&b.join("data.txt"), "B-DATA");
    link(b.join("real.txt"), &a.join("data.txt"));

    // Without skip_duplicates the collision is refused like any other.
    let err = build_with(
        &[a.clone(), b.clone()],
        &tmp.path().join("out1"),
        SymlinkMode::FollowUnsafe,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("claimed by more than one source"), "got {err}");

    // With it, the earlier root's link wins and ships its target's bytes; the losing
    // regular file surfaces nowhere.
    let out = tmp.path().join("out2");
    build_with(&[a, b], &out, SymlinkMode::FollowUnsafe, true).unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("data.txt")).unwrap(),
        "REAL"
    );
    assert!(
        !tree_contains(&out, "B-DATA"),
        "the loser's bytes appear nowhere"
    );
}

#[cfg(feature = "symlink-move")]
#[test]
fn redirect_takes_the_link_out_of_the_claims_so_the_other_source_ships() {
    // Under the redirect modes a link claims nothing, so the same fixture that
    // collides under FollowUnsafe builds cleanly and the other source's regular
    // file fills the name — controlled, not undefined.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&b.join("real.txt"), "REAL");
    write(&b.join("data.txt"), "B-DATA");
    link(b.join("real.txt"), &a.join("data.txt"));

    let out = tmp.path().join("out");
    build_with(&[a, b], &out, SymlinkMode::Redirect, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("data.txt")).unwrap(),
        "B-DATA"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("real.txt")).unwrap(),
        "REAL"
    );
}

#[cfg(feature = "symlink-move")]
#[tokio::test]
async fn redirect_serving_lets_the_first_sources_link_shadow_the_second_sources_file() {
    // A link answers immediately with its content as the Location; the second
    // source's regular file behind the same name is never consulted.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    link("theme.css", &a.join("shared.css"));
    write(&b.join("shared.css"), "FROM-B-CSS");

    let router = Frontend::new()
        .source(&a)
        .source(&b)
        .symlinks(SymlinkMode::Redirect)
        .dev();
    let (status, location, body) = get(router, "/shared.css").await;
    assert_eq!(status, StatusCode::TEMPORARY_REDIRECT);
    assert_eq!(location, "theme.css");
    assert!(body.is_empty(), "a redirect discloses nothing");
}

#[test]
fn file_link_cycles_between_sources_terminate_and_ship_nothing() {
    // Two links pointing at each other across roots resolve to nothing: the walk
    // reports them instead of hanging, the build succeeds, and neither name ships.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a.join("anchor.txt"), "A-ANCHOR");
    link(b.join("back.txt"), &a.join("loop.txt"));
    link(a.join("loop.txt"), &b.join("back.txt"));

    for (mode, out) in [
        (SymlinkMode::Follow, tmp.path().join("out-follow")),
        (SymlinkMode::FollowUnsafe, tmp.path().join("out-unsafe")),
    ] {
        build_with(&[a.clone(), b.clone()], &out, mode, false).unwrap();
        assert!(!out.join("loop.txt").exists(), "{mode:?}");
        assert!(!out.join("back.txt").exists(), "{mode:?}");
        assert_eq!(
            std::fs::read_to_string(out.join("anchor.txt")).unwrap(),
            "A-ANCHOR",
            "the rest of the tree still ships under {mode:?}"
        );
    }
}

#[test]
fn directory_link_cycles_between_sources_terminate() {
    // Each root links a subdirectory at the other root: the walk's loop detection
    // cuts the recursion, one joined level ships per root, and the build completes.
    let tmp = tempfile::tempdir().unwrap();
    let (a, b) = (tmp.path().join("a"), tmp.path().join("b"));
    write(&a.join("x.txt"), "A-FILE");
    write(&b.join("y.txt"), "B-FILE");
    link(&b, &a.join("sub"));
    link(&a, &b.join("sub"));

    // Follow: the linked directories resolve outside their roots — refused.
    let err = build_with(
        &[a.clone(), b.clone()],
        &tmp.path().join("out-follow"),
        SymlinkMode::Follow,
        false,
    )
    .unwrap_err()
    .to_string();
    assert!(err.contains("resolve outside the source root"), "got {err}");

    // FollowUnsafe: the walk terminates on the cycle; each root ships its own file
    // plus the other's, one level deep, and recursion stops there.
    let out = tmp.path().join("out-unsafe");
    build_with(&[a, b], &out, SymlinkMode::FollowUnsafe, false).unwrap();
    assert_eq!(
        std::fs::read_to_string(out.join("x.txt")).unwrap(),
        "A-FILE"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("y.txt")).unwrap(),
        "B-FILE"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("sub/y.txt")).unwrap(),
        "B-FILE",
        "each root ships the other's file one level deep"
    );
    assert_eq!(
        std::fs::read_to_string(out.join("sub/x.txt")).unwrap(),
        "A-FILE"
    );
    assert!(
        !out.join("sub/sub/sub").exists(),
        "the cycle is cut, not recursed"
    );
}
