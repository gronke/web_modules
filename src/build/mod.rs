//! Build-time toolchain for a consumer crate's `build.rs`.
//!
//! [`build()`] runs the full pipeline: vendor npm packages, compile TypeScript and
//! SCSS, and render an `index.html` and import map into an embeddable output
//! directory; [`Output::optimized`] adds minification and gzip for release builds.
//!
//! Companion helpers, each re-exported at the crate root: `bundle` folds a CommonJS
//! app and its `node_modules/` into one browser ES module, `compress` writes gzip
//! `.gz` sidecars, and `templates` renders Tera templates.

#[cfg(feature = "bundle")]
pub mod bundle;
#[cfg(feature = "compress")]
pub mod compress;
#[cfg(feature = "tera")]
pub mod templates;

// The pipeline itself needs the TypeScript toolchain; its public items (`build()`,
// `Output`, …) are re-exported as `web_modules::build::*` (kept in `pipeline.rs` so the
// `build` facade module isn't named after a child of the same name).
#[cfg(feature = "typescript")]
mod pipeline;
#[cfg(feature = "typescript")]
pub use pipeline::*;

// The fluent builder over `build()` / `BuildOptions`, re-exported at the crate root as
// `web_modules::Build` (and `web_modules::build::Build`). Behind the `builder` feature so the bare
// struct API can be used without it.
#[cfg(all(feature = "builder", feature = "typescript"))]
mod builder;
#[cfg(all(feature = "builder", feature = "typescript"))]
pub use builder::Build;
