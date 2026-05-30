# mudpuppy — implementation plan

This is the working design for mudpuppy. It captures the decisions made so far,
the rationale behind them, and a concrete build order. Like the README it's a
sketch, not a contract — but it's the canonical place to record *why* things are
the way they are. Update it when a decision changes.

See [README.md](./README.md) for the product shape and [AGENTS.md](./AGENTS.md)
for contributor ground rules.

## 1. What we're building

A keyboard-driven terminal UI for **collaborative, turn-based code review**
between a human and an AI agent. Both annotate the same diff, for each other,
through a shared on-disk store. The agent participates entirely through a
self-documenting CLI surface; mudpuppy itself contains no AI.

The diff under review comes from one of two **sources**:

- **Local** (default) — the user's `git` changes. No network.
- **PR** — a GitHub pull request, fetched read-only via `gh`.

Non-negotiables (from AGENTS.md): entirely local, no AI in the binary, no writes
to GitHub, scales to 1000+ files / 50k+ lines, keyboard-first, and the
annotation file is the source of truth and cross-process contract.

## 2. Library choices

| Concern | Crate | Rationale |
| --- | --- | --- |
| TUI | `ratatui` + `crossterm` | De-facto standard; crossterm is cross-platform (macOS dev, Linux CI). |
| CLI parsing | `clap` (derive) | Maps cleanly to the subcommand tree; generates the `agent --help` the agent reads. |
| Async runtime | `tokio` | Drives the event loop, overlaps `gh`/`git` subprocess work, and powers the blocking `agent wait`. |
| Schema (de)serialization | `serde` + `serde_json` | The annotation file *is* JSON. |
| Error types | `thiserror` (libs) + `anyhow` (binary/commands) | Per AGENTS.md conventions. |
| IDs | `nanoid` (length 8, URL-safe alphabet) | Short, JSON/filename-safe, token-cheap. We dedupe-on-write so practical collisions are zero. |
| Timestamps | `jiff` | Modern, lean ISO-8601 for `created_at` / `updated_at`. |
| Platform data dir | `directories` | Resolves `~/.local/share/mudpuppy/…` per-OS. |
| Atomic + locked writes | `tempfile` + `fs4` | Write-temp-then-rename; advisory file lock so concurrent writers don't corrupt the store. |
| Filesystem watch | `notify` | The coordination bus: live TUI reload and the `agent wait` rendezvous. |
| Syntax highlighting | `syntect` | In-process, self-contained, full control over the gutter/annotation overlay. |

Deliberately **not** used: any HTTP/GitHub-API client (`octocrab`, `reqwest`),
any LLM SDK, `git2`/`gix` (we shell out to `git`, consistent with `gh`), and any
external renderer like `delta`/`difftastic` (syntect keeps us self-contained).

## 3. Module layout

```
src/
  main.rs        # binary entry; dispatches clap commands
  cli.rs         # clap command tree (human + `agent` subcommands)
  domain/        # pure types: Annotation, Severity, Tag, Status, Side, Author,
                 #   Target, StateFile (versioned). serde + invariants. Most-tested.
  source/        # diff-source providers: trait + `local` (git) + `pr` (gh).
                 #   Resolves base ref, head sha, and produces a raw unified diff.
  diff/          # hand-rolled unified-diff parser -> files/hunks/lines;
                 #   the line<->(side,number) mapping; anchoring + staleness.
  store/         # load / merge-by-id / save; atomic+locked writes; the turn
                 #   protocol (turn counter, agent-waiting flag, approval).
  session/       # repo + target resolution, store-path derivation, resume,
                 #   liveness (session.json pidfile/heartbeat), reset.
  tui/           # ratatui app: file tree, diff pane, status bar, side panel,
                 #   event loop, virtualized rendering, live reload.
  agent/         # implementation of the `agent` subcommands over store/session.
```

Keep modules focused; `domain` and `store` carry the cross-process contract and
deserve the heaviest tests.

## 4. Data model

The store is one JSON file per `(repo, target)`. Top level is **versioned** for
forward compatibility:

```jsonc
{
  "schema_version": 1,
  "target": { "kind": "local", "base": "main", "head_sha": "abc123" },
  // or:     { "kind": "pr", "pr": "owner/repo#123", "head_sha": "abc123" }
  "turn": { "owner": "human", "seq": 7, "agent_waiting": true, "approved": true },
  "annotations": [ /* … */ ]
}
```

An **annotation**:

| Field | Type | Notes |
| --- | --- | --- |
| `id` | nanoid(8) | Stable; assigned on creation; dedup-checked against the file. |
| `author` | `agent` \| `human` | Who wrote it. |
| `file` | string | Path within the diff. |
| `line` | int | Anchored line number. |
| `side` | `RIGHT` \| `LEFT` | Which side of the diff (added vs. removed/context). Kept purely for anchoring. |
| `severity` | `info` \| `suggestion` \| `warning` \| `blocker` | — |
| `tag` | `?` \| `!` \| `>` \| `null` | question / concern / direction. |
| `status` | `open` \| `resolved` \| `wontfix` \| `withdrawn` | `withdrawn` = soft-retracted (see §7). |
| `body` | string | Markdown. |
| `reply_to` | id \| `null` | Threads a reply under a parent. |
| `created_at` | ISO-8601 | — |
| `updated_at` | ISO-8601 | Bumped on any edit/status change. |

Notably **absent**: anything about GitHub posting (no `promote_to_github`) —
mudpuppy never writes to GitHub.

### Merge semantics

Saves are **merge-by-id**, never whole-file clobber: load current → apply this
process's changes by `id` (add new, update existing, last-writer-wins per field)
→ atomic write under lock. This keeps a live TUI and a headless agent writing
"at the same time" from losing each other's work.

## 5. Sessions: identity, resume, reset

The hard problem the design has to solve: an agent invoked from a shell needs to
find the *right* review, the user needs **resume** after closing the TUI, and it
all has to work for local-only repos with no PR number.

**Session key = canonical git repo root + review target.** Target is `local`
(default) or `pr:<n>`. The store path encodes both:
`<data-dir>/mudpuppy/<repo-slug>/<target>/annotations.json`, where `<data-dir>`
is the platform data dir (`~/.local/share` on Linux) or the `MUDPUPPY_DATA_DIR`
override, and `<repo-slug>` is the remote `owner/repo` when there is one, else
the sanitized canonical repo path (so no-remote repos work). A `local` target
maps to the `local/` subdir; a PR maps to `pr/<sanitized>/`. Keying off the
**repo, not a process**, is what makes resume automatic: reopen in the same repo
→ same path → state reloads.

> **Status (milestone 2):** path derivation + the store (load / merge-by-id /
> atomic+locked save) are implemented (`session`, `store`). The liveness pidfile,
> live-session attach, and reset-vs-staleness distinction are still to come; the
> agent currently resolves the `local` target directly.

**Resolution order** for any command (no flags needed in the common case):

1. Explicit `--pr <n>` / PR URL → that target.
2. Else, if a live TUI session exists for this repo → attach to its target.
3. Else default to `local`.

Repo is inferred from the working directory's git root. "Attaching" is implicit:
same store dir = same session, because both processes only touch that dir and
watch it (the `notify` bus). A small `session.json` (pid + heartbeat, cleaned up
when stale) lets a command *know* whether a live TUI is actually listening, which
drives `wait` messaging and the approval prompt.

**Default local target** (when target is `local`):

1. Determine the repo's default branch (`origin/HEAD` → fallback `main` → `master`).
2. On a **feature branch**: diff = `merge-base(default, HEAD) … working tree` —
   all of this branch's work, **including uncommitted edits**, so nothing
   in-flight is invisible.
3. On **main/master itself**: diff = uncommitted working changes vs. `HEAD`.
4. `--base <ref>` overrides the base explicitly.

**Reset.** `mudpuppy agent reset` (and a TUI keybind) clears the current
session's annotations to start a fresh round. This is distinct from
*staleness/re-anchoring* (§8), which is automatic drift handling; reset is a
deliberate clean slate.

## 6. The turn protocol (`agent wait`)

The review is turn-based, and the synchronization primitive is `agent wait`,
implemented **serverless over the filesystem** — no socket, no daemon.

- The store dir holds the annotations plus a `turn` block with a monotonic `seq`,
  an `owner`, an `agent_waiting` flag, and an `approved` flag.
- The agent comments freely, then calls `agent wait`. `wait` records the current
  `seq`, sets `agent_waiting = true`, and **blocks** using `notify` to watch the
  store dir.
- In the TUI the human sees "agent is waiting," responds (replies, status
  changes), then **releases the turn** (a keybind), which increments `seq` and
  sets `owner = agent`.
- `wait` wakes on the change, prints everything the human did since the recorded
  `seq` (to stdout, for the agent to read), and exits 0.

**Approval / first contact.** The first time an agent shows up on a session, the
TUI surfaces "an agent wants to collaborate — approve?". The human's first
turn-release doubles as approval (`approved = true`). This also handles the
"user forgot to launch the TUI" case: `agent wait` just blocks until the TUI is
launched and the agent approved. `wait` takes a `--timeout` and handles Ctrl-C
cleanly, since it can otherwise block indefinitely.

Both processes only ever read/write/watch the store dir, which is exactly the
"annotation file is the source of truth" rule extended to coordination.

## 7. Agent CLI surface

`mudpuppy agent --help` is the agent's entry point and must be genuinely
self-documenting: severity/tag/status meanings, how anchoring works, and the
turn loop. Verbs:

- `agent diff [--file F]` — print the unified diff under review.
- `agent comment add --file F --line N [--side right|left] [--severity …] [--tag …] [--reply-to ID] --body "…"` — create.
- `agent comment list [--open] [--author human|agent] [--file F]` — read current state, including the human's replies.
- `agent comment edit --id ID [--body … | --severity … | --tag … | --status …]` — revise **its own** open annotation in place (`updated_at` bumped).
- `agent comment cancel --id ID` — retract **its own** annotation. Hard-delete if it has no replies (turn-internal "changed my mind" noise vanishes); soft-retract to `withdrawn` if the human already replied, so the thread stays coherent.
- `agent comment resolve|reopen|wontfix --id ID` — status changes.
- `agent wait [--timeout S]` — the turn rendezvous (§6).
- `agent reset` — clear the session (§5).

Edit/cancel are scoped to the agent's own annotations; it can't rewrite the
human's. Writes always work whether or not a TUI is running (they persist to the
store); only `wait` needs a live human to ever unblock.

## 8. Diff parsing, anchoring, staleness

- **Parsing**: a small hand-rolled unified-diff parser, tuned to exactly the
  shape `git diff` / `gh pr diff` emit and to the anchoring we need (file →
  hunks → lines, each line tagged with its `LEFT`/`RIGHT` number). Zero-dep,
  full control, no foreign model to adapt.
- **Anchoring**: an annotation binds to `(file, side, line)` against a specific
  `head_sha`. Rendering maps that back to a screen row.
- **Staleness**: when the diff drifts (more local commits, or a PR's head moves),
  the stored `head_sha` no longer matches. mudpuppy detects this, flags affected
  annotations ("N stale — re-anchor?"), and offers to remap (best-effort via
  surrounding context) or drop. Re-anchoring heuristics can start simple
  (exact-context match) and improve later.

## 9. TUI

- **Layout**: file tree (left) · diff pane (center) · status bar (bottom) · a
  side panel listing annotations with jump-to-line. Gutter markers on annotated
  lines; inline expand or panel view.
- **Virtualized rendering**: only build/render rows in view; never materialize a
  50k-line diff. Lazy per-file parsing — parse file metadata + counts up front,
  defer hunk/line content until a file is opened.
- **Live reload**: watch the store dir (`notify`); when the agent writes, refresh
  in place. Surface "agent is waiting" and the approval prompt here.
- **Keyboard-first**: every action has a key path; mouse optional.

## 10. Build order (milestones)

1. **Read-only viewer.** ✅ Done. Cargo project + module skeleton; the
   diff-source abstraction (local `git` + PR `gh`); hand-rolled parser; ratatui
   browsing (file tree / diff pane / status bar) with virtualized rendering. (Syntect
   highlighting deferred.) Proved the hard rendering/scale problem early.
2. **Annotations in the store + TUI.** 🟡 In progress. Done: the `store`
   (atomic+locked merge-by-id writes), `session` store-path resolution, the
   `agent comment` lifecycle (add / list / edit / cancel / resolve / reopen /
   wontfix) + `diff` + `reset`, and TUI **display** (gutter markers, side panel,
   status count) with live reload (now `notify`-based — see milestone 3). Still
   to do: authoring annotations from *inside* the TUI (needs a line cursor), and
   anchoring + staleness handling. This is the proof-of-concept slice: the agent
   writes, the human's TUI shows it live.
3. **Turn protocol.** 🟡 In progress. Done: the `agent wait` rendezvous over
   `notify` — it records `turn.seq`, marks `agent_waiting`, blocks on
   store-directory changes, wakes when the human bumps `seq`, and prints the
   human's added/changed/removed annotations (honoring `--timeout` and Ctrl-C);
   and the TUI's `r` turn-release keybind, which bumps `seq`, hands ownership
   back to the agent, surfaces "agent is waiting" in the status bar, and doubles
   as first-contact approval; and the TUI's live reload now watches the store
   directory over the same `notify` bus (replacing the mtime poll), so an agent's
   writes refresh the view in place. Still to do: the live-session pidfile.
   Closes the end-to-end turn-based loop.

## 11. Testing

- `domain` + `store`: heaviest coverage — schema round-trips, merge-by-id,
  concurrent-write safety, turn-counter transitions. This is the cross-process
  contract.
- `diff`: parser fixtures (renames, binary files, no-newline-at-EOF, empty
  hunks) and anchoring/staleness cases.
- `session`: resolution order, repo-slug derivation, resume, reset.
- CLI behavior under `tests/`, runnable headlessly (no TTY/locale assumptions),
  including the `agent` flows.

## 12. Open questions / future

- Re-anchoring heuristics beyond exact-context matching.
- Hunk-level annotations ("this whole function").
- A diff minimap showing annotation density.
- Whether multiple concurrent agents on one session ever needs richer identity
  than "author = agent".
