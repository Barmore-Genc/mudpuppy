-- mudpuppy's default keymap.
--
-- This is the single source of truth for the built-in bindings: the Rust side
-- ships no hard-coded keys except a Ctrl-C safety net (so a broken config can
-- never trap you). A user config (mudpuppy.luau) is loaded after this file and
-- can rebind anything here, last-binding-wins per (mode, key).
--
-- Bindings are functions (closures), not bare verb references: the action verbs
-- (`mudpuppy.quit`, `mudpuppy.scroll`, …) are live only while a key is being
-- dispatched, so a binding looks its verb up on the `mudpuppy` table at call
-- time. Everything composite (half-page scroll, vim-style file navigation) is
-- computed here from the read-only `mudpuppy.state()` / `mudpuppy.files()` views
-- rather than baked into Rust.

local m = mudpuppy

-- How many files are in the tree (for clamping first/last navigation).
local function file_count()
  return #m.files()
end

-- The diff viewport height, used for half- and full-page scrolling.
local function page()
  return m.state().viewport.height
end

-- The 1-based index of the selected file.
local function selected()
  return m.state().selected
end

-- ---- Global: act regardless of focus (and the fallback for tree/diff) -------

m.map("global", "q", function() m.quit() end)
m.map("global", "ctrl-c", function() m.quit() end)
m.map("global", "?", function() m.toggle_help() end)
m.map("global", "a", function() m.toggle_panel() end)
m.map("global", "r", function() m.release_turn() end)
m.map("global", "tab", function() m.toggle_focus() end)
m.map("global", "backtab", function() m.toggle_focus() end)

-- Open the fuzzy file picker to pull any working-tree file into the review.
m.map("global", "ctrl-p", function() m.open_picker() end)

-- Half / full page scroll the diff from either pane.
m.map("global", "ctrl-d", function() m.scroll(math.floor(page() / 2)) end)
m.map("global", "ctrl-u", function() m.scroll(-math.floor(page() / 2)) end)
m.map("global", "ctrl-f", function() m.scroll(page()) end)
m.map("global", "ctrl-b", function() m.scroll(-page()) end)
m.map("global", "pagedown", function() m.scroll(page()) end)
m.map("global", "pageup", function() m.scroll(-page()) end)

-- ---- Tree: move the file selection ------------------------------------------

m.map("tree", "j", function() m.select_file(selected() + 1) end)
m.map("tree", "down", function() m.select_file(selected() + 1) end)
m.map("tree", "k", function() m.select_file(selected() - 1) end)
m.map("tree", "up", function() m.select_file(selected() - 1) end)
m.map("tree", "g", function() m.select_file(1) end)
m.map("tree", "home", function() m.select_file(1) end)
m.map("tree", "G", function() m.select_file(file_count()) end)
m.map("tree", "end", function() m.select_file(file_count()) end)
m.map("tree", "enter", function() m.set_focus("diff") end)
m.map("tree", "l", function() m.set_focus("diff") end)
m.map("tree", "right", function() m.set_focus("diff") end)

-- ---- Diff: scroll, hunk-hop, and jump files without leaving the pane --------

m.map("diff", "j", function() m.scroll(1) end)
m.map("diff", "down", function() m.scroll(1) end)
m.map("diff", "k", function() m.scroll(-1) end)
m.map("diff", "up", function() m.scroll(-1) end)
m.map("diff", "g", function() m.set_scroll(0) end)
m.map("diff", "home", function() m.set_scroll(0) end)
m.map("diff", "G", function() m.set_scroll(m.state().viewport.total) end)
m.map("diff", "end", function() m.set_scroll(m.state().viewport.total) end)
m.map("diff", "}", function() m.next_hunk() end)
m.map("diff", "n", function() m.next_hunk() end)
m.map("diff", "{", function() m.prev_hunk() end)
m.map("diff", "N", function() m.prev_hunk() end)
m.map("diff", "ctrl-e", function() m.expand_down() end)
m.map("diff", "ctrl-y", function() m.expand_up() end)
m.map("diff", "+", function() m.expand_all() end)
m.map("diff", "J", function() m.select_file(selected() + 1) end)
m.map("diff", "K", function() m.select_file(selected() - 1) end)
m.map("diff", "h", function() m.set_focus("tree") end)
m.map("diff", "left", function() m.set_focus("tree") end)

-- ---- Help: the overlay is exclusive; only its own dismissal keys are bound --

m.map("help", "?", function() m.toggle_help() end)
m.map("help", "esc", function() m.toggle_help() end)
m.map("help", "q", function() m.toggle_help() end)
