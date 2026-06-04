# Changelog

All notable changes to mudpuppy are recorded here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project uses
[semantic versioning](https://semver.org/).

`dist` reads the section whose heading matches the release tag and embeds it in
the published `dist-manifest.json`, so the in-app update prompt can show the
release notes. See [docs/releasing.md](docs/releasing.md) for the release flow.

## Unreleased

### Added

- The in-app update prompt now shows the new release's changelog in a scrollable,
  help-style panel (`j`/`k`/`PgUp`/`PgDn`/`g`/`G`). The notes are read from the
  published release manifest, so no extra fetch is needed.

### Changed

- Self-update is now gated behind the off-by-default `auto-update` build feature.
  Our prebuilt release binaries enable it; a source `cargo install` or a distro
  package builds without it and updates through its own channel.
- Updating now downloads the prebuilt binary for your platform and verifies its
  checksum (via `axoupdater`) instead of recompiling from source with
  `cargo install` — no Rust toolchain required.

## 0.1.1

### Added

- Self-update support: a launch-time check against the published release manifest
  prompts to install a newer version (validated `vX.Y.Z`, installed via
  `cargo install --locked`).
- Mouse support in the TUI: scroll, click-to-focus, drag-select, and double-click.

## 0.1.0

- Initial release: turn-based diff review TUI with a Lua-configurable keymap,
  annotations, the `agent` command surface, and the cargo-dist release pipeline.
