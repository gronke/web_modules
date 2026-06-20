//! Axum integration: serve a frontend from a set of **known roots**.
//!
//! A [`Frontend`] is a list of roots (each an embedded `include_dir!` tree or a
//! filesystem directory) mounted under a URL prefix. The default is **one root at
//! `/`**; you can add several, each under its own prefix.
//!
//! - [`Frontend::router`] serves the roots **statically**, as-is (no compiler, no
//!   watcher).
//! - [`Frontend::dev`] *(feature `dev`)* compiles TS/SCSS on the fly and watches the
//!   filesystem roots, with embedded roots as the static fallback.
//! - [`Frontend::auto`] picks `dev` in debug builds, `router` in release.
//!
//! A request can never resolve to a file outside a known root (the containment in
//! `super::serving`), the same boundary the planned processor sandbox will use.
//!
//! ```ignore
//! use web_modules::{include_dir::{include_dir, Dir}, serve, Frontend};
//! static DIST: Dir = include_dir!("$OUT_DIR/dist"); // baked by build.rs via `build`
//!
//! # async fn run() -> std::io::Result<()> {
//! // debug → live-reload from `web/`; release → embedded `DIST`.
//! let app = Frontend::embedded(&DIST).source("web").auto();
//! serve(app, "127.0.0.1:8080".parse().unwrap()).await
//! # }
//! ```

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode, Uri},
    response::{IntoResponse, Response},
    Router,
};
use include_dir::Dir;

use super::serving::{
    contained_file, content_type, has_source_extension, has_traversal, is_source_file,
    relative_under,
};

/// The backing of a served root: assets baked into the binary, or a directory read
/// from the filesystem at runtime.
enum Source {
    Embedded(&'static Dir<'static>),
    Dir(PathBuf),
}

/// One served root: a URL `prefix` (normalised, no surrounding `/`; `""` = site root),
/// a [`Source`], and whether the live server watches it.
struct Root {
    prefix: String,
    source: Source,
    /// Whether `live()` watches this root. Read only by the dev server, so it's dead in
    /// a lean static build (`axum` without `dev`).
    #[cfg_attr(not(feature = "dev"), allow(dead_code))]
    watch: bool,
}

impl Root {
    fn new(prefix: &str, source: Source) -> Self {
        Self {
            prefix: prefix.trim_matches('/').to_string(),
            source,
            watch: true,
        }
    }
}

/// Serve a frontend from one or more known roots. Default: a single root at `/`.
#[derive(Default)]
pub struct Frontend {
    roots: Vec<Root>,
}

impl Frontend {
    /// No roots yet; add them with [`mount_embedded`](Self::mount_embedded) /
    /// [`mount_dir`](Self::mount_dir).
    pub fn new() -> Self {
        Self::default()
    }

    /// One **embedded** root at `/` (baked `include_dir!` assets, production).
    pub fn embedded(dir: &'static Dir<'static>) -> Self {
        Self {
            roots: vec![Root::new("", Source::Embedded(dir))],
        }
    }

    /// One **filesystem** root at `/`, served as-is at runtime.
    pub fn dir(path: impl Into<PathBuf>) -> Self {
        Self {
            roots: vec![Root::new("", Source::Dir(path.into()))],
        }
    }

    /// Add a filesystem source root at `/` (your `.ts`/`.scss` dir for live mode), a
    /// convenience for `mount_dir("/", path)`. Repeatable.
    pub fn source(self, path: impl Into<PathBuf>) -> Self {
        self.mount_dir("/", path)
    }

    /// Mount an embedded tree under `prefix`. Repeatable.
    pub fn mount_embedded(mut self, prefix: impl AsRef<str>, dir: &'static Dir<'static>) -> Self {
        self.roots
            .push(Root::new(prefix.as_ref(), Source::Embedded(dir)));
        self
    }

    /// Mount a filesystem directory under `prefix`. Repeatable.
    pub fn mount_dir(mut self, prefix: impl AsRef<str>, path: impl Into<PathBuf>) -> Self {
        self.roots
            .push(Root::new(prefix.as_ref(), Source::Dir(path.into())));
        self
    }

    /// Static file serving over the roots (embedded or filesystem), as-is, no
    /// compiler, no watcher. Most-specific prefix wins; same-prefix ties resolve to
    /// the **first root added**.
    pub fn router(self) -> Router {
        Router::new()
            .fallback(serve_static)
            .with_state(Arc::new(self.roots))
    }

    /// Compile TS/SCSS on the fly + watch the **filesystem** roots, live-reloading the
    /// browser; embedded roots are the static fallback. Requires the `dev` feature.
    #[cfg(feature = "dev")]
    pub fn dev(self) -> Router {
        let mut mounts = Vec::new();
        let mut embedded = None;
        for root in self.roots {
            match root.source {
                Source::Dir(dir) => {
                    let mount = if root.prefix.is_empty() {
                        crate::mount::Mount::root(dir)
                    } else {
                        crate::mount::Mount::new(&root.prefix, dir)
                    };
                    mounts.push(mount.watched(root.watch));
                }
                // First embedded root is the fallback tree.
                Source::Embedded(dir) => embedded = embedded.or(Some(dir)),
            }
        }
        match embedded {
            Some(dir) => crate::dev::dev_router_mounted_with_embedded(mounts, dir),
            None => crate::dev::dev_router_mounted(mounts),
        }
    }

