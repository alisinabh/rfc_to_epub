#!/usr/bin/env bash
# Print the asset names of a release, one per line.
#
# Exists because the obvious one-liner is subtly wrong in two ways that cost a
# full publish run:
#
#   gh release view "$tag" --json assets --jq '.assets[].name' 2>/dev/null || true
#
#   1. `|| true` with stderr discarded cannot tell "this bucket has no release
#      yet" (expected, contributes nothing) from "the API refused the request"
#      (a rate limit). Both silently yield an empty list, so a throttled run
#      reports a fraction of the corpus as the whole of it.
#   2. The `assets` array inlined on the release object is not a dependable full
#      listing for a release holding hundreds of assets; the dedicated endpoint
#      paginates properly.
#
# Exit status: 0 when the listing is complete (including a genuinely absent
# release, which prints nothing), 1 when it could not be determined.
set -uo pipefail

tag="${1:?usage: release-assets.sh <tag>}"
repo="${GITHUB_REPOSITORY:-$(gh repo view --json nameWithOwner --jq .nameWithOwner)}"

err="$(mktemp)"
trap 'rm -f "$err"' EXIT

if ! id="$(gh api "repos/$repo/releases/tags/$tag" --jq .id 2>"$err")"; then
  if grep -q 'HTTP 404' "$err"; then
    exit 0  # No release for this bucket yet.
  fi
  echo "release-assets: cannot resolve $tag: $(tr '\n' ' ' < "$err")" >&2
  exit 1
fi

if ! gh api --paginate "repos/$repo/releases/$id/assets" --jq '.[].name' 2>"$err"; then
  echo "release-assets: cannot list assets of $tag: $(tr '\n' ' ' < "$err")" >&2
  exit 1
fi
