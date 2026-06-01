//! The `mudpuppy` table exposed to scripts.
//!
//! Two halves:
//!
//! * **Persistent** registration functions — [`build_table`] adds `map(mode,
//!   key, fn)`, `unmap(mode, key)`, and `on(event, fn)`, which mutate the shared
//!   binding/event registries. These live for the whole session and are what
//!   `core.luau` and a user config call at load time.
//! * **Scoped** action and reader functions — [`install_scoped`] adds the
//!   mutating verbs (`scroll`, `select_file`, `quit`, …) and the readers
//!   (`state`, `current_file`, …) for the duration of a single dispatch. They
//!   borrow the live [`App`] through a [`RefCell`], so they can mutate it
//!   synchronously; when the enclosing `lua.scope` ends, mlua invalidates them
//!   and the borrow is released. Splitting it this way (mlua's `scope` is the
//!   blessed, `unsafe`-free way to lend a non-`'static` reference to Lua) is what
//!   lets a callback hold the diff (`&App`) while still driving the app's `&mut`
//!   verbs.
//!
//! For the scoped verbs to resolve, the engine turns off `safeenv` on the
//! globals (see `mod.rs`): otherwise Luau caches a `mudpuppy.quit` lookup at
//! load time and a binding written `mudpuppy.quit()` would bind to a destructed
//! scoped function. With `safeenv` off, each call does a live table lookup and
//! finds the freshly-installed verb.

use std::cell::RefCell;

use mlua::{Function, Lua, Result, Scope, Table, Value};

use super::keys::{KeyChord, KeySeq, Mode};
use super::views;
use super::{Bindings, Commands, EventKind, Events, Leader};
use crate::tui::App;

/// Build the persistent `mudpuppy` table with the `map`/`unmap`/`on`/`command`/
/// `leader` registration functions wired to the shared registries.
pub fn build_table(
    lua: &Lua,
    bindings: Bindings,
    events: Events,
    commands: Commands,
    leader: Leader,
) -> Result<Table> {
    let table = lua.create_table()?;

    let b = bindings.clone();
    let lead = leader.clone();
    table.set(
        "map",
        lua.create_function(move |_, (mode, key, func): (String, String, Function)| {
            let (mode, seq) = parse_binding(&mode, &key, *lead.borrow())?;
            // Last binding for a (mode, sequence) wins — that's how a user config
            // overrides a core default.
            b.borrow_mut().insert((mode, seq), func);
            Ok(())
        })?,
    )?;

    let b = bindings;
    let lead = leader.clone();
    table.set(
        "unmap",
        lua.create_function(move |_, (mode, key): (String, String)| {
            let (mode, seq) = parse_binding(&mode, &key, *lead.borrow())?;
            // Remove the binding outright (vs. shadowing it with a no-op), so the
            // key falls back to the Global map — or does nothing if unbound there
            // too. Removing an absent binding is a no-op.
            b.borrow_mut().remove(&(mode, seq));
            Ok(())
        })?,
    )?;

    let e = events;
    table.set(
        "on",
        lua.create_function(move |_, (event, func): (String, Function)| {
            let kind = EventKind::parse(&event)
                .ok_or_else(|| mlua::Error::runtime(format!("unknown event {event:?}")))?;
            e.borrow_mut().entry(kind).or_default().push(func);
            Ok(())
        })?,
    )?;

    let c = commands;
    table.set(
        "command",
        lua.create_function(move |_, (name, func): (String, Function)| {
            // Last registration for a name wins, mirroring `map`.
            c.borrow_mut().insert(name, func);
            Ok(())
        })?,
    )?;

    let lead = leader;
    table.set(
        "leader",
        lua.create_function(move |_, key: String| {
            let chord = KeyChord::parse(&key)
                .ok_or_else(|| mlua::Error::runtime(format!("unparseable leader key {key:?}")))?;
            *lead.borrow_mut() = chord;
            Ok(())
        })?,
    )?;

    Ok(table)
}

