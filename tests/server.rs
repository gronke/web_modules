//! HTTP behaviors of the `Frontend` routers, bound as specs and driven without binding a
//! port (`tower::ServiceExt::oneshot`). Covers multi-prefix routing, overlay precedence,
//! `index.html` resolution, 404s, path containment, **source-hiding** (both the static
//! and live routers), **live-update** on source change, and **gzip sidecar** serving.
//!
//! Needs the `live` feature (and thus `axum`); on under `--all-features`.
#![cfg(feature = "dev")]

use std::path::Path;
use std::time::{Duration, SystemTime};

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tower::ServiceExt;
use web_modules::include_dir::{include_dir, Dir};
use web_modules::Frontend;

struct Resp {
    status: StatusCode,
    content_type: String,
    content_encoding: String,
    body: Vec<u8>,
}

impl Resp {
    fn text(&self) -> &str {
        std::str::from_utf8(&self.body).unwrap_or("")
    }
}

/// GET `uri` against `router` (cloned, so callers can reuse a shared-state router across
/// requests), optionally sending `Accept-Encoding`.
async fn fetch(router: Router, uri: &str, accept_encoding: Option<&str>) -> Resp {
    let mut builder = Request::builder().uri(uri);
    if let Some(ae) = accept_encoding {
        builder = builder.header(header::ACCEPT_ENCODING, ae);
    }
    let res = router
        .oneshot(builder.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let header = |name: header::HeaderName| {
        res.headers()
            .get(&name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let status = res.status();
    let content_type = header(header::CONTENT_TYPE);
    let content_encoding = header(header::CONTENT_ENCODING);
    let body = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Resp {
        status,
        content_type,
        content_encoding,
        body,
    }
}

fn write(path: &Path, content: impl AsRef<[u8]>) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

/// Write `content`, then stamp `path` with `mtime` so the dev server's mtime cache
/// invalidates deterministically (no reliance on wall-clock resolution).
fn write_at(path: &Path, content: &str, mtime: SystemTime) {
    write(path, content);
    let f = std::fs::File::options().write(true).open(path).unwrap();
    f.set_modified(mtime).unwrap();
}

#[tokio::test]
async fn static_routes_by_most_specific_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("ui/a.json"), b"\"ui\"");
    write(&tmp.path().join("api/a.json"), b"\"api\"");
    let app = Frontend::new()
        .mount_dir("/ui", tmp.path().join("ui"))
        .mount_dir("/api", tmp.path().join("api"))
        .router();

    assert_eq!(
        fetch(app.clone(), "/ui/a.json", None).await.text(),
        "\"ui\""
    );
    assert_eq!(
        fetch(app.clone(), "/api/a.json", None).await.text(),
        "\"api\""
    );
    assert_eq!(
        fetch(app, "/nope/a.json", None).await.status,
        StatusCode::NOT_FOUND
    );
}

#[tokio::test]
async fn overlay_at_one_prefix_resolves_first_root_added() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("first/page.html"), b"first");
    write(&tmp.path().join("second/page.html"), b"second");
    // Two roots at `/`: the first added wins the tie.
    let app = Frontend::dir(tmp.path().join("first"))
        .mount_dir("/", tmp.path().join("second"))
        .router();
    assert_eq!(fetch(app, "/page.html", None).await.text(), "first");
}

#[tokio::test]
async fn directory_request_serves_index_html() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("index.html"), b"<h1>home</h1>");
    let app = Frontend::dir(tmp.path()).router();
    let res = fetch(app, "/", None).await;
    assert_eq!(res.status, StatusCode::OK);
    assert_eq!(res.text(), "<h1>home</h1>");
    assert!(res.content_type.contains("html"));
}

#[tokio::test]
async fn traversal_out_of_a_root_is_refused() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(&root.join("app.js"), b"inside");
    write(&tmp.path().join("secret.txt"), b"TOPSECRET");
    let app = Frontend::dir(&root).router();
    let res = fetch(app, "/../secret.txt", None).await;
    assert_eq!(res.status, StatusCode::NOT_FOUND);
    assert!(!res.text().contains("TOPSECRET"));
}

#[tokio::test]
async fn static_router_hides_source_files() {
    // copy_static/serve-time: a `.scss`/`.ts` source is never served raw, even in the
    // plain static router (it has no compiler — the source simply 404s).
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("app.scss"), "body{color:red}");
    write(&tmp.path().join("app.ts"), "export const x = 1;");
    write(&tmp.path().join("logo.svg"), "<svg/>");
    let app = Frontend::dir(tmp.path()).router();
    assert_eq!(
        fetch(app.clone(), "/app.scss", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fetch(app.clone(), "/app.ts", None).await.status,
        StatusCode::NOT_FOUND
    );
    // A non-source static file is still served.
    assert_eq!(fetch(app, "/logo.svg", None).await.status, StatusCode::OK);
}

