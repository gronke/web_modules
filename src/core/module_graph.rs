//! The module graph: which module specifiers each emitted file imports.
//!
//! Produced where each file is handled — the TypeScript transform captures its imports
//! from the AST it just built (see [`crate::typescript`]), and the static-copy step
//! reads verbatim `.js` as it copies — so the build never re-reads the output tree to
//! reconstruct them. That re-reading is what made the previous text scan fragile: it
//! ran against minified output whose spacing its matcher didn't expect. Reading the
//! specifiers structurally, at transform time, removes both that fragility and the
//! false positives from `import`/`from` text inside comments or strings.
//!
//! Scope: the graph describes the JavaScript the current build's steps emitted —
//! transform outputs, copied `.js`/`.mjs`, and JavaScript rendered from `*.tera` —
//! one record per output path, matching the preflight's one-winner-per-path
//! resolution. It does not cover every file in the output directory: a reused output
//! directory may retain files from earlier builds that the current run never touched.

use std::collections::BTreeMap;
use std::path::PathBuf;

/// The npm package the oxc transform imports its runtime helpers from (Runtime helper
/// mode, oxc's default and web_modules' setting).
pub const RUNTIME_MODULE: &str = "@oxc-project/runtime";

/// How a specifier is referenced — enough to tell a transform-runtime import and a
/// dynamic `import()` apart from an ordinary static application import.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecifierKind {
    /// A static `import` / `export … from`.
    Static,
    /// A dynamic `import()` call.
    Dynamic,
    /// A static import under `@oxc-project/runtime/` — the package the transform
    /// injects its helpers from. Classified by prefix, not provenance-tracked: a
    /// hand-written import of the same package is indistinguishable from an injected
    /// one, and both need the package vendored.
    OxcRuntime,
}

/// One module specifier a file references, with how it is referenced.
#[derive(Clone, Debug)]
pub struct ModuleImport {
    pub specifier: String,
    pub kind: SpecifierKind,
}

impl ModuleImport {
    fn new(specifier: String, dynamic: bool) -> Self {
        let kind = if dynamic {
            SpecifierKind::Dynamic
        } else if is_runtime_import(&specifier) {
            SpecifierKind::OxcRuntime
        } else {
            SpecifierKind::Static
        };
        Self { specifier, kind }
    }
}

/// The imports of the emitted (non-vendored) modules, keyed by output-relative path.
/// Assembled during the build as each winner is emitted, then used to decide
/// runtime-helper vendoring and to verify that every bare import resolves — no walk of
/// the output tree.
///
/// One record per output path: recording a path again replaces the earlier entry, the
/// way the corresponding write would overwrite the file. The preflight already picks
/// one winner per output path, so the build records each path once; the upsert keeps
/// the graph honest if a caller ever records in write order again.
#[derive(Clone, Debug, Default)]
pub struct ModuleGraph {
    nodes: BTreeMap<PathBuf, Vec<ModuleImport>>,
}

impl ModuleGraph {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a file's imports under its output-relative path, replacing any earlier
    /// record for the same path — the last write wins, as on the filesystem.
    pub fn insert(&mut self, path: impl Into<PathBuf>, imports: Vec<ModuleImport>) {
        self.nodes.insert(path.into(), imports);
    }

    /// Whether any emitted module imports the transform runtime — the signal to
    /// vendor `@oxc-project/runtime`.
    pub fn uses_runtime_helpers(&self) -> bool {
        self.nodes
            .values()
            .flatten()
            .any(|i| i.kind == SpecifierKind::OxcRuntime)
    }

    /// Bare specifiers the import map can't resolve, as `(file, specifier)` — the build
    /// fails on these so a browser-load 404 becomes a clear build error. Ordered by
    /// output path, so the error report is deterministic.
    pub fn unresolved(&self, importmap: &crate::importmap::Importmap) -> Vec<(String, String)> {
        let mut out = Vec::new();
        for (path, imports) in &self.nodes {
            for import in imports {
                if classify(&import.specifier) == SpecifierClass::Bare
                    && !importmap.resolves(&import.specifier)
                {
                    out.push((path.display().to_string(), import.specifier.clone()));
                }
            }
        }
        out
    }
}

