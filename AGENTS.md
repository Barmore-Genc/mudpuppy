# AGENTS.md

Guidance for humans and AI agents working **on** the mudpuppy codebase. (For how
an agent uses mudpuppy as a *tool* to review a PR, see the command surface in
[README.md](./README.md).)

## What this project is

mudpuppy is a Rust terminal UI for collaborative GitHub PR review between a human
and an AI agent. Read [README.md](./README.md) first for the product shape. The
design there is a sketch — treat specifics (flags, schema fields, module layout)
as provisional and confirm before hard-coding assumptions.

## Ground rules

These mirror the product's hard requirements and apply to the code itself:

- **Entirely local.** The only outbound network calls are to GitHub, and they go
  through the `gh` CLI (which carries the user's auth). Don't add direct HTTP
  clients to GitHub's API, telemetry, crash reporters, or update checks.
- **No AI in the binary.** mudpuppy never calls an LLM. It reads and writes the
  annotation file; the agent is a separate process. Don't add LLM SDKs.
- **Performance is a feature.** Target diffs of 1000+ files and 50k+ lines.
  Prefer lazy loading and virtualized rendering; avoid reading or rendering the
  whole diff eagerly.
- **Keyboard-first.** Every action needs a keyboard path; mouse is optional.
- **The annotation file is the source of truth.** It's a shared contract between
  the TUI and external agents. Keep it stable, versioned, forward-compatible,
  and record authorship + timestamps so a merge is always possible.

## Repository layout

Still being established. Expected shape for a Rust CLI + TUI:

- `src/` — application code (not written yet).
- `Cargo.toml` — manifest (not added yet; no source means nothing to build so
  far). When source lands, CI will build and test it.
- `.github/workflows/` — CI: formatting, lints, and tests.

When you add source, keep modules focused (e.g. GitHub/`gh` interaction, the
annotation store, diff parsing/rendering, TUI). Update this file and the README
if you make a structural decision worth knowing.

## Local development

Once a Cargo project exists:

```sh
cargo fmt --all                      # format
cargo clippy --all-targets --all-features -- -D warnings   # lint
cargo test --all-features            # tests
cargo build                          # build
```

Before pushing, run fmt, clippy, and tests — CI runs the same and will reject
warnings.

## Conventions

- **Formatting:** `rustfmt` defaults; CI enforces `cargo fmt --check`.
- **Lints:** clippy must be clean; warnings are denied in CI.
- **Toolchain:** use the latest stable Rust available on your system; we don't
  pin via `rust-toolchain.toml`. CI tracks `stable`.
- **Errors:** prefer real error types (`thiserror`/`anyhow`-style) over panics in
  library and command paths; reserve panics for genuine invariants.
- **Tests:** colocate unit tests with modules; put end-to-end CLI behavior under
  `tests/`. The annotation schema and any handoff/merge logic deserve tests
  since they're the cross-process contract.
- **Dependencies:** keep them lean and justified — this is a local tool. Vet new
  crates against the "entirely local, no AI" rules above.

## Commits & PRs

- Small, focused commits with clear messages.
- A PR should pass fmt, clippy, and tests locally before review.
- Note any change to the annotation schema or CLI surface prominently in the PR
  description — those are user- and agent-facing contracts.

## Tooling notes

- The `gh` CLI is a hard runtime dependency. Assume it's installed and
  authenticated; fail with a clear, actionable message when it isn't.
- Don't shell out to `git`/`gh` in ways that assume a specific locale or
  interactive TTY; commands must work headlessly for the agent flows.
