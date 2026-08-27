//! Read-only Lua views over the viewer's state.
//!
//! Every function here takes the live [`App`] and builds a fresh `mlua` table
//! describing some slice of it — the UI state, the file list, the open file's
//! hunks, the annotations, or the rows currently on screen. These are the
//! *reader* half of the `mudpuppy` API: scripts can inspect the diff, the
//! annotations, and what is visible, but never mutate them. All access is
//! mediated here, so the sandbox never needs to hand a script a real file handle
//! or a mutable reference.

use mlua::{IntoLua, Lua, Result, Table, Value};

use crate::diff::{DiffLine, FileDiff, FileStatus, LineKind};
use crate::domain::{AnchorScope, Annotation, Author, Severity, Side, Status, Tag, Turn};
use crate::lua::keys::Mode;
use crate::tui::{App, Focus, Row, Sidebar};

/// Read one field of the `state()` proxy from the live [`App`] — the `__index`
/// side of `mudpuppy.state()`. Recognised fields: `focus`, `selected` (1-based,
/// matching `select_file`), `scroll`, `h_scroll` (the diff's horizontal offset;
/// `0` at the left edge), `cursor`, `count` (the pending count, or
/// `nil`), `show_help`, `sidebar` (`"files"`/`"annotations"`), `selection`
/// (`{lo, hi}` or `nil`), `turn`, `viewport`, and the overlay sub-tables `help`,
/// `composer`, and `prompt` (each `nil` unless that overlay is open). The nested
/// tables are plain read-only tables. An unknown field is `nil`.
pub fn state_field(lua: &Lua, app: &App, key: &str) -> Result<Value> {
    match key {
        "focus" => focus_name(app.focus).into_lua(lua),
        "selected" => (app.selected + 1).into_lua(lua),
        "scroll" => app.scroll.into_lua(lua),
        "h_scroll" => app.h_scroll.into_lua(lua),
        "cursor" => app.cursor.into_lua(lua),
        // The pending count is exposed as a plain number (or nil when unset), so a
        // binding can read `state().count` and write it back via `__newindex`.
        "count" => match app.pending_count {
            Some(n) => n.into_lua(lua),
            None => Ok(Value::Nil),
        },
        "show_help" => app.show_help.into_lua(lua),
        "sidebar" => sidebar_name(app.sidebar).into_lua(lua),
        "selection" => match app.selection_span() {
            Some((lo, hi)) => {
                let sel = lua.create_table()?;
                sel.set("lo", lo)?;
                sel.set("hi", hi)?;
                Value::Table(sel).into_lua(lua)
            }
            None => Ok(Value::Nil),
        },
        "turn" => Value::Table(turn_table(lua, &app.turn)?).into_lua(lua),
        // The overlay sub-tables are `nil` unless that overlay is open, so a
        // binding in an overlay mode can branch on what it is looking at.
        "help" => {
            if !app.show_help {
                return Ok(Value::Nil);
            }
            let help = lua.create_table()?;
            help.set("scroll", app.help_scroll)?;
            help.set("height", app.help_height)?;
            help.set("max_scroll", app.max_help_scroll())?;
            Value::Table(help).into_lua(lua)
        }
        "composer" => match app.composer_mode() {
            Some(mode) => {
                let c = lua.create_table()?;
                c.set(
                    "mode",
                    match mode {
                        Mode::ComposerNormal => "normal",
                        _ => "insert",
                    },
                )?;
                c.set("pending", app.composer_pending())?;
                Value::Table(c).into_lua(lua)
            }
            None => Ok(Value::Nil),
        },
        "prompt" => match app.prompt_selected() {
            Some(selected) => {
                let p = lua.create_table()?;
                // 1-based, like `selected`/`select_file` elsewhere in this API.
                p.set("selected", selected + 1)?;
                p.set("options", app.prompt_len())?;
                p.set("has_details", app.prompt_has_details())?;
                Value::Table(p).into_lua(lua)
            }
            None => Ok(Value::Nil),
        },
        "viewport" => {
            let viewport = lua.create_table()?;
            viewport.set("height", app.diff_height)?;
            viewport.set("width", app.diff_width)?;
            viewport.set("total", app.view.rows.len())?;
            viewport.set("top", app.scroll)?;
            Value::Table(viewport).into_lua(lua)
        }
        _ => Ok(Value::Nil),
    }
}

