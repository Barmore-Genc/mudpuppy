<p align="center">
  <a href="LICENSE"><img src="https://img.shields.io/github/license/Barmore-Genc/mudpuppy" alt="License"></a>
  <a href="https://app.codecov.io/gh/Barmore-Genc/mudpuppy"><img src="https://img.shields.io/codecov/c/github/Barmore-Genc/mudpuppy" alt="Coverage"></a>
  <a href="https://github.com/Barmore-Genc/mudpuppy/releases"><img src="https://img.shields.io/github/v/release/Barmore-Genc/mudpuppy" alt="Release"></a>
</p>

<p align="center">
  <img src="assets/mudpuppy.png" alt="mudpuppy" width="600">
</p>

<p align="center">
  Terminal UI for collaborative, turn-based code review between a user and an AI agent.
</p>

---

mudpuppy is a Rust TUI for reviewing code together with an AI agent. Both of you
leave comments anchored to lines of a diff, then hand the turn back to the other.
It runs entirely locally, on the same machine where the code is checked out — the
agent is not embedded in mudpuppy; it's a tool both you and the agent drive
side by side.

The interface can display uncommitted work, an arbitrary diff, or a GitHub pull
request.

## How it works

- **You** open the TUI to see the diff, read the agent's comments, and reply or
  add your own.
- **The agent** uses the `mudpuppy agent` command surface to read the diff, leave
  annotations, and `wait` for you to release your turn.
- The two sides share a single on-disk annotation store, with atomic, locked
  writes so a live TUI and a headless agent never clobber each other.
- When one side is done, it hands over; `agent wait` blocks until you release the
  turn, then prints everything you changed.

## Install

Prebuilt binaries are published for macOS, Linux, and Windows on every release.

macOS / Linux:

```sh
curl --proto '=https' --tlsv1.2 -LsSf https://github.com/Barmore-Genc/mudpuppy/releases/latest/download/mudpuppy-installer.sh | sh
```

Windows (PowerShell):

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/Barmore-Genc/mudpuppy/releases/latest/download/mudpuppy-installer.ps1 | iex"
```

Or download an archive for your platform directly from the
[latest release](https://github.com/Barmore-Genc/mudpuppy/releases/latest).

To build from source instead (requires a recent Rust toolchain and a C/C++
compiler for the vendored Luau interpreter):

```sh
cargo install --path .
```

## Usage

Open the TUI on the current repository's uncommitted changes:

```sh
mudpuppy
```

Review a GitHub pull request, or a diff against an explicit base:

```sh
mudpuppy owner/repo#123
mudpuppy --base main
```

Set up the Claude Code skills that teach an agent the review loop:

```sh
mudpuppy install claude
```

The agent-facing commands live under `mudpuppy agent` — its `--help` output is
the agent's own documentation:

```sh
mudpuppy agent --help
```

## Configuration

Keybindings and event hooks are scripted in Luau. See the built-in reference:

```sh
mudpuppy help config
```

The config file lives at `$MUDPUPPY_CONFIG`, else
`$XDG_CONFIG_HOME/mudpuppy/mudpuppy.luau` (`~/.config/mudpuppy/mudpuppy.luau`).

## License

[AGPL-3.0-only](LICENSE).
