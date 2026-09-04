//! Live reload for the dev server: a change hub, fed by a coalescing file watcher or by
//! the host, an SSE endpoint that streams the changes, and a small browser client that
//! hot-swaps stylesheets and reloads the page for everything else. After each swap the
//! client dispatches `web-modules:css-reloaded` on the document, for code that mirrors
//! document stylesheets elsewhere (constructable sheets adopted by shadow roots, say).
//!
//! A [`Change`] names what the browser should do: its [`ChangeKind`] and, for anything
//! served, the URL, never a filesystem path. The stream is reachable by whatever can
//! reach the dev server (see `SECURITY.md`), so it says "`/app.css` changed", not where
//! `app.scss` lives. Stylesheet changes are mapped through a dependency index: an entry
//! compiled by the dev server records the files it read, so an edit to a partial names
//! the stylesheets that include it; an unattributed partial edit becomes a "refresh
//! every stylesheet" change instead. Hosts with their own compilers feed the index with
//! [`LiveReload::record_dependencies`] and their own watchers with [`LiveReload::notify`].
//! The watcher's behavior through symlinks is backend-defined; under
//! [`FollowUnsafe`](crate::SymlinkMode::FollowUnsafe) an edit behind an out-of-tree
//! link may not trigger a reload.
//!
//! The reload policy is the server's ([`ReloadMode`]): `full` reloads the page for
//! non-CSS changes, `css` only hot-swaps and logs the rest, `off` serves no live routes
//! at all. The client learns the mode from the stream's `hello` frame.
//!
//! Enable the `dev` feature.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::convert::Infallible;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::atomic::{AtomicU8, Ordering};
use std::sync::{Arc, Mutex};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use axum::{
    body::Body,
    extract::{Request, State},
    http::{header, StatusCode},
    middleware::Next,
    response::{
        sse::{Event, KeepAlive, Sse},
        IntoResponse, Response,
    },
    routing::get,
    Router,
};
use futures_core::Stream;
use tokio::sync::mpsc;

use crate::mount::Mount;

/// Where the dev server mounts the live routes: `<prefix>/events` (the SSE stream) and
/// `<prefix>/live.js` (the client).
pub const DEFAULT_PREFIX: &str = "/_web_modules/live";

/// The browser client, as served at `<prefix>/live.js`.
const CLIENT_SCRIPT: &str = include_str!("live.js");

/// The watcher batches events that arrive within this window (an editor's
/// write-then-rename, a formatter touching several files) into one set of changes…
const COALESCE_QUIET: Duration = Duration::from_millis(60);
/// …and flushes the batch after this long at the latest, however busy the tree is.
const COALESCE_MAX: Duration = Duration::from_millis(400);

/// What the browser does with a non-stylesheet change. Stylesheets always hot-swap.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
#[cfg_attr(feature = "cli", derive(clap::ValueEnum))]
pub enum ReloadMode {
    /// No live routes, no client, no watcher.
    #[cfg_attr(feature = "cli", value(skip))]
    Off,
    /// Hot-swap stylesheets; other changes are only logged in the browser console.
    Css,
    /// Hot-swap stylesheets and reload the page for every other change (the default).
    #[default]
    Full,
}

impl ReloadMode {
    fn from_u8(value: u8) -> Self {
        match value {
            0 => Self::Off,
            1 => Self::Css,
            _ => Self::Full,
        }
    }

    fn as_u8(self) -> u8 {
        match self {
            Self::Off => 0,
            Self::Css => 1,
            Self::Full => 2,
        }
    }
}

/// What kind of served resource changed, which decides the browser's reaction.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum ChangeKind {
    /// A stylesheet: the client swaps the `<link>` in place.
    Css,
    /// A script: a page reload (modules cannot be hot-replaced).
    Js,
    /// A page: a page reload.
    Html,
    /// Anything else the server serves or renders.
    Other,
}

