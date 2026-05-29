#!/usr/bin/env bash
# Publish the pixel-oracle review bundle to an S3-compatible object store under
# an unguessable capability path, and print the public review URL (last stdout
# line) for the caller to post as a PR comment. Adapted from willet-cloud's
# scripts/snapshot-bundle.sh — same env contract and the same org secrets/vars.
#
# Run AFTER ./scripts/test-snapshots.sh has failed: the container leaves the
# review dir (expected | actual | diff + index.html + status.tsv) on the host
# via the bind-mount. The bytes published under baseline/ are the exact `actual`
# renders the reviewer looks at, mirrored at their repo path — the approve flow
# copies them straight back and commits, so the committed pixels are precisely
# the reviewed pixels (no second-render drift).
#
# Required env (CI secrets / vars):
#   PR_NUMBER, HEAD_SHA          PR number + head commit SHA (full)
#   CI_S3_ENDPOINT               e.g. https://s3.us-west-000.backblazeb2.com
#   CI_S3_REGION                 e.g. us-west-000
#   CI_S3_BUCKET                 public bucket name
#   CI_S3_KEY_ID  / CI_S3_APP_KEY    access key id / secret  (-> AWS_*)
#   CI_S3_PUBLIC_BASE            public base URL for objects, no trailing slash
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

: "${PR_NUMBER:?}" "${HEAD_SHA:?}" "${CI_S3_ENDPOINT:?}" "${CI_S3_REGION:?}"
: "${CI_S3_BUCKET:?}" "${CI_S3_KEY_ID:?}" "${CI_S3_APP_KEY:?}" "${CI_S3_PUBLIC_BASE:?}"

# Strip CR/LF + edge whitespace from every S3 value: a stray \r anywhere in the
# org-var -> env -> shell chain glues to the hostname and the AWS CLI rejects an
# otherwise-correct endpoint as "Invalid endpoint" (logs still look clean — the
# \r just returns the cursor). None of these legitimately contain whitespace.
trim() {
  local v="$1"; v="${v//$'\r'/}"; v="${v//$'\n'/}"
  v="${v#"${v%%[![:space:]]*}"}"; v="${v%"${v##*[![:space:]]}"}"
  printf '%s' "$v"
}
CI_S3_ENDPOINT="$(trim "$CI_S3_ENDPOINT")"
CI_S3_REGION="$(trim "$CI_S3_REGION")"
CI_S3_BUCKET="$(trim "$CI_S3_BUCKET")"
CI_S3_KEY_ID="$(trim "$CI_S3_KEY_ID")"
CI_S3_APP_KEY="$(trim "$CI_S3_APP_KEY")"
CI_S3_PUBLIC_BASE="$(trim "$CI_S3_PUBLIC_BASE")"

REVIEW="e2e/review"
STATUS="$REVIEW/status.tsv"
if [[ ! -f "$STATUS" ]]; then
  echo "No review manifest at $STATUS — did the oracle run?" >&2
  exit 1
fi

# Mirror the exact `actual` bytes of every CHANGED scenario at their repo path,
# so `aws s3 cp .../baseline/ . --recursive` on approve lands them at
# e2e/baselines/<name>.png. Unchanged scenarios are skipped (committing identical
# bytes would be a no-op).
#
# The mirror goes in a temp dir, NOT under $REVIEW: the pixel oracle runs in a
# container as root and creates $REVIEW (root-owned), so the host runner can read
# those files but cannot write into the directory. We upload the mirror to
# <prefix>/baseline/ separately below.
MIRROR="$(mktemp -d)"
trap 'rm -rf "$MIRROR"' EXIT
changed=0
while IFS=$'\t' read -r name status; do
  [[ -n "$name" ]] || continue
  case "$status" in
    match) continue ;;
  esac
  mkdir -p "$MIRROR/e2e/baselines"
  cp "$REVIEW/actual/$name.png" "$MIRROR/e2e/baselines/$name.png"
  changed=$((changed + 1))
done < "$STATUS"

if [[ "$changed" -eq 0 ]]; then
  echo "No snapshot changes to review — nothing to publish." >&2
  exit 0
fi

# Unguessable capability segment — security of a public bucket rests entirely on
# this. 32 hex chars from a CSPRNG. Path: <pr>/<head-sha>/<token>/.
TOKEN="$(openssl rand -hex 16)"
PREFIX="${PR_NUMBER}/${HEAD_SHA}/${TOKEN}"

export AWS_ACCESS_KEY_ID="$CI_S3_KEY_ID"
export AWS_SECRET_ACCESS_KEY="$CI_S3_APP_KEY"
export AWS_DEFAULT_REGION="$CI_S3_REGION"
# Array keeps the endpoint a single quoted argv element (no word-split/re-glob).
S3=(aws s3 --endpoint-url "$CI_S3_ENDPOINT")

echo "==> Uploading bundle to s3://$CI_S3_BUCKET/$PREFIX/" >&2
# The expected | actual | diff PNGs the reviewer eyeballs.
"${S3[@]}" cp "$REVIEW" "s3://$CI_S3_BUCKET/$PREFIX/" --recursive \
  --exclude "*" --include "*.png" --content-type image/png --only-show-errors
# The reviewed bytes the approve flow copies back into the tree.
"${S3[@]}" cp "$MIRROR" "s3://$CI_S3_BUCKET/$PREFIX/baseline/" --recursive \
  --exclude "*" --include "*.png" --content-type image/png --only-show-errors
"${S3[@]}" cp "$REVIEW/index.html" "s3://$CI_S3_BUCKET/$PREFIX/index.html" \
  --content-type "text/html; charset=utf-8" --only-show-errors

# Last stdout line = the capability URL the caller posts to the PR.
echo "${CI_S3_PUBLIC_BASE}/${PREFIX}/index.html"
