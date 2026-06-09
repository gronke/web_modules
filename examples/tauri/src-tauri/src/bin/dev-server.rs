//! The frontend toolchain Tauri drives via its lifecycle hooks — web-modules, no Node:
//!
//! - `dev-server` (the `beforeDevCommand`) runs the **live** dev server: it compiles
//!   `web/*.ts` → JS and `web/*.scss` → CSS on the fly, watches the tree, and live-reloads
//!   the browser/webview. `cargo tauri dev` waits for it, then loads `devUrl`.
//! - `dev-server bake <dir>` (the `beforeBuildCommand`) compiles `web/` into a static
//!   directory for Tauri's `frontendDist` — the production bundle path.
//!
//! The frontend is dependency-free (no bare imports), so neither path touches the network.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // `web/` is the sibling frontend dir (CARGO_MANIFEST_DIR is the src-tauri crate dir).
    let web = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .expect("crate dir has a parent")
        .join("web");
    let mut args = std::env::args().skip(1);
    match args.next().as_deref() {
        Some("bake") => {
            let out = PathBuf::from(args.next().expect("usage: dev-server bake <dir>"));
            bake(&web, &out)?;
            println!("web-modules: baked {} -> {}", web.display(), out.display());
        }
        _ => {
            // Same live server the d3/bootstrap examples use, on the port `devUrl` expects.
            let addr = SocketAddr::from(([127, 0, 0, 1], 1420));
            web_modules::dev::serve(vec![web], addr).await?;
        }
    }
    Ok(())
}

/// Compile + copy `web/` into a static dir Tauri can embed as `frontendDist`. Dependency-free,
/// so there is nothing to vendor — the three web-modules primitives suffice.
fn bake(web: &Path, out: &Path) -> web_modules::Result<()> {
    web_modules::typescript::compile_directory(web, out)?; // *.ts   -> *.js
    web_modules::scss::compile_directory(web, out, &[out])?; // *.scss -> *.css
    web_modules::static_files::copy_static(web, out)?; // index.html + other static files
    Ok(())
}