/// One change as the browser sees it: a kind and, when the resource is served, its URL.
/// Never a filesystem path.
#[derive(Clone, Debug, PartialEq, Eq, Hash, serde::Serialize)]
#[non_exhaustive]
pub struct Change {
    pub kind: ChangeKind,
    pub url: Option<String>,
}

impl Change {
    pub fn new(kind: ChangeKind, url: Option<String>) -> Self {
        Self { kind, url }
    }

    /// The stylesheet at `url` changed: swap that one `<link>`.
    pub fn css(url: impl Into<String>) -> Self {
        Self::new(ChangeKind::Css, Some(url.into()))
    }

    /// Some stylesheet changed, which one is unknown: the client refreshes every
    /// `<link rel="stylesheet">` (the server's mtime cache keeps the unaffected ones cheap).
    pub fn css_all() -> Self {
        Self::new(ChangeKind::Css, None)
    }

    /// Something changed that has no hot path: reload the page.
    pub fn reload() -> Self {
        Self::new(ChangeKind::Other, None)
    }
}

/// Entry URL ↔ the files it was compiled from.
#[derive(Default)]
struct DependencyIndex {
    by_dep: HashMap<PathBuf, BTreeSet<String>>,
    by_url: HashMap<String, Vec<PathBuf>>,
}

impl DependencyIndex {
    fn record(&mut self, url: &str, deps: Vec<PathBuf>) {
        if let Some(previous) = self.by_url.remove(url) {
            for dep in previous {
                if let Some(urls) = self.by_dep.get_mut(&dep) {
                    urls.remove(url);
                    if urls.is_empty() {
                        self.by_dep.remove(&dep);
                    }
                }
            }
        }
        for dep in &deps {
            self.by_dep
                .entry(dep.clone())
                .or_default()
                .insert(url.to_string());
        }
        self.by_url.insert(url.to_string(), deps);
    }

    fn dependents(&self, dep: &Path) -> Vec<String> {
        self.by_dep
            .get(dep)
            .map(|urls| urls.iter().cloned().collect())
            .unwrap_or_default()
    }
}

struct Hub {
    /// Canonical mount dir → URL prefix (always ending in `/`), longest dir first.
    mounts: Vec<(PathBuf, String)>,
    mode: AtomicU8,
    clients: Mutex<Vec<mpsc::UnboundedSender<Change>>>,
    deps: Mutex<DependencyIndex>,
    /// Identifies this server process; a client that reconnects to a different session
    /// knows the server restarted.
    session: String,
}

/// The live-reload hub: publish changes, subscribe to them, serve them as SSE.
/// Cheap to clone (an `Arc`); the clones share one hub.
///
/// A host with its own watcher and compiler wires the hub like this:
///
/// ```no_run
/// # use std::path::{Path, PathBuf};
/// use web_modules::{LiveReload, Mount};
///
/// let live = LiveReload::new(&[Mount::new("/", "web")]);
/// // After each compile, record what the served stylesheet was read from:
/// live.record_dependencies("/app.css", [PathBuf::from("web/app.scss"), PathBuf::from("web/_vars.scss")]);
/// // From your own watcher, publish what an edit means for the browser:
/// live.notify(Path::new("web/_vars.scss"));
/// // Serve the SSE stream wherever you serve pages (`router` adds the client):
/// let events = live.events_router();
/// # let _ = events;
/// ```
#[derive(Clone)]
pub struct LiveReload(Arc<Hub>);

impl std::fmt::Debug for LiveReload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("LiveReload")
            .field("mounts", &self.0.mounts)
            .field("mode", &self.reload_mode())
            .field("session", &self.0.session)
            .finish()
    }
}

impl LiveReload {
    /// A hub that maps paths under `mounts` to URLs but watches nothing: for hosts with
    /// their own watcher ([`notify`](Self::notify)) and for tests.
    pub fn new(mounts: &[Mount]) -> Self {
        let mut dirs: Vec<(PathBuf, String)> = mounts
            .iter()
            .filter_map(|mount| {
                let dir = canonical_dir(mount.dir())?;
                Some((dir, mount.url_prefix().to_string()))
            })
            .collect();
        dirs.sort_by_key(|(dir, _)| std::cmp::Reverse(dir.as_os_str().len()));
        Self(Arc::new(Hub {
            mounts: dirs,
            mode: AtomicU8::new(ReloadMode::default().as_u8()),
            clients: Mutex::new(Vec::new()),
            deps: Mutex::new(DependencyIndex::default()),
            session: session_id(),
        }))
    }

