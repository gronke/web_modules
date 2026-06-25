//! TypeScript / modern JS → browser JS via [oxc].
//!
//! A **type-stripping + decorator** transform only, no ES downleveling and no
//! bundling. Bare import specifiers are left intact for the browser's import map.
//! Legacy (experimental) decorators are enabled with the class-field semantics
//! Lit requires, i.e. the `experimentalDecorators: true` + `useDefineForClassFields:
//! false` combination, so `@customElement`/`@property`/`@state` work.
//!
//! oxc does **not** type-check; it strips types assuming valid input. Run
//! `tsc --noEmit` separately (e.g. in CI) for type safety.
//!
//! [oxc]: https://oxc.rs

use std::fs::{create_dir_all, read_to_string, write};
use std::path::Path;

use oxc_allocator::Allocator;
use oxc_codegen::{Codegen, CodegenOptions};
use oxc_parser::Parser;
use oxc_semantic::SemanticBuilder;
use oxc_span::SourceType;
use oxc_transformer::{TransformOptions, Transformer};
use walkdir::WalkDir;

use crate::{Error, Result};

/// Decorator handling for the transform.
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

/// Knobs for [`compile_str_with`] / [`compile_directory_with`]. `Default` is the
/// Lit preset, so the zero-config [`compile_str`] / [`compile_directory`] keep the
/// behaviour Lit projects rely on.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct TranspileOptions {
    /// How decorators are lowered. Defaults to [`Decorators::Lit`].
    pub decorators: Decorators,
    /// Emit minified JS (an *output* option, like SCSS's compressed style). With the
    /// `minify` feature this runs the full `oxc_minifier` (compress + mangle) in the
    /// same pass; without it, codegen still strips whitespace. Defaults to `false`.
    /// For minifying JS the compiler didn't produce (vendored), use [`crate::minify`].
    pub minify: bool,
}

impl TranspileOptions {
    /// The plain (non-Lit) preset: [`Decorators::Standard`], standard decorators and
    /// oxc's default *define*-semantics class fields. Use this for codebases that aren't
    /// using Lit's decorator-free `static properties` pattern, e.g. ones that rely on a
    /// subclass `static x = …` field shadowing an inherited getter (which the Lit preset's
    /// assignment semantics would instead throw on). The inverse of the [`Default`] (Lit)
    /// preset; `minify` stays off.
    pub fn standard() -> Self {
        Self {
            decorators: Decorators::Standard,
            minify: false,
        }
    }
}

/// `--typescript-decorators` value: the CLI mirror of [`Decorators`] (which is
/// `#[non_exhaustive]` and not a `clap::ValueEnum`).
#[cfg(feature = "cli")]
#[derive(clap::ValueEnum, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum DecoratorsArg {
    /// Legacy decorators with Lit's class-field semantics (the default).
    #[default]
    Lit,
    /// Plain oxc defaults, for non-Lit / decorator-free sources.
    Standard,
}

#[cfg(feature = "cli")]
impl From<DecoratorsArg> for Decorators {
    fn from(value: DecoratorsArg) -> Self {
        match value {
            DecoratorsArg::Lit => Decorators::Lit,
            DecoratorsArg::Standard => Decorators::Standard,
        }
    }
}

/// Feature-specific `--typescript-*` flags, paired with the `--typescript` /
/// `--no-typescript` toggle in [`TypescriptArgs`].
#[cfg(feature = "cli")]
#[derive(clap::Args, Clone, Debug, Default)]
pub struct TypescriptConfig {
    /// Decorator lowering: `lit` or `standard`. Left unset (no clap default) so an explicit
    /// `--typescript-decorators` is distinguishable from a `package.json` block value; the
    /// built-in default (`lit`) is applied during config resolution.
    #[arg(long = "typescript-decorators", value_enum)]
    pub decorators: Option<DecoratorsArg>,
}

#[cfg(feature = "cli")]
crate::cli_config::feature_args!(
    TypescriptArgs,
    typescript,
    "typescript",
    no_typescript,
    "no-typescript",
    TypescriptConfig
);

