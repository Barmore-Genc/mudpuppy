# mudpuppy — testing plan

How we test mudpuppy, and *why* the layers are split the way they are. Like
[PLAN.md](./PLAN.md) this is a living design doc, not a contract — update it when
a decision changes.

See [AGENTS.md](./AGENTS.md) for the dev commands (`cargo fmt/clippy/test`) and
ground rules, and [PLAN.md](./PLAN.md) for the build order these tests track.

## The problem

mudpuppy is a TUI. You can't assert on plain text output, because the thing
under test is an interactive, full-screen terminal app: layout, styling,
scrolling, keyboard handling, and clean terminal setup/teardown. We need a way
to *render* it and check what a real user would see.

We use two layers, with deliberately different jobs:

| Layer | Tool | Job | Speed | Runs in CI |
| --- | --- | --- | --- | --- |
| **Unit / integration** | `ratatui` `TestBackend` + `insta` | Correctness of views and key→state→render, in-memory | Fast, deterministic | Every push |
| **End-to-end** | Real binary → `resvg` image-diff snapshots (pinned font + renderer) | Prove the shipped artifact actually works | Slow, few scenarios | Every push (Tier 1–2) |

The rule that keeps this sane: **the e2e layer only tests the seams the unit
layer mocks away.** It does not re-test behavior. Edge cases live in layer 1.

---

## Layer 1 — unit / integration (`TestBackend` + `insta`)

ratatui renders into an in-memory `Buffer` via `TestBackend` — no real terminal,
no PTY. We snapshot that buffer with [`insta`](https://insta.rs) (`.snap` files,
`cargo insta review` to accept/reject, test fails on mismatch).

**What it captures:** the grid of cells — characters *and* styles (fg/bg, bold,
etc.). Not "just text"; we can assert on styling too.

**Driving it by keyboard.** `TestBackend` is only the *output* half. Input is a
separate concern, so this layer is keyboard-drivable **only if the event source
is decoupled from the loop**. We commit to that design:

- Keep a pure-ish `update(state, event)` transition separate from the I/O loop.
- Tests feed real `crossterm` `KeyEvent`s (`KeyCode::Char('j')`, `Down`, …) into
  `update`, then render `state` into a `TestBackend` and snapshot between/after
  keystrokes.

This makes layer 1 a genuine keyboard-driven view harness (e.g. press `j` three
times → snapshot → assert the viewport scrolled), while staying fully
deterministic with no async or timing.

**What it covers:** key → state-change → render for everything, including vim
navigation and per-widget layout/content/style.