    /// [`new`](Self::new), plus a file watcher over every mount with
    /// [`is_watched`](Mount::is_watched): its events, coalesced over a short window, become
    /// published changes.
    pub fn watch(mounts: Vec<Mount>) -> Self {
        let live = Self::new(&mounts);
        spawn_watcher(live.clone(), mounts);
        live
    }

    /// The policy the client applies to non-stylesheet changes (default [`ReloadMode::Full`]).
    pub fn reload_mode(&self) -> ReloadMode {
        ReloadMode::from_u8(self.0.mode.load(Ordering::Relaxed))
    }

    /// Set the policy; connected clients learn it on their next `hello`.
    pub fn set_mode(&self, mode: ReloadMode) {
        self.0.mode.store(mode.as_u8(), Ordering::Relaxed);
    }

    /// Builder form of [`set_mode`](Self::set_mode).
    pub fn with_mode(self, mode: ReloadMode) -> Self {
        self.set_mode(mode);
        self
    }

    /// The process-unique session id the `hello` frame carries.
    pub fn session(&self) -> &str {
        &self.0.session
    }

    /// Receive every change published from now on.
    pub fn subscribe(&self) -> mpsc::UnboundedReceiver<Change> {
        let (tx, rx) = mpsc::unbounded_channel();
        lock(&self.0.clients).push(tx);
        rx
    }

    /// Deliver `change` to every subscriber and connected client.
    pub fn publish(&self, change: Change) {
        lock(&self.0.clients).retain(|tx| tx.send(change.clone()).is_ok());
    }

    /// A file changed (the host's own watcher noticed): publish what it means for the browser.
    pub fn notify(&self, path: &Path) {
        self.notify_all([path.to_path_buf()]);
    }

    /// Several files changed at once: publish their changes as one deduplicated set.
    pub fn notify_all<I: IntoIterator<Item = PathBuf>>(&self, paths: I) {
        for change in self.changes_for(paths) {
            self.publish(change);
        }
    }

    /// The stylesheet served at `url` was compiled from `deps` (the entry file and every
    /// partial it pulled in), so an edit to any of them names `url`. Replaces the URL's
    /// previous dependencies.
    pub fn record_dependencies<I: IntoIterator<Item = PathBuf>>(&self, url: &str, deps: I) {
        let deps: Vec<PathBuf> = deps
            .into_iter()
            .map(|dep| canonical_path(&dep))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
        lock(&self.0.deps).record(url, deps);
    }

