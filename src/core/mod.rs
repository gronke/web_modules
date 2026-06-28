//! Always-on core: resolve and vendor npm packages into a `web_modules/` tree, compose
//! the import map, the shared mount model (one source of truth for authoring specifiers,
//! serving URLs and source directories), TypeScript `tsconfig` generation, and
//! static-file copying. Each module is re-exported at the crate root (e.g.
//! `web_modules::vendor`), so callers never reference `core` directly.
//!
//! This is the counterpart to the feature-gated `processors` (which transform your
//! source) and the build/serve toolchain (which delivers the result): everything here
//! vendors and composes the inputs.

pub mod importmap;
pub mod mount;
pub mod reject;
pub mod static_files;
pub mod tsconfig;
pub mod vendor;
