//! The live-reload surface of the dev server: the SSE stream and its frames, the watcher's
//! mapping from an edited file to a browser change, the dependency index, the injected
//! client, and what the stream never says (filesystem paths).
//!
//! Needs the `dev` feature; on under `--all-features`.
#![cfg(feature = "dev")]

use std::path::Path;
use std::time::Duration;

use axum::body::Body;
use axum::http::{header, Request, StatusCode};
use axum::Router;
use http_body_util::BodyExt;
use tokio::time::timeout;
use tower::ServiceExt;
use web_modules::live::{Change, ChangeKind, LiveReload, ReloadMode, DEFAULT_PREFIX};
use web_modules::{Frontend, Mount};

fn write(path: &Path, content: &str) {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, content).unwrap();
}

async fn get(router: Router, uri: &str) -> axum::response::Response {
    router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

/// The next data frame of an SSE body, as text.
async fn next_frame(body: &mut Body) -> String {
    let frame = timeout(Duration::from_secs(5), body.frame())
        .await
        .expect("a frame within 5s")
        .expect("the stream is open")
        .expect("no body error");
    String::from_utf8(frame.into_data().expect("a data frame").to_vec()).unwrap()
}

/// The next published change, within 5 seconds.
async fn next_change(rx: &mut tokio::sync::mpsc::UnboundedReceiver<Change>) -> Change {
    timeout(Duration::from_secs(5), rx.recv())
        .await
        .expect("a change within 5s")
        .expect("the hub is alive")
}

#[tokio::test]
async fn events_stream_says_hello_then_relays_published_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let live = LiveReload::new(&[Mount::root(tmp.path())]).with_mode(ReloadMode::Css);
    let response = get(live.router(), "/events").await;
    assert_eq!(response.status(), StatusCode::OK);
    let header = |name: header::HeaderName| {
        response
            .headers()
            .get(name)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    assert!(header(header::CONTENT_TYPE).starts_with("text/event-stream"));
    assert_eq!(header(header::CACHE_CONTROL), "no-cache");

    let mut body = response.into_body();
    let hello = next_frame(&mut body).await;
    assert!(hello.contains("event: hello"), "{hello}");
    assert!(
        hello.contains(&format!("\"session\":\"{}\"", live.session())),
        "{hello}"
    );
    assert!(hello.contains("\"reload\":\"css\""), "{hello}");
    assert!(hello.contains("retry: 1000"), "{hello}");

    live.publish(Change::css("/app.css"));
    let change = next_frame(&mut body).await;
    assert!(change.contains("event: change"), "{change}");
    assert!(
        change.contains("data: {\"kind\":\"css\",\"url\":\"/app.css\"}"),
        "{change}"
    );
}

#[tokio::test]
async fn the_watcher_maps_edits_to_browser_changes() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    std::fs::create_dir_all(&root).unwrap();
    write(&root.join("app.scss"), "a { color: red }");
    let live = LiveReload::watch(vec![Mount::root(root.clone())]);
    let mut rx = live.subscribe();
    // Let the watcher settle before the first edit.
    tokio::time::sleep(Duration::from_millis(300)).await;

    write(&root.join("app.scss"), "a { color: blue }");
    assert_eq!(next_change(&mut rx).await, Change::css("/app.css"));

    // Coalescing: nothing else follows from that one save.
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(rx.try_recv().is_err(), "one change per edit");

    // A partial with no known dependents refreshes every stylesheet…
    write(&root.join("_vars.scss"), "$c: red;");
    assert_eq!(next_change(&mut rx).await, Change::css_all());
    tokio::time::sleep(Duration::from_millis(500)).await;

    // …and names its stylesheets once the index knows them.
    live.record_dependencies("/app.css", [root.join("_vars.scss"), root.join("app.scss")]);
    write(&root.join("_vars.scss"), "$c: blue;");
    assert_eq!(next_change(&mut rx).await, Change::css("/app.css"));
    tokio::time::sleep(Duration::from_millis(500)).await;

    // A script reloads the page.
    write(&root.join("lib/util.ts"), "export const v = 1;");
    assert_eq!(
        next_change(&mut rx).await,
        Change::new(ChangeKind::Js, Some("/lib/util.js".into()))
    );
}