    /// `GET /events` (the SSE stream) and `GET /live.js` (the client), to nest under a prefix.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/events", get(events))
            .route("/live.js", get(client_script))
            .with_state(self.clone())
    }

    /// `GET /events` alone, for a host that serves the client from somewhere else: an
    /// authenticated app that imports it only once signed in, say.
    pub fn events_router(&self) -> Router {
        Router::new()
            .route("/events", get(events))
            .with_state(self.clone())
    }

    /// The changes a set of edited paths means for the browser, deduplicated. Editor
    /// scratch files are ignored; anything under no mount, or of a kind that has no served
    /// URL, becomes a bare [`Change::reload`] so no path leaks into the stream. A directory
    /// stands for the files inside it: a watcher learns of a new directory before it watches
    /// it, so files created in the same instant (a checkout, an unpacked folder) would
    /// otherwise go unnoticed.
    fn changes_for<I: IntoIterator<Item = PathBuf>>(&self, paths: I) -> Vec<Change> {
        let mut seen = HashSet::new();
        let mut changes = Vec::new();
        let mut push = |change: Change, changes: &mut Vec<Change>| {
            if seen.insert(change.clone()) {
                changes.push(change);
            }
        };
        let paths: Vec<PathBuf> = paths
            .into_iter()
            .flat_map(|path| {
                if path.is_dir() {
                    walkdir::WalkDir::new(&path)
                        .into_iter()
                        .filter_map(|entry| entry.ok())
                        .filter(|entry| entry.file_type().is_file())
                        .map(|entry| entry.into_path())
                        .collect()
                } else {
                    vec![path]
                }
            })
            .collect();
        for path in paths {
            let name = match path.file_name().and_then(|n| n.to_str()) {
                Some(name) => name.to_string(),
                None => continue,
            };
            if is_editor_scratch(&name) {
                continue;
            }
            let canonical = canonical_path(&path);
            let dependents = lock(&self.0.deps).dependents(&canonical);
            let Some(rel) = self.url_for(&canonical) else {
                for url in dependents {
                    push(Change::css(url), &mut changes);
                }
                push(Change::reload(), &mut changes);
                continue;
            };
            let ext = extension(&name);
            match ext.as_str() {
                "scss" => {
                    if name.starts_with('_') {
                        if dependents.is_empty() {
                            push(Change::css_all(), &mut changes);
                        }
                    } else {
                        push(Change::css(swap_extension(&rel, "css")), &mut changes);
                    }
                    for url in dependents {
                        push(Change::css(url), &mut changes);
                    }
                }
                "ts" | "tsx" | "mts" => {
                    if !name.ends_with(".d.ts") {
                        push(
                            Change::new(ChangeKind::Js, Some(swap_extension(&rel, "js"))),
                            &mut changes,
                        );
                    }
                }
                "tera" => {
                    let target = rel.trim_end_matches(".tera").to_string();
                    let kind = match extension(&target).as_str() {
                        "html" | "htm" => ChangeKind::Html,
                        "js" | "mjs" => ChangeKind::Js,
                        "css" => ChangeKind::Css,
                        _ => ChangeKind::Other,
                    };
                    let url = (kind != ChangeKind::Other).then_some(target);
                    push(Change::new(kind, url), &mut changes);
                }
                "css" => push(Change::css(rel), &mut changes),
                "js" | "mjs" => push(Change::new(ChangeKind::Js, Some(rel)), &mut changes),
                "html" | "htm" => push(Change::new(ChangeKind::Html, Some(rel)), &mut changes),
                _ => {
                    for url in dependents {
                        push(Change::css(url), &mut changes);
                    }
                    push(Change::reload(), &mut changes);
                }
            }
        }
        changes
    }

    /// The URL a canonical path is served at, through the most specific mount containing it.
    fn url_for(&self, canonical: &Path) -> Option<String> {
        let (dir, prefix) = self
            .0
            .mounts
            .iter()
            .find(|(dir, _)| canonical.starts_with(dir))?;
        let rel = canonical.strip_prefix(dir).ok()?;
        let rel = rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/");
        Some(format!("{prefix}{rel}"))
    }
}

/// `<script defer src="{prefix}/live.js"></script>`, for hosts rendering their own page.
pub fn script_tag(prefix: &str) -> String {
    format!(
        "<script defer src=\"{}/live.js\"></script>",
        prefix.trim_end_matches('/')
    )
}

/// `<meta name="web-modules-live" content="{prefix}">`: tells a client loaded via
/// `import()` where the endpoint lives.
pub fn meta_tag(prefix: &str) -> String {
    format!(
        "<meta name=\"web-modules-live\" content=\"{}\">",
        prefix.trim_end_matches('/')
    )
}

/// The client alone, stateless: `GET /live.js`.
pub fn script_router() -> Router {
    Router::new().route("/live.js", get(client_script))
}

/// Inject [`script_tag`] before `</body>` of every `text/html` response the router
/// produces (uncompressed ones; a `Content-Encoding` is left alone).
pub fn inject_script(router: Router, prefix: &str) -> Router {
    let tag: Arc<str> = Arc::from(script_tag(prefix));
    router.layer(axum::middleware::from_fn_with_state(tag, inject_tag))
}

