//! Vendoring at the public boundary. The manifest-parsing split (registry → specs,
//! `file:` → mounts, `github:` → git) is offline; the real download/extract over each
//! [`Extract`] mode and [`Source::Git`] — the four staging shapes a real app needs — is
//! `#[ignore]`d behind the network (run with `--include-ignored`).

use std::collections::BTreeSet;

use web_modules::vendor::{
    read_package_json, specs_from_package_json, vendor, Extract, PackageSpec,
};

#[test]
fn specs_from_package_json_keeps_registry_and_github_skips_local_and_dev() {
    let tmp = tempfile::tempdir().unwrap();
    let pkg = tmp.path().join("package.json");
    std::fs::write(
        &pkg,
        r#"{
          "dependencies": {
            "lit": "^3",
            "feather": "github:feathericons/feather#v4.29.2",
            "local": "file:../local",
            "linked": "link:../linked"
          },
          "devDependencies": { "typescript": "^5" }
        }"#,
    )
    .unwrap();
    let names: BTreeSet<_> = specs_from_package_json(&pkg)
        .unwrap()
        .iter()
        .map(|s| s.name().to_string())
        .collect();
    assert!(names.contains("lit"), "registry range kept");
    assert!(names.contains("feather"), "github → git spec");
    assert!(!names.contains("local"), "file: skipped");
    assert!(!names.contains("linked"), "link: skipped");
    assert!(!names.contains("typescript"), "devDependencies not read");
}

#[test]
fn read_package_json_routes_registry_path_and_git() {
    let tmp = tempfile::tempdir().unwrap();
    let base = tmp.path();
    std::fs::create_dir_all(base.join("sibling")).unwrap();
    let pkg = base.join("package.json");
    std::fs::write(
        &pkg,
        r#"{
          "dependencies": {
            "lit": "^3",
            "sib": "file:./sibling",
            "feather": "github:feathericons/feather#v4",
            "ws": "workspace:*"
          }
        }"#,
    )
    .unwrap();
    let (specs, mounts) = read_package_json(&pkg).unwrap();
    let spec_names: BTreeSet<_> = specs.iter().map(|s| s.name().to_string()).collect();
    let mount_specs: BTreeSet<_> = mounts
        .iter()
        .map(|m| m.specifier_prefix().to_string())
        .collect();

    assert!(spec_names.contains("lit"), "registry → spec");
    assert!(spec_names.contains("feather"), "github → git spec");
    assert!(mount_specs.contains("sib/"), "file: → mount named by key");
    assert!(!spec_names.contains("sib"), "path-dep is not vended");
    assert!(!mount_specs.contains("ws/"), "workspace: skipped");

    let sib = mounts
        .iter()
        .find(|m| m.specifier_prefix() == "sib/")
        .unwrap();
    assert!(sib.dir().ends_with("sibling"));
}

// ---- network-gated: real download + extract over each staging shape ----

#[test]
#[ignore = "network: downloads from the npm registry"]
fn vendor_browser_assets_derives_importmap() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web_modules");
    let map = vendor(&root, "/web_modules", &[PackageSpec::npm("lit", "^3")]).unwrap();
    // Browser asset staged + the import map auto-derived from the package.json.
    assert!(root.join("lit/index.js").is_file());
    assert!(map.resolves("lit"));
}

#[test]
#[ignore = "network: downloads from the npm registry"]
fn vendor_full_keeps_non_browser_files() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("web_modules");
    // Full extraction keeps the whole archive — including files the default
    // browser-asset filter drops (e.g. the package's own package.json).
    vendor(
        &root,
        "/web_modules",
        &[PackageSpec::npm("bootstrap", "^5")
            .extract(Extract::Full)
            .no_imports()],
    )
    .unwrap();
    assert!(root.join("bootstrap/package.json").is_file());
    assert!(root.join("bootstrap/scss/bootstrap.scss").is_file());
}

#[test]
#[ignore = "network: downloads a GitHub archive"]
fn vendor_single_file_from_github_archive() {
    let tmp = tempfile::tempdir().unwrap();
    let root = tmp.path().join("assets");
    // A GitHub (non-npm) source, extracting exactly one *committed* file, renamed. (The
    // top-level `feather-<ref>/` archive prefix is stripped, so `from` is repo-relative;
    // built artifacts like `dist/` are absent from a source archive.)
    vendor(
        &root,
        "/assets",
        &[PackageSpec::git("feathericons/feather", "v4.29.2")
            .dir("feather")
            .extract(Extract::File {
                from: "icons/activity.svg".into(),
                to: "feather-activity.svg".into(),
            })
            .no_imports()],
    )
    .unwrap();
    assert!(root.join("feather/feather-activity.svg").is_file());
    // Exactly the one requested file — siblings aren't pulled in.
    let count = std::fs::read_dir(root.join("feather")).unwrap().count();
    assert_eq!(count, 1, "single-file extract stages only its one target");
}
