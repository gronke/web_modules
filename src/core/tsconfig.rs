//! Both directions of a TypeScript `tsconfig`: generating one, and reading one a
//! dependency brought with it.
//!
//! [`TsConfig`] is the reading side — the fields that decide where a package's compiler
//! output lands and how it is emitted, parsed from the JSONC the format actually is.
//! The rest generates `paths` from a set of [`Mount`]s: the editor / `tsc` side of import
//! resolution, co-generated from the **same** mount set as the runtime
//! [`Importmap`](crate::importmap::Importmap::from_mounts) so the two can't drift.
//!
//! For a mount with specifier `@module/contacts/` and dir `modules/contacts/web/src`,
//! this emits `"@module/contacts/*": ["./modules/contacts/web/src/*"]` (dir relative
//! to `base`, typically the workspace root). Pair with [`tsconfig_node_modules_paths`]
//! for the third-party `node_modules` paths (derived from a `package.json`) and merge
//! both into one `compilerOptions.paths`.

use std::path::Path;

use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::mount::Mount;
use crate::{Error, Result};

/// Build the `compilerOptions.paths` object resolving each mount's specifier to its
/// source dir (relative to `base`). Mounts without a specifier (root mounts) are
/// skipped. Keys/values are sorted for byte-stable output.
pub fn tsconfig_paths(mounts: &[Mount], base: &Path) -> Value {
    let mut paths = Map::new();
    for m in mounts {
        let spec = m.specifier_prefix();
        if spec.is_empty() {
            continue;
        }
        // `@module/x/` → `@module/x/*` ; `lib/` → `lib/*`.
        let key = format!("{}*", spec);
        let target = format!("{}/*", relative_dir(base, m.dir()));
        paths.insert(key, json!([target]));
    }
    Value::Object(paths)
}

/// Write a base `tsconfig.json` whose `compilerOptions.paths` is
/// [`tsconfig_paths`], creating parent directories. A starting point for a host
/// that has no other compiler options to merge.
pub fn write_tsconfig_base(mounts: &[Mount], base: &Path, path: &Path) -> Result<()> {
    let doc = json!({
        "compilerOptions": {
            "moduleResolution": "bundler",
            "paths": tsconfig_paths(mounts, base),
        }
    });
    let json = serde_json::to_string_pretty(&doc).map_err(|e| Error::Compose(e.to_string()))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, json)?;
    Ok(())
}

/// Build the `compilerOptions.paths` object resolving each third-party npm package
/// declared in `package_json` to its `./node_modules/<pkg>` location (plus a `<pkg>/*`
/// subpath glob). The package set is read via
/// [`specs_from_package_json`](crate::vendor::specs_from_package_json), so it honors the
/// `web_modules.webDependencies` whitelist and skips local (`file:`/`workspace:`) deps;
/// the editor then resolves exactly the packages the build vendors. Compose the result
/// with [`tsconfig_paths`] (first-party mounts) into one `paths` map.
///
/// `node_modules` is assumed to sit beside the `tsconfig.json` (the usual layout), so the
/// emitted values are `./node_modules/<pkg>`. Keys are sorted for byte-stable output.
pub fn tsconfig_node_modules_paths(package_json: &Path) -> Result<Value> {
    let specs = crate::vendor::specs_from_package_json(package_json)?;
    let mut paths = Map::new();
    for spec in &specs {
        let name = spec.name();
        paths.insert(name.to_string(), json!([format!("./node_modules/{name}")]));
        paths.insert(
            format!("{name}/*"),
            json!([format!("./node_modules/{name}/*")]),
        );
    }
    Ok(Value::Object(paths))
}