async fn inject_tag(State(tag): State<Arc<str>>, request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let is_html = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|ct| ct.starts_with("text/html"));
    if !is_html || response.headers().contains_key(header::CONTENT_ENCODING) {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match axum::body::to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(_) => return StatusCode::INTERNAL_SERVER_ERROR.into_response(),
    };
    parts.headers.remove(header::CONTENT_LENGTH);
    let mut html = match String::from_utf8(bytes.to_vec()) {
        Ok(html) => html,
        // Not text after all: pass the bytes through untouched.
        Err(e) => return Response::from_parts(parts, Body::from(e.into_bytes())),
    };
    match html.to_ascii_lowercase().rfind("</body>") {
        Some(at) => html.insert_str(at, &tag),
        None => html.push_str(&tag),
    }
    Response::from_parts(parts, Body::from(html))
}

async fn client_script() -> impl IntoResponse {
    (
        [
            (header::CONTENT_TYPE, "text/javascript; charset=utf-8"),
            (header::CACHE_CONTROL, "no-cache"),
        ],
        CLIENT_SCRIPT,
    )
}

async fn events(State(live): State<LiveReload>) -> impl IntoResponse {
    let hello = serde_json::json!({
        "session": live.session(),
        "reload": live.reload_mode(),
    });
    let stream = ChangeStream {
        hello: Some(
            Event::default()
                .event("hello")
                .data(hello.to_string())
                .retry(Duration::from_secs(1)),
        ),
        rx: live.subscribe(),
    };
    Sse::new(stream).keep_alive(KeepAlive::default())
}

/// The `hello` frame, then one `change` frame per published [`Change`].
struct ChangeStream {
    hello: Option<Event>,
    rx: mpsc::UnboundedReceiver<Change>,
}

impl Stream for ChangeStream {
    type Item = Result<Event, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(hello) = self.hello.take() {
            return Poll::Ready(Some(Ok(hello)));
        }
        match self.rx.poll_recv(cx) {
            Poll::Ready(Some(change)) => {
                let data = serde_json::to_string(&change).unwrap_or_else(|_| "{}".into());
                Poll::Ready(Some(Ok(Event::default().event("change").data(data))))
            }
            Poll::Ready(None) => Poll::Ready(None),
            Poll::Pending => Poll::Pending,
        }
    }
}

/// Watch the mounts on a thread of their own; every coalesced batch of filesystem events
/// becomes a set of published changes.
fn spawn_watcher(live: LiveReload, mounts: Vec<Mount>) {
    std::thread::spawn(move || {
        use notify::{RecursiveMode, Watcher};
        let (tx, rx) = std::sync::mpsc::channel::<notify::Event>();
        let mut watcher =
            match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
                if let Ok(event) = res {
                    let _ = tx.send(event);
                }
            }) {
                Ok(watcher) => watcher,
                Err(e) => {
                    eprintln!("web-modules: file watcher unavailable ({e}); live reload off");
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
        while let Ok(first) = rx.recv() {
            let started = Instant::now();
            let mut batch = BTreeSet::new();
            collect_paths(&first, &mut batch);
            loop {
                let elapsed = started.elapsed();
                if elapsed >= COALESCE_MAX {
                    break;
                }
                match rx.recv_timeout(COALESCE_QUIET.min(COALESCE_MAX - elapsed)) {
                    Ok(event) => collect_paths(&event, &mut batch),
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => break,
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
                }
            }
            live.notify_all(batch);
        }
    });
}

