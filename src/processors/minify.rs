//! JavaScript minification via [`oxc_minifier`] + minified codegen.
//!
//! Compresses the AST (constant folding, dead-code elimination) and prints
//! whitespace-free output. The build pipeline minifies inline during the
//! TypeScript transform; [`minify_str`] is the string-level entry for
//! JavaScript the compiler didn't produce.

use std::path::Path;

use crate::typescript::{rewrite_js_capturing, RewriteOptions};
use crate::Result;

/// Minify a single JS source string. `path` only informs the source type and
/// diagnostics. The thin string wrapper over the crate's one rewrite pass
/// ([`typescript`](crate::typescript)'s parse → `oxc_minifier` → codegen).
pub fn minify_str(source: &str, path: &Path) -> Result<String> {
    Ok(rewrite_js_capturing(
        source,
        path,
        path,
        RewriteOptions {
            minify: true,
            source_map: false,
            comments: crate::Comments::Keep,
        },
        None,
    )?
    .code)
}

/// Feature-specific `--minify-*` flags, paired with the `--minify` / `--no-minify`
/// toggle in [`MinifyArgs`].
#[cfg(feature = "cli")]
#[derive(clap::Args, Clone, Debug, Default)]
pub struct MinifyConfig {
    /// Also minify the vendored `web_modules/` tree and `npm://` assets (the default
    /// under `--minify`).
    #[arg(long = "minify-web-modules")]
    pub web_modules: bool,
    /// Keep the vendored `web_modules/` tree and `npm://` assets as shipped.
    #[arg(long = "no-minify-web-modules")]
    pub no_web_modules: bool,
}

// The `--minify` / `--no-minify` toggle (off by default), plus the vendor-tree knob.
#[cfg(feature = "cli")]
crate::cli_config::feature_args!(
    MinifyArgs,
    minify,
    "minify",
    no_minify,
    "no-minify",
    MinifyConfig
);

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folds_and_strips_whitespace() {
        // `export` keeps `sum` from being dead-code-eliminated; the value folds.
        let min = minify_str("export const sum = 1 + 2;\n\n", Path::new("x.js")).unwrap();
        assert!(min.contains('3'), "constant folded; got: {min}");
        assert!(!min.contains(" = "), "whitespace stripped; got: {min}");
    }
}
