//! Feature-gated processors applied to your source tree and assets: TypeScript,
//! SCSS, minification, `.d.ts` emission, XLIFF i18n, and icon generation. Each sits
//! behind its matching Cargo feature and is re-exported at the crate root (e.g.
//! `web_modules::scss`), so callers never reference `processors` directly.
//!
//! This is the deliberate counterpart to the always-on vendor / import-map core and
//! the build/serve toolchain that live at the crate root: everything here *transforms
//! your inputs*, everything there *vendors and delivers* the result.

#[cfg(feature = "typescript")]
pub mod typescript;

#[cfg(feature = "scss")]
pub mod scss;

#[cfg(feature = "minify")]
pub mod minify;

#[cfg(feature = "dts")]
pub mod dts;

#[cfg(feature = "i18n")]
pub mod i18n;

#[cfg(feature = "icons")]
pub mod icons;
