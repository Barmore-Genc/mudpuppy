//! The `mudpuppy` table exposed to scripts.
//!
//! Two halves:
//!
//! * **Persistent** registration functions — [`build_table`] adds `map(mode,
//!   key, fn)`, `unmap(mode, key)`, and `on(event, fn)`, which mutate the shared
//!   binding/event registries. These live for the whole session and are what
//!   `core.lua` and a user config call at load time.
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

use mlua::{Function, Lua, Result, Scope, Table};

use super::keys::{KeyChord, Mode};
use super::views;
use super::{Bindings, EventKind, Events};
use crate::tui::App;

/// Build the persistent `mudpuppy` table with the `map`/`unmap`/`on`
/// registration functions wired to the shared registries.
pub fn build_table(lua: &Lua, bindings: Bindings, events: Events) -> Result<Table> {
    let table = lua.create_table()?;

    let b = bindings.clone();
    table.set(
        "map",
        lua.create_function(move |_, (mode, key, func): (String, String, Function)| {
            let (mode, chord) = parse_binding(&mode, &key)?;
            // Last binding for a (mode, chord) wins — that's how a user config
            // overrides a core default.
            b.borrow_mut().insert((mode, chord), func);
            Ok(())
        })?,
    )?;

    let b = bindings;
    table.set(
        "unmap",
        lua.create_function(move |_, (mode, key): (String, String)| {
            let (mode, chord) = parse_binding(&mode, &key)?;
            // Remove the binding outright (vs. shadowing it with a no-op), so the
            // key falls back to the Global map — or does nothing if unbound there
            // too. Removing an absent binding is a no-op.
            b.borrow_mut().remove(&(mode, chord));
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

    Ok(table)
}

/// Install the scoped action and reader functions onto `table` for one dispatch,
/// each borrowing the app through `cell`. The functions stop working when the
/// surrounding `lua.scope` ends.
pub fn install_scoped<'scope>(
    scope: &'scope Scope<'scope, '_>,
    table: &Table,
    cell: &'scope RefCell<&mut App>,
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

    // --- read-only views ----------------------------------------------------

    table.set(
        "state",
        scope.create_function(move |lua, ()| views::state(lua, &cell.borrow()))?,
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

/// Parse a `(mode, key)` pair into the registry key, mapping bad spellings to a
/// Lua error the config author will see.
fn parse_binding(mode: &str, key: &str) -> Result<(Mode, KeyChord)> {
    let mode =
        Mode::parse(mode).ok_or_else(|| mlua::Error::runtime(format!("unknown mode {mode:?}")))?;
    let chord = KeyChord::parse(key)
        .ok_or_else(|| mlua::Error::runtime(format!("unparseable key {key:?}")))?;
    Ok((mode, chord))
}