    /// [`dev`](Self::dev) in debug builds (with the `dev` feature), else
    /// [`router`](Self::router) (static).
    pub fn auto(self) -> Router {
        #[cfg(all(debug_assertions, feature = "dev"))]
        return self.dev();
        #[cfg(not(all(debug_assertions, feature = "dev")))]
        return self.router();
    }
}

/// Static handler: resolve a request against the roots (most-specific prefix first,
/// same-prefix ties in declaration order) and serve the first match, staying inside
/// each root.
async fn serve_static(
    State(roots): State<Arc<Vec<Root>>>,
    headers: HeaderMap,
    uri: Uri,
) -> Response {
    let raw = uri.path().trim_start_matches('/');
    let requested = if raw.is_empty() || raw.ends_with('/') {
        format!("{raw}index.html")
    } else {
        raw.to_string()
    };
    if has_traversal(&requested) {
        return StatusCode::NOT_FOUND.into_response();
    }
    let gzip_ok = accepts_gzip(&headers);
    let mut candidates: Vec<(&Root, String)> = roots
        .iter()
        .filter_map(|root| relative_under(&root.prefix, &requested).map(|rel| (root, rel)))
        .collect();
    candidates.sort_by_key(|(root, _)| std::cmp::Reverse(root.prefix.len()));
    for (root, rel) in candidates {
        // Skip directory (empty) paths, and never serve a *source* file raw.
        if rel.is_empty() || is_source_file(&rel) {
            continue;
        }
        // The original content type, even when serving a pre-compressed `.gz`.
        let ct = content_type(&rel);
        match &root.source {
            Source::Embedded(dir) => {
                if gzip_ok {
                    if let Some(gz) = dir.get_file(format!("{rel}.gz")) {
                        return gz_response(ct, gz.contents().to_vec());
                    }
                }
                if let Some(file) = dir.get_file(&rel) {
                    return ([(header::CONTENT_TYPE, ct)], file.contents()).into_response();
                }
            }
            Source::Dir(path) => {
                if gzip_ok {
                    if let Some(gz) = contained_file(path, &format!("{rel}.gz")) {
                        // Don't hand back a gzipped *source* (e.g. an `app.scss.gz`):
                        // strip `.gz` and re-check the resolved name.
                        let degz_is_source = gz
                            .file_stem()
                            .is_some_and(|s| has_source_extension(Path::new(s)));
                        if !degz_is_source {
                            if let Ok(bytes) = std::fs::read(&gz) {
                                return gz_response(ct, bytes);
                            }
                        }
                    }
                }
                if let Some(file) = contained_file(path, &rel) {
                    // Re-check the *resolved* path, not just the request string: a
                    // case-insensitive or name-folding FS can open a source the request
                    // didn't reveal (`app.SCSS`, or `app.scss.` on Windows), which the
                    // lexical `is_source_file` guard above misses.
                    if !has_source_extension(&file) {
                        return match std::fs::read(&file) {
                            Ok(bytes) => ([(header::CONTENT_TYPE, ct)], bytes).into_response(),
                            Err(e) => {
                                (StatusCode::INTERNAL_SERVER_ERROR, e.to_string()).into_response()
                            }
                        };
                    }
                }
            }
        }
    }
    StatusCode::NOT_FOUND.into_response()
}

/// Whether the client's `Accept-Encoding` lists `gzip`.
fn accepts_gzip(headers: &HeaderMap) -> bool {
    headers
        .get(header::ACCEPT_ENCODING)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|v| {
            v.split(',')
                .any(|e| e.split(';').next().map(str::trim) == Some("gzip"))
        })
}

/// Serve pre-compressed bytes with `Content-Encoding: gzip` and the original type.
fn gz_response(content_type: String, bytes: Vec<u8>) -> Response {
    (
        [
            (header::CONTENT_TYPE, content_type),
            (header::CONTENT_ENCODING, "gzip".to_string()),
        ],
        bytes,
    )
        .into_response()
}

/// Bind `addr` and serve `app` until the process stops.
pub async fn serve(app: Router, addr: SocketAddr) -> std::io::Result<()> {
    let listener = tokio::net::TcpListener::bind(addr).await?;
    println!("web-modules: serving on http://{addr}/  (Ctrl-C to stop)");
    axum::serve(listener, app).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn root_normalises_prefix() {
        let r = Root::new("/ui/", Source::Dir(PathBuf::from("x")));
        assert_eq!(r.prefix, "ui");
        assert!(r.watch);
    }

    #[test]
    fn default_constructors_place_one_root_at_slash() {
        assert_eq!(Frontend::dir("web").roots.len(), 1);
        assert_eq!(Frontend::dir("web").roots[0].prefix, "");
        // `new()` starts empty; `source`/`mount_*` add roots.
        assert_eq!(Frontend::new().roots.len(), 0);
        assert_eq!(
            Frontend::new()
                .source("web")
                .mount_dir("/api", "api")
                .roots
                .len(),
            2
        );
    }
}
