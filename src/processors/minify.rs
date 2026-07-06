//! JavaScript minification via [`oxc_minifier`] + minified codegen.
//!
//! Compresses the AST (constant folding, dead-code elimination) and prints
//! whitespace-free output. The build pipeline minifies inline during the
//! TypeScript transform; [`minify_str`] is the string-level entry for
//! JavaScript the compiler didn't produce.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_minifier::{Minifier, MinifierOptions};
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::{Error, Result};

/// Minify a single JS source string. `path` only informs the source type and
/// diagnostics.
pub fn minify_str(source: &str, path: &Path) -> Result<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_default();

    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.has_errors() {
        let body = parsed
            .diagnostics
            .iter()
            .map(|e| format!("{e:?}"))
            .collect::<Vec<_>>()
            .join("\n");
        return Err(Error::Minify(format!(
            "parse error(s) in {}:\n{body}",
            path.display()
        )));
    }
    let mut program = parsed.program;

    // Compress + mangle the AST; codegen applies the returned mangler scoping so
    // renamed identifiers are emitted, then strips whitespace (`minify: true`).
    let ret = Minifier::new(MinifierOptions::default()).minify(&allocator, &mut program);

    let code = Codegen::new()
        .with_options(CodegenOptions {
            minify: true,
            ..CodegenOptions::default()
        })
        .with_scoping(ret.scoping)
        .build(&program)
        .code;
    Ok(code)
}

// Minify has no flags of its own beyond the on/off toggle (it's off by default).
// (`--minify` / `--no-minify`.)
#[cfg(feature = "cli")]
crate::cli_config::feature_args!(
    MinifyArgs,
    minify,
    "minify",
    no_minify,
    "no-minify",
    crate::cli_config::NoConfig
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