/// How the browser treats a module specifier before any import map is consulted — the
/// split the WHATWG "resolve a module specifier" algorithm makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SpecifierClass {
    /// Carries a URL scheme (`https:`, `data:`, `blob:`, `about:`, `node:`, a custom
    /// scheme, …) — resolved as a URL, never through the import map.
    Url,
    /// Starts with `/`, `./` or `../` — resolved against the importing module's URL.
    Relative,
    /// Everything else — resolvable only through the import map.
    Bare,
}

/// Classify `spec` the way the WHATWG "resolve a module specifier" algorithm does, in
/// order: a string the WHATWG URL parser (the `url` crate) accepts as an absolute URL
/// is a URL — the browser's own first resolution step, so scheme edge cases and
/// whitespace stripping match by construction; a `/`, `./` or `../` prefix is resolved
/// against the importing module; anything else is bare. The one classifier every check
/// shares, so the graph and the resolution report cannot disagree about a specifier.
pub fn classify(spec: &str) -> SpecifierClass {
    if url::Url::parse(spec).is_ok() {
        SpecifierClass::Url
    } else if spec.starts_with('/') || spec.starts_with("./") || spec.starts_with("../") {
        SpecifierClass::Relative
    } else {
        SpecifierClass::Bare
    }
}

fn is_runtime_import(spec: &str) -> bool {
    spec == RUNTIME_MODULE || spec.starts_with(&format!("{RUNTIME_MODULE}/"))
}

/// What reading a verbatim source file yielded: the imports found, and whether the
/// file actually parsed — an empty import list from a parse failure is a different
/// fact than a parsed file that imports nothing, and the caller may want to warn.
#[derive(Debug)]
pub struct SourceImports {
    pub imports: Vec<ModuleImport>,
    /// False when the file parsed under neither the module nor the classic-script
    /// goal, so nothing is known about its imports. The lexical fallback (without the
    /// `typescript` feature) cannot detect a parse failure and always reports true.
    pub parsed: bool,
}

/// The imports of a verbatim source file (a hand-written `.js`/`.mjs` copied
/// unchanged). With the `typescript` feature the file is parsed and its static and
/// dynamic imports read from the AST:
///
/// - Parsed as a **module** first. `module_only` (an `.mjs` file, unambiguously a
///   module) makes a parse failure an error — the browser would fail on it too.
/// - A plain `.js` that fails the module goal is re-parsed as a **classic script**:
///   dynamic `import()` is legal there and resolves through the document's import map,
///   so its literal specifiers are still captured (import declarations are module-only
///   and cannot occur).
/// - A file failing both goals yields no imports and reports `parsed == false` — it is
///   copied unchanged, and the caller can surface that its imports are unknown.
///
/// Never reads from a recovered AST: a partial import set from error recovery would
/// look authoritative without being it.
///
/// Without the `typescript` feature there is no parser, and this falls back to a
/// lexical scan of the authored text (no minifier exists in that configuration, so
/// spacing is as written). The fallback is best-effort: `import`/`from` text inside a
/// comment or string can false-positive, only the spaced authored forms match, and
/// `module_only` cannot be enforced.
pub fn imports_from_source(
    js: &str,
    module_only: bool,
) -> std::result::Result<SourceImports, String> {
    #[cfg(feature = "typescript")]
    return imports_from_source_ast(js, module_only);
    #[cfg(not(feature = "typescript"))]
    {
        let _ = module_only;
        let mut imports = Vec::new();
        static_lexical(js, &mut imports);
        dynamic_lexical(js, &mut imports);
        Ok(SourceImports {
            imports,
            parsed: true,
        })
    }
}

