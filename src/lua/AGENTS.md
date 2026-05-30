Embedded Luau scripting: the configurable keymap and event hooks. A sandboxed interpreter (no io/os/network/subprocess) where all default bindings live in Lua; user config loads on top, last-binding-wins. Scripts read the diff/files/annotations/screen and drive the app through scoped action verbs.

- `mod.rs`: `LuaEngine` (sandbox setup, `core.lua` + user-config load/hot-reload, key dispatch, event firing), `EventKind`, and `config_path` (hand-rolled XDG resolver).
- `api.rs`: builds the `mudpuppy` table — persistent `map`/`unmap`/`on` registration fns, and the per-dispatch scoped action/reader fns that borrow `&mut App` via a `RefCell`.
- `keys.rs`: `KeyChord` (parse/`from_event`/round-trip), `Key`, and the `Mode` enum (`Global`/`Tree`/`Diff`/`Help`).
- `views.rs`: read-only Lua tables built on demand from the live `App` — `state`/`files`/`current_file`/`annotations`/`screen`, plus shared annotation/turn builders.
- `core.lua`: the embedded default keymap, expressed against the primitive verbs; `include_str!` in release, read from disk in debug so it hot-reloads in dev.
