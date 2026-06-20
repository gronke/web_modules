//! A buildless **dev server**: serve a frontend straight from source, compiling
//! TypeScript and SCSS on the fly per request (mtime-cached) and live-reloading the
//! browser when files change.
//!
//! Sources are [`Mount`]s, each a URL prefix + a source dir; the default is one at
//! `/`. Resolution is **dir-observation-order-dominant**: most-specific prefix first,
//! then (among the dirs matching that prefix, **in the order given**) the first that
//! can produce the requested *target* wins (compile `foo.{ts,tsx,mts}` → `foo.js`,
//! `foo.scss` → `foo.css`, else serve a static file). So overlaying several dirs at one
//! prefix resolves "first dir wins", as a side-effect. An optional embedded fallback (a
//! baked `include_dir!` tree) supplies whatever the source dirs don't: vendored
//! `web_modules/`, a baked `index.html`. The watcher watches every source dir
//! identically and reloads on any change.
//!
//! Enable the `dev` feature.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use axum::{
    body::Body,
    extract::State,
    http::{header, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use include_dir::Dir;
use tower_livereload::LiveReloadLayer;

use super::serving::{
    contained_file, content_type, has_source_extension, has_traversal, is_source_file,
    relative_under,
};
use crate::mount::Mount;

type Cache = Mutex<HashMap<PathBuf, (SystemTime, Vec<u8>)>>;

#[derive(Clone)]
struct DevState {
    mounts: Arc<Vec<Mount>>,
    cache: Arc<Cache>,
    /// Baked assets to fall back to when a request isn't a source file.
    fallback: Option<&'static Dir<'static>>,
}

enum Kind {
    Ts,
    Scss,
}

/// Build the dev [`Router`] over flat source `roots` (each mounted at `/`, resolved in
/// order, first dir wins). For prefix-mounted composition use [`dev_router_mounted`].
pub fn dev_router(roots: Vec<PathBuf>) -> Router {
    build_router(roots.into_iter().map(Mount::root).collect(), None)
}

/// Like [`dev_router`], but unmatched requests fall back to a baked `include_dir!`
/// tree (vendored modules, `index.html`, …).
pub fn dev_router_with_embedded(roots: Vec<PathBuf>, embedded: &'static Dir<'static>) -> Router {
    build_router(roots.into_iter().map(Mount::root).collect(), Some(embedded))
}

/// Build the dev [`Router`] over prefix-mounted sources: each [`Mount`]'s dir is served
/// (and, when watched, live-reloaded) under its URL prefix, with TS/SCSS compiled on
/// the fly.
pub fn dev_router_mounted(mounts: Vec<Mount>) -> Router {
    build_router(mounts, None)
}

/// Like [`dev_router_mounted`], with a baked `include_dir!` fallback.
pub fn dev_router_mounted_with_embedded(
    mounts: Vec<Mount>,
    embedded: &'static Dir<'static>,
) -> Router {
    build_router(mounts, Some(embedded))
}

fn build_router(mounts: Vec<Mount>, fallback: Option<&'static Dir<'static>>) -> Router {
    let livereload = LiveReloadLayer::new();
    spawn_watcher(mounts.clone(), livereload.reloader());
    let state = DevState {
        mounts: Arc::new(mounts),
        cache: Arc::new(Mutex::new(HashMap::new())),
        fallback,
    };
    Router::new()
        .fallback(serve_asset)
        .with_state(state)
        .layer(livereload)
}

/// Bind `addr` and serve [`dev_router`] over `roots` until the process stops.
pub async fn serve(roots: Vec<PathBuf>, addr: SocketAddr) -> std::io::Result<()> {
    let app = dev_router(roots);
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("web-modules dev server on http://{addr}/  (Ctrl-C to stop)");
    axum::serve(listener, app).await
}

async fn serve_asset(State(state): State<DevState>, uri: Uri) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let requested = if raw.is_empty() || raw.ends_with('/') {
        format!("{raw}index.html")
    } else {
        raw.to_string()
    };
    // Layer 1: reject a traversing request path before it reaches the filesystem.
    if has_traversal(&requested) {
        return StatusCode::NOT_FOUND.into_response();
    }
    match resolve(&state, &requested) {
        Ok(Some((bytes, content_type))) => Response::builder()
            .header(header::CONTENT_TYPE, content_type)
            .body(Body::from(bytes))
            .expect("valid response"),
        Ok(None) => (StatusCode::NOT_FOUND, format!("404 Not Found: {requested}")).into_response(),
        Err(message) => {
            eprintln!("web-modules: compile error for /{requested}:\n{message}");
            (StatusCode::INTERNAL_SERVER_ERROR, message).into_response()
        }
    }
}

