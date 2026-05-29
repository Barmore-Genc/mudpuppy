# Pinned capture environment for the exact-match PIXEL oracle (Layer 2 of
# ../TESTING.md). The whole point is byte-identical renders on every machine, so
# everything that affects rasterization is frozen: arch (amd64), the renderer
# (resvg, pinned), the font, and the OS/toolchain (the base tag). Dev is Apple
# Silicon, so this runs under emulation locally and natively on CI's amd64 — the
# canonical arch. See ./README.md.
#
# The repo is bind-mounted at run time (not COPYed), so this image only changes
# when the toolchain/renderer changes — fast to iterate. e2e/scripts/run.sh is
# the entrypoint; scripts/test-snapshots.sh wraps the build + run.
#
# FIXME(hardening): pin the base by digest too —
#   docker buildx imagetools inspect rust:1.91-slim-bookworm
# and replace the tag with rust:1.91-slim-bookworm@sha256:<digest>.
FROM rust:1.91-slim-bookworm

ENV DEBIAN_FRONTEND=noninteractive \
    LANG=C.UTF-8 \
    # Keep cargo's target dir off the bind-mounted repo: the host target/ is
    # arm64 and must not collide with the amd64 build here.
    CARGO_TARGET_DIR=/tmp/cargo-target

# git: fixture repos. imagemagick: the pixel diff (`compare`). fontconfig + one
# pinned monospace font: deterministic glyphs. build-essential: link the binary
# + its PTY deps and compile resvg. ca-certificates: crate fetches.
RUN apt-get update && apt-get install -y --no-install-recommends \
        git \
        build-essential \
        ca-certificates \
        imagemagick \
        fontconfig \
        fonts-dejavu-core \
    && rm -rf /var/lib/apt/lists/*

# resvg = pure-Rust SVG rasterizer (the same renderer `agg` uses internally).
# We render the screen SVG straight to a lossless 24-bit PNG with it, skipping
# agg's 8-bit GIF step so pixel diffs localize. Pinned by version.
ARG RESVG_VERSION=0.47.0
RUN cargo install resvg --version "${RESVG_VERSION}" --locked \
    && resvg --version

# The exact font file the SVGs name, pinned so resvg never falls back.
ENV MUDPUPPY_FONT_FILE=/usr/share/fonts/truetype/dejavu/DejaVuSansMono.ttf

WORKDIR /work
ENTRYPOINT ["e2e/scripts/run.sh"]
