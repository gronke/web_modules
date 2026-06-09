//! "Plain" classic React, the way it worked before bundlers: load React's prebuilt **UMD**
//! browser build with a `<script>` tag and use the `window.React` / `window.ReactDOM`
//! globals. No bundler, no import map, no transform of React itself.
//!
//! Two things this example showcases:
//!
//! 1. **Single-asset extraction.** React's npm package is large (CommonJS sources, multiple
//!    builds), but a browser only needs one file: `umd/react.production.min.js`.
//!    `Extract::File { from, to }` pulls exactly that one asset out of the package — same for
//!    react-dom — instead of vendoring the whole tree.
//! 2. **Classic UMD just works.** The extracted files are loaded as classic `<script>`
//!    globals (see `web/index.html`); `web/app.ts` is transformed to JS on the fly by oxc and
//!    uses those globals. Nothing is bundled.
//!
//! This is the buildless counterpart to the `react-esm` example. React 18 still ships a UMD
//! build, so it runs in a browser as-is; React 19 dropped UMD and is CommonJS-only — that's
//! the case `react-esm` has to bundle with rolldown.
//!
//! `cargo run -p react-umd`, then open <http://127.0.0.1:8081/>.

use std::net::SocketAddr;
use std::path::Path;

use web_modules::vendor::{vendor, Extract, PackageSpec};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let web = Path::new(env!("CARGO_MANIFEST_DIR")).join("web");

    // Pull ONE file out of each package — the prebuilt UMD browser build — into
    // web/web_modules/{react,react-dom}/. `.no_imports()` because these are loaded as classic
    // <script> globals, not through the import map.
    let specs = [
        PackageSpec::npm("react", "^18")
            .extract(Extract::File {
                from: "umd/react.production.min.js".into(),
                to: "react.js".into(),
            })
            .no_imports(),
        PackageSpec::npm("react-dom", "^18")
            .extract(Extract::File {
                from: "umd/react-dom.production.min.js".into(),
                to: "react-dom.js".into(),
            })
            .no_imports(),
    ];
    vendor(&web.join("web_modules"), "/web_modules", &specs)?;

    // Headless mode (CI cache-warming): vendor, then exit without serving.
    if std::env::var_os("WEB_MODULES_VENDOR_ONLY").is_some() {
        return Ok(());
    }

    let addr = SocketAddr::from(([127, 0, 0, 1], 8081));
    web_modules::dev::serve(vec![web], addr).await?;
    Ok(())
}
