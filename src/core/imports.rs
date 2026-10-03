//! The imports of one ES module, read from oxc's syntax tree as the build reads them.
//!
//! Text in comments, strings and templates is no import; an `import()` without a constant
//! argument counts in [`ModuleImports::computed`]. Specifiers stay unresolved: pair them
//! with [`Importmap::resolve`].
//!
//! ```
//! use web_modules::imports::{read_module, ImportKind};
//!
//! let read = read_module(
//!     "import { html } from \"lit\";\n\
//!      // import \"./commented.js\";\n\
//!      const page = await import(`./page.js`);\n\
//!      const view = await import(`./views/${name}.js`);\n",
//! )?;
//! let found: Vec<_> = read.imports.iter().map(|i| (i.specifier.as_str(), i.kind)).collect();
//! assert_eq!(found, [("lit", ImportKind::Static), ("./page.js", ImportKind::Dynamic)]);
//! assert_eq!(read.computed, 1);
//! assert_eq!(read.decorators, 0);
//! assert!(!read.uses_runtime_helpers());
//! # Ok::<(), web_modules::Error>(())
//! ```
//!
//! [`Importmap::resolve`]: crate::importmap::Importmap::resolve

use oxc_allocator::Allocator;
use oxc_ast::ast::{Decorator, Expression, ImportExpression, Program, Statement};
use oxc_ast_visit::{walk, Visit};
use oxc_parser::{Parser, ParserReturn};
use oxc_span::SourceType;

use crate::module_graph::is_runtime_import;
use crate::{Error, Result};

pub use crate::module_graph::RUNTIME_MODULE;

/// One import of an ES module.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub struct Import {
    /// The specifier's value: escapes decoded, a lone surrogate as U+FFFD.
    pub specifier: String,
    /// How the module imports it.
    pub kind: ImportKind,
}

/// How a module imports a specifier; a phase (`defer`, `source`) counts as the form it
/// extends.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum ImportKind {
    /// `import … from`, `import "…"`, `export … from`.
    Static,
    /// `import()` with a string or a template without substitutions.
    Dynamic,
}

/// What [`read_module`] found in one module.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct ModuleImports {
    /// Static imports in statement order, then dynamic ones in source order.
    pub imports: Vec<Import>,
    /// `import(expr)` calls whose argument is not a constant string.
    pub computed: usize,
    /// Decorators left in the code: under [`Decorators::Standard`] no helper import
    /// reveals them.
    ///
    /// [`Decorators::Standard`]: crate::typescript::Decorators::Standard
    pub decorators: usize,
}

impl ModuleImports {
    /// Whether any import, a dynamic one included, names [`RUNTIME_MODULE`] or a path
    /// under it. The build vendors the package for a static import only.
    pub fn uses_runtime_helpers(&self) -> bool {
        self.imports.iter().any(|i| is_runtime_import(&i.specifier))
    }
}

/// Reads the imports of one ES module, parsed under the module goal only.
///
/// A source that does not parse is an [`Error::Build`] with the parser's first error and
/// its position; semantic errors such as a redeclared binding pass.
pub fn read_module(source: &str) -> Result<ModuleImports> {
    let allocator = Allocator::default();
    let parsed = Parser::new(&allocator, source, SourceType::mjs()).parse();
    if parsed.fatal_error || parsed.diagnostics.has_errors() {
        return Err(Error::Build(format!(
            "does not parse as an ES module: {}",
            first_parse_error(source, &parsed)
        )));
    }
    Ok(from_program(&parsed.program))
}

/// The static imports, then what [`walk_program`] finds.
pub(crate) fn from_program(program: &Program) -> ModuleImports {
    let mut read = ModuleImports::default();
    static_from_program(program, &mut read.imports);
    walk_program(program, &mut read);
    read
}

