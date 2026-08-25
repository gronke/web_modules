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

/// How class fields are emitted, which `tsc` decides with `useDefineForClassFields` — and,
/// without it, with the target: define from ES 2022 upwards, assign below it.
///
/// The two differ observably. A field *defined* on the instance shadows an inherited
/// getter; the same field *assigned* calls that getter's setter, and throws when there is
/// none. Compiling a package the other way from its own build changes what it does at run
/// time, so this travels separately from [`Decorators`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum ClassFields {
    /// Assignment semantics, and a field declared without an initializer emits nothing —
    /// `useDefineForClassFields: false`, which is `tsc`'s own default below ES 2022.
    #[default]
    Assign,
    /// Define semantics: `Object.defineProperty` per field, as ES 2022 specifies.
    Define,
}

/// Comment policy for emitted JavaScript — what the single codegen pass prints.
/// Defined here (always compiled) so the build [`Output`](crate::build::Output) can
/// carry it whether or not the `typescript` processor is enabled; re-exported at the
/// crate root as `web_modules::Comments`. It applies wherever the toolchain rewrites
/// JS: compiled TypeScript always, copied/rendered/vendored JS whenever an output
/// rewrite is active. CSS needs no policy — grass's compressed output already drops
/// everything except `/*!` loud comments.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum Comments {
    /// Print every comment the printer supports — the byte-conservative default of a
    /// plain build.
    #[default]
    Keep,
    /// Drop normal, JSDoc and annotation comments; legal comments (`//!`, `/*!`,
    /// `@license`, `@preserve`) stay inline, so license text always ships with the
    /// code. The default under minification — deliberately not oxc's own minify
    /// preset, which drops legal comments too.
    Strip,
    /// Like [`Strip`](Self::Strip), but move the legal comments into a
    /// `<output>.LEGAL.txt` sidecar beside the file, leaving a pointer comment in the
    /// code. The sidecar holds the deduplicated comment texts verbatim, blank-line
    /// separated — a stable, tool-agnostic contract.
    Collect,
    /// Drop everything, legal comments included — for tiny embedded targets. The
    /// vendored `LICENSE`/`NOTICE` files still ship.
    None,
}

/// `--comments` value: the CLI mirror of [`Comments`] (which is `#[non_exhaustive]`
/// and not a `clap::ValueEnum`).
#[cfg(feature = "cli")]
#[derive(clap::ValueEnum, Clone, Copy, Debug, PartialEq, Eq)]
pub enum CommentsArg {
    /// Keep every comment (the default without `--minify`).
    Keep,
    /// Drop normal/JSDoc/annotation comments, keep legal comments inline (the
    /// default under `--minify`).
    Strip,
    /// Strip, with legal comments collected into `<output>.LEGAL.txt` sidecars.
    Collect,
    /// Drop everything, legal comments included.
    None,
}

#[cfg(feature = "cli")]
impl From<CommentsArg> for Comments {
    fn from(value: CommentsArg) -> Self {
        match value {
            CommentsArg::Keep => Comments::Keep,
            CommentsArg::Strip => Comments::Strip,
            CommentsArg::Collect => Comments::Collect,
            CommentsArg::None => Comments::None,
        }
    }
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
