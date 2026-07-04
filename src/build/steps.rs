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
    /// when the file is not its input (wrong extension, `_` partial, `.d.ts`,
    /// reject-listed, …).
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
    fn emit(&self, cx: &EmitCx<'_>, src: &Path, _rel: &Path, dest: &Path) -> Result<Emitted> {
        let mut ctx = crate::templates::Context::new();
        ctx.insert("importmap", &cx.importmap.to_script_tag());
        let rendered = crate::templates::render_file(src, &ctx)?;
        std::fs::write(dest, rendered)?;
        Ok(Emitted::default())
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
/// `cargo:rerun-if-changed` set).
pub(crate) struct PreflightReport {
    claims: Vec<ClaimRecord>,
    walked: Vec<PathBuf>,
}

/// Walk each root once (following symlinks, dropping unreadable entries as every walk
/// before it did) and offer every file to every step. Infallible: classification is
/// pure, and I/O problems surface at emission time.
pub(crate) fn preflight(roots: &[PathBuf], steps: &[&dyn Preflight]) -> PreflightReport {
    let mut claims = Vec::new();
    let mut walked = Vec::new();
    for (root_index, root) in roots.iter().enumerate() {
        for entry in walkdir::WalkDir::new(root)
            .follow_links(true)
            .into_iter()
            .filter_map(|e| e.ok())
        {
            let path = entry.path();
            walked.push(path.to_path_buf());
            if !path.is_file() {
                continue;
            }
            let Ok(rel) = path.strip_prefix(root) else {
                continue;
            };
            for (step_index, step) in steps.iter().enumerate() {
                if let Some(claim) = step.claim(rel) {
                    claims.push(ClaimRecord {
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
    PreflightReport { claims, walked }
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
        preflight(roots, &[&CopyLike, &TransformLike])
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

        let report = preflight(std::slice::from_ref(&root), &[&Multi]);
        let conflicts = report.conflicts();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(
            conflicts[0].claimants[0].rel,
            Path::new("x.one"),
            "the lower tiebreak wins"
        );
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
}
