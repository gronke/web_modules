//! Composition behaviors, bound as executable specs.
//!
//! Two things the host (and the compose example) lean on: per-directory **mount naming
//! precedence**, and the **co-generation invariant** — the runtime import map and the
//! editor tsconfig are built from one mount set, so they cover the same specifiers and
//! can't drift. All offline: building mounts from a manifest reads files, it doesn't
//! vendor.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use web_modules::importmap::Importmap;
use web_modules::tsconfig::tsconfig_paths;
use web_modules::vendor::read_package_json;
use web_modules::Mount;

fn examples() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("examples")
}

#[test]
fn from_dir_precedence_given_over_manifest_over_basename() {
    let tmp = tempfile::tempdir().unwrap();

    // (1) package.json `name` + `web_modules.root`: name → specifier, root → served dir.
    let scoped = tmp.path().join("scoped");
    std::fs::create_dir_all(scoped.join("src")).unwrap();
    std::fs::write(
        scoped.join("package.json"),
        r#"{"name":"@acme/widgets","web_modules":{"root":"./src"}}"#,
    )
    .unwrap();
    let m = Mount::from_dir(&scoped);
    assert_eq!(m.specifier_prefix(), "@acme/widgets/");
    assert_eq!(m.url_prefix(), "/@acme/widgets/");
    assert_eq!(m.dir(), scoped.join("src"));

    // (2) no package.json → the directory basename.
    let plain = tmp.path().join("plain");
    std::fs::create_dir_all(&plain).unwrap();
    assert_eq!(Mount::from_dir(&plain).specifier_prefix(), "plain/");

    // (3) a caller-given name wins over the manifest's (npm's `file:`/alias rule).
    let given = Mount::from_dir(&scoped)
        .specifier("counter/")
        .url("/counter/");
    assert_eq!(given.specifier_prefix(), "counter/");
    assert_eq!(given.url_prefix(), "/counter/");
}

#[test]
fn path_deps_become_key_named_mounts_registry_deps_become_specs() {
    // The compose example points at its siblings with `file:` path-deps; each becomes a
    // mount named by the dependency *key* (`counter`, `chart`) — not the target's own
    // package name. Registry deps (`lit`) stay vendoring specs.
    let (specs, mounts) = read_package_json(&examples().join("compose/web/package.json")).unwrap();

    let specifiers: BTreeSet<_> = mounts
        .iter()
        .map(|m| m.specifier_prefix().to_string())
        .collect();
    assert!(specifiers.contains("counter/"), "got {specifiers:?}");
    assert!(specifiers.contains("chart/"), "got {specifiers:?}");
    assert!(
        !specifiers.contains("lit/"),
        "lit is a registry dep, not a mount"
    );
    assert!(specs.iter().any(|s| s.name() == "lit"));

    // The `counter` mount targets the lit-element example's web dir (the `file:` target).
    let counter = mounts
        .iter()
        .find(|m| m.specifier_prefix() == "counter/")
        .unwrap();
    assert!(
        counter.dir().ends_with("lit-element/web"),
        "counter -> {:?}",
        counter.dir()
    );
}

#[test]
fn importmap_and_tsconfig_cover_the_same_specifiers() {
    // The drift guard: from a single mount set, the runtime import map and the editor
    // tsconfig `paths` must describe the *same* specifiers. A root mount (no specifier)
    // contributes to neither.
    let (_specs, mut mounts) =
        read_package_json(&examples().join("compose/web/package.json")).unwrap();
    mounts.push(Mount::root(examples().join("compose/web")));

    let importmap = Importmap::from_mounts(&mounts);
    let tsconfig = tsconfig_paths(&mounts, &examples());

    // from_mounts emits prefix specifiers (`counter/`); tsconfig emits globs (`counter/*`).
    let from_map: BTreeSet<String> = importmap.iter().map(|(k, _)| format!("{k}*")).collect();
    let from_tsconfig: BTreeSet<String> = tsconfig.as_object().unwrap().keys().cloned().collect();
    assert_eq!(from_map, from_tsconfig);
    assert!(from_map.contains("counter/*") && from_map.contains("chart/*"));
    // The root mount produced no entry on either side.
    assert_eq!(from_map.len(), mounts.len() - 1);
}