**What it cannot see** (→ this is exactly layer 2's checklist):

1. The real binary starting up (`main.rs`, arg parsing, process launch).
2. Real terminal setup/teardown — entering raw mode + alt screen, and
   **restoring** them on exit. The classic TUI shipping bug.
3. Real input *decoding* — crossterm parsing actual tty byte sequences (we hand
   it already-decoded events here).
4. Real rendering by an actual terminal emulator (unicode width, color, wrap).
5. The `git` / `gh` subprocesses (we feed canned diff text here).
6. Real filesystem — atomic write + file lock of the annotation store landing on
   disk.
7. The real event loop and timing.

---

## Layer 2 — end-to-end (real binary, image-diff with a pinned renderer)

Drive the **real compiled binary** in a headless terminal against a **real
fixture git repo**, capture the rendered screen as an image, and diff it against
a committed baseline. No assumptions, no mocks — confirm that once shipped, the
end-user executable works. This is **high-level smoke testing only**; assume all
layer-1 tests pass.

### Image diffing is exact-match, made viable by pinning the renderer + font

We rasterize the captured screen with a **pinned renderer (`resvg`) and a
vendored font file**, then aim for **exact match (zero tolerance)** — a failure
is then unambiguous, with no threshold tuning.

Originally this ran inside a pinned `linux/amd64` Docker container to freeze the
whole environment (OS, libc, fontconfig/freetype, font files, toolchain). We then
measured whether the container was actually load-bearing for the pixels: render
natively on macOS/arm64 and diff against the amd64 baselines. Result — **AE=0 on
every scenario.** `resvg` is a pure IEEE-float rasterizer, so with the font file
and resvg version held constant, arch and OS don't move a pixel. The container was
dropped; what's left in the determinism contract is exactly two inputs: the
**vendored font** (`e2e/fonts/`) and the **resvg version**.

That contract does **not** cover two other sources of nondeterminism, which we
handle explicitly:

1. **App-level nondeterminism — the renderer pins none of this.** Must be
   controlled by us:
   - `jiff` timestamps (`created_at` / `updated_at`) — inject a **fixed clock**.
   - `nanoid` IDs — **seed or redact**.
   - cursor blink / mid-redraw frames — capture at a **settled point** (explicit
     wait in the capture script; cursor hidden or parked).
   - async event ordering.
2. **The capture renderer.** Choice of tool affects this (below).

### Capture tool

Goal is *exact, deterministic, reviewable* output. Preference order:

- **SVG capture** (e.g. the `term-transcript` Rust crate, or `termsvg`) —
  **preferred.** Captures the styled screen as vector SVG: no rasterization step
  to be nondeterministic, text-diffable in git, resolution-independent, still
  human-viewable. Sidesteps the renderer hole (#2) entirely.
  Tradeoff: snapshots *emulated* output, so it won't catch a true GPU
  font-rendering bug — acceptable for "did the screen come out right."
- **`agg`** (asciinema's renderer) — pure Rust rasterizer, no browser in the
  loop; good fallback if we want real pixels.
- **VHS** — avoid for exact-match: it renders xterm.js in headless Chromium, so
  the rasterizer is a whole browser whose version/render-path is one more thing
  to pin. Fine as a **docs/demo** tool (README GIFs), not the test oracle.

### Workflow rule (non-negotiable)

**Baselines are regenerated only via `./scripts/test-snapshots.sh --update`** —
the single entry point, which pins the renderer + vendored font. Don't hand-render
and commit; route every (re)bless through that script so a stray render can't
drift in. (Arch/OS don't matter — see above — but the resvg version and font
file do, and the script is what fixes them.)

### The e2e smoke suite

Roughly 4–7 scenarios total. Each: real binary in the container, real fixture
repo, scripted keystrokes, capture, diff. **Resist growing this** — every extra
scenario is slow and a flake surface; edge cases belong in layer 1.

**Tier 1 — must have (the core promise; runs every push):**

1. **Cold launch on a local diff.** Fixture repo with branch/uncommitted changes
   → run the binary → TUI shows the diff. Proves: binary starts, `git`
   subprocess works, real-diff parsing, first paint in a real emulator.
2. **Keyboard navigation.** Press j/k/page-down/etc. → diff scrolls. Capture at a
   couple of positions. Proves real input decoding end-to-end through the vim
   keymap.
3. **Clean exit restores the terminal.** Quit → assert the terminal is sane
   afterward (cursor visible, echo back, no alt-screen residue). Images barely
   catch this — assert it instead via process exit code 0 and that a follow-up
   command in the same PTY echoes normally. Highest-value, most-overlooked TUI
   check.
4. **Annotation write round-trips to disk.** Add an annotation in the TUI → quit
   → assert the real annotation file on disk has the right content/schema (atomic
   write + lock actually fired). Straddles integration/e2e but belongs here
   because it's the product's whole point.

**Tier 2 — important, not ship-blocking (runs every push):**

5. **Panic / Ctrl-C safety.** Force an error or send SIGINT → terminal still gets
   restored. A user left in raw mode is a terrible first impression.
6. **Resize.** Launch at one size; catches layout that hard-codes 80×24.
7. **Top-level edge states.** No changes to review, or "diff too large" → a sane
   full-screen state, not a blank/garbled screen.

**Tier 3 — gated / out of CI:**

8. **PR path via `gh`.** Needs auth + network, so not hermetic. Run behind a
   feature gate or nightly job; a recorded/fixture `gh` response is the hermetic
   compromise if we want it in CI.

### Assert coarsely, eyeball richly

Even with exact-match image diffs as the gate, keep cheap robust assertions
alongside: process exit code, screen non-blank, expected text present (capture
the emulator's *text* state too, e.g. via `vt100`/SVG text). The image is the
oracle **and** a human-review + docs artifact — regenerate baselines
deliberately (same spirit as `cargo insta review`).

---

## Determinism prerequisites (decide/keep these as the code grows)

These make *both* layers stable; lock them in while the `tui` module is young:

- **Fixed clock** — `jiff` timestamps injectable, not read from the real clock.
- **Seeded/redacted IDs** — `nanoid` controllable in tests.
- **Pinned font + theme + terminal size** in the capture script; disable
  terminal-capability probing so colors don't depend on the host.
- **Deterministic fixture repo** — committed, or built by a script to be
  byte-stable. Never point e2e at the mudpuppy repo itself.
- **Panic-safe terminal teardown** — a guard that restores raw mode / alt screen
  / cursor on `Drop`, so a panic or signal never wrecks the user's terminal.

## Summary

- **Layer 1** is the dense correctness layer: `TestBackend` + `insta`, keyboard
  driven via a decoupled `update(state, event)`. Fast, deterministic, all edge
  cases.
- **Layer 2** is a thin smoke layer over the real artifact: exact-match
  image/SVG snapshots with a pinned renderer + vendored font, ~4–7 scenarios,
  proving *launch → render real diff → real keys → write file → exit without
  wrecking the terminal.*
- Pinning the renderer + font makes image diffing viable (arch/OS proved
  irrelevant); we still own **app-level nondeterminism** and **baseline regen
  discipline**.

---

## Implementation status

What exists today, against the plan above. The shipped feature is the read-only
diff viewer (PLAN.md §9, milestone 1), so the tests track exactly that surface.

### Layer 1 — implemented (`src/tui.rs`, `#[cfg(test)] mod tests`)

The viewer's `App::handle_key` (the `update(state, event)` transition) is driven
with real `crossterm` `KeyEvent`s, rendered into a ratatui `TestBackend`, and
snapshotted with `insta` (`src/snapshots/*.snap`). Snapshots cover the initial
view, tree navigation, diff focus + scroll, jump-to-bottom, the help overlay,
and the binary / metadata-only notices. Behavioural tests cover the rest of the
keymap and viewport math (`j/k`, `}`/`{`, `g`/`G`, `Ctrl-d/u`, `Tab`, `J/K`,
quit keys, help swallowing input); style tests assert the buffer carries the
right colours/modifiers (cyan-bold hunk headers, green/red markers, the
selection and status-bar fills) — *not just text*.

```sh
cargo test --lib tui                 # run them
cargo insta review                   # review/accept snapshot changes
INSTA_UPDATE=always cargo test --lib tui::tests   # regen all (use sparingly)
```

The viewer is store-free today, so it has no clock/id nondeterminism to control
yet — but the fixture diff is a byte-stable `const`, never the live repo.

### Layer 2 — both sub-layers implemented

**(a) PTY smoke suite (`tests/e2e.rs`, every push, no Docker).** Drives the
**real compiled binary** through a PTY against a throwaway fixture git repo and
asserts *coarsely, robustly* — the settled screen via `vt100`, plus the verbatim
alt-screen enter/leave sequences and the process exit code:

| Scenario (TESTING.md tier) | Test |
| --- | --- |
| Tier 1 #1 cold launch on a local diff | `cold_launch_renders_local_diff` |
| Tier 1 #2 keyboard navigation | `keyboard_navigation_updates_the_screen` |
| Tier 1 #3 clean exit restores the terminal | `clean_exit_restores_the_terminal` |
| Tier 2 #7 edge state (no changes) | `no_changes_prints_notice_without_entering_tui` |

Runs via `cargo test --all-features` (ubuntu + macOS). The PTY harness lives in
`tests/common/`. Tier 1 #4 (annotation write round-trips to disk) lands with the
store milestone; Tier 3 #8 (the `gh` PR path) stays gated.

**(b) Exact-match pixel oracle ([`e2e/`](./e2e/), the gold layer).** The real
binary rendered to **actual pixels** — the settled screen emitted as a truecolor
SVG and rasterized losslessly with `resvg` (pinned version + vendored font) —
pixel-diffed (`compare -metric AE`, **zero tolerance**) against committed
baselines (`e2e/baselines/*.png`). Runs natively (no container). Capture
(deterministic Rust, `tests/image_diff.rs`) is split from rasterization+diff
(`e2e/scripts/run.sh`). The workflow — modeled on willet-cloud's
screenshot/approval loop — is:

```sh
./scripts/test-snapshots.sh            # the CI gate: render + pixel-diff
./scripts/test-snapshots.sh --update   # re-bless baselines (the only sanctioned way)
```

On a PR, the `snapshots` CI job runs the check; on any pixel change it publishes
a review bundle (expected | actual | diff `index.html`) to S3 under an
unguessable capability path and comments the link. A maintainer reviews it and
comments `approve snapshots`, which copies the **exact reviewed bytes** back from
the bundle and commits them (no re-render, SHA-pinned). This reuses willet-cloud's
architecture and the same org secrets/vars (`CI_S3_*`, `CI_APPROVE_TOKEN`) — see
`e2e/README.md`. It satisfies the workflow rule: baselines are only ever
generated through `./scripts/test-snapshots.sh --update`.
