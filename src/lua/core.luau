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

-- Lines the prompt's details body scrolls by on PageUp/PageDown. Coarse; the
-- renderer clamps to the real content height, so over-scrolling is harmless.
local PROMPT_PAGE = 10

-- Space is the leader; set it before the `<leader>` maps below expand it.
m.leader("space")

-- The diff viewport height, used for half- and full-page scrolling.
local function page()
  return m.state().viewport.height
end

-- One horizontal scroll step: a third of the diff pane's width, so a couple of
-- `l` presses reveal the rest of an over-long line. A count multiplies it
-- (`3l`), and the move is clamped to the widest line in `scroll_h`.
local function hstep()
  return math.max(1, math.floor(m.state().viewport.width / 3))
end

-- Wipe every annotation after a confirmation. This is a clean-slate action (e.g.
-- before starting a fresh review), so it drops the agent's annotations too, not
-- just the user's — hence the guard prompt. A no-op (with a hint) when empty.
local function reset_annotations()
  local n = #m.annotations()
  if n == 0 then
    print("No annotations to reset.")
    return
  end
  local noun = n == 1 and "annotation" or "annotations"
  m.prompt("Reset all " .. n .. " " .. noun .. "? This cannot be undone.", {
    { "Reset", function() m.reset_annotations() end },
    { "Cancel", function() end },
  })
end

-- ---- Commands: the `:`-palette verbs (also reusable from bindings) ----------

m.command("quit", function() m.quit() end)
m.command("help", function() m.toggle_help() end)
m.command("annotations", function() m.toggle_annotations() end)
m.command("toggle-focus", function() m.toggle_focus() end)
m.command("release-turn", function() m.release_turn() end)
m.command("add-file", function() m.open_picker() end)
m.command("unhide-files", function() m.toggle_file_filters() end)
m.command("goto-top", function() m.cursor_to_top() end)
m.command("goto-bottom", function() m.cursor_to_bottom() end)
m.command("comment-line", function() m.add_comment() end)
m.command("comment-file", function() m.comment_file() end)
m.command("reply", function() m.reply() end)
m.command("edit-comment", function() m.edit_comment() end)
m.command("delete-comment", function() m.delete_comment() end)
m.command("cycle-status", function() m.cycle_status() end)
m.command("reset-annotations", function() reset_annotations() end)
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

-- Half / full page scroll the diff from either pane. A count multiplies the
-- distance (`3 ctrl-d` = three half-pages), applied inside `scroll`.
m.map("global", "ctrl-d", function() m.scroll(math.floor(page() / 2)) end)
m.map("global", "ctrl-u", function() m.scroll(-math.floor(page() / 2)) end)
m.map("global", "ctrl-f", function() m.scroll(page()) end)
m.map("global", "ctrl-b", function() m.scroll(-page()) end)
m.map("global", "pagedown", function() m.scroll(page()) end)
m.map("global", "pageup", function() m.scroll(-page()) end)

-- Leader verbs: release the turn, switch the sidebar to the annotations tab,
-- open the file picker.
m.map("global", "<leader> t r", function() m.release_turn() end)
m.map("global", "<leader> a", function() m.toggle_annotations() end)
m.map("global", "<leader> f", function() m.open_picker() end)

-- Clear every annotation (confirmation-gated), to start a review from scratch.
m.map("global", "<leader> R", function() reset_annotations() end)

-- Panel focus under `<leader> p` (`h`/`l` map to the left/right pane, vim-window
-- style). The diff pane now uses bare `h`/`l` for horizontal scrolling, so pane
-- switching moved here (Tab still toggles between the two).
m.map("global", "<leader> p h", function() m.set_focus("tree") end)
m.map("global", "<leader> p l", function() m.set_focus("diff") end)

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

-- j/k move the line cursor; the viewport follows it. Paging (ctrl-d/u/f/b, bound
-- in global) scrolls and carries the cursor along. A count multiplies the move.
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

-- Hunk navigation: `]h`/`[h` (the gitsigns/vim-diff "next change" idiom) or
-- `}`/`{` (vim paragraph motion). The `[`/`]` family is reserved for future
-- prev/next jumps (`]c` comment, `]f` file), so it grows without rebinding.
m.map("diff", "] h", function() m.next_hunk() end)
m.map("diff", "[ h", function() m.prev_hunk() end)
m.map("diff", "}", function() m.next_hunk() end)
m.map("diff", "{", function() m.prev_hunk() end)

-- Visual selection (a whole-line region) and clearing it.
m.map("diff", "v", function() m.toggle_visual() end)
m.map("diff", "V", function() m.toggle_visual() end)
m.map("diff", "esc", function() m.clear_selection() end)
m.map("diff", "J", function() m.move_selection(1) end)
m.map("diff", "K", function() m.move_selection(-1) end)