/// `dir` relative to `base` as a `./`-prefixed, forward-slash path. Falls back to
/// the dir verbatim when it isn't under `base`.
fn relative_dir(base: &Path, dir: &Path) -> String {
    let rel = dir.strip_prefix(base).unwrap_or(dir);
    let s = rel.to_string_lossy().replace('\\', "/");
    if s.is_empty() {
        ".".to_string()
    } else if rel.is_absolute() {
        s
    } else {
        format!("./{s}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn paths_from_mounts_are_specifier_to_relative_dir() {
        let base = Path::new("/work");
        let mounts = [
            Mount::new("contacts", "/work/modules/contacts/web/src")
                .specifier("@module/contacts/")
                .url("/modules/contacts/"),
            Mount::new("lib", "/work/packages/frontend/web/src/lib"),
            // root mount contributes nothing
            Mount::root("/work/packages/frontend/web/src"),
        ];
        let paths = tsconfig_paths(&mounts, base);
        let obj = paths.as_object().unwrap();
        assert_eq!(
            obj["@module/contacts/*"],
            json!(["./modules/contacts/web/src/*"])
        );
        assert_eq!(obj["lib/*"], json!(["./packages/frontend/web/src/lib/*"]));
        assert_eq!(obj.len(), 2, "root mount has no specifier → no path entry");
    }

    #[test]
    fn write_base_roundtrips() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path();
        let mounts = [Mount::new("ui", base.join("ui/src"))];
        let out = base.join("tsconfig.base.json");
        write_tsconfig_base(&mounts, base, &out).unwrap();
        let written: Value = serde_json::from_slice(&std::fs::read(&out).unwrap()).unwrap();
        assert_eq!(
            written["compilerOptions"]["paths"]["ui/*"],
            json!(["./ui/src/*"])
        );
    }

    #[test]
    fn node_modules_paths_from_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(
            &pkg,
            r#"{ "dependencies": { "lit": "^3", "@lit/context": "1.1.6", "jose": "6.2.3" } }"#,
        )
        .unwrap();
        let paths = tsconfig_node_modules_paths(&pkg).unwrap();
        let obj = paths.as_object().unwrap();
        assert_eq!(obj["lit"], json!(["./node_modules/lit"]));
        assert_eq!(obj["lit/*"], json!(["./node_modules/lit/*"]));
        // Scoped packages are emitted verbatim.
        assert_eq!(obj["@lit/context"], json!(["./node_modules/@lit/context"]));
        assert_eq!(
            obj["@lit/context/*"],
            json!(["./node_modules/@lit/context/*"])
        );
        assert_eq!(obj["jose"], json!(["./node_modules/jose"]));
        assert_eq!(obj.len(), 6, "3 packages × (bare + /*)");
    }

    #[test]
    fn node_modules_paths_honor_webdependencies_whitelist() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        // `pg` is server-only; the webDependencies whitelist keeps it out of the editor set.
        std::fs::write(
            &pkg,
            r#"{ "dependencies": { "lit": "^3", "pg": "^8" },
                "web_modules": { "webDependencies": ["lit"] } }"#,
        )
        .unwrap();
        let paths = tsconfig_node_modules_paths(&pkg).unwrap();
        let obj = paths.as_object().unwrap();
        assert!(obj.contains_key("lit"));
        assert!(
            !obj.contains_key("pg"),
            "pg is not in webDependencies → no tsconfig path"
        );
        assert_eq!(obj.len(), 2, "only lit (bare + /*)");
    }
}

/// A package's `tsconfig.json`, in the fields that decide where its compiler output lands
/// and how that output behaves.
///
/// Read from the JSONC the format really is: `tsc` accepts comments and trailing commas,
/// which `serde_json` alone rejects. Unknown fields are ignored — a dependency's config
/// carries plenty this crate has no use for.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TsConfig {
    /// `compilerOptions`, absent meaning every option is at its own default.
    pub compiler_options: CompilerOptions,
    /// `include` globs, or file paths — the format permits either.
    pub include: Vec<String>,
    /// `files`: individual inputs, never globs.
    pub files: Vec<String>,
    /// `exclude` globs, subtracted from the inputs.
    pub exclude: Vec<String>,
    /// Present when the config inherits from another. A string or, since TypeScript 5, an
    /// array of them; kept as a value because only its presence matters here.
    pub extends: Option<Value>,
}

