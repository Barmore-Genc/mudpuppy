# e2e — the exact-match pixel oracle

This is **Layer 2's exact-match oracle** from [`../TESTING.md`](../TESTING.md):
the real binary, rendered to **actual pixels** and pixel-diffed against committed
baselines at **zero tolerance**.

The design is lifted from willet-cloud's screenshot/approval workflow (Playwright
+ Vitest + pixelmatch in a pinned Playwright image), adapted for a TUI: the
"browser" is a real PTY running the binary, and the "renderer" is
[`resvg`](https://github.com/linebender/resvg) rasterizing the settled screen
(emitted as a truecolor SVG) to a lossless PNG.

> **No container.** This oracle used to run in a pinned `linux/amd64` image. We
> measured the worst case — render natively on macOS/arm64 and diff against the
> amd64 baselines — and got **AE=0 on every scenario**: with the vendored font
> file and the `resvg` version pinned, arch and OS don't move a single pixel
> (resvg is a pure IEEE-float rasterizer). So the container only ever cost build
> time we couldn't cache. It's gone; the job now builds natively and caches deps
> like any other Rust job.

> Runnable today. The fast behavioral layers — Layer 1 (`insta` snapshots in
> `src/tui.rs`) and the Layer-2 PTY smoke suite (`../tests/e2e.rs`) — run on
> every push as plain `cargo test`. *This* layer is the slower gate that proves
> the screen is pixel-for-pixel intended.

## The loop

```sh
# Compare the real renders against committed baselines (the CI gate):
./scripts/test-snapshots.sh

# Re-bless: regenerate baselines (after an intended visual change):
./scripts/test-snapshots.sh --update
```

Both run [`e2e/scripts/run.sh`](./scripts/run.sh) natively; the only difference is
the `--update` flag. There is **no other sanctioned way to make baselines**, so a
stray render can never drift in (the workflow rule from TESTING.md). You need
`cargo`, `resvg` 0.47.0, and ImageMagick on PATH — on macOS:
`brew install resvg imagemagick`.

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
rasterization is the part whose inputs (font + renderer version) must be pinned:

- **Capture:** `tests/image_diff.rs`, gated on `MUDPUPPY_SVG_DIR`, drives the
  binary through a PTY (shared harness in `tests/common/`), waits for the screen
  to settle, and writes a **truecolor SVG** built straight from the vt100 grid —
  one `<rect>` run per background color, one `<text>` run per styled glyph span,
  `textLength`-pinned to an exact column grid.
- **Rasterize + diff:** [`scripts/run.sh`](./scripts/run.sh) rasterizes each SVG
  with `resvg` to a lossless 24-bit PNG (vendored font file via `--use-font-file`,
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
| Renderer | `resvg` 0.47.0 (lossless SVG→PNG) | `scripts/run.sh` (CI installs it in `ci.yml`) |
| Font | DejaVu Sans Mono, exact vendored file via `--use-font-file` | `e2e/fonts/`, `scripts/run.sh` |
| Geometry | 100×24, 9×18 px cells, 15 px font | `tests/common/` (the SVG) |
| Tolerance | 0 differing pixels (`compare -metric AE` == 0) | `scripts/run.sh` |

Only these two inputs — the **font file** and the **resvg version** — move a
pixel; change either and re-bless with `--update`. The viewer is store-free today
(no clock, no ids), so there's no app-level nondeterminism to control yet; revisit
when annotations land (TESTING.md §"Determinism prerequisites").

### Arch / OS note

There used to be a hard "baselines are canonical on amd64 only" rule, on the
theory that float rasterization rounds differently across arches. We tested it:
rendering natively on macOS/arm64 and diffing against the amd64 baselines gives
**AE=0 on every scenario**. So arch and OS are *not* in the determinism contract —
only the vendored font file and the resvg version are. You can regenerate
baselines on any machine with the pinned resvg + the vendored font and they'll
match CI. (If a render ever disagrees by a pixel, suspect a resvg-version skew
first.)

## Files

- `fonts/DejaVuSansMono.ttf` — the vendored, content-pinned glyph source (see
  `fonts/README.md`).
- `scripts/run.sh` — emit SVGs → rasterize → pixel-diff → bundle.
- `baselines/*.png` — committed oracle (the source of truth). ~15–20 KB each.
- `review/` — generated expected|actual|diff bundle (gitignored).
- `../scripts/test-snapshots.sh` — the single host entry point.

Resist growing the scenario set (TESTING.md: "every extra scenario is slow and a
flake surface").