-- Horizontal scroll for over-long lines: `h`/`l` (or ←/→) pan the code left and
-- right. Pane focus moved to `<leader> p h/l` (and Tab) to free these keys, but
-- panning left while already at the left edge steps focus back to the sidebar —
-- the mirror of `l`/Enter in the sidebar stepping right into the diff.
local function pan_left()
  if m.state().h_scroll == 0 then m.set_focus("tree") else m.scroll_h(-hstep()) end
end
m.map("diff", "h", pan_left)
m.map("diff", "left", pan_left)
m.map("diff", "l", function() m.scroll_h(hstep()) end)
m.map("diff", "right", function() m.scroll_h(hstep()) end)

-- Context expansion: reveal all hidden context around the cursor.
m.map("diff", "<leader> e", function() m.expand_all() end)

-- Comment authoring under `<leader> c`: comment the cursor line (c c), the whole
-- file (c f), reply (c r), edit (c e), delete (c d), and cycle status (c s).
m.map("diff", "<leader> c c", function() m.add_comment() end)
m.map("diff", "<leader> c f", function() m.comment_file() end)
m.map("diff", "<leader> c r", function() m.reply() end)
m.map("diff", "<leader> c e", function() m.edit_comment() end)
m.map("diff", "<leader> c d", function() m.delete_comment() end)
m.map("diff", "<leader> c s", function() m.cycle_status() end)

-- ---- Help: the overlay is exclusive; it does not fall back to global ---------

local function help_page()
  return m.state().help.height
end

m.map("help", "?", function() m.toggle_help() end)
m.map("help", "esc", function() m.toggle_help() end)
m.map("help", "q", function() m.toggle_help() end)
m.map("help", "j", function() m.help_scroll(1) end)
m.map("help", "down", function() m.help_scroll(1) end)
m.map("help", "k", function() m.help_scroll(-1) end)
m.map("help", "up", function() m.help_scroll(-1) end)
m.map("help", "space", function() m.help_scroll(help_page()) end)
m.map("help", "pagedown", function() m.help_scroll(help_page()) end)
m.map("help", "pageup", function() m.help_scroll(-help_page()) end)
m.map("help", "ctrl-d", function() m.help_scroll(math.floor(help_page() / 2)) end)
m.map("help", "ctrl-u", function() m.help_scroll(-math.floor(help_page() / 2)) end)
m.map("help", "g", function() m.help_set_scroll(0) end)
m.map("help", "G", function() m.help_set_scroll(-1) end)

-- ---- Picker: the fuzzy "add any file" overlay -------------------------------
--
-- Only the commands are bound; any other printable key types into the query.
-- Ctrl-p moves the selection up here — it *opens* the picker only while closed.

m.map("picker", "esc", function() m.picker_close() end)
m.map("picker", "enter", function() m.picker_confirm() end)
m.map("picker", "backspace", function() m.picker_backspace() end)
m.map("picker", "down", function() m.picker_move(1) end)
m.map("picker", "up", function() m.picker_move(-1) end)
m.map("picker", "ctrl-n", function() m.picker_move(1) end)
m.map("picker", "ctrl-p", function() m.picker_move(-1) end)

-- ---- Palette: the `:command` overlay ----------------------------------------
--
-- Same shape as the picker, plus Tab to autocomplete to the top match.

m.map("palette", "esc", function() m.palette_close() end)
m.map("palette", "enter", function() m.palette_confirm() end)
m.map("palette", "tab", function() m.palette_complete() end)
m.map("palette", "backspace", function() m.palette_backspace() end)
m.map("palette", "down", function() m.palette_move(1) end)
m.map("palette", "up", function() m.palette_move(-1) end)
m.map("palette", "ctrl-n", function() m.palette_move(1) end)
m.map("palette", "ctrl-p", function() m.palette_move(-1) end)

-- ---- Prompt: a modal question opened by `m.prompt` ---------------------------
--
-- ←/→ (and h/l, Tab) always move the highlight. ↑/↓ (and j/k) scroll the details
-- body when the prompt has one, and otherwise move the highlight, so a plain
-- yes/no prompt still answers to the arrow keys.

local function prompt_vertical(delta)
  if m.state().prompt.has_details then m.prompt_scroll(delta) else m.prompt_move(delta) end
end

m.map("prompt", "esc", function() m.prompt_close() end)
m.map("prompt", "enter", function() m.prompt_confirm() end)
m.map("prompt", "left", function() m.prompt_move(-1) end)
m.map("prompt", "h", function() m.prompt_move(-1) end)
m.map("prompt", "right", function() m.prompt_move(1) end)
m.map("prompt", "l", function() m.prompt_move(1) end)
m.map("prompt", "tab", function() m.prompt_move(1) end)
m.map("prompt", "down", function() prompt_vertical(1) end)
m.map("prompt", "j", function() prompt_vertical(1) end)
m.map("prompt", "up", function() prompt_vertical(-1) end)
m.map("prompt", "k", function() prompt_vertical(-1) end)
m.map("prompt", "pagedown", function() m.prompt_scroll(PROMPT_PAGE) end)
m.map("prompt", "pageup", function() m.prompt_scroll(-PROMPT_PAGE) end)

