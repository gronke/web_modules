//! CommonJS→ESM bundling via rolldown (the `bundle` feature).
//!
//! web_modules' core path vendors *browser-ESM* npm packages into a `web_modules/` tree and lets the
//! browser's import map resolve them; no bundler. But many packages (React and most of its
//! ecosystem) ship **only CommonJS**, which a browser can't `import`. For those, this module bundles
//! your app entry together with its installed `node_modules/` (CommonJS and all) into a single
//! browser-ready **ES module**, using [rolldown], the embedded, oxc-based Rust bundler. Still no
//! Node: rolldown runs in-process.
//!
//! The dependencies must already be installed under `<cwd>/node_modules/`; pair this with
//! [`npm_utils::install::node_modules`] (re-exported as [`crate::npm`]) for the full,
//! pure-Rust pipeline:
//!
//! ```no_run
//! use std::path::Path;
//! use web_modules::bundle::{bundle, BundleOptions};
//!
//! # fn main() -> web_modules::Result<()> {
//! let web = Path::new("web");
//! // 1. Install the (transitive) dependency tree into web/node_modules/ (pure Rust, no npm).
//! web_modules::npm::install::node_modules(&web.join("package.json"), web)
//!     .map_err(|e| web_modules::Error::Bundle(e.to_string()))?;
//! // 2. Bundle the app entry + everything it imports from node_modules/ into one browser ES module.
//! bundle(&BundleOptions {
//!     entry: &web.join("app.tsx"),
//!     cwd: web,
//!     out_dir: Path::new("dist"), // writes dist/app.js (named after the entry)
//!     production: true,
//! })?;
//! # Ok(()) }
//! ```
//!
//! `production: true` defines `process.env.NODE_ENV = "production"`, so a CommonJS dependency like
//! React selects its production build, its dev-only branches are dead-code-eliminated, and the
//! output is minified. `false` keeps a readable, un-minified development bundle. JSX/TypeScript in
//! the entry is transformed by rolldown's own oxc pass; no separate compile step is needed.
//!
//! # Multi-entry split bundling ([`bundle_split`])
//!
//! The second API serves the opposite deployment: an import-map application that ships many
//! URL-addressable ES modules and wants *fewer requests without changing a single URL*. Every
//! module that anything imports by URL becomes an entry whose output keeps its exact relative
//! path (a facade re-exporting from content-hashed shared chunks); vendored trees and other
//! mounts stay external and keep resolving through the browser's import map. Analyzable dynamic
//! imports keep pointing at the preserved entry URLs, and unanalyzable (template-literal) ones
//! survive verbatim for the browser. Input is expected to be already-transpiled ES — no
//! transform pass runs, so custom class-field/`static` semantics survive byte-for-byte.
//!
//! [rolldown]: https://rolldown.rs

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::core::importmap::Importmap;
use crate::{Error, Result};

/// Inputs for [`bundle`].
pub struct BundleOptions<'a> {
    /// The application entry module (e.g. `web/app.tsx`). rolldown's oxc transforms its JSX/TS.
    pub entry: &'a Path,
    /// Module-resolution root: the directory whose `node_modules/` holds the dependencies (usually
    /// your `web/` dir). Install it first with [`crate::npm::install::node_modules`].
    pub cwd: &'a Path,
    /// Directory the bundled `.js` (named after the entry, e.g. `app.js`) is written to.
    pub out_dir: &'a Path,
    /// Production build: define `process.env.NODE_ENV = "production"` (so CommonJS deps like React
    /// pick their production build and dead dev branches are eliminated) and minify the output.
    /// `false` keeps a readable, un-minified dev bundle.
    pub production: bool,
}

/// Bundle [`BundleOptions::entry`] and everything it imports from `<cwd>/node_modules/` into a single
/// browser ES module under `out_dir`. The dependencies must already be installed (see the module
/// docs). Pure Rust; rolldown runs in-process, no Node.
pub fn bundle(opts: &BundleOptions<'_>) -> Result<()> {
    // rolldown's bundle is async; run it on a dedicated current-thread runtime so a sync `build.rs`
    // (or any sync caller) can drive it without being async itself.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Bundle(format!("tokio runtime: {e}")))?
        .block_on(bundle_async(opts))
}