/// The parser-backed body of [`imports_from_source`]: module goal first, classic-script
/// goal as the `.js` fallback, never a recovered AST.
#[cfg(feature = "typescript")]
fn imports_from_source_ast(
    js: &str,
    module_only: bool,
) -> std::result::Result<SourceImports, String> {
    use oxc_allocator::Allocator;
    use oxc_parser::Parser;
    use oxc_span::SourceType;
    let mut imports = Vec::new();
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, js, SourceType::mjs()).parse();
    if !parsed.diagnostics.has_errors() {
        static_from_program(&parsed.program, &mut imports);
        dynamic_from_program(&parsed.program, &mut imports);
        return Ok(SourceImports {
            imports,
            parsed: true,
        });
    }
    if module_only {
        let first = parsed
            .diagnostics
            .iter()
            .next()
            .map(|d| format!("{d:?}"))
            .unwrap_or_default();
        return Err(format!("does not parse as an ES module: {first}"));
    }
    let script = Parser::new(&allocator, js, SourceType::script()).parse();
    if !script.diagnostics.has_errors() {
        dynamic_from_program(&script.program, &mut imports);
        return Ok(SourceImports {
            imports,
            parsed: true,
        });
    }
    Ok(SourceImports {
        imports: Vec::new(),
        parsed: false,
    })
}

/// Static `import` / `export … from` specifiers from a parsed program's top-level module
/// statements. Reading them from the AST is spacing-agnostic (so minified output is
/// covered) and never mistakes `import`/`from` text in a comment or string for an import.
#[cfg(feature = "typescript")]
pub fn static_from_program(program: &oxc_ast::ast::Program, imports: &mut Vec<ModuleImport>) {
    use oxc_ast::ast::Statement;
    for stmt in &program.body {
        let source = match stmt {
            Statement::ImportDeclaration(decl) => Some(decl.source.value.as_str()),
            Statement::ExportAllDeclaration(decl) => Some(decl.source.value.as_str()),
            Statement::ExportNamedDeclaration(decl) => {
                decl.source.as_ref().map(|s| s.value.as_str())
            }
            _ => None,
        };
        if let Some(spec) = source {
            imports.push(ModuleImport::new(spec.to_string(), false));
        }
    }
}

/// Dynamic `import("…")` specifiers with a string-literal argument, read by walking the
/// transformed AST — so a nested or minified `import(...)` is found the same as a
/// top-level one, with no dependence on emitted-text spacing. A computed argument
/// (`import(url)`) names no static module, so there is nothing to record.
#[cfg(feature = "typescript")]
pub fn dynamic_from_program(program: &oxc_ast::ast::Program, imports: &mut Vec<ModuleImport>) {
    use oxc_ast_visit::Visit;
    DynamicImports { imports }.visit_program(program);
}

/// Records the string-literal specifier of every dynamic `import()` it meets, anywhere
/// in the tree (in a callback, a nested expression, …), not just at the top level.
#[cfg(feature = "typescript")]
struct DynamicImports<'i> {
    imports: &'i mut Vec<ModuleImport>,
}

#[cfg(feature = "typescript")]
impl<'a> oxc_ast_visit::Visit<'a> for DynamicImports<'_> {
    fn visit_import_expression(&mut self, expr: &oxc_ast::ast::ImportExpression<'a>) {
        use oxc_ast::ast::Expression;
        // A string literal and a no-substitution template name the module statically —
        // the browser resolves `import(`lit`)` exactly like `import("lit")`. A computed
        // specifier (`import(url)`, `import(`pkg/${x}`)`) names none.
        match &expr.source {
            Expression::StringLiteral(spec) => self
                .imports
                .push(ModuleImport::new(spec.value.as_str().to_string(), true)),
            Expression::TemplateLiteral(tpl) if tpl.is_no_substitution_template() => {
                if let Some(quasi) = tpl.single_quasi() {
                    self.imports
                        .push(ModuleImport::new(quasi.as_str().to_string(), true));
                }
            }
            _ => {}
        }
        // Descend into the call's own children too — a dynamic import can nest inside
        // another's specifier expression.
        oxc_ast_visit::walk::walk_import_expression(self, expr);
    }
}

