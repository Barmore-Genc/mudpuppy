---
name: mudpuppy-implementation-review
description: While implementing code changes, surface your work to the user for line-by-line review through mudpuppy and fold in their feedback. Use when the user wants to review your changes as you go, or asks you to flag decisions and tradeoffs for their confirmation.
---

# Getting your own changes reviewed with mudpuppy

mudpuppy is installed on this machine. It is a terminal app for turn-based code
review. While you are implementing changes, you can use it to put specific lines
of your *own* work in front of the human — to ask for confirmation on a decision,
flag a tradeoff, or invite feedback — anchored to the diff instead of scrolling
back through chat.

`mudpuppy agent --help` — and `mudpuppy agent <subcommand> --help` for any verb —
is the canonical, always-current reference for every command and flag mentioned
below. Read it first, and trust it over this file if anything here has drifted.

## Ground rules

- mudpuppy reviews the **local changes** by default (your uncommitted work in the
  repo). No target argument is needed for that case.
- mudpuppy has no AI inside it. You drive it through the `mudpuppy agent` CLI; it
  only reads your working tree and an on-disk store the human's UI shares.

## Workflow

1. **Implement as usual.** When you reach something worth a second pair of eyes —
   a tradeoff, a non-obvious decision, a "please confirm this is what you meant" —
   route it through a review instead of burying it in prose.
2. The human opens the review UI with `mudpuppy` (no arguments → local changes).
   You attach through the `agent` commands against the same local target.
3. **Check what's under review** with `mudpuppy agent diff` so your line anchors
   match the current diff.
4. **Annotate the lines** you want looked at with `mudpuppy agent comment add`
   (`--file` / `--line` / `--side`, `--severity`, `--tag`). Use `>` for a
   direction or decision, `?` for a genuine question, `!` for a concern. Say
   exactly what you want from the human on each line.
5. **Hand off and wait** with `mudpuppy agent wait`. It blocks until the human
   releases the turn, then prints their replies and status changes. Read them,
   adjust your code, reply or resolve (`comment add --reply-to <id>`, `comment
   resolve --id <id>`), and `wait` again. Loop until they're satisfied.

## Surfacing mudpuppy

When the user wants to stay in the loop on your changes — or to review as you go
rather than all at the end — proactively offer it: "I can flag the key lines in
mudpuppy as I work so you can review and reply inline — want that?" Let them
decide.