/// Write one field of the `state()` proxy back to the live [`App`] — the
/// `__newindex` side. Only `count` is writable (a number sets the pending count;
/// `nil` clears it); any other field is read-only and raises a Lua error the
/// config author sees.
pub fn set_state_field(app: &mut App, key: &str, value: Value) -> Result<()> {
    match key {
        "count" => {
            app.pending_count = match value {
                Value::Nil => None,
                Value::Integer(n) if n > 0 => Some(n as u32),
                Value::Number(n) if n >= 1.0 => Some(n as u32),
                // 0, negatives, and non-numbers don't make sense as a count.
                _ => None,
            };
            Ok(())
        }
        _ => Err(mlua::Error::runtime(format!(
            "state().{key} is read-only (only `count` can be assigned)"
        ))),
    }
}

/// `mudpuppy.files()` — the file tree as a 1-based array of `{path, status,
/// additions, deletions, binary}`.
pub fn files(lua: &Lua, app: &App) -> Result<Table> {
    let arr = lua.create_table()?;
    for (i, file) in app.files.iter().enumerate() {
        arr.set(i + 1, file_summary(lua, file)?)?;
    }
    Ok(arr)
}

/// `mudpuppy.current_file()` — the open file with its hunks and lines parsed.
pub fn current_file(lua: &Lua, app: &App) -> Result<Table> {
    let file = app.current();
    let t = file_summary(lua, file)?;
    let hunks = lua.create_table()?;
    for (i, hunk) in file.hunks().into_iter().enumerate() {
        let h = lua.create_table()?;
        h.set("old_start", hunk.old_start)?;
        h.set("old_count", hunk.old_count)?;
        h.set("new_start", hunk.new_start)?;
        h.set("new_count", hunk.new_count)?;
        h.set("section", hunk.section)?;
        let lines = lua.create_table()?;
        for (j, line) in hunk.lines.iter().enumerate() {
            lines.set(j + 1, line_table(lua, line)?)?;
        }
        h.set("lines", lines)?;
        hunks.set(i + 1, h)?;
    }
    t.set("hunks", hunks)?;
    Ok(t)
}

/// `mudpuppy.annotations()` — every annotation in the store (both authors, all
/// files, every status) as a 1-based array.
pub fn annotations(lua: &Lua, app: &App) -> Result<Table> {
    let arr = lua.create_table()?;
    for (i, a) in app.annotations.iter().enumerate() {
        arr.set(i + 1, annotation_table(lua, a)?)?;
    }
    Ok(arr)
}

/// `mudpuppy.screen()` — the diff rows currently visible in the pane, in order,
/// as `{kind, text, old_lineno?, new_lineno?}`. This is "what is on screen".
pub fn screen(lua: &Lua, app: &App) -> Result<Table> {
    let arr = lua.create_table()?;
    let end = (app.scroll + app.diff_height).min(app.view.rows.len());
    for (i, row) in app.view.rows[app.scroll..end].iter().enumerate() {
        let r = lua.create_table()?;
        match row {
            Row::Hunk(text) => {
                r.set("kind", "hunk")?;
                r.set("text", text.as_str())?;
            }
            Row::Notice(text) => {
                r.set("kind", "notice")?;
                r.set("text", text.as_str())?;
            }
            Row::Line(line, _) => {
                r.set("kind", "line")?;
                r.set("text", line.content.as_str())?;
                r.set("change", line_kind_name(line.kind))?;
                if let Some(n) = line.old_lineno {
                    r.set("old_lineno", n)?;
                }
                if let Some(n) = line.new_lineno {
                    r.set("new_lineno", n)?;
                }
            }
            Row::Expander { old, new, .. } => {
                r.set("kind", "expander")?;
                r.set("text", format!("{} hidden lines", new.end - new.start))?;
                r.set("old_lineno", old.start)?;
                r.set("new_lineno", new.start)?;
            }
            Row::Comment(c) => {
                r.set("kind", "comment")?;
                r.set("text", c.text.as_str())?;
                r.set("id", c.id.as_str())?;
            }
            Row::Composer { .. } => {
                r.set("kind", "composer")?;
            }
        }
        arr.set(i + 1, r)?;
    }
    Ok(arr)
}

