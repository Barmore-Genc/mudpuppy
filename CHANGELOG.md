# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline (cargo-dist) extracts the section matching the tagged
version and uses it as the GitHub Release notes, so every release needs a
matching heading here.

## 0.5.0 - 2026-06-30

### Added

- `mudpuppy.reset_annotations()` clears the whole annotation store in one step,
  for starting a review from a clean slate. Bound to `:reset-annotations` and
  `<leader> R` (Space R), both behind a confirmation prompt that names the count
  (`Reset all 3 annotations? This cannot be undone.`) (#70).
- `mudpuppy agent reset --base <ref>` re-points a local review at a different
  base without restarting the TUI: it clears the round and switches an open TUI
  onto the new diff live (#71).
- A `mudpuppy.debug_log("<dir>")` config setting writes debug logs, split per
  role so the TUI (`mudpuppy-tui.log`) and the headless agent
  (`mudpuppy-agent.log`) don't interleave. Logs never record review content;
  file paths and branch names appear only as salted, non-reversible short
  hashes (#71).

### Changed

- Both sidebar tab chips now show their total — `Files 7 │ Annotations 3` —
  instead of only the active tab (#70).
- The changelog in the self-update prompt renders as styled Markdown (headings,
  lists, blockquotes, code, inline emphasis and links) instead of uniformly dim
  text, so GitHub release notes read as intended (#68).

## 0.4.0 - 2026-06-17

### Added

- The comment composer now has a full vim-like modal editor: normal and insert
  modes, hjkl/word/line/find motions, d/c/y operators over any motion (and
  dd/cc/yy), single-key edits (x/s/r/~/J/p), count prefixes like `3w` and
  `d2j`, and undo/redo with `u`/`Ctrl-R` (#65).
- `mudpuppy.filter_files(fn)` lets a config hide files from the tree and diff —
  for example, generated files checked into the repo. Hidden files are folded
  away, not dropped: editing or removing a filter restores them live on save,
  and `:unhide-files` (or `mudpuppy.toggle_file_filters()`) reveals them
  on demand. Files added by hand with the picker are never hidden (#63).
- `mudpuppy agent comment list` and `mudpuppy agent wait` now print the source
  code around each annotated line beneath the comment, so the agent can locate
  what a comment points at without re-reading the file. On by default (±3
  lines) and tuned with `-A`/`--after`, `-B`/`--before`, and `--context`;
  `--context 0` suppresses it (#60).

### Changed

- Pressing `h`/`←` in the diff pane while already scrolled to the left edge now
  moves focus to the sidebar, mirroring how `l`/Enter steps from the sidebar
  into the diff. While the diff still has scroll to give, `h`/`←` pan the code
  as before (#59).
- Adding a comment while the cursor sits on an existing inline comment row now
  anchors the new comment to the same line(s) as that comment, so you can reply
  to a thread without navigating back to the annotated line (#64).

### Fixed

- Reviewing very large files no longer lags on every keypress: syntax
  highlighting now runs on a background worker and is cached, so the file
  appears instantly in plain colors and fills in highlighting on the next
  frame (#66).
- Long inline comment bodies no longer clip past the right edge of the diff
  pane and silently drop words; they now wrap within the pane (#62).

## 0.3.1 - 2026-06-14

### Added

- The update prompt now shows the new release's changelog, so you can read what
  changed before choosing to update (#55).

### Changed

- Self-update downloads the prebuilt release binary and swaps it in place
  instead of compiling from source, so updating no longer needs a Rust
  toolchain installed (#55).

### Fixed

- The all-annotations sidebar can now scroll all the way to the bottom.
  Previously, once any comment body wrapped, the lower annotations were
  unreachable and pressing `G` scrolled the selection off-screen (#56).
- Selecting an annotation centers it in both the list and the diff instead of
  pinning it to the bottom edge (#56).

## 0.3.0 - 2026-06-12

### Added

- Annotations follow edits: a comment relocates to its line even after the
  surrounding file changes, and re-pins to the whole file (flagged in the file
  banner) when its line can no longer be found. The scan range is configurable
  from Lua via `mudpuppy.anchor` (#46).
- Comments render as inline threads spliced under the line they annotate, and
  the composer opens inline under the cursor — or below a thread when replying —
  instead of as a centered modal (#50).
- Syntax highlighting now covers many more languages, including Go, Python,
  Ruby, TypeScript, and TOML, and can recognize a file from its shebang line
  when the extension is missing (#48).
- Horizontal scrolling in the diff pane: pan long lines with `h`/`l` or the
  arrow keys (#53).
- The agent can supply a comment body from a file or stdin with `--body-file`,
  avoiding the shell-quoting approval prompts that multi-line `--body` triggers
  (#49).

### Changed

- The viewer paints its own background so foreground colors stay readable
  regardless of your terminal theme, with faint tints behind added and removed
  diff lines (#52).
- Pane focus switching moved off `h`/`l` (now used for horizontal scrolling) to
  `<leader> p h` / `<leader> p l` or `Tab` (#53).
- Installed Claude Code skills are version-stamped; mudpuppy detects stale
  copies at launch and offers to refresh them (#49).
- The annotations sidebar shows the full body of multi-line comments instead of
  only the first line (#47).

### Fixed

- The approval banner and status bar now name the correct turn-release chord
  (`Space t r`) (#47).

## 0.2.0 - 2026-06-09

### Added

- Self-update support: mudpuppy checks GitHub for newer releases and offers to
  install them through a modal prompt (#43).
- Mouse support in the TUI (#42).

## 0.1.1 - 2026-06-03

### Added

- `-C` option to run mudpuppy against a different directory (#39).

### Changed

- Reworked the default keybindings around a consistent scheme (#40).

## 0.1.0 - 2026-06-03

Initial release.