/// The mounts whose URL prefix matches `requested`, most-specific (longest prefix)
/// first, each paired with the request path relative to that mount. Equal-specificity
/// mounts keep declaration (observation) order (stable sort).
fn matching<'a>(state: &'a DevState, requested: &str) -> Vec<(&'a Mount, String)> {
    let mut hits: Vec<(&Mount, String)> = state
        .mounts
        .iter()
        .filter_map(|m| relative_under(m.url_prefix(), requested).map(|rel| (m, rel)))
        .collect();
    hits.sort_by_key(|hit| std::cmp::Reverse(hit.0.url_prefix().len()));
    hits
}

/// Resolve a request to `(bytes, content-type)`, **dir-observation-order-dominant**:
/// for each matching mount in order, the first that can produce the requested target
/// wins (compile a source `.ts`/`.scss`, else serve a static file), then the embedded
/// fallback.
fn resolve(state: &DevState, requested: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    for (mount, rel) in matching(state, requested) {
        // `/foo.js` ← compile `foo.{ts,tsx,mts}` from this dir.
        if let Some(stem) = rel.strip_suffix(".js") {
            for ext in ["ts", "tsx", "mts"] {
                if let Some(src) = contained_file(mount.dir(), &format!("{stem}.{ext}")) {
                    let js = compile_cached(state, &src, Kind::Ts)?;
                    return Ok(Some((js, "text/javascript; charset=utf-8".into())));
                }
            }
        }
        // `/foo.css` ← compile `foo.scss` from this dir.
        if let Some(stem) = rel.strip_suffix(".css") {
            if let Some(src) = contained_file(mount.dir(), &format!("{stem}.scss")) {
                let css = compile_cached(state, &src, Kind::Scss)?;
                return Ok(Some((css, "text/css; charset=utf-8".into())));
            }
        }
        // Static file in this dir — but never serve a *source* raw (a `.scss`/`.ts`
        // is reachable only through its compiled target, above). Re-check the resolved
        // path, not just the request string: on a case-insensitive / name-folding FS the
        // OS can open a source the request didn't reveal (`app.SCSS`, `app.scss.`).
        if !rel.is_empty() && !is_source_file(&rel) {
            if let Some(path) = contained_file(mount.dir(), &rel) {
                if !has_source_extension(&path) {
                    let bytes = std::fs::read(&path).map_err(|e| e.to_string())?;
                    return Ok(Some((bytes, content_type(&rel))));
                }
            }
        }
    }
    // Baked fallback (vendored modules, index.html, …), keyed by the full path.
    if let Some(dir) = state.fallback {
        if !is_source_file(requested) {
            if let Some(file) = dir.get_file(requested) {
                return Ok(Some((file.contents().to_vec(), content_type(requested))));
            }
        }
    }
    Ok(None)
}

/// Compile `src` (TS or SCSS), caching by modification time. SCSS `@use`/`@import`
/// load paths span every mounted dir.
fn compile_cached(state: &DevState, src: &Path, kind: Kind) -> Result<Vec<u8>, String> {
    let mtime = std::fs::metadata(src)
        .and_then(|m| m.modified())
        .map_err(|e| e.to_string())?;
    if let Some((cached_mtime, bytes)) = state
        .cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(src)
    {
        if *cached_mtime == mtime {
            return Ok(bytes.clone());
        }
    }
    let out = match kind {
        Kind::Ts => {
            let source = std::fs::read_to_string(src).map_err(|e| e.to_string())?;
            crate::typescript::compile_str(&source, src)
                .map_err(|e| e.to_string())?
                .into_bytes()
        }
        Kind::Scss => {
            let load_paths: Vec<&Path> = state.mounts.iter().map(|m| m.dir()).collect();
            crate::scss::compile_file(src, &load_paths)
                .map_err(|e| e.to_string())?
                .into_bytes()
        }
    };
    state
        .cache
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(src.to_path_buf(), (mtime, out.clone()));
    Ok(out)
}

