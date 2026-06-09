# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and the project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

The release pipeline (cargo-dist) extracts the section matching the tagged
version and uses it as the GitHub Release notes, so every release needs a
matching heading here.

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
