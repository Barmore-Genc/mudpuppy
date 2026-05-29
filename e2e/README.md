# e2e — the exact-match pixel oracle

This is **Layer 2's exact-match oracle** from [`../TESTING.md`](../TESTING.md):
the real binary, rendered to **actual pixels** inside a **pinned `linux/amd64`
container** and pixel-diffed against committed baselines at **zero tolerance**.

The design is lifted from willet-cloud's screenshot/approval workflow (Playwright
+ Vitest + pixelmatch in a pinned Playwright image), adapted for a TUI: the
"browser" is a real PTY running the binary, and the "renderer" is
[`resvg`](https://github.com/linebender/resvg) rasterizing the settled screen
(emitted as a truecolor SVG) to a lossless PNG.

> Runnable today. The fast behavioral layers — Layer 1 (`insta` snapshots in
> `src/tui.rs`) and the Layer-2 PTY smoke suite (`../tests/e2e.rs`) — run on
> every push as plain `cargo test`, no Docker. *This* layer is the slow,
> containerized gate that proves the screen is pixel-for-pixel intended.

## The loop

```sh
# Compare the real renders against committed baselines (the CI gate):
./scripts/test-snapshots.sh

# Re-bless: regenerate baselines (after an intended visual change):
./scripts/test-snapshots.sh --update
```

Both build `e2e/Dockerfile` and run it; the only difference is the `--update`
flag. There is **no other sanctioned way to make baselines**, so a stray render
from a dev host can never drift in (the workflow rule from TESTING.md).

End-to-end developer loop — mirrors willet's *write → run → see diff → approve →
commit*:

1. **Add/scope a scenario** in [`../tests/image_diff.rs`](../tests/image_diff.rs)
   (`SCENARIOS`): a name, the keystrokes to drive, and a screen marker to wait
   for. Keep the list tiny — edge cases belong in the Layer-1 `insta` suite.
2. **Generate the baseline:** `./scripts/test-snapshots.sh --update`, which
   writes `e2e/baselines/<name>.png`.
3. **Commit** `e2e/baselines/`.
4. **Open a PR.** The `snapshots` CI job (via the
   [`snapshot-review`](../.github/actions/snapshot-review/action.yml) action)
   runs the check. On any pixel difference it fails,
   [`scripts/snapshot-bundle.sh`](../scripts/snapshot-bundle.sh) publishes the
   review bundle (expected | actual | diff `index.html`) to an S3-compatible
   store under an unguessable capability path `<pr>/<sha>/<token>/`, and a single
   PR comment (marker `<!-- snapshot-review -->`) links to it.
5. **Review** the bundle at that URL. If the new rendering is correct, a
   maintainer comments exactly `approve snapshots`;
   [`approve-snapshots.yml`](../.github/workflows/approve-snapshots.yml) copies the
   **exact reviewed bytes** back from the bundle's `baseline/` (no re-render) and
   commits them to the PR branch. Otherwise it's a real regression — fix it.

### S3 + the approval flow

Mirrors willet-cloud and reuses the **same org-level** secrets/vars:

| Kind | Name | Use |
| --- | --- | --- |
| secret | `CI_S3_KEY_ID` / `CI_S3_APP_KEY` | object-store access key / secret |
| secret | `CI_APPROVE_TOKEN` | PAT to push the approved commit (so CI re-runs; `GITHUB_TOKEN` pushes don't) |
| var | `CI_S3_ENDPOINT` / `CI_S3_REGION` / `CI_S3_BUCKET` / `CI_S3_PUBLIC_BASE` | endpoint, region, public bucket, public base URL |

Why no re-render on approve: the bundle's `baseline/` holds the exact `actual`
PNGs the reviewer looked at, mirrored at their repo path. Approve does
`aws s3 cp .../baseline/ . --recursive` → lands `e2e/baselines/<name>.png` → commit.
The approve workflow also **SHA-pins**: it refuses if the branch moved since the
bundle was made (a push triggers a fresh review). `approve-snapshots.yml` runs
from the default branch, so a PR author can't edit the approval logic on their
branch. Locally you can always bypass the whole dance with
`./scripts/test-snapshots.sh --update` and commit `e2e/baselines/`.

## How it works

Capture and rasterization are split on purpose — capture is deterministic Rust;
rasterization is the host-sensitive part that must be pinned:

- **Capture (host or container):** `tests/image_diff.rs`, gated on
  `MUDPUPPY_SVG_DIR`, drives the binary through a PTY (shared harness in
  `tests/common/`), waits for the screen to settle, and writes a **truecolor
  SVG** built straight from the vt100 grid — one `<rect>` run per background
  color, one `<text>` run per styled glyph span, `textLength`-pinned to an exact
  column grid.
- **Rasterize + diff (container only):** [`scripts/run.sh`](./scripts/run.sh)
  rasterizes each SVG with `resvg` to a lossless 24-bit PNG (pinned font file,
  `--skip-system-fonts`), then `compare -metric AE` against the baseline. **AE
  must be 0.** It writes the review bundle to `e2e/review/` (gitignored) and
  exits non-zero on any mismatch or missing baseline.

> **Why SVG→resvg, not `agg`→GIF.** GIF is 8-bit (≤256 colors). The TUI's
> anti-aliased text exceeds 256 shades, so GIF re-quantizes the *whole* frame on
> any change — a one-glyph edit lit up every glyph's edges (AE in the thousands)
> and diffs stopped localizing. Rendering the SVG to a lossless PNG keeps
> unchanged regions bit-identical, so a diff shows *only* what changed (the same
> edit drops to AE≈189, confined to the status bar). `agg` can only output GIF,
> and its own default renderer is `resvg` — so this is the same glyph quality,
> minus the lossy palette step.

### What's pinned (and why)

| Knob | Value | Set in |
| --- | --- | --- |
| Arch | `linux/amd64` (emulated on Apple Silicon, native on CI) | `scripts/test-snapshots.sh`, Dockerfile |
| Base image / toolchain | `rust:1.91-slim-bookworm` | `Dockerfile` |
| Renderer | `resvg` 0.47.0 (lossless SVG→PNG) | `Dockerfile`, `scripts/run.sh` |
| Font | DejaVu Sans Mono (`fonts-dejavu-core`), exact file via `--use-font-file` | `Dockerfile`, `scripts/run.sh` |
| Geometry | 100×24, 9×18 px cells, 15 px font | `tests/common/` (the SVG) |
| Tolerance | 0 differing pixels (`compare -metric AE` == 0) | `scripts/run.sh` |

Changing any rendering knob invalidates every baseline — re-bless with
`--update`. The viewer is store-free today (no clock, no ids), so there's no
app-level nondeterminism to control yet; revisit when annotations land
(TESTING.md §"Determinism prerequisites").

### Arch note

Dev is arm64; baselines are canonical on **amd64** (CI's arch). Locally we build
and render under the pinned `--platform=linux/amd64` image (qemu emulation), so
locally-regenerated baselines should match CI's native amd64 — `resvg` is a pure
CPU/IEEE-float rasterizer, which qemu emulates faithfully. If a first CI
run ever disagrees by a pixel, re-bless on CI via `approve snapshots` so the
baselines are blessed on the canonical arch.

## Files

- `Dockerfile` — the pinned amd64 capture environment (rust + git + resvg +
  ImageMagick + font).
- `scripts/run.sh` — in-container: emit SVGs → rasterize → pixel-diff → bundle.
- `baselines/*.png` — committed oracle (the source of truth). ~15–20 KB each.
- `review/` — generated expected|actual|diff bundle (gitignored).
- `../scripts/test-snapshots.sh` — the single host entry point.

Resist growing the scenario set (TESTING.md: "every extra scenario is slow and a
flake surface").