/// Build oxc transform options from our [`TranspileOptions`]. The Lit preset sets
/// legacy decorators plus class fields *assigned* rather than *defined* (the
/// `useDefineForClassFields: false` equivalent).
fn transform_options(opts: &TranspileOptions) -> TransformOptions {
    let mut options = TransformOptions::default();
    if opts.decorators == Decorators::Lit {
        options.decorator.legacy = true;
        options.typescript.remove_class_fields_without_initializer = true;
        options.assumptions.set_public_class_fields = true;
    }
    options
}

/// Compile a single TS/JS source string to browser JS using the default
/// ([`Decorators::Lit`]) preset. `path` informs the source type
/// (`.ts`/`.tsx`/`.mts`/`.js`) and diagnostics; it is not read from disk.
pub fn compile_str(source: &str, path: &Path) -> Result<String> {
    compile_str_with(source, path, &TranspileOptions::default())
}

/// Like [`compile_str`], but with explicit [`TranspileOptions`].
pub fn compile_str_with(source: &str, path: &Path, options: &TranspileOptions) -> Result<String> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_default();

    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.has_errors() {
        return Err(Error::TypeScript(render_errors(
            "parse",
            path,
            &parsed.diagnostics[..],
        )));
    }
    let mut program = parsed.program;

    // `with_enum_eval(true)` lets the transformer evaluate TS `enum` member values;
    // oxc panics when lowering an `enum` if the scoping wasn't built with it.
    let semantic = SemanticBuilder::new().with_enum_eval(true).build(&program);
    if semantic.diagnostics.has_errors() {
        return Err(Error::TypeScript(render_errors(
            "semantic",
            path,
            &semantic.diagnostics[..],
        )));
    }
    let scoping = semantic.semantic.into_scoping();

    let oxc_options = transform_options(options);
    let transformed =
        Transformer::new(&allocator, path, &oxc_options).build_with_scoping(scoping, &mut program);
    if transformed.diagnostics.has_errors() {
        return Err(Error::TypeScript(render_errors(
            "transform",
            path,
            &transformed.diagnostics[..],
        )));
    }

    if !options.minify {
        return Ok(Codegen::new().build(&program).code);
    }
    // Minify as an output option. With `minify`, compress + mangle in the same pass
    // (no re-parse); otherwise codegen still strips whitespace.
    #[cfg(feature = "minify")]
    {
        let ret = oxc_minifier::Minifier::new(oxc_minifier::MinifierOptions::default())
            .minify(&allocator, &mut program);
        Ok(Codegen::new()
            .with_options(CodegenOptions {
                minify: true,
                ..CodegenOptions::default()
            })
            .with_scoping(ret.scoping)
            .build(&program)
            .code)
    }
    #[cfg(not(feature = "minify"))]
    {
        Ok(Codegen::new()
            .with_options(CodegenOptions {
                minify: true,
                ..CodegenOptions::default()
            })
            .build(&program)
            .code)
    }
}

/// Compile every `.ts`/`.tsx`/`.mts` under `src_dir` (skipping `_` partials and
/// `.d.ts` declarations) into a mirrored `.js` under `out_dir`, using the default
/// ([`Decorators::Lit`]) preset. Returns the count.
pub fn compile_directory(src_dir: &Path, out_dir: &Path) -> Result<usize> {
    compile_directory_with(src_dir, out_dir, &TranspileOptions::default())
}

