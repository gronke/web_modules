//! Typed markdown templating via [md-tmpl]: render `*.tmpl.md` sources to their
//! `.md` targets — the markdown sibling of the [Tera](crate::templates) step.
//!
//! A `.tmpl.md` declares a typed interface in YAML frontmatter and keeps its body
//! valid markdown (control tags carry a `> ` blockquote prefix), so the source reads
//! cleanly anywhere markdown renders. The pipeline renders each page from its declared
//! defaults; the generated import map is offered as the compile-time env var
//! `importmap`, which a template opts into by declaring it:
//!
//! ```
//! use web_modules::md_tmpl::{CompileOptions, Template, Value};
//!
//! let source = "---\nenv:\n  - importmap = str\n---\n{{ importmap }}\n";
//! let env = [("importmap", Value::from("<script type=\"importmap\">{}</script>"))];
//! let (page, _frontmatter) = Template::compile(source, CompileOptions::default().env(&env))?;
//! assert!(page.render_empty()?.contains("importmap"));
//! # Ok::<(), web_modules::md_tmpl::TemplateError>(())
//! ```
//!
//! Cross-file includes (`{% include [x](./x.tmpl.md) with a=b %}`) resolve relative to
//! the including file; `_`-prefixed files are include-only partials, never pages. A
//! page must render from its declared defaults — a required (non-defaulted) param
//! fails the build with an error naming the parameter. Templates with required params
//! are rendered programmatically instead: the re-exported [`Template`],
//! [`CompileOptions`], [`Context`] and [`Value`] let a `build.rs` (or a serving app)
//! supply real, typed data without depending on `md-tmpl` directly.
//!
//! [md-tmpl]: https://docs.rs/md-tmpl

use std::path::Path;

pub use md_tmpl::{CompileOptions, Context, Template, TemplateError, Value};

use crate::{Error, Result};

/// The compound source suffix. Its final `.md` alone is not a source extension, so
/// classification everywhere goes through
/// [`is_source_name`](crate::static_files::is_source_name) or [`MdTmplStep::claim`],
/// never a bare `extension()` check.
pub(crate) const TMPL_MD_SUFFIX: &str = ".tmpl.md";

/// The env every page template may declare: the single `importmap` variable, holding
/// the map's inline `<script>` element. One constructor — the md-tmpl counterpart of
/// [`importmap_context`](crate::templates::importmap_context) — so the build's step
/// and the dev server cannot diverge in what a template sees. Pairs a template does
/// not declare are ignored by md-tmpl, so declaring `importmap` stays opt-in.
pub(crate) fn importmap_env(importmap: &crate::importmap::Importmap) -> [(&'static str, Value); 1] {
    [("importmap", Value::from(importmap.to_script_tag()))]
}

/// Compile `src` and render it from its declared defaults, with
/// [`importmap_env`] offered as the compile-time env. Includes resolve relative to
/// the file (md-tmpl reads them from disk on each compile, so a fresh call always
/// sees fresh partials). Errors are prefixed with the source path — md-tmpl
/// diagnostics carry lines and snippets, but not always the file.
pub(crate) fn render_page(src: &Path, importmap: &crate::importmap::Importmap) -> Result<String> {
    let env = importmap_env(importmap);
    let with_path = |e: TemplateError| Error::Template(format!("{}: {e}", src.display()));
    let (page, _frontmatter) =
        Template::compile_file(src, CompileOptions::default().env(&env)).map_err(with_path)?;
    page.render_empty().map_err(with_path)
}

/// Renders `x.tmpl.md` → `x.md` with the generated import map offered as the
/// `importmap` env var — the static counterpart of the dev server's on-the-fly
/// rendering. Rendered with the [Tera](crate::build::steps::Rank) winners after
/// vendoring, so templates see the final map. `_`-prefixed files are include-only
/// partials; unlike Tera's convention-only skip, md-tmpl actually includes them
/// (`{% include [x](./_x.tmpl.md) with … %}`), explicitly-passed and type-checked.
pub(crate) struct MdTmplStep;

impl crate::build::steps::Preflight for MdTmplStep {
    fn name(&self) -> &'static str {
        "md-tmpl template"
    }

    fn rank(&self) -> crate::build::steps::Rank {
        crate::build::steps::Rank::MdTmpl
    }

    fn claim(&self, rel: &Path) -> Option<crate::build::steps::Claim> {
        let name = rel.file_name()?.to_str()?;
        let split = name.len().checked_sub(TMPL_MD_SUFFIX.len())?;
        // `split == 0` is a bare `.tmpl.md` — no stem, nothing to name the target.
        if split == 0
            || name.starts_with('_')
            || !name.as_bytes()[split..].eq_ignore_ascii_case(TMPL_MD_SUFFIX.as_bytes())
        {
            return None;
        }
        // The matched suffix is pure ASCII, so `split` is a char boundary.
        // Replace `.tmpl.md` with `.md`: `guide.tmpl.md` → `guide.md`.
        Some(crate::build::steps::Claim {
            out_rel: rel.with_file_name(format!("{}.md", &name[..split])),
            tiebreak: 0,
        })
    }
}

