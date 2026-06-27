//! Feature-gated processors applied to your source tree and assets: TypeScript,
//! SCSS, minification, `.d.ts` emission, XLIFF i18n, and icon generation. Each sits
//! behind its matching Cargo feature and is re-exported at the crate root (e.g.
//! `web_modules::scss`), so callers never reference `processors` directly.
//!
//! This is the deliberate counterpart to the always-on vendor / import-map core and
//! the build/serve toolchain that live at the crate root: everything here *transforms
//! your inputs*, everything there *vendors and delivers* the result.
//!
//! The one always-compiled item here is [`Decorators`] (re-exported at the crate root): the
//! decorator-lowering mode the build [`Processors`](crate::build::Processors) set carries
//! regardless of which processors are enabled.

/// Decorator handling for the TypeScript transform. Defined here (always compiled) so the build
/// [`Processors`](crate::build::Processors) set can carry it whether or not the `typescript`
/// processor is enabled; re-exported at the crate root as `web_modules::Decorators` (and, with the
/// `typescript` feature, as `web_modules::typescript::Decorators`). It takes effect only when the
/// `typescript` processor actually runs.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Decorators {
    /// Legacy (experimental) decorators with Lit's class-field semantics
    /// (`experimentalDecorators: true` + `useDefineForClassFields: false`), so
    /// `@customElement`/`@property`/`@state` behave correctly. The default.
    #[default]
    Lit,
    /// No decorator/class-field tweaks: plain oxc defaults, for non-Lit (or
    /// decorator-free) sources.
    Standard,
}

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