/// Dynamic `import("…")` / `import('…')` specifiers, matched lexically — the fallback
/// when the crate is built without the `typescript` feature (no oxc parser, and no
/// minifier, so sources keep authored spacing). The call parenthesis is stable, so both
/// spaced and space-less forms are found.
#[cfg(not(feature = "typescript"))]
fn dynamic_lexical(js: &str, imports: &mut Vec<ModuleImport>) {
    for spec in scan_quoted(js, &["import(\"", "import('"]) {
        imports.push(ModuleImport::new(spec, true));
    }
}

/// Static specifiers via substring scan — the fallback when the crate is built without
/// the `typescript` feature (and thus without a minifier, so sources keep their spaces).
#[cfg(not(feature = "typescript"))]
fn static_lexical(js: &str, imports: &mut Vec<ModuleImport>) {
    for spec in scan_quoted(js, &["from \"", "from '", "import \"", "import '"]) {
        imports.push(ModuleImport::new(spec, false));
    }
}

/// The quoted string following each occurrence of any pattern (each pattern ends in its
/// opening quote; the value runs to the next matching quote).
#[cfg(not(feature = "typescript"))]
fn scan_quoted(js: &str, patterns: &[&str]) -> Vec<String> {
    let mut specs = Vec::new();
    for pat in patterns {
        let Some(quote) = pat.chars().last() else {
            continue;
        };
        let mut from = 0;
        while let Some(p) = js[from..].find(pat) {
            let start = from + p + pat.len();
            match js[start..].find(quote) {
                Some(end) => {
                    specs.push(js[start..start + end].to_string());
                    from = start + end + 1;
                }
                None => break,
            }
        }
    }
    specs
}

#[cfg(test)]
mod tests {
    use super::*;

    fn specs(imports: &[ModuleImport]) -> Vec<&str> {
        imports.iter().map(|i| i.specifier.as_str()).collect()
    }

    #[test]
    fn classify_follows_url_grammar() {
        use SpecifierClass::*;
        for (spec, want) in [
            ("lit", Bare),
            ("@oxc-project/runtime/helpers/decorate", Bare),
            (".hidden", Bare),     // relative means exactly `/`, `./` or `../`
            ("1nope:x", Bare),     // a scheme starts with a letter
            ("no scheme:x", Bare), // a space is invalid in a scheme
            ("./local.js", Relative),
            ("../up.js", Relative),
            ("/x.js", Relative),
            // Protocol-relative has no scheme, so it falls through to the `/` rule —
            // the browser resolves it against the importing module's URL the same way.
            ("//host/mod.js", Relative),
            // A leading `name:` is always a scheme by URL grammar: userinfo can only
            // appear inside an authority introduced by `//`, so a scheme-less
            // `user:pass@host` form cannot exist — the browser parses `user:` as the
            // scheme here too.
            ("user:pass@example.com/mod.js", Url),
            ("https://h/y.js", Url),
            ("data:text/javascript,0", Url),
            ("blob:https://origin/uuid", Url),
            ("about:blank", Url),
            ("node:fs", Url),
            ("file:///x.js", Url),
            ("custom+scheme.v1:thing", Url),
        ] {
            assert_eq!(classify(spec), want, "classify({spec:?})");
        }
    }

    #[test]
    fn unresolved_skips_url_scheme_specifiers() {
        // `blob:`/`node:`/`about:` imports resolve as URLs, never through the import
        // map — they must not be reported as unresolved bare imports.
        let mut graph = ModuleGraph::new();
        graph.insert(
            "app.js",
            vec![
                ModuleImport::new("node:fs".into(), false),
                ModuleImport::new("blob:https://origin/uuid".into(), true),
                ModuleImport::new("about:blank".into(), false),
                ModuleImport::new("missing-package".into(), false),
            ],
        );
        let unresolved = graph.unresolved(&crate::importmap::Importmap::new());
        assert_eq!(
            unresolved.len(),
            1,
            "only the bare specifier is unresolved; got {unresolved:?}"
        );
        assert_eq!(unresolved[0].1, "missing-package");
    }

