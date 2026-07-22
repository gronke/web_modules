# md-tmpl — typed markdown templates

Markdown pages from [md-tmpl](https://docs.rs/md-tmpl) templates, in both ways web_modules supports them:

- **Pipeline pages** — `web/about.tmpl.md` renders to `/about.md` automatically, from the defaults its frontmatter declares.
  `web/_footer.tmpl.md` is `_`-prefixed and therefore include-only.
- **Programmatic, strictly typed renders** — `build.rs` reads the last ten commits of this repository with [gix](https://docs.rs/gix)
  (pure Rust, no git binary) and passes committer + title as the `commits = list(committer = str, title = str)` param of
  `templates/commits.tmpl.md`.
  A value of the wrong shape fails the build with an error naming the parameter — that is md-tmpl's point.

Every template is valid markdown on its own: GitHub renders the sources above readably, control tags and all.

Run it:

```bash
cargo run -p md-tmpl-example
```

and open <http://127.0.0.1:8080/>.
In a debug build `Frontend::auto()` runs the live dev server over `web/` with the bake as embedded fallback:
edit `web/about.tmpl.md` (or the footer it includes) and the browser reloads with a fresh render, while `/commits.md` keeps
serving from the bake.
In a release build the same binary serves the baked pages only — self-contained, no filesystem access.