async fn bundle_async(opts: &BundleOptions<'_>) -> Result<()> {
    let cwd = opts
        .cwd
        .canonicalize()
        .map_err(|e| Error::Bundle(format!("cwd {}: {e}", opts.cwd.display())))?;
    // Containment: every module rolldown resolves must live under `cwd` (the app's sources and
    // its node_modules/). The source tree may be untrusted: a `../..` import chain or a
    // symlinked package would otherwise fold arbitrary local files into the published bundle.
    // A specifier that does not canonicalize (a virtual/builtin module) is left to rolldown.
    let cwd_for_resolve = Arc::new(cwd);
    let is_external = rolldown::IsExternal::Fn(Some(Arc::new(
        move |specifier: &str, _importer: Option<&str>, resolved: bool| {
            let cwd = Arc::clone(&cwd_for_resolve);
            let specifier = specifier.to_string();
            Box::pin(async move {
                if resolved {
                    if let Ok(real) = Path::new(&specifier).canonicalize() {
                        if !real.starts_with(cwd.as_path()) {
                            // The closure's signature is `anyhow::Result` (rolldown's
                            // API); the crate's own Error converts into it.
                            return Err(Error::Bundle(format!(
                                "module {specifier} resolves outside the bundle cwd {}",
                                cwd.display()
                            ))
                            .into());
                        }
                    }
                }
                Ok(false)
            })
        },
    )));
    let mut bundler = rolldown::Bundler::new(rolldown::BundlerOptions {
        input: Some(vec![opts.entry.to_string_lossy().to_string().into()]),
        cwd: Some(opts.cwd.to_path_buf()),
        format: Some(rolldown::OutputFormat::Esm),
        dir: Some(opts.out_dir.to_string_lossy().to_string()),
        external: Some(is_external),
        minify: Some(opts.production.into()),
        // Inline `process.env.NODE_ENV` so CJS deps (React) take their production path; without it
        // the browser would hit a bare `process` reference.
        define: opts.production.then(|| {
            [(
                "process.env.NODE_ENV".to_string(),
                "\"production\"".to_string(),
            )]
            .into_iter()
            .collect()
        }),
        ..Default::default()
    })
    .map_err(|e| Error::Bundle(format!("{e:?}")))?;

    bundler
        .write()
        .await
        .map_err(|e| Error::Bundle(format!("{e:?}")))?;
    Ok(())
}

/// Inputs for [`bundle_split`] — multi-entry chunked bundling that preserves every entry's
/// public URL.
///
/// Where [`bundle`] folds one entry plus `node_modules/` into a single file, `bundle_split`
/// serves import-map applications that ship *many* URL-addressable modules: every module that
/// the browser (or unbundled code) imports by URL stays available at exactly that URL as a
/// facade re-exporting from content-hashed shared chunks. Externals are left as bare
/// specifiers / URLs for the browser's import map, so bundled and unbundled worlds compose.
pub struct SplitBundleOptions<'a> {
    /// Entry modules, **relative to `root`**. Each entry's output keeps this exact relative
    /// path under `out_dir` (the URL contract) and preserves its export signature (facade).
    pub entries: &'a [PathBuf],
    /// Module-resolution and URL-space root — the directory whose layout mirrors the served
    /// URL space (typically your built `dist/`).
    pub root: &'a Path,
    /// Output directory; may equal `root` to bundle in place.
    pub out_dir: &'a Path,
    /// Import map consulted at bundle time. A specifier that the map resolves to an absolute
    /// URL path (`/…`) is loaded from the matching file under `root`; both exact entries and
    /// trailing-`/` prefix entries apply, mirroring browser semantics.
    pub importmap: Option<&'a Importmap>,
    /// Specifiers and URL-path prefixes to keep **external** (emitted verbatim for the
    /// browser's import map): an entry matches exactly, or by prefix when it ends with `/`.
    /// Matching is applied to the raw specifier *and* to its import-map-resolved URL path.
    pub external: &'a [String],
    /// Naming template for shared chunks, e.g. `chunks/[name]-[hash].js`. Relative to
    /// `out_dir`.
    pub chunk_filenames: &'a str,
    /// Minify the output. Input is expected to be already-transpiled ES; no transform or
    /// downleveling pass is applied either way, so class-field and `static` semantics survive
    /// byte-for-byte.
    pub minify: bool,
}

