//! React (from npm, CommonJS) served as a single browser ES module that web-modules
//! produced at build time — see `build.rs`: npm-utils installs `react`/`react-dom`/`zustand`
//! into `node_modules/`, then the `bundle` feature (rolldown) folds them into one ESM file.
//! Pure Rust end to end — no Node, no CDN, no bundler at runtime.
//!
//! The whole frontend (the bundle + a tiny `index.html`) is embedded in this binary with
//! `include_dir!`, so `Frontend::embedded(&DIST).router()` is a lean static server: no
//! rolldown, no filesystem access. Run it:
//!
//! ```text
//! cargo run --manifest-path examples/react-esm/Cargo.toml
//! ```
//!
//! then open <http://127.0.0.1:8080/>. (Run via `--manifest-path` because this example is
//! excluded from the workspace.)

use std::net::SocketAddr;

use include_dir::{include_dir, Dir};
use web_modules::{serve, Frontend};

// Baked by build.rs into `$OUT_DIR/dist`: the bundled app.js + index.html.
static DIST: Dir = include_dir!("$OUT_DIR/dist");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Frontend::embedded(&DIST).router();
    serve(app, SocketAddr::from(([127, 0, 0, 1], 8080))).await?;
    Ok(())
}
