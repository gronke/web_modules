# Maintaining web_modules

What a maintainer of this repository — or of a fork — has to have, has to
configure once, and does on each release. The release flow itself is
[gronke/rust-ci](https://github.com/gronke/rust-ci)'s; its guide is
[docs/release-flow.md](https://github.com/gronke/rust-ci/blob/main/docs/release-flow.md),
and this document covers only what belongs to *this* repository.

## What you need

- **Repository admin.** Rulesets, environments and the Actions settings below
  are all admin-only.
- **A signing key registered on your GitHub profile.** Releases are authorised
  by a signed tag, and `require-signed-tag` trusts GitHub's own verification of
  the tag object — not a keyring on the runner. A key that signs locally but is
  not on your profile fails the gate.
- **`gh` authenticated** with a token carrying admin scope for this repository.
- **A Rust toolchain** at or above the `rust-version` in `Cargo.toml`, and
  `cargo install web_modules --features full` for driving the CLI locally.

No secrets are needed. Publishing to crates.io uses Trusted Publishing, so no
registry token is ever stored in the repository.

## One-time setup

`scripts/setup-release.sh` does all of it, idempotently — run it again after
changing anything and it reports instead of duplicating:

```sh
DRY_RUN=1 scripts/setup-release.sh                    # what it would change
REVIEWER=gronke CRATES_IO_TOKEN=cio… scripts/setup-release.sh
REPO=me/my-fork scripts/setup-release.sh              # a fork configures itself
```

Immutable releases is the one step it cannot take — GitHub exposes no REST
surface for it — so the script reports that and leaves it to you.

The rest of this section is what the script does, and what to reach for when a
repository needs something other than the defaults. Every block takes a
repository slug, so a fork substitutes its own:

```sh
REPO=gronke/web_modules
```

### Tag rulesets

The pipeline pushes two kinds of tag with the workflow token: a
`v<version>-rcN` candidate marker per candidate build, and the moving `v0`.
Both are unsigned by nature, so the rulesets that protect release tags must
exclude them — rust-ci's templates already do, and are importable as-is:

```sh
gh api "repos/$REPO/rulesets" --jq '.[].name'   # importing twice creates duplicates; look first

for f in tags-signed tags-maintainer-only; do
  curl -fsSL "https://raw.githubusercontent.com/gronke/rust-ci/v1/.github/rulesets/$f.json" \
    | gh api "repos/$REPO/rulesets" --input - --jq '"imported: " + .name'
done
```

Read the JSON before importing it — it is, after all, describing who may write
what.

`tags-signed` requires a verified signature on every tag; `tags-maintainer-only`
restricts who may create, update or delete one. Both exclude
`refs/tags/v*-rc*`, `refs/tags/v[0-9]` and `refs/tags/v[0-9][0-9]`. Those
exclusions are load-bearing: without them the candidate build fails its marker
push with GH013, and the moving major can never advance.

A repository whose rulesets predate this needs the exclusions patched in rather
than imported: read each one, extend `conditions.ref_name.exclude`, and write it
back — patching `conditions` in place rather than rebuilding it, so any
repository scoping survives. Names vary between repositories, so this walks every
tag ruleset the repository owns rather than a guessed list; `DRY_RUN=1` prints
the bodies instead of writing them.

```sh
ADD='["refs/tags/v*-rc*","refs/tags/v[0-9]","refs/tags/v[0-9][0-9]"]'

gh api "repos/$REPO/rulesets" \
  --jq '.[] | select(.target=="tag" and (.source_type // "Repository") == "Repository") | .id' \
| while read -r id; do
  gh api "repos/$REPO/rulesets/$id" | jq --argjson add "$ADD" '{
    name, target, enforcement,
    bypass_actors: (.bypass_actors // []),
    conditions: (.conditions | .ref_name.exclude = (((.ref_name.exclude // []) + $add) | unique)),
    rules: [.rules[] | if (.parameters // null) == null then {type} else {type, parameters} end]
  }' > /tmp/ruleset-$id.json
  if [ -n "${DRY_RUN:-}" ]; then
    jq -c '{name, exclude: .conditions.ref_name.exclude}' "/tmp/ruleset-$id.json"
  else
    gh api -X PUT "repos/$REPO/rulesets/$id" --input "/tmp/ruleset-$id.json" \
      --jq '"✓ " + .name + " → " + ((.conditions.ref_name.exclude // []) | join("  "))'
  fi
done
```

Rulesets inherited from an organisation cannot be changed through the repository
endpoint — `GET repos/$REPO/rulesets` reports them with `source_type:
Organization`, and they are edited at `orgs/<org>/rulesets/<id>`, which affects
every repository in that organisation.

### The `crates-io` environment

The crates.io job declares this environment. It is both the claim crates.io
matches when minting a token and the place a reviewer gate belongs, guarding the
one step that cannot be undone.

```sh
gh api -X PUT "repos/$REPO/environments/crates-io" \
  -F 'deployment_branch_policy[protected_branches]=false' \
  -F 'deployment_branch_policy[custom_branch_policies]=true'

# only v* tags may deploy to it
gh api -X POST "repos/$REPO/environments/crates-io/deployment-branch-policies" \
  -f name='v*' -f type=tag

# optional: require a human to approve the publish
gh api -X PUT "repos/$REPO/environments/crates-io" \
  -F "reviewers[][type]=User" -F "reviewers[][id]=$(gh api users/gronke --jq .id)"
```

### crates.io Trusted Publishing

On crates.io, under the crate's settings → trusted publishers, add a GitHub
publisher with:

| field | value |
| --- | --- |
| repository owner | `gronke` |
| repository name | `web_modules` |
| workflow filename | `release.yml` |
| environment | `crates-io` |

The workflow filename is part of the trust anchor: crates.io mints a token only
for a run of *that* file. Which is why `.github/` is code-owned (below) — an
edit there is an edit to what may publish.

### Immutable releases

Settings → General → Releases → enable immutable releases. Once a release is
published its tag and assets are frozen, which is what makes "the admin
published it" a meaningful signal. GitHub does not expose this on the REST
repository object, so it is a UI step with no scriptable equivalent today.

### Actions may open the merge-back pull request

`cut-release` opens the merge-back PR with the workflow token:

```sh
gh api "repos/$REPO/actions/permissions/workflow"   # read what is set now

gh api -X PUT "repos/$REPO/actions/permissions/workflow" \
  -F default_workflow_permissions=read \
  -F can_approve_pull_request_reviews=true
```

A `read` default is safe here: every workflow in this repository declares the
permissions it needs.

### The `ci:tauri` label

The tauri example costs ~16 minutes and nothing downstream ships it, so it is
opt-in — by this label on a pull request, or by dispatching the CI workflow.
Without the label existing, the job can never be triggered that way:

```sh
gh label create ci:tauri --repo "$REPO" --color BFD4F2 \
  --description "Build the tauri example on this PR"
```

### CODEOWNERS

`.github/CODEOWNERS`:

```
/.github/    @gronke
/action.yml  @gronke
```

Then enable code-owner review on the branch ruleset that already requires a
pull request, so the workflow Trusted Publishing trusts cannot change without
the owner reviewing it.

## Releasing

1. Run the **cut release** workflow (`gh workflow run cut.yml --repo "$REPO"`).
   It takes no inputs: `Cargo.toml` names the version. It rewrites
   `CHANGELOG.md`'s `[Unreleased]` section into a released one on a
   `release/v<version>` branch, opens the merge-back pull request, and starts
   the pipeline.
2. Review the draft pre-release and the candidate run. Every push to the release
   branch refreshes the draft and adds a new `-rcN` marker, so fixes iterate
   freely.
3. Sign and push the tag using the commands the candidate run prints in its step
   summary. That signature is the authorisation for everything after it.
4. The tag run attaches the six binaries and `SHA256SUMS` to the draft.
5. **Publish the draft as a full release.** That is the go-live signal: it
   advances the moving `v0` and publishes the crate to crates.io. A pre-release
   does not trigger it, and neither does a draft.
   Before `v0` moves, the go-live job downloads the release's own binary
   through the action's installer mode — the path every `@v0` consumer takes —
   and checks it reports this version. A release that cannot serve its binary
   leaves `v0` on the last one that could, and the crate unpublished with it.

`cargo publish` needs no manual step; `[Unreleased]` gains entries again, and the
first of them declares the next version — the changelog gate asks for it.

### The registry signature

The crates.io step publishes only when a verified signature covers the release
commit — the signed `v<version>` tag from step 3 satisfies it, so the flow
above needs nothing extra. Should a release ever go live without one, the run
stays green and rehearses the packaging instead of uploading; push a signed
companion to complete it, retroactively included:

```sh
git fetch origin 'refs/tags/v<version>:refs/tags/v<version>'
git tag -s -m "v<version>" v<version>-sig 'v<version>^{}'
git push origin refs/tags/v<version>-sig
```

The companion push runs the attest job: it requires the release to be
published (drafts stay the admin's act), reconciles the seal, the flip and the
moving major idempotently, and publishes the crate if it never was. Releases
cut before the sealed pipeline carry no candidate markers, so their
attestation fails at the re-seal — nothing to complete there; those crates
were published by hand. The `tags-signed` ruleset covers `v*`, companions
included, so an unsigned companion cannot exist.

## Forking

- Configure your own trusted publisher, or drop the `crates-io` job from
  `release.yml`. Publishing is guarded by
  `if: github.repository == 'gronke/web_modules'`, so a fork skips it rather
  than failing an exchange it cannot pass — change that guard to your slug if
  you do want to publish.
- Import the rulesets and create the environment as above, under your own slug.
- The composite action at `action.yml` downloads prebuilt binaries from
  **`gronke/web_modules`** releases regardless of where the action runs from.
  A fork that wants its own binaries must change `repo=` in that file.

## When something refuses

| Symptom | Cause | Fix |
| --- | --- | --- |
| `Can't find 'action.yml' … for action 'gronke/rust-ci/.github/actions/<name>@v1'` | The action exists on rust-ci's `main` but no release has moved `v1` yet | Release rust-ci, or pin the step to `@main` meanwhile |
| Marker push rejected, `GH013` | A tag ruleset covers `v*-rc*` | Add the exclusions above |
| `no candidate marker for v<version>` | The release was cut by hand, so no candidate build ever sealed a tree | Cut through `cut.yml`, or release from a commit predating the sealed flow |
| Gate fails immediately on a tag | The tag sits on a commit whose workflow references something unreleased | Tag the commit you intend to release, and check what that commit's `release.yml` requires |
| `publish: true needs a crates.io credential` | Trusted Publishing not configured, or the run is not the trusted workflow/environment | Check the four values above match exactly |
| `<crate> <version> is already published on crates.io` | The version was published before | Bump the version; the changelog gate asks for this too |
