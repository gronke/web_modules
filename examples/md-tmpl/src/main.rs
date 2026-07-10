//! Typed markdown pages, **served from the binary**: `build.rs` bakes `about.md` (the
//! pipeline page) and `commits.md` (the git-fed typed render) into `$OUT_DIR/dist`,
//! which is embedded here. `Frontend::auto()` serves the bake in release builds; in
//! debug builds it runs the live dev server over `web/` with the bake as fallback —
//! edit `web/about.tmpl.md` and the browser reloads with a fresh render, while
//! `commits.md` keeps coming from the bake.
//!
//! Run it: `cargo run -p md-tmpl-example`, then open <http://127.0.0.1:8080/>.

use std::net::SocketAddr;

use include_dir::{include_dir, Dir};
use web_modules::{serve, Frontend};

// Baked by build.rs: index.html, about.md, commits.md, importmap.json.
static DIST: Dir = include_dir!("$OUT_DIR/dist");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let web = concat!(env!("CARGO_MANIFEST_DIR"), "/web");
    let app = Frontend::embedded(&DIST).source(web).auto();
    serve(app, SocketAddr::from(([127, 0, 0, 1], 8080))).await?;
    Ok(())
}
