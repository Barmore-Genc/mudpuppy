# mudpuppy

A terminal UI for reviewing code diffs where a human and an AI agent leave
annotations on the diff *for each other*. Both sides read and write the same
annotation store, the review is **turn-based** (the agent comments, then blocks
until you respond), and annotations persist across sessions.

The diff under review can be your **local changes** (a feature branch, or
uncommitted work) or a **GitHub pull request**. mudpuppy never posts anything
back to GitHub — the review lives entirely on your machine.

> **Status:** early bootstrap. Nothing here is load-bearing yet — the design
> below is a sketch, not a contract. Names, flags, the on-disk schema, and the
> command surface are all expected to change as the implementation lands. See
> [PLAN.md](./PLAN.md) for the implementation plan.

## Why

Reviewing code with an agent usually means copy-pasting diffs into a chat and
losing all the line-level context. mudpuppy keeps the diff, the review, and the
back-and-forth in one place: you annotate lines in a TUI, the agent annotates
the same lines from the command line, and neither side has to reconstruct
context from chat. Because it works on local changes too, you can review your
own branch with an agent before it ever becomes a PR.

## The idea

You open a diff in a terminal UI:

- A **file tree** of changed files with `+`/`-` counts.
- A **diff pane** with syntax highlighting and hunk navigation.
- A **status bar** with diff metadata and annotation counts
  (e.g. "3 from agent, 1 from me, 2 resolved").

Annotations have an **author** (human or agent), a **severity**
(info / suggestion / warning / blocker), an optional **tag**
(`?` question, `!` concern, `>` direction), a **status**
(open / resolved / wontfix / withdrawn), and a free-text markdown **body**. They
can be line-level or (eventually) hunk-level, and replies thread under a parent
annotation.

### Reading & writing as the human

- Marked lines show a gutter marker; expand inline, or open a side panel listing
  every annotation with jump-to-line.
- Leave a comment on any line (including unchanged context), tag it, and save.
- Reply to an agent annotation to thread under it.
- Mark annotations resolved / wontfix / open.
- When the agent is waiting on you, the status bar says so; respond, then
  **release the turn** (a keybind) to hand control back to the agent.

### Handing off to the agent

The agent is a first-class CLI citizen, not a file-format consumer. You point
your agent at mudpuppy and let it drive:

> "Review my changes with mudpuppy. Run `mudpuppy agent --help` first to learn
> how."

That help text *is* the agent's instructions (severity meanings, how to comment,
how to wait). A review is then a turn-based loop, all mediated by a single
annotation file — the **state file is the source of truth**, and either side can
read or write it:

1. The agent reads the diff (`mudpuppy agent diff`).
2. It leaves annotations (`mudpuppy agent comment add --line 34 …`), each landing
   in the shared store; if your TUI is open, they appear live.
3. It can revise mid-turn — `comment edit` to change its mind, `comment cancel`
   to retract — before handing control back.
4. It calls `mudpuppy agent wait`, which **blocks** until you respond in the TUI
   and release the turn. `wait` then prints your replies and status changes, and
   the agent continues. Repeat until done.

If you haven't opened the TUI yet, that's fine: the agent's comments are still
written to the store, and `wait` simply blocks until you launch mudpuppy and
approve the agent.

### Across sessions