-- A digit picks that option outright. `0` is left alone (there is no zeroth).
for i = 1, 9 do
  m.map("prompt", tostring(i), function() m.prompt_choose(i) end)
end

-- ---- Delete confirmation ----------------------------------------------------
--
-- Exclusive, and *anything* unbound cancels — so binding only the confirm keys
-- keeps the "y or it didn't happen" guarantee.

m.map("delete-confirm", "y", function() m.confirm_delete() end)
m.map("delete-confirm", "Y", function() m.confirm_delete() end)

-- ---- Composer: the comment authoring overlay --------------------------------
--
-- `composer` holds the chords that mean the same thing while typing and in normal
-- mode; the two sub-modes fall back to it. Keys left unbound go to the buffer:
-- printable characters type in insert mode, and everything else drives the
-- vim engine in normal mode (motions, operators, counts), so this file binds the
-- commands only.

m.map("composer", "ctrl-s", function() m.composer_save() end)
m.map("composer", "ctrl-e", function() m.composer_cycle_severity() end)
m.map("composer", "ctrl-t", function() m.composer_cycle_tag() end)
m.map("composer", "ctrl-j", function() m.composer_newline() end)

m.map("composer-insert", "esc", function() m.composer_normal_mode() end)
m.map("composer-insert", "enter", function() m.composer_newline() end)
m.map("composer-insert", "backspace", function() m.composer_backspace() end)

-- While a multi-key vim command is mid-flight (an operator, an `f`/`r`/`g`
-- awaiting its argument, a count), Enter and Esc belong to that command — Esc
-- abandons it and Enter is a no-op — not to save and cancel.
m.map("composer-normal", "enter", function()
  if not m.state().composer.pending then m.composer_save() end
end)
m.map("composer-normal", "esc", function()
  if m.state().composer.pending then m.composer_clear_pending() else m.composer_cancel() end
end)
m.map("composer-normal", "ctrl-r", function() m.composer_redo() end)

-- ---- Self-update: offer to install a newer release -------------------------
--
-- The policy lives here, built on three primitives: `m.prompt` (a modal question
-- with labelled callbacks), `m.updates.check()` (returns a newer version string
-- or nil), and `m.updates.update(version)` (installs a validated vX.Y.Z). The
-- event loop runs one check at launch (off-thread) and fires `update_check` with
-- the found version; the `:check-updates` command runs a check on demand.

-- Offer to install `version`. "Install" updates now; "Ignore" dismisses until the
-- next launch; "Skip" stops checking and records that in the user config. The
-- optional `changelog` (release notes from the manifest) renders as a scrollable
-- body in the prompt so the user can see what changed before updating.
local function offer_update(version, changelog)
  m.prompt("mudpuppy " .. version .. " is available. Update now?", {
    { "Install", function()
        m.updates.update(version)
        print("Updated to " .. version .. " — restart mudpuppy to run it.")
      end },
    { "Ignore for now", function() end },
    { "Skip", function() m.updates.disable() end },
  }, changelog)
end

-- Launch-time check: the event loop already did the (off-thread) fetch and only
-- fires this when a newer release exists, passing it in the payload.
m.on("update_check", function(ev)
  if ev.version then offer_update(ev.version, ev.changelog) end
end)

-- Manual check from the `:` palette; reports when already current.
m.command("check-updates", function()
  local info = m.updates.check()
  if info then
    offer_update(info.version, info.changelog)
  else
    print("mudpuppy is up to date.")
  end
end)

-- ---- Skill refresh: offer to update stale installed Claude Code skills ------
--
-- Separate from the binary self-update above: this only rewrites the installed
-- SKILL.md files. The event loop runs a local check at launch and fires
-- `skill_update_check` (with the version + a "what changed" message) when a
-- stale install exists and the user hasn't skipped that version.

-- Offer to refresh. "Update" rewrites the files now; "Not now" defers until the
-- next launch; "Skip this version" records the refusal so it stays quiet.
local function offer_skill_refresh(version, message)
  m.prompt(message .. " Update the installed skills?", {
    { "Update", function()
        m.skills.refresh()
        print("Updated the installed mudpuppy skills.")
      end },
    { "Not now", function() end },
    { "Skip this version", function() m.skills.skip(version) end },
  })
end

m.on("skill_update_check", function(ev)
  offer_skill_refresh(ev.version, ev.message)
end)
