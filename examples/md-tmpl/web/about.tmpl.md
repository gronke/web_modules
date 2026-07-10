---
name: about
description: A pipeline-rendered page — every non-partial web/*.tmpl.md becomes its .md target.
params:
  - features = list(str) := ["typed frontmatter params", "cross-file includes", "for/else", "valid markdown sources"]
---
# About this example

The `web_modules` pipeline rendered this page from `about.tmpl.md`: every parameter
carries a default in the frontmatter, so the build (and the dev server, live) renders
it without any external data.

> {% for f in features %}

- {{ f }}

> {% /for %}

> {% include [footer](./_footer.tmpl.md) with source = "web/about" %}
