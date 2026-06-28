//! Pure-Rust, buildless toolchain for **ES modules and Web Components**. No Node, no
//! bundler.
//!
//! It vendors each npm package into a `web_modules/` tree with an import map
//! (Snowpack-style) and serves them through an on-the-fly transform dev server
//! (like `@web/dev-server`), built entirely in Rust on
//! [`npm-utils`](https://docs.rs/npm-utils), [oxc] and [`grass`](https://docs.rs/grass).
//! It emits native ES modules and leaves bare-specifier resolution to the browser's
//! import map; it is **not** a bundler.
//!
//! # Modules
//!
//! **Core** (always on):
//! - [`vendor`]: resolve, download and extract npm packages into a `web_modules/`
//!   tree (via `npm-utils`) and accumulate the import map.
//! - [`importmap`]: a deterministic import-map composer (build, merge fragments,
//!   render the `<script type="importmap">`).
//!
//! **Processors** (applied to your source and assets):
//! - [`typescript`]: TypeScript and modern JS → browser JS via oxc, with legacy
//!   decorators configured for Lit.
//! - [`scss`]: SCSS → CSS via grass.
//! - [`minify`]: JS minification via oxc_minifier.
//! - [`dts`]: `.d.ts` emission. [`i18n`]: XLIFF merge. [`icons`]: favicons from a PNG.
//!
//! **Toolchain** (build and serve):
//! - [`build`]: vendor, transform and render into an output dir.
//! - `bundle`: fold an app plus its `node_modules/` (CommonJS and all) into one
//!   browser ESM file via rolldown, for React-class packages that ship only CommonJS.
//! - [`templates`]: HTML templating (importmap injection).
//! - [`server`] / [`dev`]: serve embedded assets, or compile on the fly with
//!   file-watching and live-reload.
//!
//! [oxc]: https://oxc.rs

mod error;
pub use error::{Error, Result};

// Always-on core, grouped under `core/`: vendoring, import maps, the mount model,
// TypeScript `tsconfig` generation and static-file copying. Re-exported here so callers
// use `web_modules::vendor` etc. — the `core` module itself is private.
mod core;
pub use core::mount::Mount;
pub use core::{importmap, mount, reject, static_files, tsconfig, vendor};

/// Feature-gated source/asset processors, each re-exported at the crate root (e.g.
/// `web_modules::scss`). Grouped to separate "what we apply to your source" from the
/// vendor/import-map core and the build/serve toolchain.
mod processors;

/// The decorator-lowering mode (`web_modules::Decorators`), always available so the build
/// [`Processors`](build::Processors) set can carry it regardless of which processors are compiled.
pub use processors::Decorators;

#[cfg(feature = "dts")]
pub use processors::dts;
#[cfg(feature = "i18n")]
pub use processors::i18n;
#[cfg(feature = "icons")]
pub use processors::icons;
#[cfg(feature = "minify")]
pub use processors::minify;
#[cfg(feature = "scss")]
pub use processors::scss;
#[cfg(feature = "typescript")]
pub use processors::typescript;

/// CLI scaffolding (the `feature_args!` macro + the `NoConfig` placeholder) that lets
/// each compiler processor carry its own clap config. Compiled only with `cli`, so the
/// library path stays clap-free.
#[cfg(feature = "cli")]
pub mod cli_config;

/// Shared fluent-builder methods (the `source_builder_methods!` macro) stamped onto the
/// [`Build`] and [`Dev`] builders, behind the `builder` feature.
#[cfg(feature = "builder")]
mod builder_shared;

// Build-time toolchain, grouped under `build/`: the `build` pipeline plus the emit helpers
// `bundle` / `compress` / `templates`, each re-exported at its historical crate root path. The
// pipeline depends only on the always-on vendor/import-map core — each source processor (TypeScript,
// SCSS, Tera, gzip) applies only when its feature is on — so the module is always available.
pub mod build;

/// The fluent build builder (feature `builder`), at the crate root alongside [`Frontend`].
#[cfg(feature = "builder")]
pub use build::Build;

#[cfg(feature = "bundle")]
pub use build::bundle;
#[cfg(feature = "compress")]
pub use build::compress;
#[cfg(feature = "tera")]
pub use build::templates;

/// Re-export of [`npm_utils`] as `web_modules::npm`, the vendoring + transitive `node_modules`
/// install engine, behind the `npm` feature. Lets consumers reach the npm API without a separate
/// `npm-utils` dependency: install a tree with `web_modules::npm::install::node_modules`, then
/// bundle it via `web_modules::bundle` (enable the `bundle` feature too).
#[cfg(feature = "npm")]
pub use npm_utils as npm;

// Runtime serving, grouped under `serve/`: the axum `Frontend` router and the dev server,
// over a shared (private) `serving` containment boundary. Re-exported at historical paths.
mod serve;

#[cfg(feature = "dev")]
pub use serve::dev;

/// The fluent dev-server builder (feature `builder`), at the crate root alongside [`Frontend`].
#[cfg(all(feature = "builder", feature = "dev"))]
pub use serve::dev::Dev;

#[cfg(feature = "axum")]
pub use serve::server;
#[cfg(feature = "axum")]
pub use serve::server::{serve, Frontend};

/// Re-export of the `include_dir` crate for the [`include_dir::Dir`] type. Use the
/// `include_dir` crate **directly** for the `include_dir!` macro; it emits
/// `include_dir::`-qualified paths that don't resolve through a re-export.
#[cfg(feature = "axum")]
pub use include_dir;
