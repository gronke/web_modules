# web_modules

[![CI](https://github.com/gronke/web_modules/actions/workflows/ci.yml/badge.svg)](https://github.com/gronke/web_modules/actions/workflows/ci.yml)
[![crates.io](https://img.shields.io/crates/v/web_modules.svg)](https://crates.io/crates/web_modules)
[![docs.rs](https://img.shields.io/docsrs/web_modules)](https://docs.rs/web_modules)
[![License: MIT](https://img.shields.io/crates/l/web_modules)](LICENSE)

**Pure-Rust tooling for developing Web Components**: vendor npm packages, transform
TypeScript/SCSS, and serve or embed a native-ESM frontend, with **no Node, no npm and no
bundler** at build time. Use it as a **`web-modules` CLI** for everyday development, or as a
**library** from a `build.rs` / at runtime. Built on [`npm-utils`], [oxc], [`grass`] and [rolldown].

## What it does

- **Vendor** - resolve and download npm packages into `web_modules/<name>`, targeted or including dependencies.
- **Transform** - compile and convert source files, minify and process.
- **Dev server** - serve from source, compile on the fly, watch and live-reload.
- **Build** - vendor, transform and render a deployable `dist/` - bake it into your binary, or ship it as a static site (the `web-modules build` CLI or a [GitHub Action](#github-actions)).
- **Bundle** *(opt-in)* - fold CommonJS packages and their `node_modules/` into ES modules.

## Features

Each is a Cargo `--features` flag:

- **typescript / scss** - compile to browser JS and CSS
- **tera** - HTML and [import map] templating
- **minify · dts · i18n · icons** - optional processors
- **compress** - gzip sidecars for static serving
- **bundle** - CommonJS to ESM
- **npm** - expose the `npm-utils` API as `web_modules::npm` (resolve · install · ci)
- **axum · dev** - serve the frontend, with a live-reload dev server
- **cli · env** - the `web-modules` binary, optionally configured from `WEB_MODULES_*` variables

## CLI

```bash
cargo install web_modules --features cli
```

`--features cli` is required, and deliberately so: the CLI pulls clap and a server runtime, which would land in every library build that took the default features — including the build scripts this crate is mostly used from.
Without the flag cargo installs no binary and says which feature the target wanted.
In CI you need none of this: the [action](#github-actions) downloads a prebuilt binary.

<!-- regenerate: cargo run -p web_modules --bin web-modules --features cli -- --help -->

```console
$ web-modules --help
Buildless web frontend toolchain (no Node)

Usage: web-modules <COMMAND>

Commands:
  dev     Dev server: compile TS/SCSS on the fly, render `*.tera`, watch the tree, live-reload
  build   Build a deployable output tree — the static counterpart of `dev`
  vendor  Vendor npm packages into web_modules/ + an import map
  ci      Install a package-lock.json's exact tree into node_modules/ - a pure-Rust npm ci
  npm     Run an npm-utils command (add · install · ci · upgrade · …)
  help    Print this message or the help of the given subcommand(s)

Options:
  -h, --help     Print help
  -V, --version  Print version
```

Every flag and its `package.json` key: [docs/CLI.md](docs/CLI.md).
The behavior policies (HTML, duplicate outputs, the output directory, symlinks): [docs/POLICIES.md](docs/POLICIES.md).

## Library

```toml
[dependencies]
web_modules = "0.6"   # Rust 1.95+
```

`typescript`, `scss` and `tera` are on by default; `full` enables everything (the released binary's set), and `lean` is `full` without the heavy `bundle`/rolldown tree.

The fluent `Build` and `Dev` builders (feature `builder`, on by default) are the promoted entry points — `Build` from a `build.rs` (bake a `dist/`), `Dev` for a live-reload server:

```rust
use web_modules::{Build, Dev};

// build.rs — vendor lit, compile web/, write dist/
Build::new().root("web").vendor("lit@^3").out("dist").minify(true).run()?;

// a live-reload dev server (the `dev` feature)
Dev::new().root("web").serve("127.0.0.1:8080".parse()?).await?;
```

Both layer over the lower-level `build(&BuildOptions { … })` / `dev::serve_with`, still public for fine-grained use. For the full `build.rs` / runtime API see the **[API docs][docs.rs]**, the feature flags included.
The behavior policies (HTML, duplicate outputs, the output directory, symlinks): [docs/POLICIES.md](docs/POLICIES.md).

## GitHub Actions

A composite action builds a deployable `dist/` (vendor + transform + render, with the import map injected) with no Node on the runner. It downloads a prebuilt `web-modules` binary for the runner's OS/arch (Linux x86_64/arm64, macOS arm64/x86_64, Windows x86_64/arm64), or compiles from this action's source with `from-source: true`.

**Deploy to GitHub Pages** (grant the Pages permissions and environment on the job, then build and publish with the standard actions):

```yaml
jobs:
  deploy:
    runs-on: ubuntu-latest
    permissions: { pages: write, id-token: write }
    environment: { name: github-pages, url: "${{ steps.deploy.outputs.page_url }}" }
    steps:
      - uses: actions/checkout@v7
      - uses: gronke/web_modules@v0
        with:
          packages: "lit@^3 bootstrap@^5"
          template: web/index.html.tera
          mount: /my-repo/web_modules        # project page is served under /<repo>/
      - uses: actions/configure-pages@v6
      - uses: actions/upload-pages-artifact@v5
        with: { path: dist }
      - id: deploy
        uses: actions/deploy-pages@v5
```

The dist-artifact and install-only recipes, version pinning and the input reference: [docs/CI_GITHUB_ACTION.md](docs/CI_GITHUB_ACTION.md).

## Examples

The [`examples/`](examples/) tree is full of runnable demos; `cargo run` and open the browser. A few picks:

- [**lit-element**](examples/lit-element) - a Lit 3 component themed with Bootstrap 5, baked at build time, served by axum.
- [**d3**](examples/d3) - a bar chart with D3, a non-Lit npm dependency vendored and served as-is.
- [**react-esm**](examples/react-esm) - React from npm bundled into one browser ES module, entirely in Rust (the `bundle` feature).
- [**bundle**](examples/bundle) - the buildless sources folded per entry by `--bundle`: content-hashed `chunks/`, no import map shipped, configured entirely from `package.json`.
- [**embedded**](examples/embedded) - the whole frontend baked into the binary; no filesystem, no network.
- [**tauri**](examples/tauri) - a Tauri v2 desktop app, frontend live-served (and release-baked) by web_modules.

## Maintaining

Repository setup, the release runbook and what a fork must change: [MAINTENANCE.md](MAINTENANCE.md).

## License

MIT. See [LICENSE](LICENSE).

[import map]: https://developer.mozilla.org/en-US/docs/Web/HTML/Reference/Elements/script/type/importmap
[`npm-utils`]: https://github.com/gronke/rust-npm-utils
[oxc]: https://oxc.rs
[`grass`]: https://github.com/connorskees/grass
[rolldown]: https://rolldown.rs
[docs.rs]: https://docs.rs/web_modules
