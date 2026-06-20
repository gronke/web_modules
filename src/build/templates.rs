//! HTML templating via [Tera], primarily to render an `index.html` shell with
//! the generated import map injected.
//!
//! Re-exports [`Context`] so callers set variables without depending on `tera`
//! directly. Pair with [`crate::importmap::Importmap::to_script_tag`]:
//!
//! ```
//! use web_modules::templates::{render_str, Context};
//! let mut ctx = Context::new();
//! ctx.insert("title", "Demo");
//! ctx.insert("importmap", "<script type=\"importmap\">{}</script>");
//! let html = render_str("<title>{{ title }}</title>{{ importmap | safe }}", &ctx).unwrap();
//! assert!(html.contains("<title>Demo</title>"));
//! ```
//!
//! [Tera]: https://docs.rs/tera

use std::path::Path;

pub use tera::Context;

use crate::{Error, Result};

/// Render a template string with `context`. Autoescaping is **off** so injected
/// HTML (an importmap `<script>`, `<link>` tags) is emitted verbatim; use Tera's
/// `| escape` filter on any untrusted values you insert.
pub fn render_str(template: &str, context: &Context) -> Result<String> {
    tera::Tera::one_off(template, context, false).map_err(|e| Error::Template(e.to_string()))
}

/// Render a template file with `context` (autoescaping off, as in [`render_str`]).
pub fn render_file(path: &Path, context: &Context) -> Result<String> {
    let template = std::fs::read_to_string(path)?;
    render_str(&template, context)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_and_passes_html_through_unescaped() {
        let mut ctx = Context::new();
        ctx.insert("title", "BMF");
        ctx.insert("importmap", "<script type=\"importmap\">{}</script>");
        let html = render_str("<title>{{ title }}</title>{{ importmap | safe }}", &ctx).unwrap();
        assert!(html.contains("<title>BMF</title>"));
        assert!(html.contains("<script type=\"importmap\">"));
    }
}
