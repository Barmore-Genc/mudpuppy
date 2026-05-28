# mudpuppy

A terminal UI for reviewing GitHub pull requests where a human and an AI agent
leave annotations on the diff *for each other*. Both sides read and write the
same annotation store, and annotations persist across sessions.

> **Status:** early bootstrap. Nothing here is load-bearing yet — the design
> below is a sketch, not a contract. Names, flags, the on-disk schema, and the
> command surface are all expected to change as the implementation lands.

## Why

Reviewing a PR with an agent usually means copy-pasting diffs and losing all the
line-level context. mudpuppy keeps the diff, the review, and the conversation in
one place: you annotate lines in a TUI, the agent annotates the same lines from
a headless command, and neither side has to reconstruct context from chat.

## The idea

You open a PR in a terminal UI:

- A **file tree** of changed files with `+`/`-` counts.
- A **diff pane** with syntax highlighting and hunk navigation.
- A **status bar** with PR metadata and annotation counts
  (e.g. "3 from agent, 1 from me, 2 resolved").

Annotations have an **author** (human or agent), a **severity**
(info / suggestion / warning / blocker), an optional **tag**
(`?` question, `!` concern, `>` direction), a **status**
(open / resolved / wontfix), and a free-text markdown **body**. They can be
line-level or (eventually) hunk-level, and replies thread under a parent
annotation.

### Reading & writing as the human

- Marked lines show a gutter marker; expand inline, or open a side panel listing
  every annotation with jump-to-line.
- Leave a comment on any line (including unchanged context), tag it, and save.
- Reply to an agent annotation to thread under it.
- Mark annotations resolved / wontfix / open.

### Handing off to the agent

Two directions, both mediated by a single annotation file — the **state file is
the source of truth**, and either side can read or write it:

- **Agent-initiated:** the agent runs a headless command that fetches the diff,
  does its own analysis, and writes annotations to a documented JSON schema.
  Next time you open the PR, they're there.
- **Human-initiated:** export the current state (diff + your notes + open agent
  notes) to a predictable path, hand it to your agent ("address my notes"), and
  reload when the agent writes its response back.

### Posting back to GitHub

When you're done, submit: preview which annotations become review comments
(by default, your unresolved ones at severity ≥ suggestion), toggle individual
ones in or out, and post them as a **single batched review** via `gh`. Agent
annotations are only posted if you explicitly promote them.

### Across sessions

State persists locally per PR. Reopen tomorrow and pick up where you left off.
If the PR got new commits, mudpuppy flags annotations now anchored to changed
lines ("3 annotations stale — re-anchor?") and offers to remap or drop them.

## Hard requirements (guiding principles)

- **Entirely local.** The only network calls are to GitHub via `gh`, using the
  user's existing auth. No servers, no accounts.
- **No AI built in.** mudpuppy never calls an LLM. It only reads and writes the
  annotation file; the agent runs as a separate process.
- **Scales to large diffs** (1000+ files, 50k+ lines) — virtualized rendering,
  lazy file loading.
- **Keyboard-driven**, mouse optional.

## Command surface (sketch)

The binary is `mudpuppy` (a shorter `mudpup` alias is likely too). Roughly:

| Command | Purpose |
| --- | --- |
| `mudpuppy <pr-url>` | Open the TUI |
| `mudpuppy review <pr-url> --emit <path>` | Headless; write initial annotations (agent use) |
| `mudpuppy ingest <path>` | Merge an annotation file into the PR's state (agent replies) |
| `mudpuppy export <pr-url>` | Dump current state as markdown for handoff |
| `mudpuppy submit <pr-url>` | Post pending human annotations as one GitHub review |

PR references accept both `<owner>/<repo>#123` and full PR URLs.

## Annotation schema (sketch)

```json
{
  "pr": "owner/repo#123",
  "head_sha": "abc123",
  "annotations": [
    {
      "id": "uuid",
      "author": "agent",
      "file": "src/auth.ts",
      "line": 42,
      "side": "RIGHT",
      "severity": "suggestion",
      "tag": "?",
      "body": "markdown text",
      "status": "open",
      "reply_to": null,
      "created_at": "iso8601",
      "promote_to_github": false
    }
  ]
}
```

`author` is `agent` | `human`; `side` is `RIGHT` | `LEFT`; `severity` is
`info` | `suggestion` | `warning` | `blocker`; `tag` is `?` | `!` | `>` | `null`;
`status` is `open` | `resolved` | `wontfix`; `reply_to` is a `uuid` | `null`.

State is expected to live under the platform data dir, e.g.
`~/.local/share/mudpuppy/<repo>/<pr>/annotations.json`.

## Ideas not yet committed to

- `--ai-context` flag that prints a one-page reference for the agent (severity
  meanings, schema, where to write the file).
- Hunk-level annotations ("this whole function is the problem").
- A diff minimap showing annotation density for fast jumps.
- Reusing [`delta`](https://github.com/dandavison/delta) or
  [`difftastic`](https://github.com/Wilfred/difftastic) for rendering instead of
  reimplementing highlighting.

## Building

```sh
cargo build
cargo test
```

Requires a working [`gh`](https://cli.github.com/) install authenticated against
GitHub. See [AGENTS.md](./AGENTS.md) for contributor and agent guidance.

## License

[GNU AGPL v3.0 only](./LICENSE).
