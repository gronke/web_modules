//! Emit TypeScript declaration files (`.d.ts`) via [`oxc_isolated_declarations`].
//!
//! Implements TypeScript's `isolatedDeclarations`: declarations are produced
//! per-file with no type-checking, so module boundaries must carry explicit
//! types. Useful when a crate's vendored/authored TS should publish typings.

use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::Codegen;
use oxc_isolated_declarations::IsolatedDeclarations;
use oxc_parser::Parser;
use oxc_span::SourceType;

use crate::{Error, Result};

/// Generate the `.d.ts` text for a single TS source. `path` informs the source
/// type and diagnostics.
pub fn emit_dts(source: &str, path: &Path) -> Result<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_default();

    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.has_errors() {
        return Err(Error::Dts(format!(
            "parse errors in {}: {:?}",
            path.display(),
            &parsed.diagnostics[..]
        )));
    }

    let ret = IsolatedDeclarations::new(&allocator, Default::default()).build(&parsed.program);
    Ok(Codegen::new().build(&ret.program).code)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn emits_declarations_without_bodies() {
        let dts = emit_dts(
            "export function add(a: number, b: number): number { return a + b; }",
            Path::new("m.ts"),
        )
        .unwrap();
        assert!(dts.contains("declare function add"), "got: {dts}");
        assert!(!dts.contains("return a + b"), "implementation stripped");
    }
}
