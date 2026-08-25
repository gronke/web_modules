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
use std::path::{Path, PathBuf};

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
pub use super::{ClassFields, Decorators};

/// Knobs for [`compile_str_with`] / [`compile_directory_with`]. `Default` is the
/// Lit preset, so the zero-config [`compile_str`] / [`compile_directory`] keep the
/// behaviour Lit projects rely on.
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct TranspileOptions {
    /// How decorators are lowered. Defaults to [`Decorators::Lit`].
    pub decorators: Decorators,
    /// How class fields are emitted. Defaults to [`ClassFields::Assign`], which with the
    /// default decorators is the Lit preset; a dependency built against ES 2022 or above
    /// needs [`ClassFields::Define`] to be emitted the way it builds.
    pub class_fields: ClassFields,
    /// Rewrite a relative import's TypeScript extension to the one emitted beside it —
    /// `./util.ts` becomes `./util.js`, `.mts` becomes `.mjs`. This is `tsconfig`'s
    /// `rewriteRelativeImportExtensions`, and a package whose sources name `.ts` files
    /// needs it for its output to resolve. Defaults to `false`, as TypeScript does.
    pub rewrite_import_extensions: bool,
    /// Emit minified JS (an *output* option, like SCSS's compressed style). With the
    /// `minify` feature this runs the full `oxc_minifier` (compress + mangle) in the
    /// same pass; without it, codegen still strips whitespace. Defaults to `false`.
    /// For minifying JS the compiler didn't produce (vendored), use
    /// [`crate::minify::minify_str`] on the file's content.
    pub minify: bool,
    /// Emit a source map (an *output* option like `minify`), from the same single
    /// compile pass. The sources ship inside the map (`sourcesContent`), so it works
    /// although the `.ts` files themselves are not published. Where the map lands
    /// follows the API shape: the string API ([`compile_str_with`]) appends it inline
    /// as a `data:` URL comment, while everything that writes files — the build
    /// pipeline, [`compile_directory_with`] — writes a `<file>.map` sidecar and links
    /// it by file name. Defaults to `false`.
    pub source_map: bool,
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
            class_fields: ClassFields::Define,
            rewrite_import_extensions: false,
            minify: false,
            source_map: false,
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

// Sourcemaps have no flags of their own beyond the on/off toggle (off by default, so
// embedded dists stay lean): `build` writes `<file>.map` sidecars, `dev` serves maps
// inline as `data:` URLs. (`--sourcemap` / `--no-sourcemap`.)
#[cfg(feature = "cli")]
crate::cli_config::feature_args!(
    SourcemapArgs,
    sourcemap,
    "sourcemap",
    no_sourcemap,
    "no-sourcemap",
    crate::cli_config::NoConfig
);

/// Build oxc transform options from our [`TranspileOptions`]. Decorator lowering and
/// class-field semantics are set independently: the default pairing — legacy decorators
/// with fields *assigned* rather than *defined* — is the Lit preset.
fn transform_options(opts: &TranspileOptions) -> TransformOptions {
    let mut options = TransformOptions::default();
    if opts.decorators == Decorators::Lit {
        options.decorator.legacy = true;
    }
    if opts.class_fields == ClassFields::Assign {
        options.typescript.remove_class_fields_without_initializer = true;
        options.assumptions.set_public_class_fields = true;
    }
    if opts.rewrite_import_extensions {
        options.typescript.rewrite_import_extensions =
            Some(oxc_transformer::RewriteExtensionsMode::Rewrite);
    }
    options
}

/// Compile a single TS/JS source string to browser JS using the default
/// ([`Decorators::Lit`]) preset. `path` informs the source type
/// (`.ts`/`.tsx`/`.mts`/`.js`) and diagnostics; it is not read from disk.
pub fn compile_str(source: &str, path: &Path) -> Result<String> {
    compile_str_with(source, path, &TranspileOptions::default())
}

/// Like [`compile_str`], but with explicit [`TranspileOptions`]. With
/// [`TranspileOptions::source_map`] set, the map is appended inline as a `data:` URL
/// comment — a returned string has no place for a sidecar; the build pipeline and
/// [`compile_directory_with`] write a `<file>.map` sidecar instead.
pub fn compile_str_with(source: &str, path: &Path, options: &TranspileOptions) -> Result<String> {
    let out = compile_str_capturing(source, path, options, None)?;
    let mut code = out.code;
    if let Some(map) = out.map {
        append_source_map_comment(&mut code, &map.data_url);
    }
    Ok(code)
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
    /// The source map, when [`TranspileOptions::source_map`] asked for one.
    pub map: Option<SourceMapArtifact>,
}

/// A serialized source map, in both shapes an emitter needs: `json` for a `<file>.map`
/// sidecar, `data_url` for inlining (the dev server, the string API). Both are captured
/// from the one codegen pass, because the map object does not outlive the compile's
/// allocator.
#[derive(Debug, Clone)]
pub(crate) struct SourceMapArtifact {
    pub json: String,
    pub data_url: String,
}

/// The one place [`CodegenOptions`] derive from output policy, so every JS-emitting
/// pass in the crate prints under the same rules — whitespace stripping and the map's
/// source label today; whatever output policy comes next joins here.
pub(crate) fn codegen_options(minify: bool, source_map_path: Option<PathBuf>) -> CodegenOptions {
    CodegenOptions {
        minify,
        source_map_path,
        ..CodegenOptions::default()
    }
}

/// Append a `sourceMappingURL` footer to emitted JS, on its own final line.
pub(crate) fn append_source_map_comment(code: &mut String, url: &str) {
    if !code.ends_with('\n') {
        code.push('\n');
    }
    code.push_str("//# sourceMappingURL=");
    code.push_str(url);
    code.push('\n');
}

/// Write emitted JS to `dest`; with a map, link it by bare file name (`app.js` →
/// `app.js.map`, a same-directory relative URL that survives any mount or subpath)
/// and write the sidecar beside it.
pub(crate) fn write_js_output(
    dest: &Path,
    mut code: String,
    map_json: Option<String>,
) -> Result<()> {
    let Some(map_json) = map_json else {
        write(dest, code)?;
        return Ok(());
    };
    let name = dest
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::TypeScript(format!("{}: no usable file name", dest.display())))?;
    append_source_map_comment(&mut code, &format!("{name}.map"));
    write(dest, code)?;
    let mut map_path = dest.as_os_str().to_owned();
    map_path.push(".map");
    write(PathBuf::from(map_path), map_json)?;
    Ok(())
}

