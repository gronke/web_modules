//! Runtime serving (feature `axum`): a `Frontend` router that serves the built frontend
//! from baked-in (embedded) assets, or (with `dev`) compiles TypeScript/SCSS on the fly
//! with file-watching and live-reload. Both routers share the private `serving`
//! containment boundary, so no request can resolve to a file outside a known root.
//! `server` and `dev` are re-exported at the crate root; `serving` stays private.

#[cfg(feature = "axum")]
mod serving;

#[cfg(feature = "axum")]
pub mod server;

#[cfg(feature = "dev")]
pub mod dev;
