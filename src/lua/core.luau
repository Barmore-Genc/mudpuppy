-- mudpuppy's default keymap.
--
-- This is the single source of truth for the built-in bindings: the Rust side
-- ships no hard-coded keys except a Ctrl-C safety net (so a broken config can
-- never trap you). A user config (mudpuppy.luau) is loaded after this file and
-- can rebind anything here, last-binding-wins per (mode, sequence).
--
-- Bindings are keyed on a *sequence* of keys, so multi-key chords (`g g`) and a
-- leader (`<leader> t r`, leader = Space by default) are first-class; a number
-- typed before a motion is an ambient count (`5j`, `100G`), which the count-aware
-- verbs apply. Authoring constraint: a sequence that is a prefix of a longer
-- binding can't also be its own terminal binding (the prefix always wins), so
-- this file never binds both `g` and `g g`.
--
-- Bindings are functions (closures), not bare verb references: the action verbs
-- (`mudpuppy.quit`, `mudpuppy.scroll`, …) are live only while a key is being
-- dispatched, so a binding looks its verb up on the `mudpuppy` table at call
-- time. Everything composite (page math, vim-style file navigation) is computed
-- here from the read-only `mudpuppy.state()` / `mudpuppy.files()` views rather
-- than baked into Rust.

local m = mudpuppy

-- Space is the leader; set it before the `<leader>` maps below expand it.
m.leader("space")

-- The diff viewport height, used for half- and full-page scrolling.
local function page()
  return m.state().viewport.height
end

-- ---- Commands: the `:`-palette verbs (also reusable from bindings) ----------

m.command("quit", function() m.quit() end)
m.command("help", function() m.toggle_help() end)
m.command("annotations", function() m.toggle_annotations() end)
m.command("toggle-focus", function() m.toggle_focus() end)
m.command("release-turn", function() m.release_turn() end)
m.command("add-file", function() m.open_picker() end)
m.command("goto-top", function() m.cursor_to_top() end)
m.command("goto-bottom", function() m.cursor_to_bottom() end)
m.command("comment-line", function() m.add_comment() end)
m.command("comment-file", function() m.comment_file() end)
m.command("reply", function() m.reply() end)
m.command("edit-comment", function() m.edit_comment() end)
m.command("delete-comment", function() m.delete_comment() end)
m.command("cycle-status", function() m.cycle_status() end)
m.command("expand-down", function() m.expand_down() end)
m.command("expand-up", function() m.expand_up() end)
m.command("expand-all", function() m.expand_all() end)

-- ---- Global: act regardless of focus (and the fallback for tree/diff) -------

m.map("global", "q", function() m.quit() end)
m.map("global", "ctrl-c", function() m.quit() end)
m.map("global", "?", function() m.toggle_help() end)
m.map("global", "tab", function() m.toggle_focus() end)
m.map("global", "backtab", function() m.toggle_focus() end)

-- The command palette.
m.map("global", ":", function() m.open_palette() end)

-- Half / full page scroll the diff from either pane (was ctrl-d/u/f/b). A count
-- multiplies the distance (`3d` = three half-pages), applied inside `scroll`.
m.map("global", "d", function() m.scroll(math.floor(page() / 2)) end)
m.map("global", "u", function() m.scroll(-math.floor(page() / 2)) end)
m.map("global", "f", function() m.scroll(page()) end)
m.map("global", "b", function() m.scroll(-page()) end)
m.map("global", "pagedown", function() m.scroll(page()) end)
m.map("global", "pageup", function() m.scroll(-page()) end)

-- Leader verbs: release the turn, switch the sidebar to the annotations tab,
-- open the file picker.
m.map("global", "<leader> t r", function() m.release_turn() end)
m.map("global", "<leader> a", function() m.toggle_annotations() end)
m.map("global", "<leader> f", function() m.open_picker() end)

-- ---- Sidebar: move the selection in whichever tab is showing -----------------

-- These verbs act on the focused sidebar tab — the file tree, or the
-- all-annotations list. `g g`/Home jump to the first item; `G`/End to the last,
-- or to item N with a count (`3G`). Enter/`l` open the file (tree) or jump to the
-- selected annotation (annotations tab).
m.map("tree", "j", function() m.sidebar_move(1) end)
m.map("tree", "down", function() m.sidebar_move(1) end)
m.map("tree", "k", function() m.sidebar_move(-1) end)
m.map("tree", "up", function() m.sidebar_move(-1) end)
m.map("tree", "g g", function() m.sidebar_first() end)
m.map("tree", "home", function() m.sidebar_first() end)
m.map("tree", "G", function() m.sidebar_last(m.state().count or -1) end)
m.map("tree", "end", function() m.sidebar_last(-1) end)
m.map("tree", "enter", function() m.sidebar_confirm() end)
m.map("tree", "l", function() m.sidebar_confirm() end)
m.map("tree", "right", function() m.sidebar_confirm() end)

-- ---- Diff: move a cursor (GitHub-style), author comments, hunk-hop ----------

-- j/k move the line cursor; the viewport follows it. Paging (d/u/f/b, bound in
-- global) scrolls and carries the cursor along. A count multiplies the move.
m.map("diff", "j", function() m.move_cursor(1) end)
m.map("diff", "down", function() m.move_cursor(1) end)
m.map("diff", "k", function() m.move_cursor(-1) end)
m.map("diff", "up", function() m.move_cursor(-1) end)
m.map("diff", "g g", function() m.cursor_to_top() end)
m.map("diff", "home", function() m.cursor_to_top() end)
-- `G` jumps to the bottom, or to row N with a count (`100G`).
m.map("diff", "G", function()
  local n = m.state().count
  if n then m.set_cursor(n - 1) else m.cursor_to_bottom() end
end)
m.map("diff", "end", function() m.cursor_to_bottom() end)
m.map("diff", "}", function() m.next_hunk() end)
m.map("diff", "n", function() m.next_hunk() end)
m.map("diff", "{", function() m.prev_hunk() end)
m.map("diff", "N", function() m.prev_hunk() end)
m.map("diff", "J", function() m.move_selection(1) end)
m.map("diff", "K", function() m.move_selection(-1) end)
m.map("diff", "h", function() m.set_focus("tree") end)
m.map("diff", "left", function() m.set_focus("tree") end)

-- Context expansion (was ctrl-e/ctrl-y): expand the nearest gap down / up, or
-- the whole gap with `+`.
m.map("diff", "<leader> e", function() m.expand_down() end)
m.map("diff", "<leader> y", function() m.expand_up() end)
m.map("diff", "+", function() m.expand_all() end)

-- Visual selection (a whole-line region) and clearing it.
m.map("diff", "v", function() m.toggle_visual() end)
m.map("diff", "V", function() m.toggle_visual() end)
m.map("diff", "esc", function() m.clear_selection() end)

-- Authoring behind the leader: comment the cursor line/region (c c), the whole
-- file (c f), reply (c r), edit (c e), delete (c d, confirmed with y), and cycle
-- status (c s).
m.map("diff", "<leader> c c", function() m.add_comment() end)
m.map("diff", "<leader> c f", function() m.comment_file() end)
m.map("diff", "<leader> c r", function() m.reply() end)
m.map("diff", "<leader> c e", function() m.edit_comment() end)
m.map("diff", "<leader> c d", function() m.delete_comment() end)
m.map("diff", "<leader> c s", function() m.cycle_status() end)

-- ---- Help: the overlay is exclusive; only its own dismissal keys are bound --

m.map("help", "?", function() m.toggle_help() end)
m.map("help", "esc", function() m.toggle_help() end)
m.map("help", "q", function() m.toggle_help() end)
