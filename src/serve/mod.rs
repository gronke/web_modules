//! Runtime serving (feature `axum`): a `Frontend` router that serves the built frontend
//! from baked-in (embedded) assets, or (with `dev`) compiles TypeScript/SCSS on the fly
//! with file-watching and live-reload. Both routers share the private `serving`
//! containment boundary, so no request can resolve to a file outside a known root.
//! `server` and `dev` are re-exported at the crate root; `serving` stays private.

#[cfg(feature = "axum")]
mod serving;

/// The redirect symlink modes — the crate's own special sauce, one module so the
/// default-on `symlink-move` feature gates it in one place.
#[cfg(all(feature = "axum", feature = "symlink-move"))]
mod symlink_move;

#[cfg(feature = "axum")]
pub mod server;

#[cfg(feature = "dev")]
pub mod dev;
