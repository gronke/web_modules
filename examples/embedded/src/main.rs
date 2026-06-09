//! A frontend **entirely baked into this binary** — HTML, minified JS, compressed CSS,
//! and their `.gz` sidecars (see `build.rs`). There is no filesystem access and no dev
//! server: `Frontend::embedded(&DIST).router()` is a lean static server that streams the
//! embedded bytes, preferring a `.gz` sidecar when the client sends `Accept-Encoding:
//! gzip`, and refusing to serve source files.
//!
//! Run it: `cargo run -p embedded`, then open <http://127.0.0.1:8080/>. Because the whole
//! site lives in the binary, the executable is self-contained — copy it anywhere and it
//! still serves the same site.

use std::net::SocketAddr;

use include_dir::{include_dir, Dir};
use web_modules::{serve, Frontend};

// Baked by build.rs into `$OUT_DIR/dist` and embedded here: index.html, app.js(.gz),
// styles.css(.gz), and the (empty) web_modules/ tree.
static DIST: Dir = include_dir!("$OUT_DIR/dist");

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let app = Frontend::embedded(&DIST).router();
    serve(app, SocketAddr::from(([127, 0, 0, 1], 8080))).await?;
    Ok(())
}