/// The paths of an event worth reacting to: creations, removals, renames and content or
/// write-time changes (`touch`, `File::set_modified`). Access and other metadata events are noise.
fn collect_paths(event: &notify::Event, batch: &mut BTreeSet<PathBuf>) {
    use notify::event::{MetadataKind, ModifyKind};
    use notify::EventKind;
    let relevant = match &event.kind {
        EventKind::Create(_) | EventKind::Remove(_) | EventKind::Any => true,
        EventKind::Modify(kind) => matches!(
            kind,
            ModifyKind::Data(_)
                | ModifyKind::Any
                | ModifyKind::Name(_)
                | ModifyKind::Metadata(MetadataKind::WriteTime | MetadataKind::Any)
        ),
        EventKind::Access(_) | EventKind::Other => false,
    };
    if relevant {
        batch.extend(event.paths.iter().cloned());
    }
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|e| e.into_inner())
}

fn canonical_dir(dir: &Path) -> Option<PathBuf> {
    dir.canonicalize().ok()
}

/// The canonical form of a path that may no longer exist (a removed file): its parent's
/// canonical form plus the file name.
fn canonical_path(path: &Path) -> PathBuf {
    if let Ok(real) = path.canonicalize() {
        return real;
    }
    match (path.parent(), path.file_name()) {
        (Some(parent), Some(name)) => parent
            .canonicalize()
            .map(|p| p.join(name))
            .unwrap_or_else(|_| path.to_path_buf()),
        _ => path.to_path_buf(),
    }
}

fn extension(name: &str) -> String {
    Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| e.to_ascii_lowercase())
        .unwrap_or_default()
}

fn swap_extension(url: &str, ext: &str) -> String {
    match url.rfind('.') {
        Some(dot) if !url[dot..].contains('/') => format!("{}.{ext}", &url[..dot]),
        _ => format!("{url}.{ext}"),
    }
}

/// Editors and tools leave transient files next to the sources: vim's swap files and its
/// `4913` write probe, emacs' `#name#` autosaves, `name~` backups, partial downloads.
fn is_editor_scratch(name: &str) -> bool {
    name.starts_with('.')
        || name.ends_with('~')
        || (name.starts_with('#') && name.ends_with('#'))
        || (!name.is_empty() && name.bytes().all(|b| b.is_ascii_digit()))
        || matches!(
            extension(name).as_str(),
            "swp" | "swx" | "swo" | "tmp" | "part" | "orig" | "bak"
        )
}