impl crate::build::steps::Step for MdTmplStep {
    /// Render and write. The target is always `.md`, which never joins the module
    /// graph, so the emission records no imports.
    fn emit(
        &self,
        cx: &crate::build::steps::EmitCx<'_>,
        src: &Path,
        _rel: &Path,
        dest: &Path,
    ) -> Result<crate::build::steps::Emitted> {
        let rendered = render_page(src, cx.importmap)?;
        std::fs::write(dest, rendered)?;
        Ok(crate::build::steps::Emitted::default())
    }
}

// md-tmpl has no flags of its own beyond the on/off toggle, so it uses the `NoConfig`
// placeholder. (`--md-tmpl` / `--no-md-tmpl`.)
#[cfg(feature = "cli")]
crate::cli_config::feature_args!(
    MdTmplArgs,
    md_tmpl,
    "md-tmpl",
    no_md_tmpl,
    "no-md-tmpl",
    crate::cli_config::NoConfig
);

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::build::steps::Preflight;

    fn claimed(rel: &str) -> Option<PathBuf> {
        MdTmplStep.claim(Path::new(rel)).map(|c| c.out_rel)
    }

    #[test]
    fn claims_tmpl_md_to_md_target() {
        assert_eq!(claimed("guide.tmpl.md"), Some(PathBuf::from("guide.md")));
        assert_eq!(
            claimed("docs/setup.tmpl.md"),
            Some(PathBuf::from("docs/setup.md"))
        );
        // Case variants are the same source (case-insensitive FS safety).
        assert_eq!(claimed("Guide.TMPL.MD"), Some(PathBuf::from("Guide.md")));
    }

    #[test]
    fn skips_partials_plain_md_and_bare_suffix() {
        assert_eq!(
            claimed("_header.tmpl.md"),
            None,
            "partials are include-only"
        );
        assert_eq!(claimed("notes.md"), None, "plain markdown is static");
        assert_eq!(claimed("page.tera"), None, "another engine's source");
        assert_eq!(claimed(".tmpl.md"), None, "no stem, no target");
    }

    #[test]
    fn renders_with_the_importmap_env() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("page.tmpl.md");
        std::fs::write(
            &src,
            "---\nenv:\n  - importmap = str\n---\n# Page\n\n{{ importmap }}\n",
        )
        .unwrap();
        let mut map = crate::importmap::Importmap::new();
        map.insert("lit", "/web_modules/lit/index.js");
        let rendered = render_page(&src, &map).unwrap();
        assert!(rendered.contains("<script type=\"importmap\">"));
        assert!(rendered.contains("lit"));
    }

    #[test]
    fn renders_without_declaring_the_env() {
        // The env is offered, not imposed: a template that never declares
        // `importmap` compiles and renders untouched.
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("plain.tmpl.md");
        // `params: []` is md-tmpl's minimal (empty) interface — the frontmatter
        // block itself is required.
        std::fs::write(&src, "---\nparams: []\n---\n# Plain\n").unwrap();
        let rendered = render_page(&src, &crate::importmap::Importmap::new()).unwrap();
        assert!(rendered.contains("# Plain"));
    }

    #[test]
    fn required_param_fails_naming_the_parameter_and_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("needs-data.tmpl.md");
        std::fs::write(&src, "---\nparams:\n  - title = str\n---\n{{ title }}\n").unwrap();
        let err = render_page(&src, &crate::importmap::Importmap::new()).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("title") && msg.contains("needs-data.tmpl.md"),
            "got: {msg}"
        );
    }

    #[test]
    fn follows_includes_relative_to_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let src = dir.path().join("page.tmpl.md");
        std::fs::write(
            dir.path().join("_footer.tmpl.md"),
            "---\nparams:\n  - site = str\n---\n_{{ site }}_\n",
        )
        .unwrap();
        std::fs::write(
            &src,
            "---\nparams: []\n---\n# Page\n\n> {% include [footer](./_footer.tmpl.md) with site = \"demo\" %}\n",
        )
        .unwrap();
        let rendered = render_page(&src, &crate::importmap::Importmap::new()).unwrap();
        assert!(rendered.contains("_demo_"), "got: {rendered}");
    }
}