/// Top-level import and export declarations.
fn static_from_program(program: &Program, imports: &mut Vec<Import>) {
    for stmt in &program.body {
        let source = match stmt {
            Statement::ImportDeclaration(decl) => Some(&decl.source),
            Statement::ExportAllDeclaration(decl) => Some(&decl.source),
            Statement::ExportFromDeclaration(decl) => Some(&decl.source),
            _ => None,
        };
        if let Some(source) = source {
            imports.push(Import {
                specifier: literal_value(source.value.as_str(), source.lone_surrogates),
                kind: ImportKind::Static,
            });
        }
    }
}

/// `import()` calls and decorators anywhere in the tree: all a classic script can carry.
pub(crate) fn walk_program(program: &Program, read: &mut ModuleImports) {
    Walk { read }.visit_program(program);
}

/// Every override walks on into the node's children.
struct Walk<'r> {
    read: &'r mut ModuleImports,
}

impl<'a> Visit<'a> for Walk<'_> {
    fn visit_import_expression(&mut self, expr: &ImportExpression<'a>) {
        match constant_specifier(&expr.source) {
            Some(specifier) => self.read.imports.push(Import {
                specifier,
                kind: ImportKind::Dynamic,
            }),
            None => self.read.computed += 1,
        }
        // An import can nest inside another's specifier or options.
        walk::walk_import_expression(self, expr);
    }

    fn visit_decorator(&mut self, decorator: &Decorator<'a>) {
        self.read.decorators += 1;
        // Its expression can hold an import or another decorator.
        walk::walk_decorator(self, decorator);
    }
}

/// A string or a template without substitutions, parenthesized or not.
fn constant_specifier(expr: &Expression) -> Option<String> {
    match expr.without_parentheses() {
        Expression::StringLiteral(literal) => Some(literal_value(
            literal.value.as_str(),
            literal.lone_surrogates,
        )),
        Expression::TemplateLiteral(template) => match template.quasis.as_slice() {
            [quasi] => quasi
                .value
                .cooked
                .map(|cooked| literal_value(cooked.as_str(), quasi.lone_surrogates)),
            _ => None,
        },
        _ => None,
    }
}

/// A literal's value as the browser reads it: with `lone_surrogates`, oxc stores each lone
/// surrogate and each U+FFFD as U+FFFD plus four hex digits, which read as U+FFFD.
fn literal_value(value: &str, lone_surrogates: bool) -> String {
    if !lone_surrogates {
        return value.to_string();
    }
    let mut decoded = String::with_capacity(value.len());
    let mut chars = value.chars();
    while let Some(c) = chars.next() {
        decoded.push(c);
        if c == '\u{FFFD}' {
            for _ in 0..4 {
                chars.next();
            }
        }
    }
    decoded
}

