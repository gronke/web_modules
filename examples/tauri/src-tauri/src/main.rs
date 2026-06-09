//! A minimal Tauri v2 desktop app whose frontend is served by **web-modules**.
//!
//! - `cargo tauri dev` runs the `beforeDevCommand` (`dev-server`, the web-modules LIVE
//!   server) and points the webview at `devUrl`. Edit `web/app.ts` or `web/styles.scss`
//!   and save — web-modules recompiles and the window live-reloads.
//! - `cargo tauri build` runs the `beforeBuildCommand` (`dev-server bake dist`), which
//!   compiles `web/` into the static `frontendDist`; Tauri embeds it into the binary —
//!   no server, no Node, pure Rust.
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    tauri::Builder::default()
        .run(tauri::generate_context!())
        .expect("error while running the Tauri application");
}
