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
//! [rolldown]: https://rolldown.rs

use std::path::Path;

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
    let mut bundler = rolldown::Bundler::new(rolldown::BundlerOptions {
        input: Some(vec![opts.entry.to_string_lossy().to_string().into()]),
        cwd: Some(opts.cwd.to_path_buf()),
        format: Some(rolldown::OutputFormat::Esm),
        dir: Some(opts.out_dir.to_string_lossy().to_string()),
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
