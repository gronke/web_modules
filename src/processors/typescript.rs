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
pub use super::{ClassFields, Comments, Decorators};

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
    /// Comment policy for the emitted code (an *output* option; see
    /// [`Comments`]). Defaults to [`Comments::Keep`]. Through the string API a
    /// [`Comments::Collect`] falls back to keeping legal comments inline — a returned
    /// string has no place for the sidecar.
    pub comments: Comments,
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
            comments: Comments::Keep,
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

/// How already-emitted JavaScript is rewritten: the same single
/// parse→codegen pass the TypeScript step compiles through, minus the
/// transform. The build derives it from the output policy via
/// [`Output::js_rewrite`](crate::build::Output); consumers use it through
/// [`rewrite_str`].
#[derive(Clone, Copy, Debug, Default)]
#[non_exhaustive]
pub struct RewriteOptions {
    /// Compress + mangle via `oxc_minifier` (whitespace-only stripping without the
    /// `minify` feature), then minified codegen.
    pub minify: bool,
    /// Emit a source map for the rewritten file; its immediate input is the source.
    pub source_map: bool,
    /// Comment policy for the rewritten code.
    pub comments: Comments,
}

/// Rewrite plain JavaScript through one parse→\[minify\]→codegen pass, capturing the
/// imports from the final AST like [`compile_str_capturing`] does (dead-code
/// elimination may drop one), a source map, and any collected legal comments. The
/// oxc `Transformer` never runs here: the Lit preset's class-field assumptions must
/// not change the semantics of hand-written JS. `map_label` names the map's source;
/// `path` informs the source type and diagnostics; `legal_file` names the sidecar a
/// [`Comments::Collect`] caller will write.
pub(crate) fn rewrite_js_capturing(
    source: &str,
    path: &Path,
    map_label: &Path,
    options: RewriteOptions,
    legal_file: Option<&str>,
) -> Result<TranspileOutput> {
    let allocator = Allocator::default();
    let source_type = SourceType::from_path(path).unwrap_or_default();
    let parsed = Parser::new(&allocator, source, source_type).parse();
    if parsed.diagnostics.has_errors() {
        return Err(Error::Minify(render_errors(
            "parse",
            path,
            &parsed.diagnostics[..],
        )));
    }
    #[allow(unused_mut)]
    let mut program = parsed.program;

    let map_source = options.source_map.then(|| map_label.to_path_buf());
    let ret = if options.minify {
        #[cfg(feature = "minify")]
        {
            let minified = oxc_minifier::Minifier::new(oxc_minifier::MinifierOptions::default())
                .minify(&allocator, &mut program);
            Codegen::new()
                .with_options(codegen_options(
                    true,
                    map_source,
                    options.comments,
                    legal_file,
                ))
                .with_scoping(minified.scoping)
                .build(&program)
        }
        #[cfg(not(feature = "minify"))]
        {
            Codegen::new()
                .with_options(codegen_options(
                    true,
                    map_source,
                    options.comments,
                    legal_file,
                ))
                .build(&program)
        }
    } else {
        Codegen::new()
            .with_options(codegen_options(
                false,
                map_source,
                options.comments,
                legal_file,
            ))
            .build(&program)
    };
    let code = ret.code;
    let map = ret.map.map(|map| SourceMapArtifact {
        json: map.to_json_string(),
        data_url: map.to_data_url(),
    });
    let legal = collect_legal(&ret.legal_comments, source);

    let mut imports = Vec::new();
    crate::module_graph::static_from_program(&program, &mut imports);
    crate::module_graph::dynamic_from_program(&program, &mut imports);

    Ok(TranspileOutput {
        code,
        imports,
        map,
        legal,
    })
}

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
    let out = compile_str_capturing(source, path, options, None, None)?;
    let mut code = out.code;
    if let Some(map) = out.map {
        append_source_map_comment(&mut code, &map.data_url);
    }
    Ok(code)
}