#[tokio::test]
async fn live_serves_compiled_targets_and_hides_sources() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("app.scss"), "body { color: red }");
    write(&tmp.path().join("app.ts"), "export const x: number = 1;");
    let app = Frontend::dir(tmp.path()).dev();

    // Sources are hidden; only the compiled targets are reachable.
    assert_eq!(
        fetch(app.clone(), "/app.scss", None).await.status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        fetch(app.clone(), "/app.ts", None).await.status,
        StatusCode::NOT_FOUND
    );

    let css = fetch(app.clone(), "/app.css", None).await;
    assert_eq!(css.status, StatusCode::OK);
    assert!(css.content_type.contains("css"));
    assert!(css.text().contains("red"));

    let js = fetch(app, "/app.js", None).await;
    assert_eq!(js.status, StatusCode::OK);
    assert!(js.content_type.contains("javascript"));
    assert!(js.text().contains("x"));
}

/// The embedded fallback of a production bake, standing in for a `build.rs`-baked dist:
/// it carries the `importmap.json` the build emitted next to its vendored modules.
static EMBEDDED_MAP: Dir = include_dir!("$CARGO_MANIFEST_DIR/tests/embedded_map_fixture");

#[tokio::test]
async fn live_renders_tera_with_the_embedded_import_map() {
    // The `Frontend::embedded(&DIST).source("web")` composition: a live-edited
    // `index.html.tera` must render with the bake's import map, or the page cannot
    // resolve the bare specifiers (`import ... from 'lit'`) the fallback's vendored
    // modules exist to serve.
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("index.html.tera"),
        "<head>{{ importmap | safe }}</head>",
    );
    let app = Frontend::embedded(&EMBEDDED_MAP).source(tmp.path()).dev();

    let page = fetch(app.clone(), "/", None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        page.text().contains("/web_modules/lit/index.js"),
        "the baked map reaches the live render; got:\n{}",
        page.text()
    );

    // Without an embedded fallback the map stays empty — a pure source tree.
    let bare = Frontend::new().source(tmp.path()).dev();
    let page = fetch(bare, "/", None).await;
    assert_eq!(page.status, StatusCode::OK);
    assert!(
        !page.text().contains("lit") && page.text().contains("importmap"),
        "no fallback, empty map; got:\n{}",
        page.text()
    );
}

#[tokio::test]
async fn live_recompiles_after_a_source_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let scss = tmp.path().join("app.scss");
    let ts = tmp.path().join("app.ts");
    let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_000_000);
    let t1 = t0 + Duration::from_secs(60);
    write_at(&scss, "body { color: red }", t0);
    write_at(&ts, "export const v = 1;", t0);

    // A single shared-state router across requests (clones share the mtime cache).
    let app = Frontend::dir(tmp.path()).dev();

    assert!(fetch(app.clone(), "/app.css", None)
        .await
        .text()
        .contains("red"));
    assert!(fetch(app.clone(), "/app.js", None)
        .await
        .text()
        .contains('1'));

    // Edit the sources and bump their mtimes — the same `.css`/`.js` URL now reflects it.
    write_at(&scss, "body { color: blue }", t1);
    write_at(&ts, "export const v = 2;", t1);

    let css = fetch(app.clone(), "/app.css", None).await;
    assert!(
        css.text().contains("blue"),
        "css after edit: {}",
        css.text()
    );
    assert!(!css.text().contains("red"));
    let js = fetch(app, "/app.js", None).await;
    assert!(js.text().contains('2'), "js after edit: {}", js.text());
}

#[tokio::test]
async fn gzip_sidecar_served_only_when_accepted() {
    let tmp = tempfile::tempdir().unwrap();
    // Distinct bytes so we can tell which file was served (the server streams the `.gz`
    // verbatim with Content-Encoding: gzip; it doesn't decode).
    write(&tmp.path().join("app.js"), b"ORIGINAL-JS");
    write(&tmp.path().join("app.js.gz"), b"PRETEND-GZIP-BYTES");
    let app = Frontend::dir(tmp.path()).router();

    // With `Accept-Encoding: gzip` → the sidecar, tagged gzip, original content type.
    let gz = fetch(app.clone(), "/app.js", Some("gzip, deflate")).await;
    assert_eq!(gz.status, StatusCode::OK);
    assert_eq!(gz.body, b"PRETEND-GZIP-BYTES");
    assert_eq!(gz.content_encoding, "gzip");
    assert!(gz.content_type.contains("javascript"));

    // Without it → the original, no Content-Encoding.
    let plain = fetch(app, "/app.js", None).await;
    assert_eq!(plain.body, b"ORIGINAL-JS");
    assert_eq!(plain.content_encoding, "");
}

/// A compile failure answers 500 with a *generic* body: the detail (which can embed
/// absolute local paths — the SCSS sandbox's refusal notes name them) goes to the
/// developer's console, never to whatever client can reach the server.
#[tokio::test]
async fn dev_500_body_does_not_disclose_local_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    write(&root.join("app.scss"), "@import '../secret';\n");
    // A partial outside the source root, so the sandbox's refusal note names a real,
    // absolute outside path in the compile error.
    write(&tmp.path().join("_secret.scss"), "$leak: red;\n");
    let app = Frontend::dir(&root).dev();

    let res = fetch(app, "/app.css", None).await;
    assert_eq!(res.status, StatusCode::INTERNAL_SERVER_ERROR);
    let outside = tmp.path().canonicalize().unwrap();
    assert!(
        !res.text().contains("_secret.scss"),
        "the refusal detail must stay on the console; got: {}",
        res.text()
    );
    assert!(
        !res.text().contains(&outside.display().to_string()),
        "no absolute local path in the body; got: {}",
        res.text()
    );
}
