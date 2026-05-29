#!/usr/bin/env bash
# Run the exact-match pixel oracle inside the pinned amd64 container so renders
# are byte-identical on every machine (see e2e/Dockerfile). This is the single
# canonical entry point — the *only* sanctioned way to (re)generate baselines,
# so they can never drift in from a dev host. Mirrors willet-cloud's
# scripts/test-snapshots.sh.
#
#   ./scripts/test-snapshots.sh            compare against committed baselines
#   ./scripts/test-snapshots.sh --update   regenerate (re-bless) baselines
#
# The fast Layer-1 (`cargo test`) suite is unaffected; this is the slow,
# containerized layer.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$REPO_ROOT"

IMAGE="mudpuppy-snapshots:local"
PLATFORM="linux/amd64"   # the canonical arch; emulated on Apple Silicon

UPDATE_ARG=()
[[ "${1:-}" == "--update" ]] && UPDATE_ARG=(--update)

echo "==> Building $IMAGE ($PLATFORM)"
docker build --platform "$PLATFORM" -f e2e/Dockerfile -t "$IMAGE" e2e

echo "==> Running snapshot oracle in container"
# - Repo bind-mounted RW so regenerated baselines + the review bundle land back
#   on the host. CARGO_TARGET_DIR is set inside the image to /tmp (off the
#   mount) so the host's arm64 target/ never collides with the amd64 build.
# - Named volumes cache the cargo registry + target across runs (first run is
#   slow: it compiles the binary and its deps).
exec docker run --rm \
  --platform "$PLATFORM" \
  -v "$REPO_ROOT:/work" \
  -v mudpuppy-e2e-cargo-registry:/usr/local/cargo/registry \
  -v mudpuppy-e2e-cargo-target:/tmp/cargo-target \
  "$IMAGE" \
  ${UPDATE_ARG[@]+"${UPDATE_ARG[@]}"}
