//! Themed Bootstrap compiled from its SCSS sources — vendored and served by
//! web-modules. `web/app.scss` overrides Bootstrap variables and then builds
//! Bootstrap from its own `.scss` (kept by web-modules' default filter) with
//! grass — no Node, no dart-sass.
//!
//! `cargo run -p bootstrap-scss`, then open the printed URL.

use std::net::SocketAddr;
use std::path::Path;

use web_modules::vendor::{vendor, PackageSpec};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");
    // Vended for its SCSS sources (themed below); loaded as CSS, no import entry.
    let specs = [PackageSpec::npm("bootstrap", "^5").no_imports()];
    vendor(&web.join("web_modules"), "/web_modules", &specs)?;

    // Compile the themed stylesheet from Bootstrap's SCSS up front — validates the
    // SCSS→CSS path (also exercised in CI). The dev server recompiles on request.
    let css = web_modules::scss::compile_file(&web.join("app.scss"), &[web.as_path()])?;
    std::fs::write(web.join("app.css"), css)?;

    // Headless mode (CI cache-warming / Docker image build): vendor + build, then exit.
    if std::env::var_os("WEB_MODULES_VENDOR_ONLY").is_some() {
        return Ok(());
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], 8080));
    web_modules::dev::serve(vec![web], addr).await?;
    Ok(())
}