/// The `compilerOptions` this crate reads. Emit-relevant options are here so that a
/// vendored package can be compiled the way its own build would compile it.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct CompilerOptions {
    /// Where output goes; absent means beside the sources.
    pub out_dir: Option<String>,
    /// The input root output paths are made relative to.
    pub root_dir: Option<String>,
    /// The emit target, which decides the `useDefineForClassFields` default and, without
    /// an explicit `module`, the module format.
    pub target: Option<String>,
    /// The module format the package emits.
    pub module: Option<String>,
    /// ES-2022 *define* semantics for class fields, rather than assignment.
    pub use_define_for_class_fields: Option<bool>,
    /// Legacy (stage-2) decorators.
    pub experimental_decorators: Option<bool>,
    /// `design:type` metadata emission, which needs a decorator runtime.
    pub emit_decorator_metadata: Option<bool>,
    /// Rewrite a relative import's `.ts`/`.mts`/`.cts` extension to the emitted one, which
    /// a package writing `./util.ts` in its sources needs for its output to resolve.
    pub rewrite_relative_import_extensions: Option<bool>,
    /// JSX transform mode, and the factories it resolves through. Read to be refused: a
    /// `.tsx` source is compiled, but none of these are honoured.
    pub jsx: Option<String>,
    /// The module a JSX factory is imported from.
    pub jsx_import_source: Option<String>,
    /// An explicit element factory.
    pub jsx_factory: Option<String>,
    /// An explicit fragment factory.
    pub jsx_fragment_factory: Option<String>,
    /// Alias resolution root.
    pub base_url: Option<String>,
    /// Alias table.
    pub paths: Option<Value>,
}

/// Where a package's compiler output goes, and which directory its sources are rooted at —
/// the two facts needed to reproduce `tsc`'s own file mapping.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceLayout {
    /// `rootDir` as declared, or `None` when the config leaves it to be inferred from the
    /// input files — which is what [`common_input_dir`] does, once those are known.
    pub root: Option<String>,
    /// `outDir`, or the package root, which is compiling in place.
    pub out: String,
}

impl TsConfig {
    /// Parse a `tsconfig.json`'s text.
    ///
    /// Comments and trailing commas are accepted, as `tsc` accepts them.
    pub fn parse(raw: &str) -> Result<Self> {
        // Deserializes straight into the struct: the parser tolerates the JSONC, serde
        // takes the fields, and an empty document is a config with every default.
        let parsed: Option<Self> = jsonc_parser::parse_to_serde_value(raw, &Default::default())
            .map_err(|e| Error::Vendor(format!("tsconfig.json does not parse: {e}")))?;
        Ok(parsed.unwrap_or_default())
    }

    /// Read `<dir>/tsconfig.json`, or `Ok(None)` when the directory carries none.
    pub fn load(dir: &Path) -> Result<Option<Self>> {
        let path = dir.join("tsconfig.json");
        let Ok(raw) = std::fs::read_to_string(&path) else {
            return Ok(None);
        };
        Self::parse(&raw)
            .map(Some)
            .map_err(|e| Error::Vendor(format!("{}: {e}", path.display())))
    }

    /// Whether class fields are *defined*, as ES 2022 does, rather than assigned.
    ///
    /// `useDefineForClassFields` decides it when set. Otherwise `tsc`'s own rule applies:
    /// define from target ES 2022 upwards, assign below it — which is why a package
    /// targeting ES 2019 needs assignment semantics to be emitted the way it builds.
    pub fn defines_class_fields(&self) -> bool {
        if let Some(explicit) = self.compiler_options.use_define_for_class_fields {
            return explicit;
        }
        let Some(target) = &self.compiler_options.target else {
            return false;
        };
        let target = target.to_ascii_lowercase();
        if target == "esnext" {
            return true;
        }
        target
            .strip_prefix("es")
            .and_then(|year| year.parse::<u32>().ok())
            .is_some_and(|year| year >= 2022)
    }

