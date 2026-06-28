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
use crate::build::Processors;
#[cfg(feature = "builder")]
use crate::builder_shared::source_builder_methods;
use crate::mount::Mount;

type Cache = Mutex<HashMap<PathBuf, (SystemTime, Vec<u8>)>>;

/// Which processors the dev server applies — **unified with the build pipeline's
/// [`Processors`](crate::build::Processors)**, so `dev` and `build` configure the same set
/// (minify/gzip are build *output* options in [`Output`](crate::build::Output), not here).
/// This is a type alias kept for the historical `dev::DevConfig` name; build one with
/// [`Processors::default`](crate::build::Processors) (all on, Lit decorators) and adjust
/// fields, or use the [`Dev`] builder.
pub type DevConfig = Processors;

/// Fluent builder for the dev server: compile TS/SCSS on the fly, render `*.tera`, watch
/// the source roots and live-reload the browser.
///
/// ```no_run
/// use web_modules::Dev;
///
/// # async fn run() -> std::io::Result<()> {
/// Dev::new().root("web").serve("127.0.0.1:8080".parse().unwrap()).await
/// # }
/// ```
///
/// Shared source inputs (`root`/`roots`, `typescript`/`scss`/`tera`, `decorators`,
/// `scss_load_path(s)`) come from [`source_builder_methods!`](crate::builder_shared); the
/// terminals are [`serve`](Self::serve) and [`router`](Self::router). For prefix-mounted
/// composition (several dirs under different URL prefixes), use [`Frontend`](crate::Frontend).
#[cfg(feature = "builder")]
#[derive(Clone, Debug, Default)]
pub struct Dev {
    roots: Vec<PathBuf>,
    processors: Processors,
}

#[cfg(feature = "builder")]
source_builder_methods!(Dev);

#[cfg(feature = "builder")]
impl Dev {
    /// A new builder with no roots and all processors on (Lit decorators).
    pub fn new() -> Self {
        Self::default()
    }

    /// The dev [`Router`] (compile-on-the-fly, watch, live-reload) over the roots, each
    /// mounted at `/` and resolved first-match-wins. Compose it into your own axum app, or
    /// use [`serve`](Self::serve) to bind and run.
    pub fn router(self) -> Router {
        dev_router_with(self.roots, self.processors)
    }

    /// Bind `addr` and serve [`router`](Self::router) until the process stops.
    pub async fn serve(self, addr: SocketAddr) -> std::io::Result<()> {
        serve_with(self.roots, addr, self.processors).await
    }
}

#[derive(Clone)]
struct DevState {
    mounts: Arc<Vec<Mount>>,
    cache: Arc<Cache>,
    /// Baked assets to fall back to when a request isn't a source file.
    fallback: Option<&'static Dir<'static>>,
    /// Which processors to apply (and how), shared with the bin's `--<name>` toggles.
    config: Arc<DevConfig>,
}

enum Kind {
    Ts,
    Scss,
    #[cfg(feature = "tera")]
    Tera,
}

/// Build the dev [`Router`] over flat source `roots` (each mounted at `/`, resolved in
/// order, first dir wins), with all processors on. For prefix-mounted composition use
/// [`dev_router_mounted`]; to choose which processors run, [`dev_router_with`].
pub fn dev_router(roots: Vec<PathBuf>) -> Router {
    dev_router_with(roots, DevConfig::default())
}

/// Like [`dev_router`], but with an explicit [`DevConfig`] (which processors run, and
/// how) — the toggle-aware entry the `web-modules dev` command uses.
pub fn dev_router_with(roots: Vec<PathBuf>, config: DevConfig) -> Router {
    build_router(roots.into_iter().map(Mount::root).collect(), None, config)
}

/// Like [`dev_router`], but unmatched requests fall back to a baked `include_dir!`
/// tree (vendored modules, `index.html`, …).
pub fn dev_router_with_embedded(roots: Vec<PathBuf>, embedded: &'static Dir<'static>) -> Router {
    build_router(
        roots.into_iter().map(Mount::root).collect(),
        Some(embedded),
        DevConfig::default(),
    )
}

/// Build the dev [`Router`] over prefix-mounted sources: each [`Mount`]'s dir is served
/// (and, when watched, live-reloaded) under its URL prefix, with TS/SCSS compiled on
/// the fly.
pub fn dev_router_mounted(mounts: Vec<Mount>) -> Router {
    build_router(mounts, None, DevConfig::default())
}

/// Like [`dev_router_mounted`], with a baked `include_dir!` fallback.
pub fn dev_router_mounted_with_embedded(
    mounts: Vec<Mount>,
    embedded: &'static Dir<'static>,
) -> Router {
    build_router(mounts, Some(embedded), DevConfig::default())
}