#[tokio::test]
async fn the_stream_never_names_a_filesystem_path() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web");
    std::fs::create_dir_all(&root).unwrap();
    let live = LiveReload::new(&[Mount::root(root.clone())]);
    let response = get(live.router(), "/events").await;
    let mut body = response.into_body();
    next_frame(&mut body).await; // hello

    live.notify(&root.join("notes.txt"));
    let frame = next_frame(&mut body).await;
    assert!(
        frame.contains("{\"kind\":\"other\",\"url\":null}"),
        "{frame}"
    );
    let local = tmp.path().canonicalize().unwrap().display().to_string();
    assert!(
        !frame.contains(&local),
        "no local path in the stream: {frame}"
    );
    assert!(!frame.contains("notes"), "not even the file name: {frame}");
}

#[tokio::test]
async fn the_dev_router_injects_and_serves_the_client() {
    let tmp = tempfile::tempdir().unwrap();
    write(
        &tmp.path().join("index.html"),
        "<!doctype html><html><head></head><body><p>hi</p></body></html>",
    );
    write(&tmp.path().join("app.scss"), "a { color: red }");
    let app = Frontend::dir(tmp.path()).dev();

    let page = get(app.clone(), "/").await;
    assert_eq!(page.status(), StatusCode::OK);
    let declared_length = page
        .headers()
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok());
    let html = String::from_utf8(
        page.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    // The original body's length header must not survive the injection: a length, when
    // declared, is the injected body's.
    if let Some(length) = declared_length {
        assert_eq!(
            length,
            html.len(),
            "Content-Length matches the injected body"
        );
    }
    let tag = format!("<script defer src=\"{DEFAULT_PREFIX}/live.js\"></script>");
    assert!(html.contains(&tag), "{html}");
    assert!(
        html.find(&tag).unwrap() < html.find("</body>").unwrap(),
        "injected before </body>: {html}"
    );

    let script = get(app.clone(), &format!("{DEFAULT_PREFIX}/live.js")).await;
    assert_eq!(script.status(), StatusCode::OK);
    assert!(script
        .headers()
        .get(header::CONTENT_TYPE)
        .unwrap()
        .to_str()
        .unwrap()
        .starts_with("text/javascript"));
    assert_eq!(
        script.headers().get(header::CACHE_CONTROL).unwrap(),
        "no-cache"
    );
    let js = String::from_utf8(
        script
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(
        js.contains("EventSource"),
        "the client connects to the stream"
    );

    // Non-HTML responses are left alone.
    let css = get(app.clone(), "/app.css").await;
    let css =
        String::from_utf8(css.into_body().collect().await.unwrap().to_bytes().to_vec()).unwrap();
    assert!(!css.contains("<script"), "{css}");

    let events = get(app, &format!("{DEFAULT_PREFIX}/events")).await;
    assert_eq!(events.status(), StatusCode::OK);
    let mut body = events.into_body();
    assert!(next_frame(&mut body).await.contains("\"reload\":\"full\""));
}

#[tokio::test]
async fn reload_off_serves_neither_client_nor_stream() {
    let tmp = tempfile::tempdir().unwrap();
    write(&tmp.path().join("index.html"), "<body></body>");
    let app = web_modules::dev::dev_router_with_live(
        vec![tmp.path().to_path_buf()],
        web_modules::dev::DevConfig::default(),
        ReloadMode::Off,
    );
    let page = get(app.clone(), "/").await;
    let html = String::from_utf8(
        page.into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes()
            .to_vec(),
    )
    .unwrap();
    assert!(!html.contains("<script"), "{html}");
    assert_eq!(
        get(app, &format!("{DEFAULT_PREFIX}/live.js"))
            .await
            .status(),
        StatusCode::NOT_FOUND
    );
}
