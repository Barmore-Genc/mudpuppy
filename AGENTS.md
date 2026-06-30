mudpuppy is a Rust TUI for collaborative, turn-based code review between
a user and an AI agent. Both user and AI agent can leave comments attached
to lines of code, then hand over to the other once they are done.
The rich interface can display uncommitted work, any diff, or Github pull requests.

- mudpuppy is local, working on the same machine as the AI agent, on the machine where the code is checked out.
- AI is not integrated into mudpuppy, instead it is a tool that both a user and an AI agent interact with.

## Conventions

- **Errors:** prefer real error types (`thiserror`/`anyhow`-style), avoid panics.
- **Tests:** colocate unit tests with modules; put end-to-end CLI behavior under
  `tests/`. There are two levels of tests:
  - The Layer-1 `insta` `.snap` file tests capture only the character grid, not
    color. Assert colors with buffer helpers in the `tui` tests.
  - The Layer-2 pixel-oracle baselines (`e2e/baselines/`) render the actual terminal
    as an image, including all colors and styles. Do not try to regenerate these
    baselines yourself, there is an approval workflow once a PR is created that
    will automatically trigger.
- **Unsafe**: No unsafe Rust allowed. The only exception is libraries that
  use unsafe code internally, for FFI for example.
- **Comments:**
  - Keep comments brief. Do not explain what the code is doing, explain *why* it's doing it.
    Do not explain things obvious from reading the code.
  - **Doc-comment links.** In Rust doc comments (`///`, `//!`) the syntax
    `` [`name`] `` is an *intra-doc link*, not emphasis or highlighting, and CI
    builds docs with `-D warnings` so a link to a private or unresolved item is a
    hard build failure. Mention items with a plain code span (`` `run_loop` ``);
    only use the bracket form when the target is public and you want a real
    clickable link (`` [`crate::highlight`] ``).
- **File size:** Avoid code files of length 1000+ lines. If a file gets too long, try to split it logically into multiple files, creating additional folders if needed.

## Where the code lives

`src/` (the binary is a thin shell; all logic is in the `mudpuppy` library):

- `main.rs`: binary entry point; parses the CLI and dispatches into the library.
- `lib.rs`: crate root and module map.
- `cli.rs`: the clap command tree and top-level dispatch (`run`); opens this process's role-split debug log (`init_debug_log`) when the config enabled it.
- `agent.rs`: the `mudpuppy agent` subcommands (add/comment/wait/reset…) over the store; `reset [--base REF]` clears the round and can switch the review base.
- `install/`: `mudpuppy install claude` — writes the two Claude Code skills (PR review, implementation review) at a chosen scope (project/local/user).
- `source.rs`: diff-source providers — shells out to `git` (local) and `gh` (PR); `diff_for_target` re-fetches a diff for an existing target (TUI base switch); emits privacy-safe base/merge-base/shallow diagnostics.
- `blob.rs`: full file-content provider (working tree / `git show` / `gh api`) for context expansion and added/comment-only files; also captures relocation signatures (`capture_signature`).
- `diff.rs`: hand-rolled unified-diff parser, lazy hunks, line ↔ side anchoring.
- `anchor.rs`: change-resilient location anchors — capture a line+context signature and relocate it in an edited file (exact-then-fuzzy edit-distance cascade), else orphan.
- `highlight.rs`: syntect syntax highlighting for the diff pane.
- `store.rs`: load / merge-by-id / atomic+locked save of the annotation store.
- `session.rs`: repo + target resolution and store-path derivation (resume).
- `update.rs`: self-update — release check by HTTPS GET of the latest GitHub Release's `dist-manifest.json` (stable `releases/latest/download` URL, no `gh` needed), semver comparison, and install by downloading the prebuilt release archive for this build's target (`build.rs` captures the triple), verifying its SHA-256, and swapping the running binary in place (`self_replace`) — no Rust toolchain needed; surfaced to Lua as `mudpuppy.updates` and driven by `core.luau`'s update prompt.
- `tui/`: the ratatui app — file tree, diff pane, gutter marks, panels, turn release; key presses route through the Lua engine.
- `picker.rs`: fuzzy-find file picker state + subsequence matcher for the "add any file" overlay.
- `command.rs`: the `:command` palette state — fuzzy filtering over registered command names (reuses the picker's matcher).
- `logging.rs`: file logging gated by `MUDPUPPY_LOG` (single file) or the `mudpuppy.debug_log` config setting (role-split dir); thread-local capture sink for tests; `log_debug!`/`info`/`warn`/`error` macros; `hash()` salted non-reversible labels (salt = store `log_seed`) so logs never record names/paths.
- `lua/`: embedded Luau sandbox — the configurable keymap and event hooks. Bindings are keyed on key *sequences* (multi-key chords, a `<leader>`, and count prefixes), resolved by a sequence state machine. All default bindings live in `lua/core.luau`; the user config is `$MUDPUPPY_CONFIG`, else `$XDG_CONFIG_HOME`/`~/.config/mudpuppy/mudpuppy.luau` (`%APPDATA%` on Windows). Rust keeps only a hardwired Ctrl-C quit.
- `domain/`: pure on-disk schema types (`Annotation`, `StateFile`, enums).

`tests/` (end-to-end over the real compiled binary):

- `agent.rs`: e2e tests of the `agent` command surface (captured stdout, throwaway repo).
- `e2e.rs`: PTY smoke tests over a real fixture repo (Layer 2, coarse grid assertions).
- `image_diff.rs`: emits truecolor SVGs for the pixel-oracle baselines (gated on `MUDPUPPY_SVG_DIR`).
- `common/mod.rs`: shared PTY harness used by both e2e suites.

## Keeping AGENTS.md files updated

Each `src/` subfolder has an `AGENTS.md` (mirrored by a `CLAUDE.md` containing only `@AGENTS.md`) mapping its files to one-line descriptions, so a reader can find code by topic without grepping. `src/` and `tests/` themselves are covered by the "Where the code lives" section above, not their own files.

When you add, remove, or repurpose a file or folder, update the matching `AGENTS.md` entry in the same change. Keep each entry to 10–20 words: say where to find what, not how it works. New `src/` subfolder → add both `AGENTS.md` and `CLAUDE.md` (`@AGENTS.md`).

## PR Descriptions

Write complete but concise PR descriptions.
Describe the work from the point of view of someone who may not have context on what
work you were doing. What did you implement, and why? Don't just repeat the code or
explain things that are obvious from the code.

<examples>

Bad: (obvious, routine process, already enforced via CI)

    Per our workflow, re-blessing baselines is the reviewer's call, not part of this change. 

Bad: (already enforced via CI)

    cargo fmt --check, cargo clippy --all-targets --all-features -D warnings, and the full cargo test suite are green. 

Good:

    Closes #12. Adds syntax highlighting using syntect.

Bad: (the fact it was review discussion is irrelevant, only final state is) 

    Per review discussion, we changed ... 

Good:

    Replaces the TUI's mtime-poll live-reload stand-in with a notify watch on the store directory.

</examples>