fn build_router(
    mounts: Vec<Mount>,
    fallback: Option<&'static Dir<'static>>,
    config: DevConfig,
) -> Router {
    let livereload = LiveReloadLayer::new();
    spawn_watcher(mounts.clone(), livereload.reloader());
    let state = DevState {
        mounts: Arc::new(mounts),
        cache: Arc::new(Mutex::new(HashMap::new())),
        fallback,
        config: Arc::new(config),
    };
    Router::new()
        .fallback(serve_asset)
        .with_state(state)
        .layer(livereload)
}

/// Bind `addr` and serve [`dev_router`] over `roots` (all processors on) until the
/// process stops.
pub async fn serve(roots: Vec<PathBuf>, addr: SocketAddr) -> std::io::Result<()> {
    serve_with(roots, addr, DevConfig::default()).await
}

/// Like [`serve`], but with an explicit [`DevConfig`] (which processors run, and how).
pub async fn serve_with(
    roots: Vec<PathBuf>,
    addr: SocketAddr,
    config: DevConfig,
) -> std::io::Result<()> {
    let app = dev_router_with(roots, config);
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
/// wins — render a `.tera` (checked first, the final-overlay precedence build uses),
/// else compile a source `.ts`/`.scss`, else serve a static file — then the embedded
/// fallback.
fn resolve(state: &DevState, requested: &str) -> Result<Option<(Vec<u8>, String)>, String> {
    // Reject list: never serve config / secret / source-code paths (see `reject`). Checked on the
    // request string here, and on the resolved file below, so case-folding / a trailing dot can't
    // smuggle a rejected file past.
    if state.config.reject.rejects(requested) {
        crate::reject::warn_rejected(requested);
        return Ok(None);
    }
    for (mount, rel) in matching(state, requested) {
        // `/foo.html` (any rendered target) ← render `foo.html.tera` from this dir, the live
        // counterpart of the build pipeline's tree-wide `.tera`. Checked **first** so a `.tera`
        // takes precedence over a same-named compiled/static target — matching build, where the
        // tera pass overlays everything. The `.tera` source itself stays hidden from raw serving
        // (it's a source extension).
        #[cfg(feature = "tera")]
        if state.config.tera && !rel.is_empty() {
            if let Some(src) = contained_file(mount.dir(), &format!("{rel}.tera")) {
                // Never render a `_`-prefixed partial as a page (matches the build tree).
                let is_partial = src
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.starts_with('_'));
                if !is_partial {
                    let html = compile_cached(state, &src, Kind::Tera)?;
                    return Ok(Some((html, content_type(&rel))));
                }
            }
        }
        // `/foo.js` ← compile `foo.{ts,tsx,mts}` from this dir.
        if state.config.typescript {
            if let Some(stem) = rel.strip_suffix(".js") {
                for ext in ["ts", "tsx", "mts"] {
                    if let Some(src) = contained_file(mount.dir(), &format!("{stem}.{ext}")) {
                        let js = compile_cached(state, &src, Kind::Ts)?;
                        return Ok(Some((js, "text/javascript; charset=utf-8".into())));
                    }
                }
            }
        }
        // `/foo.css` ← compile `foo.scss` from this dir.
        if state.config.scss {
            if let Some(stem) = rel.strip_suffix(".css") {
                if let Some(src) = contained_file(mount.dir(), &format!("{stem}.scss")) {
                    let css = compile_cached(state, &src, Kind::Scss)?;
                    return Ok(Some((css, "text/css; charset=utf-8".into())));
                }
            }
        }
        // Static file in this dir — but never serve a *source* raw (a `.scss`/`.ts`
        // is reachable only through its compiled target, above). Re-check the resolved
        // path, not just the request string: on a case-insensitive / name-folding FS the
        // OS can open a source the request didn't reveal (`app.SCSS`, `app.scss.`).
        if !rel.is_empty() && !is_source_file(&rel) {
            if let Some(path) = contained_file(mount.dir(), &rel) {
                // Re-check the resolved file *name* (the fold-prone part; not the absolute path,
                // whose parent dirs are out of our control) so OS case-folding / a trailing dot
                // can't smuggle a rejected file past the lexical check above.
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if state.config.reject.rejects(name) {
                    crate::reject::warn_rejected(&rel);
                    return Ok(None);
                }
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

/// Compile `src` (TS, SCSS, or Tera), caching by modification time. SCSS `@use`/`@import`
/// load paths span every mounted dir (plus any `extra_scss_load_paths`); Tera renders
/// with an empty `importmap` variable (the dev server doesn't vendor).
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
            let options = crate::typescript::TranspileOptions {
                decorators: state.config.ts_decorators,
                ..Default::default()
            };
            crate::typescript::compile_str_with(&source, src, &options)
                .map_err(|e| e.to_string())?
                .into_bytes()
        }
        Kind::Scss => {
            let mut load_paths: Vec<&Path> = state.mounts.iter().map(|m| m.dir()).collect();
            load_paths.extend(
                state
                    .config
                    .extra_scss_load_paths
                    .iter()
                    .map(PathBuf::as_path),
            );
            crate::scss::compile_file(src, &load_paths)
                .map_err(|e| e.to_string())?
                .into_bytes()
        }
        #[cfg(feature = "tera")]
        Kind::Tera => {
            // dev doesn't vendor, so the import map is empty here (a no-op `<script>`).
            // Live TS/SCSS still load by their relative URLs; a baked fallback may carry
            // a real map, but live source serving doesn't need one.
            let mut ctx = crate::templates::Context::new();
            ctx.insert(
                "importmap",
                &crate::importmap::Importmap::new().to_script_tag(),
            );
            crate::templates::render_file(src, &ctx)
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
        state_with(mounts, DevConfig::default())
    }

    fn state_with(mounts: Vec<Mount>, config: DevConfig) -> DevState {
        DevState {
            mounts: Arc::new(mounts),
            cache: Arc::new(Mutex::new(HashMap::new())),
            fallback: None,
            config: Arc::new(config),
        }
    }

    #[test]
    fn dev_rejects_config_and_dotfiles() {
        // The default (all-presets) reject list 404s config / secret / dotfile paths even though
        // the files exist on disk; legitimate assets still serve.
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), b"<x>").unwrap();
        std::fs::write(root.join("package.json"), b"{}").unwrap();
        std::fs::write(root.join(".env"), b"S=1").unwrap();
        let state = state(vec![Mount::root(root)]);
        assert!(resolve(&state, "index.html").unwrap().is_some());
        assert!(
            resolve(&state, "package.json").unwrap().is_none(),
            "config manifest rejected"
        );
        assert!(
            resolve(&state, ".env").unwrap().is_none(),
            "dotfile rejected"
        );
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

    #[cfg(feature = "tera")]
    #[test]
    fn dev_renders_tera_to_target() {
        // The live counterpart of the build pipeline's tree-wide `.tera`: a request for
        // the stripped target renders the `.tera` (the dev import map is empty).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            root.join("index.html.tera"),
            "<head>{{ importmap | safe }}</head>",
        )
        .unwrap();
        let state = state(vec![Mount::root(root)]);
        let (bytes, ct) = resolve(&state, "index.html").unwrap().unwrap();
        let html = String::from_utf8(bytes).unwrap();
        assert!(
            html.contains("<script type=\"importmap\">"),
            "rendered with the importmap var; got:\n{html}"
        );
        assert!(ct.starts_with("text/html"), "served as html; got {ct}");
    }

    #[cfg(feature = "tera")]
    #[test]
    fn dev_hides_tera_source() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html.tera"), "<p>hi</p>").unwrap();
        let state = state(vec![Mount::root(root)]);
        // The rendered target is reachable; the raw `.tera` source is not.
        assert!(resolve(&state, "index.html").unwrap().is_some());
        assert!(resolve(&state, "index.html.tera").unwrap().is_none());
    }

    #[test]
    fn dev_respects_disabled_processor() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("app.scss"), "a{color:red}").unwrap();
        // SCSS disabled ⇒ no on-the-fly compile, and the `.scss` source stays hidden,
        // so `/app.css` 404s.
        let config = DevConfig {
            scss: false,
            ..DevConfig::default()
        };
        let state = state_with(vec![Mount::root(root)], config);
        assert!(resolve(&state, "app.css").unwrap().is_none());
    }

    #[cfg(feature = "tera")]
    #[test]
    fn dev_tera_wins_over_literal_same_target() {
        // Lock-step with the build pipeline: a `.tera` overlays a same-named literal (dev checks
        // `.tera` first, build renders it as a final overlay).
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("web");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("index.html"), "LITERAL").unwrap();
        std::fs::write(root.join("index.html.tera"), "TERA").unwrap();
        let state = state(vec![Mount::root(root)]);
        let (bytes, _) = resolve(&state, "index.html").unwrap().unwrap();
        assert_eq!(
            String::from_utf8(bytes).unwrap(),
            "TERA",
            "dev renders the .tera over the literal same-target"
        );
    }
}
