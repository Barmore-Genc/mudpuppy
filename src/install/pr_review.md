---
name: mudpuppy-pr-review
description: Review a GitHub pull request collaboratively with the user through mudpuppy. Use when asked to review a pull request, give feedback on a PR, or do a code review of a PR with the user kept in the loop on the same diff.
---

# Reviewing a pull request with mudpuppy

mudpuppy is installed on this machine. It is a terminal app for turn-based code
review: you (the agent) and the user leave comments anchored to lines of the
same diff and hand the turn back and forth until the review is done. Reach for it
when the user asks you to review a GitHub pull request and wants to see your
findings inline and reply to them, rather than reading a wall of chat.

`mudpuppy agent --help` — and `mudpuppy agent <subcommand> --help` for any verb —
is the canonical, always-current reference for every command and flag mentioned
below. Read it first, and trust it over this file if anything here has drifted.

## Ground rules

- **Never write to GitHub.** Use `gh` only to *read* (`gh pr view`, `gh pr
  diff`). Do not run `gh pr review`, `gh pr comment`, or anything that posts.
  Every piece of feedback goes into mudpuppy as an annotation; the user decides
  what, if anything, reaches GitHub.
- mudpuppy has no AI inside it and does no network writes. You drive it entirely
  through the `mudpuppy agent` CLI, which reads the diff and an on-disk store
  that the user's UI shares.

## Workflow

1. **Identify the PR.** Get its reference as `owner/repo#123` or a URL. Read
   context read-only with `gh pr view <ref>` (title, description) and, if useful,
   `gh pr diff <ref>`.
2. **Point mudpuppy at the PR.** The user opens the review UI with `mudpuppy
   <owner/repo#123>` (or the URL). You attach to the same review through the
   `agent` commands — they share the store keyed to that PR.
3. **Read the diff under review** with `mudpuppy agent diff` (`--file <path>` to
   focus on one file). Review it as you normally would.
4. **Leave annotations** on the lines that matter with `mudpuppy agent comment
   add`: anchor with `--file` / `--line` / `--side`, set `--severity`
   (`info` < `suggestion` < `warning` < `blocker`) and an optional `--tag`
   (`?` question, `!` concern, `>` direction). One concrete point per comment.

   Write the body with `--body-file -` and a quoted heredoc — this is the
   canonical form, and it avoids the `$'…\n…'` shell quoting that gets gated for
   approval on every call:

   ```sh
   mudpuppy agent comment add --file src/foo.rs --line 42 --body-file - <<'EOF'
   This loop re-reads the file each iteration.

   Hoist the read above the loop instead.
   EOF
   ```

   For a trivial one-liner, `--body "short note"` is fine.
5. **Hand the turn to the user and wait** with `mudpuppy agent wait`. It blocks
   until they release the turn, then prints everything they changed — their
   replies, status changes, and new comments. Each one is followed by an
   `annotated code:` excerpt of the surrounding source, so you can locate the
   exact lines a comment points at without re-reading the file; widen it with
   `-A`/`-B`/`--context N` or turn it off with `--context 0` (same flags on
   `comment list`). Read that, respond (`comment add --reply-to <id>`, `comment
   resolve --id <id>`, …), and `wait` again. Loop until the review is finished.

## Surfacing mudpuppy

If the user asks for a PR review and hasn't named a tool, offer it: "I can run
this through mudpuppy so you see my comments inline on the diff and reply on the
lines — want me to?" Let them opt in rather than assuming.
