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
| **End-to-end** | Real binary in a **pinned Docker container** → image-diff snapshots | Prove the shipped artifact actually works | Slow, few scenarios | Every push (Tier 1–2) |

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

## Layer 2 — end-to-end (real binary, image-diff in a pinned container)

Drive the **real compiled binary** in a headless terminal against a **real
fixture git repo**, capture the rendered screen as an image, and diff it against
a committed baseline. No assumptions, no mocks — confirm that once shipped, the
end-user executable works. This is **high-level smoke testing only**; assume all
layer-1 tests pass.

### Image diffing is exact-match, made viable by a pinned container

We run capture inside a Docker container with everything pinned (OS, libc,
fontconfig/freetype, font files, theme, terminal/renderer version). Inside that
box we aim for **exact match (zero tolerance)** — a failure is then unambiguous,
with no threshold tuning.

The container removes *environmental* drift. It does **not** remove three other
sources of nondeterminism, which we handle explicitly:

1. **CPU architecture.** Dev is Apple Silicon (arm64); CI may be amd64. Font
   rasterization does float math that rounds differently across arches, so "same
   image, different arch" can differ by pixels. **Decision: baselines are
   canonical on one arch only** (CI's). Pin `--platform`; if dev and CI arch
   differ, dev regenerates under the canonical arch (qemu) or simply doesn't
   commit host-generated baselines. See the workflow rule below.
2. **App-level nondeterminism — the container pins none of this.** Must be
   controlled by us:
   - `jiff` timestamps (`created_at` / `updated_at`) — inject a **fixed clock**.
   - `nanoid` IDs — **seed or redact**.
   - cursor blink / mid-redraw frames — capture at a **settled point** (explicit
     wait in the capture script; cursor hidden or parked).
   - async event ordering.
3. **The capture renderer.** Choice of tool affects this (below).

### Capture tool

Goal is *exact, deterministic, reviewable* output. Preference order:

- **SVG capture** (e.g. the `term-transcript` Rust crate, or `termsvg`) —
  **preferred.** Captures the styled screen as vector SVG: no rasterization step
  to be nondeterministic, text-diffable in git, resolution-independent, still
  human-viewable. Sidesteps the arch (#1) and renderer (#3) holes entirely.
  Tradeoff: snapshots *emulated* output, so it won't catch a true GPU
  font-rendering bug — acceptable for "did the screen come out right."
- **`agg`** (asciinema's renderer) — pure Rust rasterizer, no browser in the
  loop; good fallback if we want real pixels.
- **VHS** — avoid for exact-match: it renders xterm.js in headless Chromium, so
  the rasterizer is a whole browser whose version/render-path is one more thing
  to pin. Fine as a **docs/demo** tool (README GIFs), not the test oracle.

### Workflow rule (non-negotiable)

**Baselines are regenerated only inside the pinned container, on the canonical
arch.** Never capture on the host Mac and commit that — it will differ from CI
and thrash. "Regen baselines" must be a single containerized command so there is
no other path.

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
  image/SVG snapshots in a pinned container, ~4–7 scenarios, proving *launch →
  render real diff → real keys → write file → exit without wrecking the
  terminal.*
- The container makes image diffing viable; we still own **arch**, **app-level
  nondeterminism**, and **baseline regen discipline**.
