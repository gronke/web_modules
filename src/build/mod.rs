//! Build-time toolchain for a consumer crate's `build.rs`.
//!
//! [`build()`] runs the full pipeline: vendor npm packages, compile TypeScript and
//! SCSS, and render an `index.html` and import map into an embeddable output
//! directory; [`Output::optimized`] adds minification and gzip for release builds.
//!
//! Companion helpers, each re-exported at the crate root: `bundle` folds a CommonJS
//! app and its `node_modules/` into one browser ES module, `compress` writes gzip
//! `.gz` sidecars, `templates` renders Tera templates, and `md_tmpl` renders typed
//! markdown templates.

#[cfg(feature = "bundle")]
pub mod bundle;
#[cfg(feature = "compress")]
pub mod compress;
#[cfg(feature = "md-tmpl")]
pub mod md_tmpl;
#[cfg(feature = "tera")]
pub mod templates;

// The pipeline itself needs the TypeScript toolchain; its public items (`build()`,
// `Output`, …) are re-exported as `web_modules::build::*` (kept in `pipeline.rs` so the
// `build` facade module isn't named after a child of the same name).
// The pipeline depends only on the always-on core (vendor / import map / static-file copy); each
// source processor (TypeScript, SCSS, Tera, gzip) applies only behind its own feature, so the
// pipeline itself is always compiled.
mod pipeline;
pub use pipeline::*;

// The preflight-capable step abstraction the pipeline (and the dev server's duplicate
// warnings) run on: every stage states what it would emit before anything is written.
pub(crate) mod steps;

// The fluent builder over `build()` / `BuildOptions`, re-exported at the crate root as
// `web_modules::Build` (and `web_modules::build::Build`). Behind the `builder` feature so the bare
// struct API can be used without it.
#[cfg(feature = "builder")]
mod builder;
#[cfg(feature = "builder")]
pub use builder::Build;
