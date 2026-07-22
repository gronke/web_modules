---
name: commit_row
description: One commit as a list item — included per commit, with explicit typed args.
params:
  - committer = str
  - title = str
---
- **{{ title }}** — {{ committer }}