State persists locally, keyed by the **repository** and what's being reviewed
(local changes vs. a specific PR) — not by a transient process. Close mudpuppy
(even by accident) and reopen it in the same repo to pick up exactly where you
left off. If the diff drifts (you commit more, or a PR gets new commits),
mudpuppy flags annotations now anchored to changed lines ("3 annotations stale —
re-anchor?") and offers to remap or drop them. To start a review over from
scratch, `reset` clears the current session's annotations.

## Hard requirements (guiding principles)

- **Entirely local.** Local diffs use `git` and touch no network at all.
  Reviewing a PR uses the [`gh`](https://cli.github.com/) CLI (your existing
  auth) **only to fetch** the diff — mudpuppy never writes to GitHub. No servers,
  no accounts.
- **No AI built in.** mudpuppy never calls an LLM. It only reads and writes the
  annotation file; the agent runs as a separate process.
- **Scales to large diffs** (1000+ files, 50k+ lines) — virtualized rendering,
  lazy file loading.
- **Keyboard-driven**, mouse optional.

## Command surface (sketch)

The binary is `mudpuppy` (a shorter `mudpup` alias is likely too).

**Human-facing:**

| Command | Purpose |
| --- | --- |
| `mudpuppy` | Open the TUI on the current repo's local changes |
| `mudpuppy --base <ref>` | Review against an explicit base ref |
| `mudpuppy <pr-url>` | Open the TUI on a GitHub PR |

**Agent-facing** (all under `mudpuppy agent`):

| Command | Purpose |
| --- | --- |
| `mudpuppy agent --help` | Self-documenting usage the agent reads first |
| `mudpuppy agent diff [--file F]` | Print the diff under review |
| `mudpuppy agent comment add --file F --line N [--side right\|left] [--severity …] [--tag …] [--reply-to ID] --body "…"` | Leave an annotation |
| `mudpuppy agent comment list [--open] [--author human\|agent] [--file F]` | Read current annotations (incl. your replies) |
| `mudpuppy agent comment edit --id ID [--body … \| --severity … \| --tag … \| --status …]` | Revise its own annotation |
| `mudpuppy agent comment cancel --id ID` | Retract its own annotation |
| `mudpuppy agent comment resolve\|reopen\|wontfix --id ID` | Change status |
| `mudpuppy agent wait [--timeout S]` | Block until the human releases the turn; print their changes |
| `mudpuppy agent reset` | Clear the current session's annotations and start fresh |

PR references accept both `<owner>/<repo>#123` and full PR URLs. Agent commands
target the current repo by default (inferred from the working directory) and
attach to a running TUI session if one is open; see [PLAN.md](./PLAN.md) for the
full session-resolution rules.

## Annotation schema (sketch)

```json
{
  "schema_version": 1,
  "target": { "kind": "local", "base": "main", "head_sha": "abc123" },
  "annotations": [
    {
      "id": "V1StGXR8",
      "author": "agent",
      "file": "src/auth.ts",
      "line": 42,
      "side": "RIGHT",
      "severity": "suggestion",
      "tag": "?",
      "body": "markdown text",
      "status": "open",
      "reply_to": null,
      "created_at": "2026-05-28T12:00:00Z",
      "updated_at": "2026-05-28T12:00:00Z"
    }
  ]
}
```

`id` is a short [nanoid](https://github.com/ai/nanoid); `author` is `agent` |
`human`; `side` is `RIGHT` | `LEFT`; `severity` is `info` | `suggestion` |
`warning` | `blocker`; `tag` is `?` | `!` | `>` | `null`; `status` is `open` |
`resolved` | `wontfix` | `withdrawn`; `reply_to` is an `id` | `null`. `target`
records what's being reviewed so a stored review can be matched back to its diff.

State lives under the platform data dir, keyed by repo and target, e.g.
`~/.local/share/mudpuppy/<repo-slug>/<target>/annotations.json`. Set
`MUDPUPPY_DATA_DIR` to override where the store lives (handy for scratch reviews
and tests).

## Ideas not yet committed to

- Hunk-level annotations ("this whole function is the problem").
- A diff minimap showing annotation density for fast jumps.

## Building

```sh
cargo build
cargo test
```

`git` is required (for local diffs). [`gh`](https://cli.github.com/) is required
only when reviewing a GitHub PR; mudpuppy fails with a clear message if you ask
for a PR without it. See [AGENTS.md](./AGENTS.md) for contributor and agent
guidance and [PLAN.md](./PLAN.md) for the implementation plan.

## License

[GNU AGPL v3.0 only](./LICENSE).