fn session_id() -> String {
    let mut hasher = std::collections::hash_map::RandomState::new().build_hasher();
    hasher.write_u128(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0),
    );
    format!("{:016x}", hasher.finish())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tree() -> (tempfile::TempDir, LiveReload) {
        let tmp = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(tmp.path().join("web")).unwrap();
        std::fs::create_dir_all(tmp.path().join("modules/x")).unwrap();
        let live = LiveReload::new(&[
            Mount::root(tmp.path().join("web")),
            Mount::new("modules/x", tmp.path().join("modules/x")),
        ]);
        (tmp, live)
    }

    fn one(live: &LiveReload, path: PathBuf) -> Vec<Change> {
        live.changes_for([path])
    }

    #[test]
    fn stylesheets_and_scripts_map_to_their_served_urls() {
        let (tmp, live) = tree();
        assert_eq!(
            one(&live, tmp.path().join("web/app.scss")),
            vec![Change::css("/app.css")]
        );
        assert_eq!(
            one(&live, tmp.path().join("web/lib/util.ts")),
            vec![Change::new(ChangeKind::Js, Some("/lib/util.js".into()))]
        );
        assert_eq!(
            one(&live, tmp.path().join("modules/x/list.tsx")),
            vec![Change::new(
                ChangeKind::Js,
                Some("/modules/x/list.js".into())
            )]
        );
        assert_eq!(
            one(&live, tmp.path().join("web/vendor.css")),
            vec![Change::css("/vendor.css")]
        );
    }

    #[test]
    fn a_partial_names_its_dependents_or_every_stylesheet() {
        let (tmp, live) = tree();
        let partial = tmp.path().join("web/_vars.scss");
        assert_eq!(one(&live, partial.clone()), vec![Change::css_all()]);
        live.record_dependencies(
            "/app.css",
            [partial.clone(), tmp.path().join("web/app.scss")],
        );
        live.record_dependencies("/modules/x/style.css", [partial.clone()]);
        assert_eq!(
            one(&live, partial),
            vec![Change::css("/app.css"), Change::css("/modules/x/style.css")]
        );
    }

    #[test]
    fn templates_take_the_kind_of_their_target() {
        let (tmp, live) = tree();
        assert_eq!(
            one(&live, tmp.path().join("web/index.html.tera")),
            vec![Change::new(ChangeKind::Html, Some("/index.html".into()))]
        );
        assert_eq!(
            one(&live, tmp.path().join("web/config.js.tera")),
            vec![Change::new(ChangeKind::Js, Some("/config.js".into()))]
        );
        assert_eq!(
            one(&live, tmp.path().join("web/robots.txt.tera")),
            vec![Change::reload()]
        );
    }

    #[test]
    fn declarations_and_editor_scratch_files_are_ignored() {
        let (tmp, live) = tree();
        assert!(one(&live, tmp.path().join("web/types.d.ts")).is_empty());
        for name in [
            ".app.scss.swp",
            "4913",
            "#app.ts#",
            "app.ts~",
            "app.scss.tmp",
        ] {
            assert!(
                one(&live, tmp.path().join("web").join(name)).is_empty(),
                "{name} is scratch"
            );
        }
        // An empty directory carries no change of its own…
        assert!(one(&live, tmp.path().join("web")).is_empty());
    }

    #[test]
    fn a_directory_stands_for_the_files_inside_it() {
        // …a populated one means its files changed (a new folder's files arrive before the
        // watcher covers the folder).
        let (tmp, live) = tree();
        std::fs::create_dir_all(tmp.path().join("web/lib")).unwrap();
        std::fs::write(tmp.path().join("web/lib/a.ts"), "").unwrap();
        std::fs::write(tmp.path().join("web/lib/b.scss"), "").unwrap();
        let changes = one(&live, tmp.path().join("web/lib"));
        assert!(
            changes.contains(&Change::new(ChangeKind::Js, Some("/lib/a.js".into()))),
            "{changes:?}"
        );
        assert!(changes.contains(&Change::css("/lib/b.css")), "{changes:?}");
        assert_eq!(changes.len(), 2);
    }

    #[test]
    fn unknown_files_and_paths_outside_every_mount_reload_without_a_name() {
        let (tmp, live) = tree();
        assert_eq!(
            one(&live, tmp.path().join("web/notes.md")),
            vec![Change::reload()]
        );
        assert_eq!(
            one(&live, tmp.path().join("elsewhere.scss")),
            vec![Change::reload()]
        );
    }

    #[test]
    fn a_batch_is_deduplicated() {
        let (tmp, live) = tree();
        let changes = live.changes_for([
            tmp.path().join("web/app.scss"),
            tmp.path().join("web/app.scss"),
            tmp.path().join("web/a.ts"),
            tmp.path().join("web/b.ts"),
            tmp.path().join("web/notes.md"),
            tmp.path().join("web/other.md"),
        ]);
        assert_eq!(
            changes,
            vec![
                Change::css("/app.css"),
                Change::new(ChangeKind::Js, Some("/a.js".into())),
                Change::new(ChangeKind::Js, Some("/b.js".into())),
                Change::reload(),
            ]
        );
    }

    #[test]
    fn tags_and_mode_serialise_as_the_client_expects() {
        assert_eq!(
            script_tag("/_web_modules/live/"),
            "<script defer src=\"/_web_modules/live/live.js\"></script>"
        );
        assert_eq!(
            meta_tag("/-/dev/live"),
            "<meta name=\"web-modules-live\" content=\"/-/dev/live\">"
        );
        assert_eq!(serde_json::to_string(&ReloadMode::Css).unwrap(), "\"css\"");
        assert_eq!(
            serde_json::to_string(&Change::css("/a.css")).unwrap(),
            "{\"kind\":\"css\",\"url\":\"/a.css\"}"
        );
        assert_eq!(
            serde_json::to_string(&Change::reload()).unwrap(),
            "{\"kind\":\"other\",\"url\":null}"
        );
    }
}
