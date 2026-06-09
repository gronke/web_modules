//! Build-time toolchain. The build pipeline — vendor + transform (TypeScript → JS,
//! SCSS → CSS) + render an `index.html` / import map into an embeddable output dir — is
//! re-exported as `web_modules::build`. Alongside it live the emit helpers, each behind
//! its own Cargo feature and also re-exported at the crate root: `bundle`
//! (CommonJS → ESM via rolldown → `web_modules::bundle`), `compress` (gzip `.gz`
//! sidecars → `web_modules::compress`) and `templates` (HTML / import-map rendering →
//! `web_modules::templates`).

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
