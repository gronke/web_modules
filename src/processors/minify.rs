//! JavaScript minification via [`oxc_minifier`] + minified codegen.
//!
//! Compresses the AST (constant folding, dead-code elimination) and prints
//! whitespace-free output. Intended for release/embedded builds over the `.js`
//! emitted by [`super::typescript`].

use std::fs::{read_to_string, write};
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_minifier::{Minifier, MinifierOptions};
use oxc_parser::Parser;
use oxc_span::SourceType;
use walkdir::WalkDir;

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

/// Minify every `.js` under `dir` **in place**, returning the count. Mirrors the
/// [`super::typescript::compile_directory`] convention for the emitted-JS tree of a
/// release/embedded build. Note this rewrites *every* `.js` it finds — point it at
/// the subtree you want minified (e.g. exclude an already-minified vendored tree).
pub fn minify_directory(dir: &Path) -> Result<usize> {
    let mut count = 0;
    for entry in WalkDir::new(dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "js"))
    {
        let path = entry.path();
        let source = read_to_string(path)?;
        let minified = minify_str(&source, path)?;
        write(path, minified)?;
        count += 1;
    }
    Ok(count)
}

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

    #[test]
    fn minify_directory_processes_js_in_place() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("a.js");
        write(&f, "export const sum = 1 + 2;\n").unwrap();
        let n = minify_directory(dir.path()).unwrap();
        assert_eq!(n, 1);
        assert!(read_to_string(&f).unwrap().contains('3'));
    }
}
