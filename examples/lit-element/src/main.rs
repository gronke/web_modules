//! Lit 3 + Bootstrap 5, baked at build time by web-modules (see `build.rs`) and
//! served by axum via the `Frontend` factory.
//!
//! - `cargo run -p lit-element` → **live-reload** dev server: edit `web/app.ts` or
//!   `web/styles.scss` and the browser refreshes (recompiled on the fly), with the
//!   vendored modules + `index.html` served from the build-time bake.
//! - `cargo run -p lit-element --release` → serves the whole frontend **embedded**
//!   in the binary (no filesystem).

use std::net::SocketAddr;
use std::path::PathBuf;

use include_dir::{include_dir, Dir};
use web_modules::{serve, Frontend};

// Baked by build.rs: vendored web_modules/ + compiled app.js/styles.css + index.html.
static DIST: Dir = include_dir!("$OUT_DIR/dist");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("web");
    // debug → live-reload from web/ (falling back to the baked assets);
    // release → serve everything embedded. `WEB_MODULES_EMBEDDED=1` forces embedded
    // serving in any build — used by the Playwright e2e tests for a deterministic run.
    let app = if std::env::var_os("WEB_MODULES_EMBEDDED").is_some() {
        Frontend::embedded(&DIST).router()
    } else {
        Frontend::embedded(&DIST).source(web).auto()
    };
    serve(app, SocketAddr::from(([127, 0, 0, 1], 8080))).await?;
    Ok(())
}