    /// The layout to reproduce: `outDir`, and `rootDir` when the config states one.
    ///
    /// Both are refused unless they are package-relative. They come from a config the
    /// package itself shipped, which for a vendored dependency means from a downloaded
    /// archive: `rootDir: ".."` would otherwise name the directory *holding* the package,
    /// and everything done to a root — walking it, writing beside it, deleting it — would
    /// happen there. An absolute path is refused rather than read as relative, since
    /// compiling to a different layout than the one asked for is its own kind of wrong.
    ///
    /// A `rootDir` the config leaves out is not guessed here. `tsc` derives it from the
    /// program's own input files, so [`common_input_dir`] does that once they are known.
    pub fn layout(&self) -> Result<SourceLayout> {
        let contained = |option: &str, value: &str| -> Result<String> {
            if value.is_empty() {
                return Ok(String::new());
            }
            if Path::new(value).is_absolute() {
                return Err(Error::Vendor(format!(
                    "tsconfig.json `{option}` is `{value}`, which is absolute"
                )));
            }
            npm_utils::path_safety::ensure_within(value).map_err(|_| {
                Error::Vendor(format!(
                    "tsconfig.json `{option}` is `{value}`, which leaves the package"
                ))
            })?;
            // Package-relative and in one spelling: `.`, `./src` and `src/` all name what
            // `src` names, and `.` names the package root, which is the empty prefix.
            Ok(Path::new(value)
                .components()
                .filter_map(|component| match component {
                    std::path::Component::Normal(segment) => Some(segment.to_string_lossy()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("/"))
        };

        Ok(SourceLayout {
            root: match self.compiler_options.root_dir.as_deref() {
                Some(root) => Some(contained("rootDir", root)?),
                None => None,
            },
            out: match self.compiler_options.out_dir.as_deref() {
                Some(out) => contained("outDir", out)?,
                None => String::new(),
            },
        })
    }

    /// Whether the config *declares* CommonJS output, which this path does not produce.
    ///
    /// An explicit `module` says so directly. `node16` and its successors decide per file
    /// from the nearest `package.json`, so they count here too: the answer cannot be had
    /// from the config alone. Without a `module`, only a target `tsc` would emit CommonJS
    /// for counts — ES 3 and ES 5.
    ///
    /// A config naming neither is not read as CommonJS, even though `tsc`'s own defaults
    /// would make it so. Nothing has been stated to contradict, a package that ships
    /// browser-consumable output essentially always states it, and refusing silence would
    /// refuse every dependency that carries no `tsconfig.json` at all.
    pub fn emits_commonjs(&self) -> bool {
        match self.compiler_options.module.as_deref() {
            Some(module) => matches!(
                module.to_ascii_lowercase().as_str(),
                "commonjs" | "umd" | "amd" | "system" | "node16" | "node18" | "nodenext"
            ),
            None => matches!(
                self.compiler_options
                    .target
                    .as_deref()
                    .map(str::to_ascii_lowercase)
                    .as_deref(),
                Some("es3" | "es5")
            ),
        }
    }

    /// Whether `rel` — a path relative to the package — is a **root file** of the program
    /// this config describes.
    ///
    /// `files` and `include` choose root files; they are not the whole program, because a
    /// file a root imports is in it too. `exclude` only filters what `include` discovers:
    /// it does not remove a file `files` names, and it does not remove a file that arrives
    /// through an import. With neither field, every file is a root, as `tsc` does.
    ///
    /// The directories `tsc` excludes whether or not the config lists them are excluded
    /// here too, along with the output directory, which is never its own input.
    pub fn is_root_file(&self, rel: &Path) -> bool {
        let path = rel.to_string_lossy().replace('\\', "/");
        let path = path.trim_start_matches("./");

        // An explicit `files` entry is a root whatever `exclude` says.
        if self
            .files
            .iter()
            .any(|file| file.trim_start_matches("./").trim_matches('/') == path)
        {
            return true;
        }

        const ALWAYS_EXCLUDED: [&str; 3] = ["node_modules", "bower_components", "jspm_packages"];
        if ALWAYS_EXCLUDED.iter().any(|dir| glob_matches(dir, path)) {
            return false;
        }
        if let Some(out) = self.compiler_options.out_dir.as_deref() {
            if !out.trim_matches('/').is_empty() && glob_matches(out, path) {
                return false;
            }
        }
        if self.exclude.iter().any(|glob| glob_matches(glob, path)) {
            return false;
        }
        if self.files.is_empty() && self.include.is_empty() {
            return true;
        }
        self.include.iter().any(|glob| glob_matches(glob, path))
    }
}

/// The directory every input file shares: output paths are made relative to it, so it is
/// read off the program rather than off the patterns that discovered it.
///
/// `["src/deep/index.ts", "src/deep/util.ts"]` is rooted at `src/deep`, not at `src`.
///
/// This is what TypeScript up to 5.9 infers a missing `rootDir` to be. TypeScript 6 changed
/// it: with a `tsconfig.json` present the default became the config's own directory, and the
/// common-directory rule survives only for a CLI compile without one. A vendored package
/// does not say which compiler built it, so the older rule is the one used here — it is the
/// rule that produced the layouts published packages actually ship, and the manifest check
/// after compiling is what catches a package for which it is wrong. A package that wants
/// neither guess states `rootDir`.
pub fn common_input_dir<'a>(inputs: impl IntoIterator<Item = &'a Path>) -> String {
    let dirs: Vec<String> = inputs
        .into_iter()
        .map(|path| {
            path.parent()
                .map(|dir| dir.to_string_lossy().replace('\\', "/"))
                .unwrap_or_default()
        })
        .collect();
    common_dir(&dirs)
}

/// The longest directory prefix every entry shares, segment by segment. No entries, or
/// nothing in common, yields the empty prefix — the package root.
fn common_dir(dirs: &[String]) -> String {
    let mut shared: Option<Vec<&str>> = None;
    for dir in dirs {
        let segs: Vec<&str> = dir.split('/').filter(|s| !s.is_empty()).collect();
        shared = Some(match shared {
            None => segs,
            Some(common) => common
                .iter()
                .zip(segs.iter())
                .take_while(|(a, b)| a == b)
                .map(|(a, _)| *a)
                .collect(),
        });
    }
    shared.unwrap_or_default().join("/")
}

#[cfg(test)]
mod read_tests {
    use super::*;

    /// The format's own leniency: comments, trailing commas, and a quote inside a string.
    #[test]
    fn parses_the_jsonc_a_tsconfig_really_is() {
        let raw = r#"{
            // where the build goes
            "compilerOptions": {
                "outDir": "lib",   /* beside the sources */
                "rootDir": "src",
                "target": "ES2019",
            },
            "include": ["src/**/*"],
        }"#;
        let cfg = TsConfig::parse(raw).unwrap();
        assert_eq!(cfg.compiler_options.out_dir.as_deref(), Some("lib"));
        assert_eq!(cfg.compiler_options.root_dir.as_deref(), Some("src"));
        assert_eq!(cfg.include, vec!["src/**/*"]);

        // An escaped quote does not end the string it is in.
        let cfg =
            TsConfig::parse(r#"{ "compilerOptions": { "outDir": "a\"b" } } // tail"#).unwrap();
        assert_eq!(cfg.compiler_options.out_dir.as_deref(), Some("a\"b"));
    }

    #[test]
    fn an_empty_or_absent_config_is_all_defaults() {
        assert_eq!(TsConfig::parse("{}").unwrap(), TsConfig::default());
        assert_eq!(TsConfig::parse("").unwrap(), TsConfig::default());
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(TsConfig::load(dir.path()).unwrap(), None);
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        assert_eq!(
            TsConfig::load(dir.path()).unwrap(),
            Some(TsConfig::default())
        );
    }

    /// `tsc` switches the default on the target, so a package targeting ES 2019 is built
    /// with assignment semantics whether or not it says so.
    #[test]
    fn class_field_semantics_follow_the_target_unless_stated() {
        let define = |raw: &str| TsConfig::parse(raw).unwrap().defines_class_fields();
        assert!(!define("{}"), "no target: assignment, as tsc does");
        assert!(!define(r#"{"compilerOptions":{"target":"ES2019"}}"#));
        assert!(!define(r#"{"compilerOptions":{"target":"es2021"}}"#));
        assert!(define(r#"{"compilerOptions":{"target":"ES2022"}}"#));
        assert!(define(r#"{"compilerOptions":{"target":"ESNext"}}"#));
        // An explicit setting wins over the target either way.
        assert!(define(
            r#"{"compilerOptions":{"target":"ES2019","useDefineForClassFields":true}}"#
        ));
        assert!(!define(
            r#"{"compilerOptions":{"target":"ESNext","useDefineForClassFields":false}}"#
        ));
    }

    #[test]
    fn root_dir_and_out_dir_are_taken_as_given() {
        let cfg = TsConfig::parse(r#"{"compilerOptions":{"rootDir":"./src/","outDir":"./lib"}}"#)
            .unwrap();
        assert_eq!(
            cfg.layout().unwrap(),
            SourceLayout {
                root: Some("src".into()),
                out: "lib".into()
            }
        );
        // Every spelling of the package root is the empty prefix.
        for root in [".", "./", "", "./."] {
            let cfg = TsConfig::parse(&format!(
                r#"{{"compilerOptions":{{"rootDir":"{root}","outDir":"lib"}}}}"#
            ))
            .unwrap();
            assert_eq!(cfg.layout().unwrap().root, Some(String::new()), "{root:?}");
        }
        // No `rootDir` is not the package root: it is inferred from the program's inputs.
        let cfg = TsConfig::parse(r#"{"compilerOptions":{"outDir":"lib"},"include":["src/**/*"]}"#)
            .unwrap();
        assert_eq!(cfg.layout().unwrap().root, None);
    }

    /// `tsc` reads a missing `rootDir` off the input files, not off the patterns that
    /// selected them: with every input under `src/deep`, output is relative to `src/deep`.
    #[test]
    fn the_inferred_root_is_the_directory_the_inputs_share() {
        let dir = |paths: &[&str]| {
            common_input_dir(paths.iter().map(std::path::Path::new).collect::<Vec<_>>())
        };
        assert_eq!(dir(&["src/deep/index.ts", "src/deep/util.ts"]), "src/deep");
        assert_eq!(dir(&["src/index.ts", "src/deep/util.ts"]), "src");
        assert_eq!(dir(&["src/index.ts", "lib/other.ts"]), "");
        assert_eq!(dir(&["index.ts"]), "");
        assert_eq!(dir(&[]), "");
    }

    /// A vendored package's config came out of a downloaded archive, so its paths are
    /// untrusted input: `rootDir: ".."` names the directory holding the package, and the
    /// compile walks, writes beside, and finally deletes whatever the root turns out to be.
    #[test]
    fn a_layout_component_that_leaves_the_package_is_refused() {
        for body in [
            r#"{"compilerOptions":{"rootDir":".."}}"#,
            r#"{"compilerOptions":{"rootDir":"../../elsewhere"}}"#,
            r#"{"compilerOptions":{"rootDir":"src/../.."}}"#,
            r#"{"compilerOptions":{"outDir":"../lib"}}"#,
            r#"{"compilerOptions":{"outDir":"../../../etc"}}"#,
            r#"{"compilerOptions":{"rootDir":"src","outDir":"../out"}}"#,
        ] {
            let Err(err) = TsConfig::parse(body).unwrap().layout() else {
                panic!("expected a refusal for {body}");
            };
            assert!(
                err.to_string().contains("leaves the package"),
                "says why: {err}"
            );
        }
    }

    /// An absolute path is refused rather than read as relative: compiling to a layout the
    /// dependency did not ask for is its own kind of wrong, even when it is contained.
    #[test]
    fn an_absolute_layout_component_is_refused() {
        for body in [
            r#"{"compilerOptions":{"rootDir":"/etc"}}"#,
            r#"{"compilerOptions":{"outDir":"/tmp/out"}}"#,
        ] {
            let Err(err) = TsConfig::parse(body).unwrap().layout() else {
                panic!("expected a refusal for {body}");
            };
            assert!(err.to_string().contains("absolute"), "says why: {err}");
        }
    }

    /// `files` and `include` name root files; `exclude` only filters what `include` finds,
    /// so it removes neither a `files` entry nor a file that arrives through an import.
    #[test]
    fn root_files_are_discovered_not_filtered() {
        let root = |body: &str, path: &str| {
            TsConfig::parse(body)
                .unwrap()
                .is_root_file(std::path::Path::new(path))
        };

        // Neither field: everything is a root, as tsc does.
        assert!(root("{}", "src/index.ts"));
        assert!(root("{}", "src/dev/scratch.ts"));

        // `files` is exact, and `exclude` does not take it away.
        let files = r#"{"files":["src/index.ts"]}"#;
        assert!(root(files, "src/index.ts"));
        assert!(!root(files, "src/dev.ts"));
        assert!(
            root(
                r#"{"files":["src/index.ts"],"exclude":["src/index.ts"]}"#,
                "src/index.ts"
            ),
            "exclude only filters include"
        );

        // `include` matches globs, and a plain entry stands for its whole subtree.
        let include = r#"{"include":["src/index.ts"]}"#;
        assert!(root(include, "src/index.ts"));
        assert!(!root(include, "src/sibling.ts"));
        assert!(root(r#"{"include":["src"]}"#, "src/deep/inner.ts"));
        assert!(root(r#"{"include":["src/**/*"]}"#, "src/deep/inner.ts"));
        assert!(root(r#"{"include":["src/*.ts"]}"#, "src/top.ts"));
        assert!(!root(r#"{"include":["src/*.ts"]}"#, "src/deep/inner.ts"));

        // `exclude` subtracts from `include`, and several includes are a union.
        let excluded = r#"{"include":["src/**/*"],"exclude":["src/dev/**"]}"#;
        assert!(root(excluded, "src/index.ts"));
        assert!(!root(excluded, "src/dev/scratch.ts"));
        let union = r#"{"include":["lib/a/**","lib/b/**"]}"#;
        assert!(root(union, "lib/a/one.ts"));
        assert!(root(union, "lib/b/two.ts"));
        assert!(!root(union, "lib/c/three.ts"), "not their ancestor");

        // What tsc excludes whether or not the config says so, plus the output itself.
        assert!(!root("{}", "node_modules/dep/index.ts"));
        assert!(!root("{}", "jspm_packages/dep/index.ts"));
        assert!(!root(
            r#"{"compilerOptions":{"outDir":"lib"}}"#,
            "lib/index.js"
        ));
    }

    /// `tsc` takes CommonJS below ES 2015 and ES modules above it, unless told otherwise.
    #[test]
    fn the_module_format_follows_module_then_target() {
        let cjs = |body: &str| TsConfig::parse(body).unwrap().emits_commonjs();
        assert!(
            !cjs("{}"),
            "a config stating neither is not read as CommonJS"
        );
        assert!(cjs(r#"{"compilerOptions":{"target":"ES5"}}"#));
        assert!(cjs(r#"{"compilerOptions":{"target":"ES3"}}"#));
        assert!(!cjs(r#"{"compilerOptions":{"target":"ES2019"}}"#));
        assert!(!cjs(r#"{"compilerOptions":{"target":"ESNext"}}"#));
        // An explicit module wins either way.
        assert!(cjs(
            r#"{"compilerOptions":{"target":"ESNext","module":"CommonJS"}}"#
        ));
        assert!(!cjs(
            r#"{"compilerOptions":{"target":"ES5","module":"ES2020"}}"#
        ));
        assert!(!cjs(r#"{"compilerOptions":{"module":"Preserve"}}"#));
        // Per-file formats cannot be settled from the config alone.
        for module in ["node16", "node18", "NodeNext", "umd", "amd", "system"] {
            assert!(
                cjs(&format!(
                    r#"{{"compilerOptions":{{"target":"ESNext","module":"{module}"}}}}"#
                )),
                "{module}"
            );
        }
    }

    #[test]
    fn globs_match_the_forms_a_tsconfig_uses() {
        assert!(glob_matches("src/**/*", "src/a/b/c.ts"));
        assert!(glob_matches("./src/**/*.ts", "src/a.ts"));
        assert!(glob_matches("**/*.ts", "deep/nested/a.ts"));
        assert!(glob_matches("src/?.ts", "src/a.ts"));
        assert!(!glob_matches("src/?.ts", "src/ab.ts"));
        assert!(!glob_matches("src/*", "other/a.ts"));
        assert!(glob_matches("src/*.spec.ts", "src/a.spec.ts"));
        assert!(!glob_matches("src/*.spec.ts", "src/a.ts"));
    }
}

/// Whether a `tsconfig` `include`/`exclude` glob matches a package-relative path.
///
/// The format's own wildcards: `*` for any run within one segment, `?` for one character,
/// `**` for any number of segments. An entry with no wildcard names a file or a directory,
/// and a directory stands for everything under it — `include: ["src"]` is `src/**/*`.
fn glob_matches(glob: &str, path: &str) -> bool {
    let glob = glob.trim_start_matches("./").trim_matches('/');
    if !glob.contains(['*', '?']) {
        return path == glob || path.starts_with(&format!("{glob}/"));
    }
    let segments = |s: &str| -> Vec<String> { s.split('/').map(str::to_string).collect() };
    matches_segments(&segments(glob), &segments(path))
}

/// Segment-wise match, where `**` stands for any number of segments including none.
fn matches_segments(glob: &[String], path: &[String]) -> bool {
    match glob.split_first() {
        None => path.is_empty(),
        Some((first, rest)) if first == "**" => {
            (0..=path.len()).any(|skip| matches_segments(rest, &path[skip..]))
        }
        Some((first, rest)) => match path.split_first() {
            Some((name, tail)) if segment_matches(first, name) => matches_segments(rest, tail),
            _ => false,
        },
    }
}

/// One segment against one name: `*` any run, `?` one character, neither crossing a `/`.
fn segment_matches(glob: &str, name: &str) -> bool {
    let (glob, name): (Vec<char>, Vec<char>) = (glob.chars().collect(), name.chars().collect());
    // Walk both, remembering the last `*` so a failed tail can backtrack onto it.
    let (mut g, mut n) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while n < name.len() {
        if g < glob.len() && (glob[g] == '?' || glob[g] == name[n]) {
            g += 1;
            n += 1;
        } else if g < glob.len() && glob[g] == '*' {
            star = Some((g, n));
            g += 1;
        } else if let Some((sg, sn)) = star {
            g = sg + 1;
            n = sn + 1;
            star = Some((sg, sn + 1));
        } else {
            return false;
        }
    }
    glob[g..].iter().all(|c| *c == '*')
}
