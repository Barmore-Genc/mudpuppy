The ratatui diff viewer, split into the event-loop shell, the state, and the
drawing.

- `mod.rs`: module root — `launch`, the `run_loop` event loop (terminal input, store reload, config hot-reload), the notify watchers, and the `Snapshot`/`fire_changes` event-hook diffing.
- `app.rs`: viewer state — `App` and its verbs (count-aware scroll/cursor move/`move_selection`/hunk-hop, visual selection, select, focus, release turn, reload), the pending count/sequence fields, the `:command` palette state + `handle_palette_key`, plus `Focus`, `Row`, and the lazily-built per-file `FileView`.
- `annotate.rs`: the human's annotation mutations (`impl App`) — cursor/selection → anchor, add/region/file/reply/edit/delete/status writes through `store::update`, and the delete-confirm key handler.
- `composer.rs`: the modal comment composer (`Composer`, `ComposerTarget`), its key handler (`handle_composer_key`), and the composer-opening verbs (`add_comment`/`comment_file`/`reply`/`edit_comment`).
- `render.rs`: per-frame drawing — one function per pane (tree, diff, panel, status, help, banner) plus the picker and `:command` palette overlays, the status-bar pending count/sequence hint, row → styled-line, and the small style/format helpers; defines `MARK`.
- `tests/`: Layer-1 `insta` snapshot + buffer-style tests driving the real keymap through `App::handle_key`; `mod.rs` holds the shared fixtures/helpers, with tests grouped by topic into `rendering`, `keymap`, `annotations`, `turns`, `expansion`, `authoring`.
- `tests/snapshots/`: generated `insta` `.snap` baselines for the tests here.
