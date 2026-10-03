//! `imports::read_module` on its own, on the transform's output and against the build.
#![cfg(feature = "typescript")]

use std::path::{Path, PathBuf};

use web_modules::build::{build, BuildOptions, Output, Processors};
use web_modules::imports::{read_module, ImportKind, ModuleImports, RUNTIME_MODULE};
use web_modules::typescript::{compile_str, compile_str_with, TranspileOptions};
use web_modules::Error;

fn specifiers(read: &ModuleImports, kind: ImportKind) -> Vec<&str> {
    read.imports
        .iter()
        .filter(|i| i.kind == kind)
        .map(|i| i.specifier.as_str())
        .collect()
}

#[test]
fn static_and_dynamic_imports_are_reported() {
    let read = read_module(
        "import { html } from \"lit\";\n\
         import \"./side-effect.js\";\n\
         export * from \"./reexport.js\";\n\
         export { a } from \"./named.js\";\n\
         const page = await import(\"./page.js\");\n\
         const view = await import(`./view.js`);\n",
    )
    .unwrap();
    assert_eq!(
        specifiers(&read, ImportKind::Static),
        ["lit", "./side-effect.js", "./reexport.js", "./named.js"]
    );
    assert_eq!(
        specifiers(&read, ImportKind::Dynamic),
        ["./page.js", "./view.js"]
    );
    assert_eq!(read.computed, 0);
}

#[test]
fn comments_strings_and_member_calls_are_not_imports() {
    let read = read_module(
        "// import \"./comment.js\";\n\
         const s = \"import './string.js'\";\n\
         const t = `export * from \"./template.js\"`;\n\
         console.log(import.meta.url, router.import(\"./member.js\"));\n",
    )
    .unwrap();
    assert!(read.imports.is_empty(), "{:?}", read.imports);
}

#[test]
fn computed_dynamic_imports_are_counted_not_guessed() {
    let read = read_module(
        "const a = await import(`./sections/${name}.js`);\n\
         const b = await import(url);\n",
    )
    .unwrap();
    assert!(read.imports.is_empty(), "{:?}", read.imports);
    assert_eq!(read.computed, 2);
}

#[test]
fn a_source_that_is_no_module_is_refused() {
    for source in ["import { broken from \"lit\";", "var await = 1;"] {
        match read_module(source) {
            Err(Error::Build(message)) => assert!(
                message.starts_with("does not parse as an ES module: "),
                "{message}"
            ),
            other => panic!("expected Error::Build for {source:?}, got {other:?}"),
        }
    }
}

const DECORATED: &str = "function tag(name: string) { return (c: any) => c; }\n\
                         @tag(\"x-el\")\n\
                         export class El { declare value: string; }\n";

/// The Lit preset lowers decorators through runtime helpers.
#[test]
fn the_transform_output_reads_back_with_its_runtime_helpers() {
    let js = compile_str(DECORATED, Path::new("el.ts")).unwrap();
    let read = read_module(&js).unwrap();
    assert!(read.uses_runtime_helpers(), "{js}");
    assert_eq!(read.decorators, 0, "{js}");
    assert!(
        read.imports
            .iter()
            .any(|i| i.specifier.starts_with(&format!("{RUNTIME_MODULE}/"))),
        "{:?}",
        read.imports
    );

    let plain =
        read_module(&compile_str("export const a: number = 1;", Path::new("a.ts")).unwrap())
            .unwrap();
    assert!(!plain.uses_runtime_helpers());
}

/// oxc lowers only legacy decorators; this fails once it lowers standard ones.
#[test]
fn standard_decorators_reach_the_output_as_written() {
    let js =
        compile_str_with(DECORATED, Path::new("el.ts"), &TranspileOptions::standard()).unwrap();
    let read = read_module(&js).unwrap();
    assert!(read.decorators > 0, "{js}");
    assert!(!read.uses_runtime_helpers(), "{js}");
}

#[test]
fn the_build_checks_what_the_reader_reports() {
    let source = "// import \"commented-package\";\n\
                  const s = 'import \"string-package\"';\n\
                  export const load = () => import(`missing-package`);\n";
    let read = read_module(source).unwrap();
    assert_eq!(specifiers(&read, ImportKind::Dynamic), ["missing-package"]);
    assert!(specifiers(&read, ImportKind::Static).is_empty());

    let dir = tempfile::tempdir().unwrap();
    let root: PathBuf = dir.path().join("web");
    std::fs::create_dir_all(&root).unwrap();
    std::fs::write(root.join("app.js"), source).unwrap();
    let err = build(&BuildOptions {
        specs: &[],
        roots: std::slice::from_ref(&root),
        out: &dir.path().join("dist"),
        mount: "/web_modules",
        html: "<!doctype html>{importmap}",
        template: None,
        processors: Processors::default(),
        output: Output::default(),
    })
    .unwrap_err()
    .to_string();
    assert!(err.contains("1 unresolved bare import(s)"), "{err}");
    assert!(err.contains("app.js: import \"missing-package\""), "{err}");
    assert!(
        !err.contains("commented-package") && !err.contains("string-package"),
        "{err}"
    );
}