/// Watch each watched mount's dir and trigger a browser reload on any change.
fn spawn_watcher(mounts: Vec<Mount>, reloader: tower_livereload::Reloader) {
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    if event.kind.is_modify() || event.kind.is_create() || event.kind.is_remove() {
                        reloader.reload();
                    }
                }
            }) {
                Ok(w) => w,
                Err(e) => {
                    eprintln!("web-modules: file watcher unavailable ({e}); live-reload off");
                    return;
                }
            };
        for mount in &mounts {
            if mount.is_watched() {
                if let Err(e) = watcher.watch(mount.dir(), RecursiveMode::Recursive) {
                    eprintln!("web-modules: cannot watch {}: {e}", mount.dir().display());
                }
            }
        }
        loop {
            std::thread::park();
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(mounts: Vec<Mount>) -> DevState {
        DevState {
            mounts: Arc::new(mounts),
            cache: Arc::new(Mutex::new(HashMap::new())),
            fallback: None,
        }
    }

    #[test]
    fn resolve_serves_inside_and_blocks_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("data.json"), b"{}").unwrap();
        std::fs::write(tmp.path().join("secret"), b"s").unwrap();
        let state = state(vec![Mount::root(root)]);
        assert!(resolve(&state, "data.json").unwrap().is_some());
        // Even bypassing Layer 1, resolve's own containment blocks the escape.
        assert!(resolve(&state, "../secret").unwrap().is_none());
    }

    #[test]
    fn resolve_routes_by_prefix_mount() {
        let tmp = tempfile::tempdir().unwrap();
        let ui = tmp.path().join("ui");
        let api = tmp.path().join("api");
        std::fs::create_dir_all(&ui).unwrap();
        std::fs::create_dir_all(&api).unwrap();
        std::fs::write(ui.join("a.json"), b"\"ui\"").unwrap();
        std::fs::write(api.join("a.json"), b"\"api\"").unwrap();
        let state = state(vec![Mount::new("ui", ui), Mount::new("api", api)]);
        assert_eq!(&resolve(&state, "ui/a.json").unwrap().unwrap().0, b"\"ui\"");
        assert_eq!(
            &resolve(&state, "api/a.json").unwrap().unwrap().0,
            b"\"api\""
        );
        assert!(resolve(&state, "nope/a.json").unwrap().is_none());
    }

    #[test]
    fn overlay_same_prefix_first_dir_wins() {
        // Two dirs at the same (root) prefix offering the same target → first wins.
        let tmp = tempfile::tempdir().unwrap();
        let first = tmp.path().join("first");
        let second = tmp.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("page.html"), b"first").unwrap();
        std::fs::write(second.join("page.html"), b"second").unwrap();
        let state = state(vec![Mount::root(first), Mount::root(second)]);
        assert_eq!(&resolve(&state, "page.html").unwrap().unwrap().0, b"first");
    }

    #[test]
    fn sources_are_not_served_raw() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("app.scss"), "a{color:red}").unwrap();
        std::fs::write(root.join("app.ts"), "export const x = 1;").unwrap();
        let state = state(vec![Mount::root(root)]);
        // Originals are hidden; only the compiled targets are reachable.
        assert!(resolve(&state, "app.scss").unwrap().is_none());
        assert!(resolve(&state, "app.ts").unwrap().is_none());
        assert!(resolve(&state, "app.css").unwrap().is_some()); // compiled from app.scss
        assert!(resolve(&state, "app.js").unwrap().is_some()); // compiled from app.ts
    }
}
