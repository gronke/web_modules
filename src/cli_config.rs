//! CLI scaffolding for the compiler processors — compiled **only** with the `cli`
//! feature, so the library path stays clap-free (the deliberate design noted on the
//! `cli` feature in `Cargo.toml`).
//!
//! Each compiler processor owns its own CLI surface: it knows its name best, so it
//! declares — via [`feature_args!`] — a `--<name>` / `--no-<name>` enable/disable toggle
//! plus whatever feature-specific flags it offers. The `build` and `dev` commands then
//! `#[command(flatten)]` every compiled-in processor's args together. A processor with
//! no flags of its own uses [`NoConfig`].
//!
//! **Prefix rule:** a processor's feature-specific flags must each be spelled with the
//! feature-name prefix — `#[arg(long = "scss-…")]`, `#[arg(long = "typescript-…")]` —
//! because clap's `#[command(flatten)]` does not namespace fields, so two processors
//! that both declared, say, `--out` would clash. The `--<name>` / `--no-<name>` toggle
//! is the one exception (it *is* the feature name).

use clap::Args;

/// Placeholder config for a processor with no flags of its own (e.g. `minify`, `gzip`,
/// `tera`). Flattening it contributes nothing, leaving just the processor's
/// `--<name>` / `--no-<name>` toggle.
///
/// `#[group(skip)]` is required: clap names a flattened struct's implicit `ArgGroup` after
/// the type, so the *same* `NoConfig` flattened by several processors would collide on the
/// group id ("`NoConfig` is already in use"). Skipping the (empty, pointless) group lets it
/// be the shared placeholder it's meant to be.
#[derive(Args, Clone, Debug, Default)]
#[group(skip)]
pub struct NoConfig {}

/// Define a processor's flattenable CLI args: a `--<on>` / `--<off>` enable/disable
/// toggle the processor owns, plus a flattened `$config` of feature-specific flags
/// (use [`NoConfig`] when there are none).
///
/// Invoked as `feature_args!(ScssArgs, scss, "scss", no_scss, "no-scss", ScssConfig)`:
/// the struct name, then the on-field ident + its `--flag`, the off-field ident + its
/// `--flag`, and the config type. Distinct field idents keep the toggles' clap arg ids
/// unique once several `…Args` are flattened into one command.
macro_rules! feature_args {
    ($name:ident, $on:ident, $on_flag:literal, $off:ident, $off_flag:literal, $config:ty) => {
        #[doc = concat!("CLI args (enable/disable toggle + config) for the `", $on_flag, "` processor.")]
        #[derive(clap::Args, Clone, Debug, Default)]
        pub struct $name {
            // `help =` (an expression), not a `///` doc comment: clap only picks up doc
            // comments that are string *literals*, so a macro-built `#[doc = concat!(…)]`
            // would be silently ignored (leaving the flag with no help).
            #[arg(long = $on_flag, help = concat!("Enable the `", $on_flag, "` processor."))]
            $on: bool,
            #[arg(long = $off_flag, help = concat!("Disable the `", $on_flag, "` processor."))]
            $off: bool,
            /// Feature-specific flags (each spelled with the feature-name prefix).
            #[command(flatten)]
            pub config: $config,
        }

        impl $name {
            /// Resolve whether this processor runs: `--no-<name>` wins, then `--<name>`,
            /// then `default_on` (which `--no-default-features` suppresses).
            pub fn enabled(&self, default_on: bool, no_default_features: bool) -> bool {
                self.enabled_with(None, default_on, no_default_features)
            }

            /// Like [`enabled`](Self::enabled), but a `block` override — a processor toggle
            /// read from a `package.json` `web_modules` block — sits between the flags and the
            /// compiled-in default: `--no-<name>` > `--<name>` > `block` >
            /// (`default_on && !no_default_features`). So an explicit flag still wins, but the
            /// block beats the (possibly `--no-default-features`-suppressed) built-in default.
            pub fn enabled_with(
                &self,
                block: Option<bool>,
                default_on: bool,
                no_default_features: bool,
            ) -> bool {
                if self.$off {
                    false
                } else if self.$on {
                    true
                } else if let Some(b) = block {
                    b
                } else {
                    default_on && !no_default_features
                }
            }
        }
    };
}

pub(crate) use feature_args;