/// Build a single annotation table — shared by `annotations()` and the
/// `annotation_added` event payload.
pub fn annotation_table(lua: &Lua, a: &Annotation) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("id", a.id.as_str())?;
    t.set("author", author_name(a.author))?;
    t.set("file", a.file.as_str())?;
    t.set("line", a.line)?;
    if let Some(end) = a.end_line {
        t.set("end_line", end)?;
    }
    t.set("side", side_name(a.side))?;
    t.set("scope", scope_name(a.scope))?;
    t.set("severity", severity_name(a.severity))?;
    if let Some(tag) = a.tag {
        t.set("tag", tag_name(tag))?;
    }
    t.set("status", status_name(a.status))?;
    t.set("body", a.body.as_str())?;
    if let Some(parent) = &a.reply_to {
        t.set("reply_to", parent.as_str())?;
    }
    t.set("created_at", a.created_at.to_string())?;
    t.set("updated_at", a.updated_at.to_string())?;
    Ok(t)
}

/// Build the turn table — shared by `state()` and the `turn_change` payload.
pub fn turn_table(lua: &Lua, turn: &Turn) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("owner", author_name(turn.owner))?;
    t.set("seq", turn.seq)?;
    t.set("agent_waiting", turn.agent_waiting)?;
    t.set("approved", turn.approved)?;
    Ok(t)
}

/// The `{path, status, additions, deletions, binary}` summary common to the file
/// list, the open file, and the `filter_files` predicate.
pub fn file_summary(lua: &Lua, file: &FileDiff) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("path", file.display_path())?;
    t.set("status", file_status_name(file.status.clone()))?;
    t.set("additions", file.additions)?;
    t.set("deletions", file.deletions)?;
    t.set("binary", file.is_binary)?;
    Ok(t)
}

fn line_table(lua: &Lua, line: &DiffLine) -> Result<Table> {
    let t = lua.create_table()?;
    t.set("change", line_kind_name(line.kind))?;
    t.set("content", line.content.as_str())?;
    if let Some(n) = line.old_lineno {
        t.set("old_lineno", n)?;
    }
    if let Some(n) = line.new_lineno {
        t.set("new_lineno", n)?;
    }
    t.set("no_newline", line.no_newline)?;
    Ok(t)
}

fn focus_name(focus: Focus) -> &'static str {
    match focus {
        Focus::Tree => "tree",
        Focus::Diff => "diff",
    }
}

fn sidebar_name(sidebar: Sidebar) -> &'static str {
    match sidebar {
        Sidebar::Files => "files",
        Sidebar::Annotations => "annotations",
    }
}

fn line_kind_name(kind: LineKind) -> &'static str {
    match kind {
        LineKind::Context => "context",
        LineKind::Addition => "addition",
        LineKind::Deletion => "deletion",
    }
}

fn file_status_name(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "added",
        FileStatus::Deleted => "deleted",
        FileStatus::Modified => "modified",
        FileStatus::Renamed => "renamed",
        FileStatus::Unchanged => "unchanged",
    }
}

fn author_name(author: Author) -> &'static str {
    match author {
        Author::Agent => "agent",
        Author::User => "user",
    }
}

fn severity_name(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Suggestion => "suggestion",
        Severity::Warning => "warning",
        Severity::Blocker => "blocker",
    }
}

fn side_name(side: Side) -> &'static str {
    match side {
        Side::Right => "RIGHT",
        Side::Left => "LEFT",
    }
}

fn status_name(status: Status) -> &'static str {
    match status {
        Status::Open => "open",
        Status::Resolved => "resolved",
        Status::Wontfix => "wontfix",
        Status::Withdrawn => "withdrawn",
    }
}

fn scope_name(scope: AnchorScope) -> &'static str {
    match scope {
        AnchorScope::Line => "line",
        AnchorScope::File => "file",
    }
}

fn tag_name(tag: Tag) -> &'static str {
    match tag {
        Tag::Question => "?",
        Tag::Concern => "!",
        Tag::Direction => ">",
    }
}
