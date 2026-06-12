# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline (cargo-dist) extracts the section matching the tagged
version and uses it as the GitHub Release notes, so every release needs a
matching heading here.

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
