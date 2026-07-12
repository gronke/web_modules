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

use crate::module_graph::ModuleImport;
use crate::{Error, Result};

/// Decorator handling for the transform. Defined in the always-compiled [`processors`](super)
/// module so the build `Processors` set can carry it without the `typescript` feature; re-exported
/// here as `web_modules::typescript::Decorators` for the transform that consumes it.
pub use super::Decorators;

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
    /// For minifying JS the compiler didn't produce (vendored), use
    /// [`crate::minify::minify_str`] on the file's content.
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
    /// Decorator lowering: `lit` (default) or `standard`.
    #[arg(long = "typescript-decorators", value_enum, default_value = "lit")]
    pub decorators: DecoratorsArg,
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
    Ok(compile_str_capturing(source, path, options)?.code)
}

/// The emitted JS plus the module specifiers it references.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub(crate) struct TranspileOutput {
    /// The compiled (and, if requested, minified) JavaScript.
    pub code: String,
    /// The module specifiers the emitted code imports — static `import` / `export …
    /// from`, the injected transform-runtime helpers, and dynamic `import()`, all read
    /// from the final AST after any minification (so an import that dead-code
    /// elimination removed is not reported). Captured here, at transform time, so the
    /// build never re-parses or text-scans the output to rediscover them.
    pub imports: Vec<ModuleImport>,
}

/// Like [`compile_str_with`], but also returns the module specifiers the emitted code
/// imports (see [`TranspileOutput`]).
pub(crate) fn compile_str_capturing(
    source: &str,
    path: &Path,
    options: &TranspileOptions,
) -> Result<TranspileOutput> {
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

    // Minify as an output option. With `minify`, compress + mangle in the same pass
    // (no re-parse); otherwise codegen still strips whitespace.
    let code = if !options.minify {
        Codegen::new().build(&program).code
    } else {
        #[cfg(feature = "minify")]
        {
            let ret = oxc_minifier::Minifier::new(oxc_minifier::MinifierOptions::default())
                .minify(&allocator, &mut program);
            Codegen::new()
                .with_options(CodegenOptions {
                    minify: true,
                    ..CodegenOptions::default()
                })
                .with_scoping(ret.scoping)
                .build(&program)
                .code
        }
        #[cfg(not(feature = "minify"))]
        {
            Codegen::new()
                .with_options(CodegenOptions {
                    minify: true,
                    ..CodegenOptions::default()
                })
                .build(&program)
                .code
        }
    };

    // Capture the imports — static `import` / `export … from`, the helpers the transform
    // injected, and dynamic `import()` — from the final AST, after any minification has
    // rewritten it: dead-code elimination can drop an import the transform still carried,
    // and the graph must describe the code that ships. Still structural — the emitted
    // text is never scanned.
    let mut imports = Vec::new();
    crate::module_graph::static_from_program(&program, &mut imports);
    crate::module_graph::dynamic_from_program(&program, &mut imports);

    Ok(TranspileOutput { code, imports })
}

/// Compile every `.ts`/`.tsx`/`.mts` under `src_dir` (skipping `.d.ts`
/// declarations) into a mirrored `.js` under `out_dir`, using the default
/// ([`Decorators::Lit`]) preset. Returns the count.
pub fn compile_directory(src_dir: &Path, out_dir: &Path) -> Result<usize> {
    compile_directory_with(src_dir, out_dir, &TranspileOptions::default())
}

