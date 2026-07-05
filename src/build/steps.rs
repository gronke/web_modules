//! Preflight-capable pipeline steps: every build stage states what it WOULD emit for a
//! given source file, before anything is written.
//!
//! The driver walks the source roots exactly once, offers every file to every enabled
//! step, and collects the claims — which output-relative path each step would produce.
//! The build resolves the claims up front: duplicate output paths are detected before
//! the first write, and each output path is then written by exactly one winner, so
//! precedence is an explicit policy instead of an emergent write order.
//!
//! Feature and toggle variance lives in [`enabled_steps`] alone. The same step list
//! drives the preflight and the emission, and the dev server builds the identical list
//! for its startup duplicate warnings — so what the preflight predicts is, by
//! construction, what the pipeline does. Each step's `claim` logic lives next to the
//! processor it describes, so classification cannot drift from emission.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use crate::module_graph::ModuleImport;
use crate::Result;

/// Within-root precedence, lowest wins: a `*.tera` beats a literal file, which beats a
/// transformed sibling (a copied `app.js` over the output of `app.ts`) — the order the
/// dev server probes candidates in, so `dev` and `build` resolve a shadowed path alike.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) enum Rank {
    Tera = 0,
    Static = 1,
    Transform = 2,
}

/// What one step would emit for one source file.
#[derive(Clone, Debug)]
pub(crate) struct Claim {
    /// The output-relative path the emission would write.
    pub out_rel: PathBuf,
    /// Orders same-rank claims on one target (TypeScript: `ts` = 0, `tsx` = 1,
    /// `mts` = 2 — dev's probe order). Zero for every other step.
    pub tiebreak: u8,
}

/// A stage that can say what it would emit without emitting.
pub(crate) trait Preflight {
    /// Stable name for conflict reports (e.g. "static copy", "TypeScript transform").
    fn name(&self) -> &'static str;
    fn rank(&self) -> Rank;
    /// The claim this step makes on the source file at root-relative `rel`, or `None`
    /// when the file is not its input (wrong extension, `_` partial, `.d.ts`, …).
    /// Steps need not consult the reject list: the driver drops every claim whose
    /// *target* is rejected, centrally. (The static step still applies it to its
    /// sources, because a raw copy ships the source file itself.)
    fn claim(&self, rel: &Path) -> Option<Claim>;
}

/// A preflight-capable stage that can also emit one file for real.
pub(crate) trait Step: Preflight {
    /// Emit the source at `src` (absolute; `rel` names it in messages) to `dest`,
    /// returning what the module graph should record. The caller creates `dest`'s
    /// parent directories. `cx` carries the generated import map — final only by the
    /// time the Tera step runs, which is the only step reading it.
    fn emit(&self, cx: &EmitCx<'_>, src: &Path, rel: &Path, dest: &Path) -> Result<Emitted>;
}

/// Late-bound emission context: what is not known when the step list is built.
pub(crate) struct EmitCx<'a> {
    pub importmap: &'a crate::importmap::Importmap,
}

/// The module-graph contribution of one emission: `Some` for JavaScript-producing
/// emissions (a transform output, a copied `.js`/`.mjs`), `None` otherwise.
#[derive(Debug, Default)]
pub(crate) struct Emitted {
    pub imports: Option<Vec<ModuleImport>>,
}

/// Per-build tuning the steps need, threaded through [`enabled_steps`]. Fields exist
/// only for the features that use them; [`Default`] is the claims-only configuration
/// the dev server uses for its duplicate warnings.
#[derive(Default)]
pub(crate) struct StepConfig {
    #[cfg(feature = "typescript")]
    pub transpile: crate::typescript::TranspileOptions,
    #[cfg(feature = "scss")]
    pub scss_load_paths: Vec<PathBuf>,
}