    #[test]
    fn source_imports_static_dynamic_and_runtime_kinds() {
        let js = "import { a } from \"lit\";\n\
                  import _d from \"@oxc-project/runtime/helpers/decorate\";\n\
                  import \"./local.js\";\n\
                  const m = import(\"bootstrap\");";
        let imports = imports_from_source(js, false).unwrap().imports;
        let found = specs(&imports);
        for want in [
            "lit",
            "@oxc-project/runtime/helpers/decorate",
            "./local.js",
            "bootstrap",
        ] {
            assert!(found.contains(&want), "missing {want:?} in {found:?}");
        }
        let kind = |s: &str| imports.iter().find(|i| i.specifier == s).unwrap().kind;
        assert_eq!(kind("lit"), SpecifierKind::Static);
        assert_eq!(
            kind("@oxc-project/runtime/helpers/decorate"),
            SpecifierKind::OxcRuntime
        );
        assert_eq!(kind("bootstrap"), SpecifierKind::Dynamic);
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn finds_minified_space_less_forms() {
        // What the minifier emits: no space after `import`/`from`, plus a re-export.
        // The specifiers come from the AST, so spacing is irrelevant.
        let js = "import\"@oxc-project/runtime/helpers/decorate\";\
                  import{a as b}from\"lit\";\
                  export{x}from\"bootstrap\";\
                  const m=import(\"lit-html\");";
        let imports = imports_from_source(js, false).unwrap().imports;
        let found = specs(&imports);
        for want in [
            "@oxc-project/runtime/helpers/decorate",
            "lit",
            "bootstrap",
            "lit-html",
        ] {
            assert!(found.contains(&want), "missing {want:?} in {found:?}");
        }
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn ignores_imports_in_comments_and_strings() {
        // A shim documenting the import it replaces, and a log line quoting one — the
        // old substring scan flagged both as unresolved bare imports.
        let js = "// Satisfies `import nodeCrypto from \"crypto\"` in the browser.\n\
                  const msg = 'import \"nope\" failed';\n\
                  export default {};";
        let imports = imports_from_source(js, false).unwrap().imports;
        let found = specs(&imports);
        assert!(
            !found.contains(&"crypto") && !found.contains(&"nope"),
            "comment/string text must not register as imports; got {found:?}"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn finds_nested_and_minified_dynamic_import_from_ast() {
        // A dynamic import buried in a callback, minified (no spaces), plus a computed
        // one. The AST walk finds the string-literal import wherever it sits; a
        // top-level-only walk or a text scan keyed on spacing would miss or misread it.
        let js =
            "document.addEventListener(\"click\",()=>{import(\"./lazy.js\").then(m=>m.run())});\
                  const load=(n)=>import(n);";
        let imports = imports_from_source(js, false).unwrap().imports;
        let dynamic: Vec<&str> = imports
            .iter()
            .filter(|i| i.kind == SpecifierKind::Dynamic)
            .map(|i| i.specifier.as_str())
            .collect();
        // The computed `import(n)` names no static module, so only the literal is recorded.
        assert_eq!(dynamic, ["./lazy.js"], "got {dynamic:?}");
    }

    #[test]
    fn graph_flags_unresolved_but_agrees_on_resolved() {
        let mut graph = ModuleGraph::new();
        graph.insert(
            "app.js",
            vec![
                ModuleImport::new("lit".into(), false),
                ModuleImport::new("@oxc-project/runtime/helpers/decorate".into(), false),
                ModuleImport::new("./local.js".into(), false),
            ],
        );
        assert!(graph.uses_runtime_helpers());

        let mut map = crate::importmap::Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js");
        let unresolved = graph.unresolved(&map);
        // `lit` resolves, `./local.js` is not bare, the runtime import is unresolved.
        assert_eq!(unresolved.len(), 1, "got {unresolved:?}");
        assert_eq!(unresolved[0].0, "app.js");
        assert!(unresolved[0].1.starts_with("@oxc-project/runtime"));
    }

    #[test]
    fn graph_without_helpers_needs_no_runtime() {
        let mut graph = ModuleGraph::new();
        graph.insert("app.js", vec![ModuleImport::new("lit".into(), false)]);
        assert!(!graph.uses_runtime_helpers());
    }

    #[test]
    fn insert_replaces_same_path_last_write_wins() {
        // A shadowed file's record must not linger: neither its unresolved import nor
        // its helper usage may outlive the overwrite that removed the file's content.
        let mut graph = ModuleGraph::new();
        graph.insert(
            "app.js",
            vec![
                ModuleImport::new("missing-package".into(), false),
                ModuleImport::new("@oxc-project/runtime/helpers/decorate".into(), false),
            ],
        );
        graph.insert("other.js", vec![ModuleImport::new("lit".into(), false)]);
        // The same output path written again — e.g. the first root overwriting a
        // fallback root's file, or a copied `.js` overwriting a transformed sibling.
        graph.insert("app.js", vec![]);

        assert!(
            !graph.uses_runtime_helpers(),
            "the replaced record's helper must not trigger vendoring"
        );
        let unresolved = graph.unresolved(&crate::importmap::Importmap::new());
        assert_eq!(
            unresolved,
            vec![("other.js".to_string(), "lit".to_string())],
            "only the shipped files' imports count; got {unresolved:?}"
        );
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn unparsable_source_contributes_no_imports_and_reports_it() {
        // A file that fails both parse goals (broken under the module AND the script
        // grammar) is copied unchanged but adds nothing to the graph — no partial
        // import set from a recovered AST — and the parse failure is reported so the
        // caller can warn that the file's imports are unknown.
        let js = "import { broken from \"lit\";";
        let read = imports_from_source(js, false).unwrap();
        assert!(read.imports.is_empty());
        assert!(!read.parsed, "both goals failed, so nothing is known");
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn classic_script_dynamic_import_is_captured() {
        // `await` is a reserved word in modules but a legal identifier in a classic
        // script, so the module goal fails at parse time — but dynamic `import()` is
        // legal in a classic script and resolves through the document's import map,
        // so the script-goal fallback must still record it.
        let js = "var await = 1;\nimport(\"missing-package\");";
        let read = imports_from_source(js, false).unwrap();
        assert!(read.parsed, "the classic-script goal parses this");
        assert_eq!(read.imports.len(), 1, "got {:?}", read.imports);
        assert_eq!(read.imports[0].specifier, "missing-package");
        assert_eq!(read.imports[0].kind, SpecifierKind::Dynamic);
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn module_only_source_fails_on_module_syntax_error() {
        // An `.mjs` is unambiguously a module: a parse failure is an error for the
        // caller to surface, not a silent empty import set. (`await` cannot be an
        // identifier in module code.)
        let err = imports_from_source("var await = 1;", true).unwrap_err();
        assert!(err.contains("does not parse as an ES module"), "got: {err}");
    }

    #[cfg(feature = "typescript")]
    #[test]
    fn no_substitution_template_dynamic_imports_are_captured() {
        // `import(`lit`)` names its module as statically as `import("lit")` — the
        // browser resolves both identically. A substituted template names none.
        let js = "const a = import(`lit`);\n\
                  const b = import(`./local.js`);\n\
                  const c = (name) => import(`pkg/${name}`);";
        let imports = imports_from_source(js, false).unwrap().imports;
        let dynamic: Vec<&str> = imports
            .iter()
            .filter(|i| i.kind == SpecifierKind::Dynamic)
            .map(|i| i.specifier.as_str())
            .collect();
        assert_eq!(dynamic, ["lit", "./local.js"], "got {dynamic:?}");
    }
}
