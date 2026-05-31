The ratatui diff viewer, split into the event-loop shell, the state, and the
drawing.

- `mod.rs`: module root — `launch`, the `run_loop` event loop (terminal input, store reload, config hot-reload), the notify watchers, and the `Snapshot`/`fire_changes` event-hook diffing.
- `app.rs`: viewer state — `App` and its verbs (scroll, select, focus, release turn, reload), plus `Focus`, `Row`, and the lazily-built per-file `FileView`.
- `render.rs`: per-frame drawing — one function per pane (tree, diff, panel, status, help, banner), row → styled-line, and the small style/format helpers; defines `MARK`.
- `tests.rs`: Layer-1 `insta` snapshot + buffer-style tests driving the real keymap through `App::handle_key`.
- `snapshots/`: generated `insta` `.snap` baselines for the tests here.
