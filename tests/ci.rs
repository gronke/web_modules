#![cfg(feature = "cli")]
//! End-to-end test for the `web-modules ci` subcommand — a pure-Rust `npm ci` (no npm).
//!
//! `npm-utils` already unit-tests the underlying `install::from_lockfile`; this covers
//! web-modules' own CLI layer: parse `ci <dir>`, resolve `<dir>/package-lock.json`, run the
//! install, and report the count. Network-gated (`#[ignore]`) — it fetches one frozen package from
//! the npm registry:
//!
//! ```text
//! cargo test --features cli --test ci -- --include-ignored
//! ```

use std::process::Command;

/// A minimal v3 `package-lock.json` pinning `ms@2.1.3` — a frozen package with a known sha512, so
/// the install really verifies integrity end to end. Mirrors npm-utils' own install fixture.
const LOCKFILE: &str = r#"{
  "name": "fixture",
  "lockfileVersion": 3,
  "packages": {
    "": { "name": "fixture", "dependencies": { "ms": "2.1.3" } },
    "node_modules/ms": {
      "version": "2.1.3",
      "resolved": "https://registry.npmjs.org/ms/-/ms-2.1.3.tgz",
      "integrity": "sha512-6FlzubTLZG3J2a/NVCAleEhjzq5oxgHyaCU9yYXvcLsvoVaHJq/s5xXI6/XXP6tz7R9xAOtHnSO/tXtF3WRTlA=="
    }
  }
}"#;

/// `web-modules ci <dir>` installs the lockfile's exact tree into `<dir>/node_modules/`,
/// driving the real binary the way a user does.
#[test]
#[ignore = "network: fetches ms@2.1.3 from the npm registry"]
fn ci_subcommand_installs_a_locked_tree() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path();
    std::fs::write(dir.join("package-lock.json"), LOCKFILE).unwrap();

    // Cargo sets CARGO_BIN_EXE_<bin> for the package's bin target (built here because `cli` is on),
    // so no extra dependency is needed to locate the executable.
    let output = Command::new(env!("CARGO_BIN_EXE_web-modules"))
        .args(["ci", dir.to_str().unwrap()])
        .output()
        .expect("run web-modules ci");

    assert!(
        output.status.success(),
        "ci failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        dir.join("node_modules/ms/package.json").is_file(),
        "ms was downloaded, integrity-verified and extracted into node_modules/"
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains("installed 1 package(s)"),
        "the CLI reports the install count; got: {stdout}"
    );
}