/// The enabled steps: compiled-in features ∩ `Processors` toggles — the one place that
/// variance lives. Build and dev both construct their step list here, so the
/// preflighted pipeline is the pipeline that runs.
pub(crate) fn enabled_steps(
    processors: &crate::build::Processors,
    config: StepConfig,
) -> Vec<Box<dyn Step>> {
    let mut steps: Vec<Box<dyn Step>> = Vec::new();
    #[cfg(feature = "tera")]
    if processors.tera {
        steps.push(Box::new(TeraStep));
    }
    steps.push(Box::new(crate::static_files::StaticStep::new(
        processors.reject.clone(),
    )));
    #[cfg(feature = "typescript")]
    if processors.typescript {
        steps.push(Box::new(crate::typescript::TypeScriptStep::new(
            config.transpile,
        )));
    }
    #[cfg(feature = "scss")]
    if processors.scss {
        steps.push(Box::new(crate::scss::ScssStep::new(config.scss_load_paths)));
    }
    steps
}

/// Renders `x.y.tera` → `x.y` with the generated import map exposed as the
/// `importmap` template variable — the static counterpart of the dev server's
/// on-the-fly rendering. `tera::one_off` (via [`crate::templates`]) has no template
/// registry, so each file renders independently; the `_`-partial skip is a
/// convention, not an inheritance system.
#[cfg(feature = "tera")]
pub(crate) struct TeraStep;

#[cfg(feature = "tera")]
impl Preflight for TeraStep {
    fn name(&self) -> &'static str {
        "Tera template"
    }

    fn rank(&self) -> Rank {
        Rank::Tera
    }

    fn claim(&self, rel: &Path) -> Option<Claim> {
        let name = rel.file_name()?.to_str()?;
        let ext = rel.extension()?.to_str()?;
        if !ext.eq_ignore_ascii_case("tera") || name.starts_with('_') {
            return None;
        }
        // Drop the final `.tera`: `index.html.tera` → `index.html`, `page.tera` → `page`.
        Some(Claim {
            out_rel: rel.with_extension(""),
            tiebreak: 0,
        })
    }
}

#[cfg(feature = "tera")]
impl Step for TeraStep {
    fn emit(&self, cx: &EmitCx<'_>, src: &Path, rel: &Path, dest: &Path) -> Result<Emitted> {
        let mut ctx = crate::templates::Context::new();
        ctx.insert("importmap", &cx.importmap.to_script_tag());
        let rendered = crate::templates::render_file(src, &ctx)?;

        // A template can render JavaScript (`app.js.tera`); the result joins the
        // module graph like any other emitted JS, read from the rendered text before
        // the write. A rendered `.mjs` must parse — the browser would fail on it —
        // and a rendered `.js` that parses under neither goal warns, matching copies.
        let ext = dest.extension().and_then(|x| x.to_str()).unwrap_or("");
        let imports = if ["js", "mjs"].iter().any(|e| ext.eq_ignore_ascii_case(e)) {
            let module_only = ext.eq_ignore_ascii_case("mjs");
            let read = crate::module_graph::imports_from_source(&rendered, module_only).map_err(
                |reason| crate::Error::Build(format!("web-modules: {}: {reason}", rel.display())),
            )?;
            if !read.parsed {
                crate::static_files::build_warning(&format!(
                    "web-modules: {}: renders neither a module nor a classic script; \
                     its imports are not validated",
                    rel.display()
                ));
            }
            Some(read.imports)
        } else {
            None
        };
        std::fs::write(dest, rendered)?;
        Ok(Emitted { imports })
    }
}

/// One recorded claim: which step, from which root, on which source file, targeting
/// which output path.
#[derive(Debug)]
pub(crate) struct ClaimRecord {
    pub root: usize,
    pub rel: PathBuf,
    pub step: usize,
    pub rank: Rank,
    pub tiebreak: u8,
    pub out_rel: PathBuf,
}

impl ClaimRecord {
    /// The precedence key, lowest wins: earlier root, then rank (tera < static <
    /// transform), then the step's own tiebreak, then the source path for stability.
    fn precedence(&self) -> (usize, Rank, u8, &Path) {
        (self.root, self.rank, self.tiebreak, &self.rel)
    }
}