/// Like [`compile_directory`], but with explicit [`TranspileOptions`]. Symlinks are
/// skipped entirely — file or directory; the pipeline's preflight, not this
/// standalone helper, honors [`SymlinkMode`](crate::SymlinkMode).
pub fn compile_directory_with(
    src_dir: &Path,
    out_dir: &Path,
    options: &TranspileOptions,
) -> Result<usize> {
    let mut count = 0;
    for entry in WalkDir::new(src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| !e.path_is_symlink())
    {
        let path = entry.path();
        let rel = path
            .strip_prefix(src_dir)
            .map_err(|e| Error::TypeScript(e.to_string()))?;
        if TypeScriptStep::claims_source(rel).is_none() {
            continue;
        }
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

/// The TypeScript stage as a pipeline step: claims `.ts`/`.tsx`/`.mts` (minus
/// `.d.ts` declarations) for a mirrored `.js`, and emits through
/// [`compile_str_capturing`] so the transform's imports feed the module graph.
pub(crate) struct TypeScriptStep {
    options: TranspileOptions,
}

impl TypeScriptStep {
    pub(crate) fn new(options: TranspileOptions) -> Self {
        Self { options }
    }

    /// The claim rule, shared with [`compile_directory_with`]'s walk: the tiebreak is
    /// the extension's position in dev's probe order (`ts`, `tsx`, `mts`).
    fn claims_source(rel: &Path) -> Option<u8> {
        let name = rel.file_name()?.to_str()?;
        let ext = rel.extension()?.to_str()?;
        let tiebreak = ["ts", "tsx", "mts"]
            .iter()
            .position(|e| ext.eq_ignore_ascii_case(e))? as u8;
        // `.d.ts` declarations emit no JS. An `_`-prefixed name stays an ordinary
        // module: the partial convention belongs to SCSS, where `_x.scss` is an
        // import-only fragment — ES modules have no such concept, and a source tree
        // using `_Base.ts` for abstract classes needs its `.js` emitted like any
        // other (skipping it strands every `import './_Base.js'` in the output).
        if name.to_ascii_lowercase().ends_with(".d.ts") {
            return None;
        }
        Some(tiebreak)
    }
}

impl crate::build::steps::Preflight for TypeScriptStep {
    fn name(&self) -> &'static str {
        "TypeScript transform"
    }

    fn rank(&self) -> crate::build::steps::Rank {
        crate::build::steps::Rank::Transform
    }

    fn claim(&self, rel: &Path) -> Option<crate::build::steps::Claim> {
        let tiebreak = Self::claims_source(rel)?;
        Some(crate::build::steps::Claim {
            out_rel: rel.with_extension("js"),
            tiebreak,
        })
    }
}

impl crate::build::steps::Step for TypeScriptStep {
    fn emit(
        &self,
        _cx: &crate::build::steps::EmitCx<'_>,
        src: &Path,
        _rel: &Path,
        dest: &Path,
    ) -> Result<crate::build::steps::Emitted> {
        let source = read_to_string(src)?;
        let compiled = compile_str_capturing(&source, src, &self.options)?;
        write(dest, compiled.code)?;
        Ok(crate::build::steps::Emitted {
            imports: Some(compiled.imports),
        })
    }
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

    #[cfg(unix)]
    #[test]
    fn compile_directory_skips_symlinks_entirely() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        create_dir_all(src.join("real")).unwrap();
        write(src.join("app.ts"), "export const x: number = 1;").unwrap();
        write(src.join("real/mod.ts"), "export const real = 1;").unwrap();
        write(dir.path().join("outside.ts"), "export const outside = 1;").unwrap();
        std::os::unix::fs::symlink(dir.path().join("outside.ts"), src.join("linked.ts")).unwrap();
        std::os::unix::fs::symlink(src.join("real"), src.join("aliased")).unwrap();

        let n = compile_directory(&src, &out).unwrap();
        assert_eq!(n, 2, "app.ts and real/mod.ts; links contribute nothing");
        assert!(out.join("app.js").exists());
        assert!(out.join("real/mod.js").exists());
        assert!(
            !out.join("linked.js").exists(),
            "a file link is never compiled"
        );
        assert!(
            !out.join("aliased").exists(),
            "a directory link is not descended"
        );
    }

    #[test]
    fn underscore_named_ts_is_an_ordinary_module() {
        // `_Base.ts` is a real module (the SCSS partial convention does not apply to
        // ES modules): its `.js` must emit, or every `import './_Base.js'` in the
        // output is stranded. `.d.ts` stays no-emit.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("src");
        let out = dir.path().join("out");
        create_dir_all(&src).unwrap();
        write(
            src.join("_Base.ts"),
            "export class Base { id: number = 1; }",
        )
        .unwrap();
        write(
            src.join("app.ts"),
            "import { Base } from './_Base.js';\nexport const b: Base = new Base();",
        )
        .unwrap();
        write(src.join("_types.d.ts"), "export type Id = number;").unwrap();

        let n = compile_directory(&src, &out).unwrap();
        assert_eq!(
            n, 2,
            "_Base.ts and app.ts; the declaration contributes nothing"
        );
        assert!(out.join("_Base.js").exists(), "underscore module emitted");
        assert!(out.join("app.js").exists());
        assert!(!out.join("_types.js").exists(), ".d.ts emits no JS");
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

    #[cfg(feature = "minify")]
    #[test]
    fn captured_imports_match_the_minified_output() {
        // Dead-code elimination removes the unreachable dynamic import, so the
        // captured set must not report it — the graph describes the code that ships.
        // Without minification the branch survives and the import is real.
        let src = "if (false) { import(\"gone-package\"); }\nexport const value = 1;";
        let path = Path::new("m.ts");

        let plain = compile_str_capturing(src, path, &TranspileOptions::default()).unwrap();
        assert!(
            plain.imports.iter().any(|i| i.specifier == "gone-package"),
            "unminified output keeps the branch; got {:?}",
            plain.imports
        );

        let minified = compile_str_capturing(
            src,
            path,
            &TranspileOptions {
                minify: true,
                ..TranspileOptions::default()
            },
        )
        .unwrap();
        assert!(
            !minified.code.contains("gone-package"),
            "the minifier eliminates the dead branch; got:\n{}",
            minified.code
        );
        assert!(
            !minified
                .imports
                .iter()
                .any(|i| i.specifier == "gone-package"),
            "captured imports must match the emitted code; got {:?}",
            minified.imports
        );
    }

    #[cfg(feature = "minify")]
    #[test]
    fn minified_capture_still_reports_injected_helpers() {
        // The decorator helper is used by the lowered output, so it survives
        // minification — and must still be captured for vendoring.
        let src = "import { LitElement } from 'lit';\n\
                   import { customElement } from 'lit/decorators.js';\n\
                   @customElement('x-el')\n\
                   export class XEl extends LitElement {}";
        let out = compile_str_capturing(
            src,
            Path::new("x-el.ts"),
            &TranspileOptions {
                minify: true,
                ..TranspileOptions::default()
            },
        )
        .unwrap();
        let specs: Vec<&str> = out.imports.iter().map(|i| i.specifier.as_str()).collect();
        assert!(
            specs.contains(&"@oxc-project/runtime/helpers/decorate"),
            "helper import captured post-minify; got {specs:?}"
        );
        assert!(specs.contains(&"lit"), "used import kept; got {specs:?}");
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