/// Install the scoped action and reader functions onto `table` for one dispatch,
/// each borrowing the app through `cell`. The functions stop working when the
/// surrounding `lua.scope` ends.
pub fn install_scoped<'scope>(
    lua: &Lua,
    scope: &'scope Scope<'scope, '_>,
    table: &Table,
    cell: &'scope RefCell<&mut App>,
    commands: &Commands,
) -> Result<()> {
    // --- mutating actions ---------------------------------------------------

    table.set(
        "quit",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().quit();
            Ok(())
        })?,
    )?;
    table.set(
        "toggle_focus",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().toggle_focus();
            Ok(())
        })?,
    )?;
    table.set(
        "set_focus",
        scope.create_function(move |_, pane: String| {
            cell.borrow_mut().set_focus(&pane);
            Ok(())
        })?,
    )?;
    table.set(
        "toggle_help",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().toggle_help();
            Ok(())
        })?,
    )?;
    table.set(
        "toggle_panel",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().toggle_panel();
            Ok(())
        })?,
    )?;
    table.set(
        "release_turn",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().release_turn();
            Ok(())
        })?,
    )?;
    table.set(
        "select_file",
        scope.create_function(move |_, index: i64| {
            cell.borrow_mut().select_file(index);
            Ok(())
        })?,
    )?;
    table.set(
        "set_scroll",
        scope.create_function(move |_, n: i64| {
            cell.borrow_mut().set_scroll(n);
            Ok(())
        })?,
    )?;
    table.set(
        "scroll",
        scope.create_function(move |_, delta: i64| {
            cell.borrow_mut().scroll_by(delta as isize);
            Ok(())
        })?,
    )?;
    table.set(
        "next_hunk",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().next_hunk();
            Ok(())
        })?,
    )?;
    table.set(
        "prev_hunk",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().prev_hunk();
            Ok(())
        })?,
    )?;
    table.set(
        "expand_down",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().expand_down();
            Ok(())
        })?,
    )?;
    table.set(
        "expand_up",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().expand_up();
            Ok(())
        })?,
    )?;
    table.set(
        "expand_all",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().expand_all();
            Ok(())
        })?,
    )?;
    table.set(
        "open_picker",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().open_picker();
            Ok(())
        })?,
    )?;
    // Seed the palette with the registered command names (captured here so the
    // verb can read the live registry).
    let cmds = commands.clone();
    table.set(
        "open_palette",
        scope.create_function(move |_, ()| {
            let mut names: Vec<String> = cmds.borrow().keys().cloned().collect();
            names.sort();
            cell.borrow_mut().open_palette(names);
            Ok(())
        })?,
    )?;
    // Move the tree's file selection by `delta` (count-aware), the sequence-era
    // replacement for the `select_file(selected()+delta)` idiom.
    table.set(
        "move_selection",
        scope.create_function(move |_, delta: i64| {
            cell.borrow_mut().move_selection(delta);
            Ok(())
        })?,
    )?;

    // --- cursor, visual selection, and authoring ----------------------------

    table.set(
        "move_cursor",
        scope.create_function(move |_, delta: i64| {
            cell.borrow_mut().move_cursor(delta);
            Ok(())
        })?,
    )?;
    table.set(
        "set_cursor",
        scope.create_function(move |_, n: i64| {
            cell.borrow_mut().set_cursor(n);
            Ok(())
        })?,
    )?;
    table.set(
        "cursor_to_top",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().cursor_to_top();
            Ok(())
        })?,
    )?;
    table.set(
        "cursor_to_bottom",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().cursor_to_bottom();
            Ok(())
        })?,
    )?;
    table.set(
        "toggle_visual",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().toggle_visual();
            Ok(())
        })?,
    )?;
    table.set(
        "clear_selection",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().clear_selection();
            Ok(())
        })?,
    )?;
    table.set(
        "add_comment",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().add_comment();
            Ok(())
        })?,
    )?;
    table.set(
        "comment_file",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().comment_file();
            Ok(())
        })?,
    )?;
    table.set(
        "reply",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().reply();
            Ok(())
        })?,
    )?;
    table.set(
        "edit_comment",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().edit_comment();
            Ok(())
        })?,
    )?;
    table.set(
        "delete_comment",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().request_delete();
            Ok(())
        })?,
    )?;
    table.set(
        "cycle_status",
        scope.create_function(move |_, ()| {
            cell.borrow_mut().cycle_annotation_status();
            Ok(())
        })?,
    )?;

    // --- read-only views ----------------------------------------------------

    // `state()` returns a live proxy, not an eager snapshot: an empty table whose
    // metatable reads fields from `App` on access (`__index`) and writes the few
    // writable ones back (`__newindex`, erroring on read-only fields). This is
    // what makes `state().count = n` work while keeping every existing read
    // (`state().selected`, `.viewport.height`, …) unchanged. `pairs()` won't
    // enumerate it (there are no real keys), which no binding relies on.
    let meta = lua.create_table()?;
    meta.set(
        "__index",
        scope.create_function(move |lua, (_, key): (Table, String)| {
            views::state_field(lua, &cell.borrow(), &key)
        })?,
    )?;
    meta.set(
        "__newindex",
        scope.create_function(move |_, (_, key, value): (Table, String, Value)| {
            views::set_state_field(&mut cell.borrow_mut(), &key, value)
        })?,
    )?;
    table.set(
        "state",
        scope.create_function(move |lua, ()| {
            let proxy = lua.create_table()?;
            proxy.set_metatable(Some(meta.clone()));
            Ok(proxy)
        })?,
    )?;
    table.set(
        "files",
        scope.create_function(move |lua, ()| views::files(lua, &cell.borrow()))?,
    )?;
    table.set(
        "current_file",
        scope.create_function(move |lua, ()| views::current_file(lua, &cell.borrow()))?,
    )?;
    table.set(
        "annotations",
        scope.create_function(move |lua, ()| views::annotations(lua, &cell.borrow()))?,
    )?;
    table.set(
        "screen",
        scope.create_function(move |lua, ()| views::screen(lua, &cell.borrow()))?,
    )?;

    Ok(())
}

/// Parse a `(mode, key)` pair into the registry key — a mode plus a chord
/// *sequence*, with `<leader>` expanded to `leader` — mapping bad spellings to a
/// Lua error the config author will see.
fn parse_binding(mode: &str, key: &str, leader: KeyChord) -> Result<(Mode, KeySeq)> {
    let mode =
        Mode::parse(mode).ok_or_else(|| mlua::Error::runtime(format!("unknown mode {mode:?}")))?;
    let seq = KeyChord::parse_sequence(key, leader)
        .ok_or_else(|| mlua::Error::runtime(format!("unparseable key {key:?}")))?;
    Ok((mode, seq))
}