/// Every claim from one walk of the source roots, plus every visited path (the
/// `cargo:rerun-if-changed` set), the files that resolve outside their root, and
/// the walk problems that would make the preflight incomplete.
pub(crate) struct PreflightReport {
    claims: Vec<ClaimRecord>,
    walked: Vec<PathBuf>,
    escaping_sources: Vec<EscapingSource>,
    walk_errors: Vec<String>,
}

/// A source file whose canonical location is not under its canonical root — a
/// symlink pointing outside the tree. No step gets to claim such a file: the build
/// refuses to publish it, matching the dev server's canonical containment
/// (`contained_file`), which refuses to serve it.
#[derive(Debug)]
pub(crate) struct EscapingSource {
    pub root: usize,
    pub rel: PathBuf,
    /// Where the path actually resolves.
    pub target: PathBuf,
}

/// Walk each root once (following symlinks) and offer every file to every step.
/// Containment is canonical: each root resolves once, every file must resolve under
/// it, and one that does not is recorded as an [`EscapingSource`] instead of being
/// offered. In-root symlinks work; a link out of the tree never publishes.
/// Unreadable entries and unresolvable links land in `walk_errors` rather than being
/// silently dropped — a partial preflight would otherwise pass for a complete one.
/// The `reject` list guards emission centrally, by **target**: a claim on a rejected
/// output path is dropped (with a warning) no matter which step makes it, so a
/// template or a compiled source cannot materialize `.env` or `private.key` — the
/// same decision the dev server takes on the request path.
pub(crate) fn preflight(
    roots: &[PathBuf],
    steps: &[&dyn Preflight],
    reject: &crate::reject::Reject,
) -> PreflightReport {
    let mut report = PreflightReport {
        claims: Vec::new(),
        walked: Vec::new(),
        escaping_sources: Vec::new(),
        walk_errors: Vec::new(),
    };
    for (root_index, root) in roots.iter().enumerate() {
        let canonical_root = match std::fs::canonicalize(root) {
            Ok(canonical) => canonical,
            Err(e) => {
                report.walk_errors.push(format!("{}: {e}", root.display()));
                continue;
            }
        };
        for entry in walkdir::WalkDir::new(root).follow_links(true) {
            let entry = match entry {
                Ok(entry) => entry,
                Err(e) => {
                    report.walk_errors.push(e.to_string());
                    continue;
                }
            };
            let path = entry.path();
            report.walked.push(path.to_path_buf());
            if !path.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            let canonical = match std::fs::canonicalize(path) {
                Ok(canonical) => canonical,
                Err(e) => {
                    report.walk_errors.push(format!("{}: {e}", path.display()));
                    continue;
                }
            };
            if !canonical.starts_with(&canonical_root) {
                report.escaping_sources.push(EscapingSource {
                    root: root_index,
                    rel: rel.to_path_buf(),
                    target: canonical,
                });
                continue;
            }
            for (step_index, step) in steps.iter().enumerate() {
                if let Some(claim) = step.claim(rel) {
                    if reject.rejects_path(&claim.out_rel) {
                        crate::reject::warn_rejected(&claim.out_rel.display().to_string());
                        continue;
                    }
                    report.claims.push(ClaimRecord {
                        root: root_index,
                        rel: rel.to_path_buf(),
                        step: step_index,
                        rank: step.rank(),
                        tiebreak: claim.tiebreak,
                        out_rel: claim.out_rel,
                    });
                }
            }
        }
    }
    report
}

/// Two or more sources claiming one output path.
pub(crate) struct Conflict<'a> {
    pub out_rel: &'a Path,
    /// All claimants in precedence order — `[0]` is what `--skip-duplicates` ships.
    pub claimants: Vec<&'a ClaimRecord>,
}

