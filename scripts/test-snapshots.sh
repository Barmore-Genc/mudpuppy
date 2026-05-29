#!/usr/bin/env bash
# Single host entry point for the exact-match pixel oracle (see e2e/README.md).
# Renders the real binary's settled screen and pixel-diffs it against the
# committed baselines at zero tolerance. It is the *only* sanctioned way to
# (re)generate baselines, so they can never drift in from an ad-hoc render.
#
#   ./scripts/test-snapshots.sh            compare against committed baselines
#   ./scripts/test-snapshots.sh --update   regenerate (re-bless) baselines
#
# Runs NATIVELY — no container. We verified the renders are byte-identical across
# arch/OS (macOS/arm64 == linux/amd64, AE=0) once the font file and resvg version
# are pinned, so the container only ever cost build time we couldn't cache. The
# two pinned knobs live in e2e/scripts/run.sh (the vendored font + resvg version).
#
# Requires: cargo, resvg 0.47.0, ImageMagick (`compare`/`identify`). On macOS:
#   brew install resvg imagemagick
#
# The fast Layer-1 (`cargo test`) suite is unaffected; this is the slower layer.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
exec "$REPO_ROOT/e2e/scripts/run.sh" "$@"