/// The parser's first error with its `line:column`, both from 1, the column in characters.
pub(crate) fn first_parse_error(source: &str, parsed: &ParserReturn) -> String {
    let Some(error) = parsed.diagnostics.errors().next() else {
        return "the parser stopped".to_string();
    };
    match error.labels.first() {
        Some(label) => {
            // Labels sit on token boundaries; clamp anyway.
            let mut offset = (label.offset() as usize).min(source.len());
            while !source.is_char_boundary(offset) {
                offset -= 1;
            }
            let before = &source[..offset];
            let line = before.matches('\n').count() + 1;
            let column = before.rsplit('\n').next().unwrap_or("").chars().count() + 1;
            format!("{error} at {line}:{column}")
        }
        None => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(source: &str) -> ModuleImports {
        read_module(source).unwrap_or_else(|e| panic!("{e}\n{source}"))
    }

    fn found(read: &ModuleImports) -> Vec<(&str, ImportKind)> {
        read.imports
            .iter()
            .map(|i| (i.specifier.as_str(), i.kind))
            .collect()
    }

    use ImportKind::{Dynamic, Static};

    #[test]
    fn every_import_and_export_form_is_read() {
        let read = read(
            "import a from \"./a.js\";\n\
             import * as b from './b.js';\n\
             import { c, d as e } from \"./c.js\";\n\
             import f, { g } from \"./f.js\";\n\
             import h, * as i from \"./h.js\";\n\
             import \"./side.js\";\n\
             import data from \"./data.json\" with { type: \"json\" };\n\
             import {} from \"./empty.js\";\n\
             export { j } from \"./j.js\";\n\
             export { default } from \"./default.js\";\n\
             export { k as kay } from \"./k.js\";\n\
             export * from \"./all.js\";\n\
             export * as ns from \"./ns.js\";\n\
             export { l };\n\
             export const m = 1;\n\
             export function n() {}\n\
             const o = await import(\"./o.js\");\n\
             import(\"./p.json\", { with: { type: \"json\" } });\n\
             let l;\n",
        );
        assert_eq!(
            found(&read),
            [
                ("./a.js", Static),
                ("./b.js", Static),
                ("./c.js", Static),
                ("./f.js", Static),
                ("./h.js", Static),
                ("./side.js", Static),
                ("./data.json", Static),
                ("./empty.js", Static),
                ("./j.js", Static),
                ("./default.js", Static),
                ("./k.js", Static),
                ("./all.js", Static),
                ("./ns.js", Static),
                ("./o.js", Dynamic),
                ("./p.json", Dynamic),
            ]
        );
        assert_eq!(read.computed, 0);
    }

    #[test]
    fn minified_forms_are_read() {
        let read = read(
            "import\"./side.js\";import{a as b}from\"lit\";export{x}from\"./x.js\";\
             export*from\"./all.js\";const m=import(\"lit-html\");",
        );
        assert_eq!(
            found(&read),
            [
                ("./side.js", Static),
                ("lit", Static),
                ("./x.js", Static),
                ("./all.js", Static),
                ("lit-html", Dynamic),
            ]
        );
    }

    #[test]
    fn escaped_specifiers_read_as_their_value() {
        let read = read(
            "import \"\\u0068ttps://example.com/x.js\";\n\
             export * from \"\\x2F\\u{2F}h/x.js\";\n\
             import(\"\\u006Cit\");\n\
             import(`\\u006Cit/html.js`);\n",
        );
        assert_eq!(
            found(&read),
            [
                ("https://example.com/x.js", Static),
                ("//h/x.js", Static),
                ("lit", Dynamic),
                ("lit/html.js", Dynamic),
            ]
        );
    }

    #[test]
    fn a_template_without_substitutions_is_a_dynamic_import() {
        let read = read("const a = import(`lit`);\nconst b = import(`./local.js`);\n");
        assert_eq!(found(&read), [("lit", Dynamic), ("./local.js", Dynamic)]);
        assert_eq!(read.computed, 0);
    }

    #[test]
    fn a_parenthesized_literal_is_a_dynamic_import() {
        let read = read("import((\"lit\"));\nimport(((`./a.js`)));\n");
        assert_eq!(found(&read), [("lit", Dynamic), ("./a.js", Dynamic)]);
        assert_eq!(read.computed, 0);
    }

    #[test]
    fn computed_imports_are_counted() {
        let read = read(
            "import(`./${name}.js`);\n\
             import(`${base}`);\n\
             import(name);\n\
             import(\"./a\" + b);\n\
             import(urls[0]);\n\
             import(config.url);\n\
             import(load());\n\
             import(cond ? \"./a.js\" : \"./b.js\");\n",
        );
        assert_eq!(read.computed, 8);
        assert!(read.imports.is_empty(), "{:?}", read.imports);
    }

    #[test]
    fn a_nested_import_is_read_and_its_host_counted() {
        // The outer call's argument is computed; the inner one names a module.
        let read = read("import((await import(\"./table.js\")).default);\n");
        assert_eq!(found(&read), [("./table.js", Dynamic)]);
        assert_eq!(read.computed, 1);
    }

    #[test]
    fn dynamic_imports_are_read_wherever_they_sit() {
        let read = read(
            "document.addEventListener(\"click\",()=>{import(\"./lazy.js\").then(m=>m.run())});\n\
             class View { async load() { return import(\"./view.js\"); } }\n\
             function f() { if (x) { return import(\"./deep.js\"); } }\n",
        );
        assert_eq!(
            found(&read),
            [
                ("./lazy.js", Dynamic),
                ("./view.js", Dynamic),
                ("./deep.js", Dynamic),
            ]
        );
    }

    // `/` after `}`: division or a regex, which only the parser tells apart.
    #[test]
    fn an_import_after_a_closing_brace_or_class_body_is_read() {
        let read = read(
            "let x = {} / import(\"https://e.example/x.js\") / 1;\n\
             let y = class {} / import(\"https://e.example/y.js\") / 1;\n\
             let z = function () {} / import(name);\n\
             if (x) {} /import\\(\"https:\\/\\/e.example\\/regex.js\"\\)/.test(y);\n\
             class C {}\n\
             import(\"https://e.example/after-class.js\");\n",
        );
        assert_eq!(
            found(&read),
            [
                ("https://e.example/x.js", Dynamic),
                ("https://e.example/y.js", Dynamic),
                ("https://e.example/after-class.js", Dynamic),
            ]
        );
        assert_eq!(read.computed, 1);
    }

    #[test]
    fn import_phases_read_as_the_form_they_extend() {
        let read = read(
            "import defer * as d from \"./deferred.js\";\n\
             import source s from \"./module.wasm\";\n\
             import.defer(\"./later.js\");\n\
             import.source(\"./other.wasm\");\n\
             import.source(name);\n",
        );
        assert_eq!(
            found(&read),
            [
                ("./deferred.js", Static),
                ("./module.wasm", Static),
                ("./later.js", Dynamic),
                ("./other.wasm", Dynamic),
            ]
        );
        assert_eq!(read.computed, 1);
    }

    #[test]
    fn import_meta_and_import_members_are_no_imports() {
        let read = read(
            "console.log(import.meta.url, import.meta.resolve(\"./meta.js\"));\n\
             obj.import(\"./member.js\");\n\
             obj?.import(\"./optional.js\");\n\
             class C { import(x) { return x; } static import() {} }\n\
             const o = { import() {}, import: \"./property.js\" };\n",
        );
        assert!(read.imports.is_empty(), "{:?}", read.imports);
        assert_eq!(read.computed, 0);
    }

    #[test]
    fn comments_strings_and_templates_are_no_imports() {
        let read = read(
            "// Satisfies `import nodeCrypto from \"crypto\"` in the browser.\n\
             /* import \"./block.js\"; export * from \"./block.js\"; */\n\
             /** @example import(\"./doc.js\") */\n\
             const s = 'import \"nope\" failed';\n\
             const t = `import \"./template.js\" ${x} from \"./sub.js\" import(\"./t.js\")`;\n\
             const r = /import \"\\.\\/regex.js\"/;\n\
             const from = \"./not.js\";\n\
             export default {};\n",
        );
        assert!(read.imports.is_empty(), "{:?}", read.imports);
        assert_eq!(read.computed, 0);
    }

    #[test]
    fn a_lone_surrogate_reads_as_the_replacement_character() {
        let read = read(
            "import \"./\\uD800.js\";\n\
             export * from \"./\\uDC00\\uFFFD.js\";\n\
             import \"./\u{FFFD}\\uDBFF.js\";\n\
             import(\"./\\uD83D\\uDE00\\uDFFF.js\");\n\
             import(`./\\u{D800}.js`);\n\
             import(\"./\\uFFFDd800.js\");\n",
        );
        assert_eq!(
            found(&read),
            [
                ("./\u{FFFD}.js", Static),
                ("./\u{FFFD}\u{FFFD}.js", Static),
                ("./\u{FFFD}\u{FFFD}.js", Static),
                ("./\u{1F600}\u{FFFD}.js", Dynamic),
                ("./\u{FFFD}.js", Dynamic),
                // No lone surrogate: U+FFFD and the text after it are the value.
                ("./\u{FFFD}d800.js", Dynamic),
            ]
        );
    }

    #[test]
    fn decorators_are_counted() {
        let read = read(
            "@tag class A {}\n\
             const B = @tag class {};\n\
             class C {\n\
               @field x = 1;\n\
               @field static y;\n\
               @method m() {}\n\
               @accessor accessor z;\n\
             }\n\
             const t = `text ${@inner class {}} text`;\n\
             @first @second(1) class D {}\n",
        );
        // A, B, C's four elements, the class in the template's substitution, D's two.
        assert_eq!(read.decorators, 9);
        assert!(read.imports.is_empty(), "{:?}", read.imports);
    }

    #[test]
    fn imports_and_decorators_inside_an_export_are_read() {
        let read = read(
            "export const f = () => import(\"./in-export.js\");\n\
             @tag export class A {}\n\
             export @tag class B {}\n\
             export default @tag class {}\n\
             export const C = @tag class { @field x = import(\"./in-field.js\"); };\n\
             export class D { @(load(import(\"./in-decorator.js\"))) m() {} }\n",
        );
        assert_eq!(
            found(&read),
            [
                ("./in-export.js", Dynamic),
                ("./in-field.js", Dynamic),
                ("./in-decorator.js", Dynamic),
            ]
        );
        // A, B, the default class, C and its field, D's method.
        assert_eq!(read.decorators, 6);
    }

    #[test]
    fn an_at_sign_outside_code_is_no_decorator() {
        let read = read(
            "// @comment\n\
             /* @block */\n\
             /** @param {string} x @returns {void} */\n\
             function f(x) {}\n\
             const s = \"@string\";\n\
             const t = `@template ${x} @text`;\n\
             const r = /@regex[/@]/g;\n\
             const d = a / b / c;\n\
             if (x) /@/.test(y);\n\
             const e = (a) / 2 + /@/.source;\n",
        );
        assert_eq!(read.decorators, 0);
    }

    #[test]
    fn runtime_helpers_are_any_import_of_the_package() {
        let statically = read("import _decorate from \"@oxc-project/runtime/helpers/decorate\";");
        assert!(statically.uses_runtime_helpers());
        let dynamically = read("import(\"@oxc-project/runtime/helpers/decorate\");");
        assert!(dynamically.uses_runtime_helpers());
        let package = read("export * from \"@oxc-project/runtime\";");
        assert!(package.uses_runtime_helpers());
        // Another package.
        let lookalike = read("import \"@oxc-project/runtime-extra/x.js\";\nimport \"lit\";");
        assert!(!lookalike.uses_runtime_helpers());
    }

    #[test]
    fn a_source_that_does_not_parse_is_an_error_with_its_position() {
        let err = read_module("import { a } from \"./a.js\";\nimport { broken from \"lit\";\n")
            .unwrap_err();
        let Error::Build(message) = &err else {
            panic!("expected Error::Build, got {err:?}");
        };
        assert!(
            message.starts_with("does not parse as an ES module: "),
            "{message}"
        );
        assert!(message.ends_with(" at 2:17"), "{message}");
    }

    #[test]
    fn a_classic_script_is_no_module() {
        // `await` is an identifier in a classic script and reserved in a module.
        let err = read_module("var await = 1;\nimport(\"./x.js\");\n").unwrap_err();
        assert!(
            err.to_string().contains("does not parse as an ES module"),
            "{err}"
        );
    }

    #[test]
    fn typescript_is_no_javascript() {
        assert!(read_module("import type { A } from \"./a.js\";\nlet a: A;\n").is_err());
    }

    #[test]
    fn positions_count_lines_and_characters_from_one() {
        let err = read_module("const ä = 1;\nconst ö = ;\n").unwrap_err();
        assert!(err.to_string().ends_with(" at 2:11"), "{err}");
    }
}
