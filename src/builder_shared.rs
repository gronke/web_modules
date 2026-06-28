//! Shared fluent-builder methods for [`Build`](crate::build::Build) and
//! [`Dev`](crate::dev::Dev).
//!
//! Both builders accumulate the same source inputs — a list of roots and a
//! [`Processors`](crate::build::Processors) set — so the methods that set them are
//! written once here and stamped onto each builder by [`source_builder_methods!`], the
//! same `macro_rules!` + `pub(crate) use` approach `cli_config`'s `feature_args!` uses.
//! Each builder then adds its own mode-specific methods (`Build` its output/vendor
//! options and `run`, `Dev` its `serve`/`router` terminals), all in the
//! `self`-consuming, `Self`-returning chain style of [`Frontend`](crate::Frontend).

/// Stamp the shared source-input methods onto a fluent builder that holds
/// `roots: Vec<PathBuf>` and `processors: crate::build::Processors` fields.
macro_rules! source_builder_methods {
    ($ty:ty) => {
        impl $ty {
            /// Add a source root. Roots are merged first-match-wins (the first root
            /// added wins a path conflict). Repeatable.
            pub fn root(mut self, root: impl Into<std::path::PathBuf>) -> Self {
                self.roots.push(root.into());
                self
            }

            /// Add several source roots at once (see [`root`](Self::root)). Repeatable.
            pub fn roots<I, P>(mut self, roots: I) -> Self
            where
                I: IntoIterator<Item = P>,
                P: Into<std::path::PathBuf>,
            {
                self.roots.extend(roots.into_iter().map(Into::into));
                self
            }

            /// Enable or disable the TypeScript / modern-JS transform (default on).
            pub fn typescript(mut self, on: bool) -> Self {
                self.processors.typescript = on;
                self
            }

            /// Enable or disable SCSS compilation (default on).
            pub fn scss(mut self, on: bool) -> Self {
                self.processors.scss = on;
                self
            }

            /// Enable or disable `*.tera` rendering (default on).
            pub fn tera(mut self, on: bool) -> Self {
                self.processors.tera = on;
                self
            }

            /// Decorator lowering for the TypeScript transform (default
            /// [`Decorators::Lit`](crate::Decorators::Lit)).
            pub fn decorators(mut self, decorators: crate::Decorators) -> Self {
                self.processors.ts_decorators = decorators;
                self
            }

            /// Add an extra SCSS `@use`/`@import` load path, on top of the source
            /// roots. Repeatable.
            pub fn scss_load_path(mut self, path: impl Into<std::path::PathBuf>) -> Self {
                self.processors.extra_scss_load_paths.push(path.into());
                self
            }

            /// Add several extra SCSS load paths at once (see
            /// [`scss_load_path`](Self::scss_load_path)). Repeatable.
            pub fn scss_load_paths<I, P>(mut self, paths: I) -> Self
            where
                I: IntoIterator<Item = P>,
                P: Into<std::path::PathBuf>,
            {
                self.processors
                    .extra_scss_load_paths
                    .extend(paths.into_iter().map(Into::into));
                self
            }

            /// Select which reject [`Presets`](crate::reject::Presets) keep paths out of the
            /// output and out of serving (default: [`Presets::ALL`](crate::reject::Presets::ALL)).
            /// Replaces the current selection; compose with the bitwise operators, e.g.
            /// `Presets::ALL & !Presets::CONFIG`. Add individual patterns on top with
            /// [`reject`](Self::reject).
            pub fn reject_preset(mut self, presets: crate::reject::Presets) -> Self {
                self.processors.reject = presets.into();
                self
            }

            /// Reject one extra pattern (`*.ext`, `name/`, or an exact `name`) on top of the
            /// selected [`presets`](Self::reject_preset). Case-insensitive. Repeatable.
            pub fn reject(mut self, pattern: impl AsRef<str>) -> Self {
                self.processors.reject.add(pattern);
                self
            }
        }
    };
}

pub(crate) use source_builder_methods;
