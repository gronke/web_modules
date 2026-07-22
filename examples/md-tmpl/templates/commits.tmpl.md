---
name: commits
description: The most recent commits of the repository this example was baked from.
params:
  - commits = list(committer = str, title = str)
---
# Latest commits

The last {{ len(commits) }} commits, read with [gix](https://docs.rs/gix) at build time
and rendered through md-tmpl's strictly typed params — a commit that is not a
`(committer = str, title = str)` struct would fail the build, not the page.

> {% for c in commits %}
> {% include [row](./_commit_row.tmpl.md) with committer = c.committer, title = c.title %}
> {% else %}

_No commit history was available at build time._

> {% /for %}
