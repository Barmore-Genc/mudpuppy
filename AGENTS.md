# AGENTS.md

Guidance for humans and AI agents working **on** the mudpuppy codebase. (For how
an agent uses mudpuppy as a *tool* to review a diff, see the command surface in
[README.md](./README.md), and [PLAN.md](./PLAN.md) for the implementation plan.)

## What this project is

mudpuppy is a Rust terminal UI for collaborative, turn-based code review between
a human and an AI agent. The diff under review can be **local changes** (a
feature branch or uncommitted work, via `git`) or a **GitHub pull request** (via
`gh`, fetch-only). Read [README.md](./README.md) first for the product shape and
[PLAN.md](./PLAN.md) for the design. Both are sketches — treat specifics (flags,
schema fields, module layout) as provisional and confirm before hard-coding
assumptions.

## Ground rules

These mirror the product's hard requirements and apply to the code itself:

- **Entirely local.** Local diffs use `git` and make no network calls at all.
  Reviewing a PR uses the `gh` CLI (which carries the user's auth) **only to
  fetch** the diff — never to write. Don't add direct HTTP clients to GitHub's
  API, don't post reviews/comments, and don't add telemetry, crash reporters, or
  update checks.
- **No AI in the binary.** mudpuppy never calls an LLM. It reads and writes the
  annotation file; the agent is a separate process. Don't add LLM SDKs.
- **Performance is a feature.** Target diffs of 1000+ files and 50k+ lines.
  Prefer lazy loading and virtualized rendering; avoid reading or rendering the
  whole diff eagerly.
- **Keyboard-first.** Every action needs a keyboard path; mouse is optional.
- **The annotation file is the source of truth.** It's a shared contract between
  the TUI and external agents, and the coordination bus for the turn-based loop
  (`agent wait` blocks on filesystem changes to it). Keep it stable, versioned,
  forward-compatible, and record authorship + timestamps so a merge is always
  possible. Writes are atomic (temp + rename) and file-locked, since two
  processes may write concurrently.

## Repository layout

Bootstrapped (see PLAN.md for detail). The crate is split into a library (the
testable core) and a thin binary:

- `src/lib.rs` + `src/main.rs` — library root and the thin binary that parses
  the command tree and dispatches into it.
- `src/` modules: `domain` (schema types — implemented and tested), `cli` (clap
  command surface — implemented), `source` (local-`git` / PR-`gh` diff
  providers), `diff` (hand-rolled unified-diff parser + anchoring), `store`
  (load / merge-by-id / atomic+locked save), `session` (repo/target resolution +
  store-path derivation), `tui` (ratatui app — read-only diff browsing plus
  annotation display + live reload + the `r` turn-release keybind), and `agent`
  (the `agent comment` lifecycle, `diff`, `reset`, and the `agent wait` turn
  rendezvous — it blocks on store-directory changes via `notify`, wakes when the
  human bumps `turn.seq`, and prints what they changed). The human's `r` release
  bumps `seq`, hands ownership back, and doubles as first-contact approval. Still
  to land (milestone 3): `notify`-based reload for the TUI (which still polls the
  store's mtime), the live-session pidfile, authoring annotations from inside the
  TUI, and staleness re-anchoring. Modules carry doc comments describing their
  job and the milestone boundary (PLAN.md §10).
- `Cargo.toml` — manifest. Dependencies are added per-milestone as they're first
  used, not all up front; see the note there and PLAN.md §2.
- `.github/workflows/` — CI: formatting, lints, tests, docs, and a security
  audit. The cargo jobs activate automatically now that `Cargo.toml` exists.

When you add source, keep modules focused. Update this file, the README, and
PLAN.md if you make a structural decision worth knowing.

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

- `git` is a hard runtime dependency (local diffs). `gh` is required only for the
  PR diff source. For each, fail with a clear, actionable message when it's
  missing or (for `gh`) unauthenticated — and only require `gh` when a PR is
  actually requested.
- Don't shell out to `git`/`gh` in ways that assume a specific locale or
  interactive TTY; commands must work headlessly for the agent flows.
- `gh` is read-only here: `gh pr diff` / `gh pr view`. Never call any `gh`
  subcommand that writes to GitHub (`gh pr review`, `gh pr comment`, etc.).