/// Like [`compile_str_with`], but also returns the module specifiers the emitted code
/// imports (see [`TranspileOutput`]) and, when asked, the raw source map. `map_label`
/// names the map's source (pass the root-relative path); diagnostics keep using `path`,
/// so error messages stay clickable while absolute build paths never leak into a
/// published map. `None` falls back to `path`.
pub(crate) fn compile_str_capturing(
    source: &str,
    path: &Path,
    options: &TranspileOptions,
    map_label: Option<&Path>,
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
    // (no re-parse); otherwise codegen still strips whitespace. A requested source map
    // is emitted from this same single pass, so it never needs composing with another.
    let map_source = options
        .source_map
        .then(|| map_label.unwrap_or(path).to_path_buf());
    let ret = if !options.minify {
        Codegen::new()
            .with_options(codegen_options(false, map_source))
            .build(&program)
    } else {
        #[cfg(feature = "minify")]
        {
            let ret = oxc_minifier::Minifier::new(oxc_minifier::MinifierOptions::default())
                .minify(&allocator, &mut program);
            Codegen::new()
                .with_options(codegen_options(true, map_source))
                .with_scoping(ret.scoping)
                .build(&program)
        }
        #[cfg(not(feature = "minify"))]
        {
            Codegen::new()
                .with_options(codegen_options(true, map_source))
                .build(&program)
        }
    };
    let code = ret.code;
    // Both serializations are captured here: the map borrows the compile's allocator
    // and cannot leave this function.
    let map = ret.map.map(|map| SourceMapArtifact {
        json: map.to_json_string(),
        data_url: map.to_data_url(),
    });

    // Capture the imports — static `import` / `export … from`, the helpers the transform
    // injected, and dynamic `import()` — from the final AST, after any minification has
    // rewritten it: dead-code elimination can drop an import the transform still carried,
    // and the graph must describe the code that ships. Still structural — the emitted
    // text is never scanned.
    let mut imports = Vec::new();
    crate::module_graph::static_from_program(&program, &mut imports);
    crate::module_graph::dynamic_from_program(&program, &mut imports);

    Ok(TranspileOutput { code, imports, map })
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
        let compiled = compile_str_capturing(&source, path, options, Some(rel))?;
        write_js_output(&out, compiled.code, compiled.map.map(|m| m.json))?;
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
        rel: &Path,
        dest: &Path,
    ) -> Result<crate::build::steps::Emitted> {
        let source = read_to_string(src)?;
        let compiled = compile_str_capturing(&source, src, &self.options, Some(rel))?;
        write_js_output(dest, compiled.code, compiled.map.map(|m| m.json))?;
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
    fn source_map_is_inlined_through_the_string_api() {
        let src = "export const x: number = 1;\n";
        let opts = TranspileOptions {
            source_map: true,
            ..TranspileOptions::default()
        };
        let code = compile_str_with(src, Path::new("x.ts"), &opts).unwrap();
        assert!(
            code.contains("//# sourceMappingURL=data:application/json;charset=utf-8;base64,"),
            "a returned string has no place for a sidecar, so the map inlines; got:\n{code}"
        );
        let plain = compile_str(src, Path::new("x.ts")).unwrap();
        assert!(!plain.contains("sourceMappingURL"), "off by default");
    }

    #[test]
    fn source_map_labels_sources_and_embeds_content() {
        let src = "export const answer: number = 6 * 7;\n";
        let opts = TranspileOptions {
            source_map: true,
            ..TranspileOptions::default()
        };
        // Diagnostics keep the (absolute) source path; the map takes the root-relative
        // label, so machine paths never leak into a published map.
        let out = compile_str_capturing(
            src,
            Path::new("/abs/build/app.ts"),
            &opts,
            Some(Path::new("app.ts")),
        )
        .unwrap();
        let map = out.map.expect("map requested");
        let json: serde_json::Value = serde_json::from_str(&map.json).unwrap();
        assert_eq!(json["sources"], serde_json::json!(["app.ts"]));
        assert!(
            json["sourcesContent"][0]
                .as_str()
                .unwrap()
                .contains("6 * 7"),
            "the TS source ships inside the map; got {json}"
        );
        assert!(!json["mappings"].as_str().unwrap().is_empty());
        assert!(map
            .data_url
            .starts_with("data:application/json;charset=utf-8;base64,"));

        let off =
            compile_str_capturing(src, Path::new("app.ts"), &Default::default(), None).unwrap();
        assert!(off.map.is_none(), "no map unless asked");
    }

    #[test]
    fn captured_imports_match_the_minified_output() {
        // Dead-code elimination removes the unreachable dynamic import, so the
        // captured set must not report it — the graph describes the code that ships.
        // Without minification the branch survives and the import is real.
        let src = "if (false) { import(\"gone-package\"); }\nexport const value = 1;";
        let path = Path::new("m.ts");

        let plain = compile_str_capturing(src, path, &TranspileOptions::default(), None).unwrap();
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
            None,
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
            None,
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
