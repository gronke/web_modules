#!/usr/bin/env bash
# One-time repository setup for the release flow, idempotent: run it again
# after changing anything and it reports rather than duplicates.
#
#   scripts/setup-release.sh                     # configure, print what changed
#   DRY_RUN=1 scripts/setup-release.sh           # print what would change
#   REPO=me/my-fork scripts/setup-release.sh     # a fork configures itself
#
# Environment:
#   REPO              owner/name; default: the repository `gh` resolves here
#   CRATE             crate name; default: the repository name
#   REVIEWER          GitHub login to gate the crates-io environment on (optional)
#   CRATES_IO_TOKEN   crates.io API token; registers Trusted Publishing (optional)
#   DRY_RUN           any value: change nothing
#
# What it cannot do: immutable releases have no REST surface, so that one stays
# a UI step and is only reported here.
set -euo pipefail

REPO="${REPO:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"
CRATE="${CRATE:-${REPO#*/}}"
OWNER="${REPO%/*}"
NAME="${REPO#*/}"
WORKFLOW="release.yml"
ENVIRONMENT="crates-io"
RULESET_BASE="https://raw.githubusercontent.com/gronke/rust-ci/v1/.github/rulesets"

say() { printf '%s\n' "$*"; }
act() { # act <description> <command…> — describes under DRY_RUN, else runs quietly
  local desc="$1"
  shift
  if [ -n "${DRY_RUN:-}" ]; then
    say "   would: $desc"
    return 0
  fi
  "$@" >/dev/null
}

say "repository: ${REPO}   crate: ${CRATE}"

# --- the crates-io environment --------------------------------------------------
# The claim crates.io matches when minting a token, and the place a reviewer gate
# belongs. Referencing an environment in a workflow creates it implicitly and
# unprotected, so create it deliberately instead.
say
say "1. the ${ENVIRONMENT} environment"
# PUT replaces the environment's settings, so the reviewer and the branch policy
# travel in one call — adding the reviewer afterwards would drop the policy.
env_args=(-F 'deployment_branch_policy[protected_branches]=false'
  -F 'deployment_branch_policy[custom_branch_policies]=true')
reviewer_note=""
if [ -n "${REVIEWER:-}" ]; then
  env_args+=(-F "reviewers[][type]=User" -F "reviewers[][id]=$(gh api "users/${REVIEWER}" --jq .id)")
  reviewer_note=", reviewer ${REVIEWER}"
fi
if gh api "repos/${REPO}/environments/${ENVIRONMENT}" >/dev/null 2>&1; then
  say "   exists${reviewer_note:+ — reapplying settings${reviewer_note}}"
fi
act "configure the ${ENVIRONMENT} environment${reviewer_note}" \
  gh api -X PUT "repos/${REPO}/environments/${ENVIRONMENT}" "${env_args[@]}"
[ -n "${DRY_RUN:-}" ] || say "   configured${reviewer_note}"

# Only v* tags may deploy to it: a branch can never reach the registry. Tag
# policies live behind their own endpoint and survive the PUT above.
if gh api "repos/${REPO}/environments/${ENVIRONMENT}/deployment-branch-policies" \
  --jq '.branch_policies[]?.name' 2>/dev/null | grep -qx 'v\*'; then
  say "   tag policy v* already present"
else
  act "restrict deployments to v* tags" \
    gh api -X POST "repos/${REPO}/environments/${ENVIRONMENT}/deployment-branch-policies" \
    -f name='v*' -f type=tag
  [ -n "${DRY_RUN:-}" ] || say "   tag policy v* added"
fi

# --- tag rulesets ---------------------------------------------------------------
# The pipeline pushes unsigned v<version>-rcN markers and the moving major with
# the workflow token, so the templates exclude both from the signing and
# creation rules.
say
say "2. tag rulesets"
existing="$(gh api "repos/${REPO}/rulesets" --jq '[.[] | select(.target == "tag")] | length')"
if [ "$existing" -gt 0 ]; then
  say "   ${existing} tag ruleset(s) already configured — review them against ${RULESET_BASE}/"
else
  for f in tags-signed tags-maintainer-only; do
    if [ -n "${DRY_RUN:-}" ]; then
      say "   would: import ${f}.json"
    else
      curl -fsSL "${RULESET_BASE}/${f}.json" \
        | gh api "repos/${REPO}/rulesets" --input - --jq '"   imported: " + .name'
    fi
  done
fi

# --- the ci:tauri label ---------------------------------------------------------
say
say "3. the ci:tauri label"
if gh label list --repo "$REPO" --search "ci:tauri" --json name --jq '.[].name' | grep -qx 'ci:tauri'; then
  say "   exists"
else
  act "create the ci:tauri label" \
    gh label create ci:tauri --repo "$REPO" --color BFD4F2 \
    --description "Build the tauri example on this PR"
  [ -n "${DRY_RUN:-}" ] || say "   created"
fi

# --- crates.io Trusted Publishing -----------------------------------------------
# The trust anchor is the tuple below: crates.io mints a short-lived token only
# for a run of that workflow, in that environment, on that repository.
say
say "4. crates.io Trusted Publishing"
say "   owner=${OWNER} repo=${NAME} workflow=${WORKFLOW} environment=${ENVIRONMENT}"
if [ -z "${CRATES_IO_TOKEN:-}" ]; then
  say "   no CRATES_IO_TOKEN — set one, or add it by hand:"
  say "   https://crates.io/crates/${CRATE}/settings → Trusted Publishing"
else
  configured="$(curl -sS -H "authorization: ${CRATES_IO_TOKEN}" \
    "https://crates.io/api/v1/trusted_publishing/github_configs?crate=${CRATE}" \
    | jq -r --arg o "$OWNER" --arg n "$NAME" --arg w "$WORKFLOW" --arg e "$ENVIRONMENT" \
      '[.github_configs[]? | select(.repository_owner == $o and .repository_name == $n
        and .workflow_filename == $w and (.environment // "") == $e)] | length')"
  if [ "$configured" -gt 0 ]; then
    say "   already registered"
  elif [ -n "${DRY_RUN:-}" ]; then
    say "   would: register the publisher"
  else
    jq -n --arg c "$CRATE" --arg o "$OWNER" --arg n "$NAME" --arg w "$WORKFLOW" --arg e "$ENVIRONMENT" \
      '{github_config: {crate: $c, repository_owner: $o, repository_name: $n,
        workflow_filename: $w, environment: $e}}' \
      | curl -sSf -X POST -H "authorization: ${CRATES_IO_TOKEN}" \
        -H "content-type: application/json" --data @- \
        "https://crates.io/api/v1/trusted_publishing/github_configs" >/dev/null
    say "   registered"
  fi
fi

# --- what stays manual ------------------------------------------------------------
say
say "5. immutable releases (UI only)"
say "   Settings → General → Releases → Immutable releases"
say "   Freezing a published release's tag and assets is what makes \"the admin published it\" final."