/// What [`bundle_split`] produced.
pub struct SplitBundleOutput {
    /// Absolute paths of every source module folded into the output (entries included).
    /// External modules never appear. Callers that bundle a served tree in place can prune
    /// exactly these files (minus the entries) — nothing else — from the original layout.
    pub bundled_modules: Vec<PathBuf>,
}

/// Bundle [`SplitBundleOptions::entries`] into facades + shared chunks under `out_dir`,
/// preserving each entry's relative path and export signature. See [`SplitBundleOptions`].
pub fn bundle_split(opts: &SplitBundleOptions<'_>) -> Result<SplitBundleOutput> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| Error::Bundle(format!("tokio runtime: {e}")))?
        .block_on(bundle_split_async(opts))
}

/// Decide whether `specifier` (raw, or an import-map-resolved URL path) is external.
fn matches_external(external: &[String], value: &str) -> bool {
    external.iter().any(|e| {
        if let Some(prefix) = e.strip_suffix('/') {
            value == prefix
                || value
                    .strip_prefix(prefix)
                    .is_some_and(|r| r.starts_with('/'))
        } else {
            value == e
        }
    })
}

/// Resolve `specifier` through import-map pairs exactly like the browser would: exact entry
/// first, then the longest trailing-`/` prefix entry. Returns the mapped URL.
fn importmap_resolve(pairs: &[(String, String)], specifier: &str) -> Option<String> {
    let mut exact = None;
    let mut best_prefix: Option<(&str, &str)> = None;
    for (spec, url) in pairs {
        if spec == specifier {
            exact = Some(url.clone());
        } else if let Some(prefix) = spec.strip_suffix('/') {
            if let Some(rest) = specifier.strip_prefix(prefix) {
                if rest.starts_with('/')
                    && best_prefix.is_none_or(|(best, _)| prefix.len() > best.len())
                {
                    best_prefix = Some((prefix, url));
                }
            }
        }
    }
    if let Some(url) = exact {
        return Some(url);
    }
    best_prefix.map(|(prefix, url)| {
        let rest = &specifier[prefix.len() + 1..];
        format!("{}{}", url, rest)
    })
}

