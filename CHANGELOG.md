# Changelog

All notable changes to this project are documented here.
The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

Per-release notes are also published on each [GitHub Release](https://github.com/gronke/web_modules/releases) (sourced from the annotated tag) and on [crates.io](https://crates.io/crates/web_modules).

## [Unreleased]

### Added

- feat(build): duplicate output detection — `build` fails before writing anything when two sources claim one output path, listing every conflict; `dev` warns about each conflict at startup instead of failing; `--skip-duplicates` (both commands, `Processors`, and the builders) keeps the highest-precedence source silently

### Changed

- refactor(build): one preflight scan of the source roots decides what every stage emits, and each output path is written exactly once by its winner; runtime-helper vendoring and the unresolved-import check read imports captured as each file is emitted instead of re-scanning the emitted `.js`
- Under `--skip-duplicates`, a conflict resolves by one rule in `build` and `dev` alike: earlier root first, then a Tera template over a literal file over a transformed sibling — a later root's `.tera` no longer overwrites an earlier root's file, and `dev` now serves a literal `.js`/`.css` instead of compiling a shadowed sibling source
- The unresolved-import check runs after Tera rendering, and JavaScript rendered from a template joins the module graph — an unresolvable import in it now fails the build
- `build` warns when a copied `.js` parses under neither the module nor the classic-script goal — its imports cannot be validated

### Fixed

- fix(build): find import specifiers in minified output by reading the AST
- fix(build): specifiers with a URL scheme (`blob:`, `node:`, `about:`, …) are no longer reported as unresolved bare imports — classification asks the WHATWG URL parser (the `url` crate), the browser's own first resolution step
- fix(build): a source file that canonically resolves outside its root (a symlink out of the tree) fails the build instead of being published — the dev server's containment already refused to serve such a path; source-walk problems surface as warnings instead of being silently dropped
- fix(build): the reject list applies to every emitted target, not only static copies — a template or compiled source can no longer materialize a rejected path (`.env.tera` → `.env`, `.env.ts` → `.env.js`), matching what the dev server refuses to serve

## [0.4.0] - 2026-06-28

### Added

- Fluent `Build` / `Dev` builders (`web_modules::Build` / `Dev`), behind a default-on `builder` feature.
- Zero-config `web_modules` block in `package.json` drives `dev` / `build`; `build` auto-vendors its `dependencies`.
- `PackageSpec::parse`; `web_modules::Decorators` at the crate root.

### Changed

- `build` is the static counterpart of `dev`: positional `[ROOTS]…`, `--out` (default `dist`), vendoring only when given packages/manifests.
- Processor-agnostic pipeline — `build()` / `BuildOptions` / `Processors` need no `typescript`; `DevConfig` aliases `Processors`.
- npm-utils 0.5.3 (native TLS roots, stricter sha512 integrity, hardened extraction); drop grass's clap CLI from the default build.
- The minimum supported Rust version is 1.95 (tracks the oxc transform toolchain).

### Removed

- The `compile` command (folded into `build`).

## [0.3.0] - 2026-06-24

### Added

- The reusable **`web-modules build` GitHub Action** — a composite action that builds a deployable `dist/` (vendor npm, transform TS/SCSS, render `index.html` with the import map injected) with no Node on the runner.
  - Downloads a prebuilt `web-modules` binary for the runner's OS/arch, or builds from source with `from-source: true`.
  - Prebuilt binaries for Linux x86_64/arm64, macOS arm64/x86_64, and Windows x86_64 plus native arm64 (built on `windows-11-arm`); on Windows ARM it prefers the native binary and falls back to the x86_64 build under x64 emulation.
  - With no `version` input the binary matches the pinned action tag (`uses: …@v0.3.0` fetches the v0.3.0 binary — reproducible); moving tags, branches, and commit SHAs use the latest release.
  - A moving `v0` major tag, recreated by CI after each release to point at the highest stable 0.x, so `uses: gronke/web_modules@v0` tracks the latest 0.x.
  - A job summary of each build, and a clear error when the `src` directory is missing.
- A single `SHA256SUMS` per release, which the action verifies the downloaded binary against.
- README badges (CI / crates.io / docs.rs / license), this changelog, and Dependabot for the workflow actions.
- CI: an `actionlint` job (hardened Docker container) linting the workflows; the Pages workflow dogfoods the action end-to-end via the download path.

### Fixed

- Vendor: emit `cargo:rerun-if-changed` for vendored destinations.

## [0.2.0] - 2026-06-20

### Added

- Icons: configurable icon-set builder (`from_image_path` → `generate`).
- `tsconfig_node_modules_paths`: resolve 3rd-party paths from `package.json`.

### Changed

- Gate the `npm-utils` re-export behind a dedicated `npm` feature.
- Require npm-utils 0.5.1; oxc 0.135 → 0.137.
- Docs: cleanup, consistency, brevity.

## [0.1.0] - 2026-06-13

- Initial release: a pure-Rust, buildless toolchain for ES modules and Web Components.

[0.4.0]: https://github.com/gronke/web_modules/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/gronke/web_modules/compare/v0.2.0...v0.3.0
[0.2.0]: https://github.com/gronke/web_modules/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/gronke/web_modules/releases/tag/v0.1.0