impl PreflightReport {
    /// Claims grouped by output path (deterministic order), each group sorted by
    /// precedence, winner first.
    fn grouped(&self) -> BTreeMap<&Path, Vec<&ClaimRecord>> {
        let mut groups: BTreeMap<&Path, Vec<&ClaimRecord>> = BTreeMap::new();
        for claim in &self.claims {
            groups.entry(&claim.out_rel).or_default().push(claim);
        }
        for group in groups.values_mut() {
            group.sort_by_key(|c| c.precedence());
        }
        groups
    }

    /// Every output path claimed more than once, with all claimants (winner first).
    pub(crate) fn conflicts(&self) -> Vec<Conflict<'_>> {
        self.grouped()
            .into_iter()
            .filter(|(_, claimants)| claimants.len() > 1)
            .map(|(out_rel, claimants)| Conflict { out_rel, claimants })
            .collect()
    }

    /// Claims whose output path is not a purely normal relative path — a root, prefix,
    /// `.` or `..` component would let a write land outside the output directory. The
    /// bundled steps cannot produce one (their targets derive from walk-relative source
    /// paths, and a file name cannot contain a separator), but the invariant is
    /// enforced by the build rather than assumed of every step.
    pub(crate) fn escaping(&self) -> Vec<&ClaimRecord> {
        self.claims
            .iter()
            .filter(|claim| {
                !claim
                    .out_rel
                    .components()
                    .all(|c| matches!(c, std::path::Component::Normal(_)))
                    || claim.out_rel.as_os_str().is_empty()
            })
            .collect()
    }

    /// One claim per output path — the winner under the unified precedence — in
    /// output-path order.
    pub(crate) fn winners(&self) -> Vec<&ClaimRecord> {
        self.grouped().into_values().map(|group| group[0]).collect()
    }

    /// Whether any source claims `out_rel` — the fallback-index gate.
    pub(crate) fn claims_target(&self, out_rel: impl AsRef<Path>) -> bool {
        let out_rel = out_rel.as_ref();
        self.claims.iter().any(|c| c.out_rel == out_rel)
    }

    /// Every path the scan visited, directories included — the
    /// `cargo:rerun-if-changed` set.
    pub(crate) fn walked_paths(&self) -> &[PathBuf] {
        &self.walked
    }

    /// Files whose canonical location is outside their canonical source root.
    pub(crate) fn escaping_sources(&self) -> &[EscapingSource] {
        &self.escaping_sources
    }

    /// Walk problems — unreadable directories, unresolvable links. The preflight
    /// describes the complete output only when this is empty.
    pub(crate) fn walk_errors(&self) -> &[String] {
        &self.walk_errors
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A copy-like step: claims every file except `.src` sources, target = same path.
    struct CopyLike;

    impl Preflight for CopyLike {
        fn name(&self) -> &'static str {
            "copy"
        }
        fn rank(&self) -> Rank {
            Rank::Static
        }
        fn claim(&self, rel: &Path) -> Option<Claim> {
            let ext = rel.extension().and_then(|x| x.to_str()).unwrap_or("");
            (ext != "src").then(|| Claim {
                out_rel: rel.to_path_buf(),
                tiebreak: 0,
            })
        }
    }

    /// A transform-like step: claims `.src` files, target = `.txt` sibling.
    struct TransformLike;

    impl Preflight for TransformLike {
        fn name(&self) -> &'static str {
            "transform"
        }
        fn rank(&self) -> Rank {
            Rank::Transform
        }
        fn claim(&self, rel: &Path) -> Option<Claim> {
            let ext = rel.extension().and_then(|x| x.to_str()).unwrap_or("");
            (ext == "src").then(|| Claim {
                out_rel: rel.with_extension("txt"),
                tiebreak: 0,
            })
        }
    }

    fn scan(roots: &[PathBuf]) -> PreflightReport {
        preflight(
            roots,
            &[&CopyLike, &TransformLike],
            &crate::reject::Reject::none(),
        )
    }

    #[test]
    fn preflight_detects_a_within_root_conflict_and_ranks_the_winner() {
        // `a.txt` (copy) and `a.src` (transform → a.txt) claim one target; the
        // literal copy outranks the transform, exactly like `app.js` over `app.ts`.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.txt"), "literal").unwrap();
        std::fs::write(root.join("a.src"), "source").unwrap();
        std::fs::write(root.join("b.md"), "fine").unwrap();

        let report = scan(std::slice::from_ref(&root));
        let conflicts = report.conflicts();
        assert_eq!(conflicts.len(), 1, "one contested target");
        assert_eq!(conflicts[0].out_rel, Path::new("a.txt"));
        assert_eq!(conflicts[0].claimants.len(), 2);
        assert_eq!(
            conflicts[0].claimants[0].rel,
            Path::new("a.txt"),
            "the literal file wins"
        );

        let winners = report.winners();
        assert_eq!(winners.len(), 2, "one winner per target: a.txt and b.md");
        assert!(winners.iter().any(|w| w.out_rel == Path::new("b.md")));
    }

    #[test]
    fn preflight_detects_a_cross_root_conflict_and_the_first_root_wins() {
        let dir = tempfile::tempdir().unwrap();
        let first = dir.path().join("first");
        let second = dir.path().join("second");
        std::fs::create_dir_all(&first).unwrap();
        std::fs::create_dir_all(&second).unwrap();
        std::fs::write(first.join("app.txt"), "first").unwrap();
        std::fs::write(second.join("app.txt"), "second").unwrap();

        let roots = vec![first, second];
        let report = scan(&roots);
        let conflicts = report.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].claimants[0].root, 0,
            "the earlier root outranks the later one"
        );
        assert_eq!(report.winners().len(), 1);
    }

    #[test]
    fn tiebreak_orders_same_rank_claims() {
        // Two same-rank claims on one target differ only by tiebreak — the probe
        // order dev uses for ts/tsx/mts.
        struct Multi;
        impl Preflight for Multi {
            fn name(&self) -> &'static str {
                "multi"
            }
            fn rank(&self) -> Rank {
                Rank::Transform
            }
            fn claim(&self, rel: &Path) -> Option<Claim> {
                let ext = rel.extension()?.to_str()?;
                let tiebreak = ["one", "two"].iter().position(|e| *e == ext)? as u8;
                Some(Claim {
                    out_rel: rel.with_extension("out"),
                    tiebreak,
                })
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("x.two"), "later").unwrap();
        std::fs::write(root.join("x.one"), "earlier").unwrap();

        let report = preflight(
            std::slice::from_ref(&root),
            &[&Multi],
            &crate::reject::Reject::none(),
        );
        let conflicts = report.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].claimants[0].rel,
            Path::new("x.one"),
            "the lower tiebreak wins"
        );
    }

    #[test]
    fn escaping_claims_are_flagged_and_honest_ones_are_not() {
        // A step that (wrongly) derives a traversing or absolute target: the report
        // flags every such claim, so the build can refuse it before any write. The
        // real steps never produce one — their targets come from walk-relative paths.
        struct Hostile;
        impl Preflight for Hostile {
            fn name(&self) -> &'static str {
                "hostile"
            }
            fn rank(&self) -> Rank {
                Rank::Static
            }
            fn claim(&self, rel: &Path) -> Option<Claim> {
                let ext = rel.extension().and_then(|x| x.to_str()).unwrap_or("");
                match ext {
                    "up" => Some(Claim {
                        out_rel: PathBuf::from("../evil.txt"),
                        tiebreak: 0,
                    }),
                    "abs" => Some(Claim {
                        out_rel: PathBuf::from("/etc/evil.txt"),
                        tiebreak: 0,
                    }),
                    _ => None,
                }
            }
        }
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.up"), "x").unwrap();
        std::fs::write(root.join("b.abs"), "x").unwrap();
        std::fs::write(root.join("fine.txt"), "x").unwrap();

        let report = preflight(
            std::slice::from_ref(&root),
            &[&Hostile, &CopyLike],
            &crate::reject::Reject::none(),
        );
        let escaping = report.escaping();
        assert_eq!(escaping.len(), 2, "got {escaping:?}");
        assert!(escaping
            .iter()
            .all(|claim| claim.out_rel.starts_with("..") || claim.out_rel.is_absolute()));

        // The honest steps' walk-derived claims never escape.
        let honest = scan(std::slice::from_ref(&root));
        assert!(honest.escaping().is_empty());
    }

    #[test]
    fn claims_target_gates_on_any_claim_and_walk_records_every_entry() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("sub")).unwrap();
        std::fs::write(root.join("sub/page.src"), "src").unwrap();

        let report = scan(std::slice::from_ref(&root));
        assert!(
            report.claims_target("sub/page.txt"),
            "the transform's target counts as claimed"
        );
        assert!(!report.claims_target("index.html"));
        // The walk records the root, the subdirectory and the file — directory mtimes
        // catch add/remove for rerun-if-changed.
        assert!(report.walked_paths().len() >= 3);
    }

    #[test]
    fn preflight_drops_claims_with_rejected_targets() {
        // Rejection is by target, centrally: the transform's `.txt` output dies even
        // though its `.src` source matches no pattern, and it dies for every step.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(root.join("a.src"), "source").unwrap();
        std::fs::write(root.join("b.md"), "fine").unwrap();

        let reject = crate::reject::Reject::from_list(["*.txt"]);
        let report = preflight(
            std::slice::from_ref(&root),
            &[&CopyLike, &TransformLike],
            &reject,
        );
        assert!(
            !report.claims_target("a.txt"),
            "the rejected target is not claimed"
        );
        assert!(report.claims_target("b.md"), "unrejected claims survive");
    }

    #[cfg(unix)]
    #[test]
    fn preflight_flags_files_resolving_outside_the_root() {
        // `root/exposed -> ../private`: reachable through the root lexically, but
        // canonically outside it — recorded as escaping, never offered to a step.
        let dir = tempfile::tempdir().unwrap();
        let private = dir.path().join("private");
        std::fs::create_dir_all(&private).unwrap();
        std::fs::write(private.join("credentials.txt"), "secret").unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(&private, root.join("exposed")).unwrap();

        let report = scan(std::slice::from_ref(&root));
        let escaping = report.escaping_sources();
        assert_eq!(escaping.len(), 1, "one file resolves outside the root");
        assert_eq!(escaping[0].rel, Path::new("exposed/credentials.txt"));
        assert!(escaping[0].target.ends_with("private/credentials.txt"));
        assert!(
            !report.claims_target("exposed/credentials.txt"),
            "no step may claim an escaping file"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_allows_symlinks_resolving_inside_the_root() {
        // In-root links are legitimate tree layout: the linked file and the file
        // inside the linked dir both claim normally.
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(root.join("real")).unwrap();
        std::fs::write(root.join("real/a.txt"), "data").unwrap();
        std::os::unix::fs::symlink(root.join("real/a.txt"), root.join("alias.txt")).unwrap();
        std::os::unix::fs::symlink(root.join("real"), root.join("linked")).unwrap();

        let report = scan(std::slice::from_ref(&root));
        assert!(report.escaping_sources().is_empty(), "nothing escapes");
        assert!(report.walk_errors().is_empty(), "no walk problems");
        assert!(report.claims_target("alias.txt"), "linked file claims");
        assert!(
            report.claims_target("linked/a.txt"),
            "file in a linked dir claims"
        );
    }

    #[cfg(unix)]
    #[test]
    fn preflight_surfaces_a_dangling_link_as_a_walk_error() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("root");
        std::fs::create_dir_all(&root).unwrap();
        std::os::unix::fs::symlink(root.join("missing"), root.join("dangling")).unwrap();

        let report = scan(std::slice::from_ref(&root));
        assert_eq!(
            report.walk_errors().len(),
            1,
            "got {:?}",
            report.walk_errors()
        );
        assert!(report.escaping_sources().is_empty());
        assert!(report.conflicts().is_empty(), "the tree still preflights");
    }
}