/// Like [`compile_directory`], but with explicit [`TranspileOptions`].
pub fn compile_directory_with(
    src_dir: &Path,
    out_dir: &Path,
    options: &TranspileOptions,
) -> Result<usize> {
    let mut count = 0;
    for entry in WalkDir::new(src_dir)
        .follow_links(true)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        let name = match path.file_name().and_then(|n| n.to_str()) {
            Some(n) => n,
            None => continue,
        };
        let is_ts = path.extension().and_then(|x| x.to_str()).is_some_and(|x| {
            ["ts", "tsx", "mts"]
                .iter()
                .any(|e| x.eq_ignore_ascii_case(e))
        });
        // `.d.ts` declarations emit no JS; skip them (case-insensitively, to match `is_ts`).
        let is_decl = name.to_ascii_lowercase().ends_with(".d.ts");
        if !is_ts || name.starts_with('_') || is_decl {
            continue;
        }
        let rel = path
            .strip_prefix(src_dir)
            .map_err(|e| Error::TypeScript(e.to_string()))?;
        let out = out_dir.join(rel).with_extension("js");
        if let Some(parent) = out.parent() {
            create_dir_all(parent)?;
        }
        let source = read_to_string(path)?;
        let js = compile_str_with(&source, path, options)?;
        write(&out, js)?;
        count += 1;
    }
    Ok(count)
}

/// Format an oxc diagnostic slice into a multi-line error message. Generic over
/// the diagnostic type so we don't depend on `oxc_diagnostics` directly.
fn render_errors<E: std::fmt::Debug>(stage: &str, path: &Path, errors: &[E]) -> String {
    let body = errors
        .iter()
        .map(|e| format!("{e:?}"))
        .collect::<Vec<_>>()
        .join("\n");
    format!("{stage} error(s) in {}:\n{body}", path.display())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_types_keeps_used_bare_imports() {
        // A *used* bare import must survive (it's what the import map resolves);
        // unused imports are elided by the TS transform, which is correct.
        let src = "import { LitElement, html } from 'lit';\n\
                   export class Foo extends LitElement {\n\
                       render() { return html`<p>hi</p>`; }\n\
                       greet(name: string): string { return `hi ${name}`; }\n\
                   }";
        let js = compile_str(src, Path::new("foo.ts")).unwrap();
        assert!(
            js.contains("\"lit\"") || js.contains("'lit'"),
            "used bare import retained for the import map; got:\n{js}"
        );
        assert!(!js.contains(": string"), "type annotations stripped");
    }

    #[test]
    fn transforms_lit_decorators() {
        let src = "import { LitElement } from 'lit';\n\
                   import { customElement, property } from 'lit/decorators.js';\n\
                   @customElement('x-el')\n\
                   export class XEl extends LitElement {\n\
                       @property({ type: Number }) count: number = 0;\n\
                   }";
        let js = compile_str(src, Path::new("x-el.ts")).unwrap();
        // Decorator + decorated field survive the transform (legacy decorators).
        assert!(js.contains("customElement"));
        assert!(js.contains("count"));
        assert!(!js.contains(": number"), "type annotation stripped");
    }

    #[test]
    fn lit_and_standard_presets_diverge_on_class_fields() {
        // Lit declares reactive props via `static properties`; an *uninitialized* class
        // field of the same name would shadow the generated accessor, so the Lit preset
        // removes it (`remove_class_fields_without_initializer`). The Standard preset keeps
        // it (plain oxc). Same source → different output — the reason the preset exists.
        let src = "export class Foo {\n  count: number;\n  constructor() {}\n}";
        let path = Path::new("foo.ts");

        let lit = compile_str(src, path).unwrap(); // default = Lit
        let standard = compile_str_with(src, path, &TranspileOptions::standard()).unwrap();

        assert_ne!(lit, standard, "the presets must diverge on class fields");
        assert!(
            !lit.contains("count"),
            "Lit preset drops the bare field; got:\n{lit}"
        );
        assert!(
            standard.contains("count"),
            "Standard preset keeps the field; got:\n{standard}"
        );
    }

    #[test]
    fn lowers_typescript_enum() {
        // Regression: oxc panics lowering an `enum` unless the scoping was built
        // with `SemanticBuilder::with_enum_eval(true)` (found test-building a real app).
        let src = "export enum Dir { Asc, Desc }\nexport const d: Dir = Dir.Asc;";
        let js = compile_str(src, Path::new("e.ts")).unwrap();
        assert!(js.contains("Dir"));
        assert!(!js.contains(": Dir"), "type annotation stripped");
        assert!(
            !js.contains("enum "),
            "enum keyword lowered away; got:\n{js}"
        );
    }
}