async fn bundle_split_async(opts: &SplitBundleOptions<'_>) -> Result<SplitBundleOutput> {
    let root = opts
        .root
        .canonicalize()
        .map_err(|e| Error::Bundle(format!("root {}: {e}", opts.root.display())))?;

    // Entries: rolldown nests output by the input item's `name`, so handing it the
    // extension-less relative path (plus `[name].js` below) reproduces the URL layout.
    let input = opts
        .entries
        .iter()
        .map(|entry| {
            let rel = entry.to_string_lossy().replace('\\', "/");
            let name = rel.strip_suffix(".js").unwrap_or(&rel).to_string();
            rolldown::InputItem {
                name: Some(name),
                import: root.join(entry).to_string_lossy().to_string(),
            }
        })
        .collect::<Vec<_>>();

    // External decisions + import-map resolution live in one closure: a specifier is external
    // when it (or the URL the import map gives it) matches the external list; otherwise a
    // mapped URL under `/` is rewritten to the file under `root` and gets bundled.
    let external_list: Arc<[String]> = opts.external.to_vec().into();
    // Externality also holds by RESOLVED LOCATION: a relative import that lands on an
    // external file's location (an entry importing '../config.js') must stay external too,
    // or the file would be folded (and evaluated twice next to its still-served original).
    // rolldown re-relativizes resolved-absolute externals per emitted chunk, so the emitted
    // import stays correct wherever the importer ends up.
    let external_paths: Arc<[(PathBuf, bool)]> = opts
        .external
        .iter()
        .filter_map(|e| {
            let (is_prefix, name) = match e.strip_suffix('/') {
                Some(p) => (true, p),
                None => (false, e.as_str()),
            };
            let path = root.join(name.trim_start_matches('/'));
            Some((path.canonicalize().ok()?, is_prefix))
        })
        .collect::<Vec<_>>()
        .into();
    let map_pairs: Arc<[(String, String)]> = opts
        .importmap
        .map(|m| {
            m.iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default()
        .into();
    let root_for_resolve = Arc::new(root.clone());
    let is_external = rolldown::IsExternal::Fn(Some(Arc::new(
        move |specifier: &str, _importer: Option<&str>, resolved: bool| {
            let external_list = Arc::clone(&external_list);
            let external_paths = Arc::clone(&external_paths);
            let map_pairs = Arc::clone(&map_pairs);
            let root = Arc::clone(&root_for_resolve);
            let specifier = specifier.to_string();
            Box::pin(async move {
                // Second pass: rolldown re-asks with the resolver's filesystem
                // path. External locations stay external however they were
                // reached; anything else that resolved is part of the graph.
                if resolved {
                    let path = Path::new(&specifier);
                    if external_paths.iter().any(|(external, is_prefix)| {
                        if *is_prefix {
                            path.starts_with(external)
                        } else {
                            path == external
                        }
                    }) {
                        return Ok(true);
                    }
                    // Containment: a module outside root is never bundled. The source tree
                    // may be untrusted — a `../..` chain or a symlinked node_modules entry
                    // must not fold local files into the published output. A specifier that
                    // does not canonicalize (a virtual module) is left to rolldown.
                    if let Ok(real) = path.canonicalize() {
                        if !real.starts_with(root.as_path()) {
                            // The closure's signature is `anyhow::Result` (rolldown's
                            // API); the crate's own Error converts into it.
                            return Err(Error::Bundle(format!(
                                "module {specifier} resolves outside the bundle root {}",
                                root.display()
                            ))
                            .into());
                        }
                    }
                    return Ok(false);
                }
                // Relative imports resolve within the bundle (the resolved
                // pass above re-checks their final location).
                if specifier.starts_with('.') {
                    return Ok(false);
                }
                // URL-absolute imports ("/web_modules/…") are the browser's
                // domain unless the file lives under root and is not external.
                if specifier.starts_with('/') {
                    return Ok(matches_external(&external_list, &specifier)
                        || !root.join(specifier.trim_start_matches('/')).exists());
                }
                if matches_external(&external_list, &specifier) {
                    return Ok(true);
                }
                if let Some(url) = importmap_resolve(&map_pairs, &specifier) {
                    return Ok(matches_external(&external_list, &url));
                }
                Ok(false)
            })
        },
    )));

    // Import-map alias: rewrite each map entry to its file path under `root` so rolldown's
    // resolver loads the same file the browser would fetch. External entries are excluded —
    // the closure above already keeps them out of the graph.
    let alias = opts.importmap.map(|map| {
        map.iter()
            .filter(|(spec, url)| {
                !matches_external(opts.external, spec) && !matches_external(opts.external, url)
            })
            .filter_map(|(spec, url)| {
                let path = url.strip_prefix('/')?;
                let target = root.join(path).to_string_lossy().to_string();
                Some((
                    spec.strip_suffix('/').unwrap_or(spec).to_string(),
                    vec![Some(target)],
                ))
            })
            .collect::<Vec<_>>()
    });

    let mut bundler = rolldown::Bundler::new(rolldown::BundlerOptions {
        input: Some(input),
        cwd: Some(root.clone()),
        format: Some(rolldown::OutputFormat::Esm),
        dir: Some(opts.out_dir.to_string_lossy().to_string()),
        entry_filenames: Some("[name].js".to_string().into()),
        chunk_filenames: Some(opts.chunk_filenames.to_string().into()),
        external: Some(is_external),
        resolve: alias.map(|alias| rolldown::ResolveOptions {
            alias: Some(alias),
            ..Default::default()
        }),
        minify: Some(opts.minify.into()),
        ..Default::default()
    })
    .map_err(|e| Error::Bundle(format!("{e:?}")))?;

    let output = bundler
        .write()
        .await
        .map_err(|e| Error::Bundle(format!("{e:?}")))?;

    // Report which source files got folded: chunk module ids are absolute
    // filesystem paths for file modules; externals never join a chunk.
    let mut bundled_modules = Vec::new();
    for asset in &output.assets {
        if let rolldown_common::Output::Chunk(chunk) = asset {
            for id in &chunk.module_ids {
                let path = PathBuf::from(id.to_string());
                if path.is_absolute() && path.exists() {
                    bundled_modules.push(path);
                }
            }
        }
    }
    bundled_modules.sort();
    bundled_modules.dedup();
    Ok(SplitBundleOutput { bundled_modules })
}
