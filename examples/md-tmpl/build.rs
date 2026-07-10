//! Bakes the markdown site at build time into `$OUT_DIR/dist`, in two stages:
//!
//! 1. The regular pipeline: `web/about.tmpl.md` renders to `about.md` from its declared
//!    defaults (`web/_footer.tmpl.md` is include-only), exactly as the dev server serves
//!    it live.
//! 2. A **programmatic, strictly typed render**: the last commits of the surrounding git
//!    repository — committer and title, read with [gix] — become the `commits` param of
//!    `templates/commits.tmpl.md` (kept outside `web/`, since a template with required
//!    params is not a pipeline page). The rendered `commits.md` is written into the dist
//!    for `main.rs` to embed.
//!
//! No npm dependencies are vendored, so the bake never touches the network.
//!
//! [gix]: https://docs.rs/gix

use std::path::{Path, PathBuf};

use web_modules::build::{build, BuildOptions, Output};
use web_modules::md_tmpl::{CompileOptions, Context, Template, Value};

const HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>web-modules · md-tmpl</title>
{importmap}
</head>
<body>
<h1>Typed markdown templates</h1>
<ul>
<li><a href="/commits.md">commits.md</a> — the repository's last commits, typed params filled from gix at build time</li>
<li><a href="/about.md">about.md</a> — rendered by the pipeline from its declared defaults</li>
</ul>
</body>
</html>
"#;

fn main() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let out = PathBuf::from(std::env::var("OUT_DIR").unwrap()).join("dist");
    let web = manifest.join("web");

    build(&BuildOptions {
        specs: &[], // no npm dependencies → the build never touches the network
        roots: std::slice::from_ref(&web),
        out: &out,
        mount: "/web_modules",
        html: HTML,
        template: None,
        processors: Default::default(),
        output: Output::default(),
    })
    .expect("build md-tmpl example frontend");

    // Stage 2: the typed render. `commits` must be a `list(committer = str,
    // title = str)` — the wrong shape is a render error naming the mismatch.
    let (template, _frontmatter) = Template::compile_file(
        &manifest.join("templates/commits.tmpl.md"),
        CompileOptions::default(),
    )
    .expect("compile commits.tmpl.md");
    let mut ctx = Context::new();
    ctx.set("commits", Value::from(last_commits(&manifest, 10)));
    let markdown = template.render_ctx(&ctx).expect("render commits.tmpl.md");
    std::fs::write(out.join("commits.md"), markdown).expect("write commits.md");

    println!(
        "cargo:rerun-if-changed={}",
        manifest.join("templates").display()
    );
}

/// The repository's last `n` commits as typed template values, newest first. Without a
/// repository (a source tarball, a shallow environment) the list is empty and the
/// template's `for … else` arm renders instead — the page degrades, the build does not.
fn last_commits(from: &Path, n: usize) -> Vec<Value> {
    let Ok(repo) = gix::discover(from) else {
        return Vec::new();
    };
    // New commits move HEAD without touching this crate's sources; watching HEAD (and
    // the ref log it appends to) keeps the baked list current across `git commit`.
    println!(
        "cargo:rerun-if-changed={}",
        repo.git_dir().join("HEAD").display()
    );
    let Ok(head) = repo.head_commit() else {
        return Vec::new();
    };
    let Ok(walk) = repo.rev_walk([head.id]).all() else {
        return Vec::new();
    };
    walk.take(n)
        .filter_map(|info| {
            let commit = info.ok()?.object().ok()?;
            let title = commit.message().ok()?.summary().to_string();
            let committer = commit.committer().ok()?.name.to_string();
            Some(Value::new_struct([
                ("committer", Value::from(committer)),
                ("title", Value::from(title)),
            ]))
        })
        .collect()
}
