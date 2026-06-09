//! D3 — a non-Lit dependency — rendering a chart, vendored and served by
//! web-modules. Shows the toolchain serves arbitrary vendored deps, not only ES
//! modules: D3 ships a UMD bundle, loaded here as a classic global `<script>`,
//! while `web/chart.ts` is transformed to JS on the fly by oxc.
//!
//! `cargo run -p d3`, then open the printed URL.

use std::net::SocketAddr;
use std::path::Path;

use web_modules::vendor::{vendor, PackageSpec};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    // D3 ships a UMD bundle and Bootstrap is CSS-only here — both loaded as a
    // global `<script>` / stylesheet, so neither needs an import-map entry.
    let specs = [
        PackageSpec::npm("d3", "^7").no_imports(),
        PackageSpec::npm("bootstrap", "^5").no_imports(),
    ];
    vendor(&web.join("web_modules"), "/web_modules", &specs)?;

    // Headless mode (CI cache-warming / Docker image build): vendor, then exit.
    if std::env::var_os("WEB_MODULES_VENDOR_ONLY").is_some() {
        return Ok(());
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    web_modules::dev::serve(vec![web], addr).await?;
    Ok(())
}