/// Rewrite plain JavaScript under a [`RewriteOptions`] policy, without the
/// oxc `Transformer` (unlike [`compile_str_with`], whose Lit-preset
/// transform may alter hand-written JS semantics). A requested source map is
/// appended inline as a `data:` URL; [`Comments::Collect`] keeps legal
/// comments inline. `path` informs source type and diagnostics; it is not
/// read from disk.
pub fn rewrite_str(source: &str, path: &Path, options: RewriteOptions) -> Result<String> {
    let out = rewrite_js_capturing(source, path, path, options, None)?;
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
    /// The collected legal comments (verbatim, oxc-deduplicated, blank-line joined) a
    /// [`Comments::Collect`] emitter writes as the `<output>.LEGAL.txt` sidecar.
    /// `None` when nothing was collected — an empty set writes no sidecar.
    pub legal: Option<String>,
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
/// pass in the crate prints under the same rules: whitespace stripping, the comment
/// policy, and the map's source label. `legal_file` names the `<output>.LEGAL.txt`
/// sidecar a [`Comments::Collect`] emitter will write; a caller with no place for a
/// sidecar passes `None` and legal comments stay inline.
pub(crate) fn codegen_options(
    minify: bool,
    source_map_path: Option<PathBuf>,
    comments: Comments,
    legal_file: Option<&str>,
) -> CodegenOptions {
    use oxc_codegen::{CommentOptions, LegalComment};
    // `Strip` deliberately deviates from oxc's own minify preset, which drops legal
    // comments: license text must ship with the code, inline or collected.
    let comments = match comments {
        Comments::Keep => CommentOptions::default(),
        Comments::Strip => CommentOptions {
            normal: false,
            jsdoc: false,
            annotation: false,
            legal: LegalComment::Inline,
        },
        Comments::Collect => CommentOptions {
            normal: false,
            jsdoc: false,
            annotation: false,
            legal: match legal_file {
                Some(name) => LegalComment::Linked(name.to_string()),
                None => LegalComment::Inline,
            },
        },
        Comments::None => CommentOptions::disabled(),
    };
    CodegenOptions {
        minify,
        comments,
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

/// The `<output>.LEGAL.txt` sidecar name for an output path — the one string the
/// codegen's pointer comment and the writer must agree on.
pub(crate) fn legal_file_name(dest: &Path) -> Option<String> {
    dest.file_name()
        .and_then(|n| n.to_str())
        .map(|name| format!("{name}.LEGAL.txt"))
}

/// Write emitted JS to `dest`, plus its sidecars: with a map, link it by bare file
/// name (`app.js` → `app.js.map`, a same-directory relative URL that survives any
/// mount or subpath) and write the JSON beside it; with collected legal comments,
/// write `<dest>.LEGAL.txt`.
pub(crate) fn write_js_output(
    dest: &Path,
    mut code: String,
    map_json: Option<String>,
    legal: Option<String>,
) -> Result<()> {
    if let Some(text) = legal {
        let mut legal_path = dest.as_os_str().to_owned();
        legal_path.push(".LEGAL.txt");
        write(PathBuf::from(legal_path), text)?;
    }
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
/// imports (see [`TranspileOutput`]), the raw source map, and any collected legal
/// comments. `map_label` names the map's source (pass the root-relative path);
/// diagnostics keep using `path`, so error messages stay clickable while absolute
/// build paths never leak into a published map (`None` falls back to `path`).
/// `legal_file` is the `<output>.LEGAL.txt` sidecar name a [`Comments::Collect`]
/// caller will write; `None` keeps legal comments inline.
pub(crate) fn compile_str_capturing(
    source: &str,
    path: &Path,
    options: &TranspileOptions,
    map_label: Option<&Path>,
    legal_file: Option<&str>,
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
            .with_options(codegen_options(
                false,
                map_source,
                options.comments,
                legal_file,
            ))
            .build(&program)
    } else {
        #[cfg(feature = "minify")]
        {
            let ret = oxc_minifier::Minifier::new(oxc_minifier::MinifierOptions::default())
                .minify(&allocator, &mut program);
            Codegen::new()
                .with_options(codegen_options(
                    true,
                    map_source,
                    options.comments,
                    legal_file,
                ))
                .with_scoping(ret.scoping)
                .build(&program)
        }
        #[cfg(not(feature = "minify"))]
        {
            Codegen::new()
                .with_options(codegen_options(
                    true,
                    map_source,
                    options.comments,
                    legal_file,
                ))
                .build(&program)
        }
    };
    let code = ret.code;
    // Both serializations are captured here: the map borrows the compile's allocator
    // and cannot leave this function. Same for the collected legal comments.
    let map = ret.map.map(|map| SourceMapArtifact {
        json: map.to_json_string(),
        data_url: map.to_data_url(),
    });
    let legal = collect_legal(&ret.legal_comments, source);

    // Capture the imports — static `import` / `export … from`, the helpers the transform
    // injected, and dynamic `import()` — from the final AST, after any minification has
    // rewritten it: dead-code elimination can drop an import the transform still carried,
    // and the graph must describe the code that ships. Still structural — the emitted
    // text is never scanned.
    let mut imports = Vec::new();
    crate::module_graph::static_from_program(&program, &mut imports);
    crate::module_graph::dynamic_from_program(&program, &mut imports);

    Ok(TranspileOutput {
        code,
        imports,
        map,
        legal,
    })
}

/// The `<output>.LEGAL.txt` sidecar body: the collected legal comments verbatim
/// (oxc returns them deduplicated), blank-line separated. `None` for an empty set —
/// no sidecar is written for a file without legal comments.
fn collect_legal(comments: &[oxc_ast::Comment], source: &str) -> Option<String> {
    if comments.is_empty() {
        return None;
    }
    Some(
        comments
            .iter()
            .map(|comment| comment.span.source_text(source))
            .collect::<Vec<_>>()
            .join("\n\n"),
    )
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
        let legal_file = (options.comments == Comments::Collect)
            .then(|| legal_file_name(&out))
            .flatten();
        let compiled =
            compile_str_capturing(&source, path, options, Some(rel), legal_file.as_deref())?;
        write_js_output(
            &out,
            compiled.code,
            compiled.map.map(|m| m.json),
            compiled.legal,
        )?;
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
        let legal_file = (self.options.comments == Comments::Collect)
            .then(|| legal_file_name(dest))
            .flatten();
        let compiled = compile_str_capturing(
            &source,
            src,
            &self.options,
            Some(rel),
            legal_file.as_deref(),
        )?;
        write_js_output(
            dest,
            compiled.code,
            compiled.map.map(|m| m.json),
            compiled.legal,
        )?;
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
    fn rewrite_str_strips_comments_but_keeps_legal_inline() {
        let src = "// build header\n/*! (c) legal */\nexport const answer = 40 + 2;\n";
        let options = RewriteOptions {
            minify: true,
            source_map: false,
            comments: Comments::Strip,
        };
        let js = rewrite_str(src, Path::new("gen.js"), options).unwrap();
        assert!(!js.contains("build header"), "normal comment stripped");
        assert!(js.contains("legal"), "legal comment stays inline");
        assert!(!js.contains("sourceMappingURL"), "no map unless asked");
    }

    #[test]
    fn rewrite_str_never_runs_the_transformer() {
        // The Lit-preset transform would rewrite the static class field.
        let src = "export class A { static properties = { x: {} }; #secret = 1; }\n";
        let js = rewrite_str(src, Path::new("plain.js"), RewriteOptions::default()).unwrap();
        assert!(
            js.contains("static properties = "),
            "class field survives untransformed; got:\n{js}"
        );
        assert!(js.contains("#secret"), "private field survives; got:\n{js}");
    }

    #[test]
    fn rewrite_str_minify_and_inline_map_are_one_pass() {
        let src = "export const value = 1 + 1;\n";
        let options = RewriteOptions {
            minify: true,
            source_map: true,
            comments: Comments::Strip,
        };
        let js = rewrite_str(src, Path::new("gen.js"), options).unwrap();

        // The wrapper must append exactly the capturing pass's data: URL.
        let captured =
            rewrite_js_capturing(src, Path::new("gen.js"), Path::new("gen.js"), options, None)
                .unwrap();
        let map = captured.map.expect("map requested");
        assert!(
            js.ends_with(&format!("//# sourceMappingURL={}\n", map.data_url))
                || js.ends_with(&format!("//# sourceMappingURL={}", map.data_url)),
            "inline footer carries the capturing pass's data: URL"
        );
        let parsed: serde_json::Value = serde_json::from_str(&map.json).unwrap();
        assert_eq!(
            parsed["sources"][0], "gen.js",
            "the map points at the input"
        );
        assert_eq!(
            parsed["sourcesContent"][0], src,
            "sources ship inside the map"
        );
    }

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
    fn comment_policy_matrix() {
        let src = "/*! (c) 2026 example */\n// normal note\n/** jsdoc */\nexport const x = 1;\n";
        let compile = |comments: Comments| {
            compile_str_capturing(
                src,
                Path::new("x.ts"),
                &TranspileOptions {
                    comments,
                    ..TranspileOptions::default()
                },
                None,
                None,
            )
            .unwrap()
        };

        let keep = compile(Comments::Keep);
        assert!(keep.code.contains("(c) 2026") && keep.code.contains("normal note"));
        assert!(keep.legal.is_none());

        let strip = compile(Comments::Strip);
        assert!(
            strip.code.contains("(c) 2026"),
            "legal inline; got {}",
            strip.code
        );
        assert!(!strip.code.contains("normal note") && !strip.code.contains("jsdoc"));

        // Collect without a sidecar name (the string API) keeps legal inline.
        let collect = compile(Comments::Collect);
        assert!(collect.code.contains("(c) 2026"), "got {}", collect.code);

        let none = compile(Comments::None);
        assert!(!none.code.contains("(c) 2026"), "got {}", none.code);
    }

    #[test]
    fn collect_extracts_deduplicated_legal_comments() {
        let src = "/*! (c) duplicated */\nexport const x = 1;\n/*! (c) duplicated */\nexport const y = 2;\n";
        let out = compile_str_capturing(
            src,
            Path::new("x.ts"),
            &TranspileOptions {
                comments: Comments::Collect,
                ..TranspileOptions::default()
            },
            None,
            Some("x.js.LEGAL.txt"),
        )
        .unwrap();
        assert!(
            out.code.contains("x.js.LEGAL.txt"),
            "the pointer comment names the sidecar; got {}",
            out.code
        );
        assert!(!out.code.contains("(c) duplicated"), "moved out");
        let legal = out.legal.expect("collected");
        assert_eq!(
            legal.matches("(c) duplicated").count(),
            1,
            "deduplicated; got {legal:?}"
        );
    }

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
            None,
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

        let off = compile_str_capturing(src, Path::new("app.ts"), &Default::default(), None, None)
            .unwrap();
        assert!(off.map.is_none(), "no map unless asked");
    }

    #[test]
    fn captured_imports_match_the_minified_output() {
        // Dead-code elimination removes the unreachable dynamic import, so the
        // captured set must not report it — the graph describes the code that ships.
        // Without minification the branch survives and the import is real.
        let src = "if (false) { import(\"gone-package\"); }\nexport const value = 1;";
        let path = Path::new("m.ts");

        let plain =
            compile_str_capturing(src, path, &TranspileOptions::default(), None, None).unwrap();
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
