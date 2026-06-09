# `tauri` example — web-modules live-serving in `cargo tauri dev`

A minimal [Tauri v2](https://v2.tauri.app) desktop app whose frontend is served by
**web-modules** — no Node, no bundler. In dev, the webview points at the web-modules **live**
dev server, so editing a `.ts`/`.scss` recompiles and live-reloads the window; for release, the
same toolchain **bakes** the frontend into the bundle.

This example is **excluded from the web-modules workspace** because Tauri pulls a heavyweight
webkit2gtk + crate tree. Build it from this directory.

## Prerequisites

Tauri's Linux system libraries and the Tauri CLI:

```bash
# Debian/Ubuntu (Debian 13 ships webkit2gtk 4.1):
sudo apt-get install -y libwebkit2gtk-4.1-dev libgtk-3-dev librsvg2-dev \
  libayatana-appindicator3-dev libxdo-dev libssl-dev build-essential pkg-config file

cargo install tauri-cli --locked   # the `cargo tauri` subcommand (latest 2.x)
```

See [tauri.app/start/prerequisites](https://v2.tauri.app/start/prerequisites/) for macOS/Windows.

## Run it (dev)

```bash
cd examples/tauri
cargo tauri dev
```

`cargo tauri dev` runs the `beforeDevCommand` — `cargo run --manifest-path src-tauri/Cargo.toml
--bin dev-server`, the web-modules live server on <http://localhost:1420> — waits for it, then
opens a native window pointed at it. Edit `web/app.ts` or `web/styles.scss`, save, and watch the
window live-reload. Because `web/` lives **outside** `src-tauri/`, Tauri's own dev-watcher (which
rebuilds the Rust app) ignores it — web-modules alone drives the frontend hot-reload.

You can run just the dev server (e.g. to open it in a browser) without Tauri:

```bash
cargo run --manifest-path src-tauri/Cargo.toml --bin dev-server   # serves web/ live on :1420
```

## Build it (release)

```bash
cargo tauri build --no-bundle
```

`cargo tauri build` runs the `beforeBuildCommand` — `cargo run --manifest-path src-tauri/Cargo.toml
--bin dev-server -- bake dist` — which compiles `web/` into the static `dist/` directory
(`frontendDist`); Tauri then embeds it
into a self-contained release binary. `--no-bundle` skips OS installers. To produce installers,
add icons (`cargo tauri icon <icon.png>`) and set `bundle.active: true` in `tauri.conf.json`.

## How it fits together

| Piece | Role |
|---|---|
| `src-tauri/src/main.rs` | the Tauri app — `tauri::Builder::default().run(generate_context!())` |
| `src-tauri/src/bin/dev-server.rs` | `serve` (live, via `web_modules::dev::serve`) and `bake <dir>` (static `frontendDist`) |
| `src-tauri/tauri.conf.json` | `beforeDevCommand` + `devUrl` (dev); `beforeBuildCommand` + `frontendDist` (build) |
| `web/` | dependency-free frontend (`index.html`, `app.ts`, `styles.scss`) — outside `src-tauri/`, so Tauri's watcher leaves frontend reloads to web-modules |

The frontend has no third-party dependencies, so both the live server and the bake run offline.
