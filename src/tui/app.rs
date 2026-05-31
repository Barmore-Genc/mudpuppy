//! The viewer's state and verbs. `App` holds everything a frame needs — the
//! parsed files, the selected file and its lazily-built `FileView`, scroll and
//! focus, the loaded annotations and turn block — and exposes the mutating verbs
//! the Lua keymap drives (scroll, select, focus, release the turn, …). The Lua
//! engine reads `&App` and calls these through a per-dispatch `&mut` borrow.
//!
//! `FileView` is the per-file render model: a file's hunks are parsed and
//! syntax-highlighted into `Row`s the first time it is opened, then cached, so a
//! huge diff is never materialized in full.

use std::collections::HashMap;
use std::path::PathBuf;

use crate::diff::{DiffLine, FileDiff};
use crate::domain::{Annotation, Author, Severity, Side, StateFile, Target, Turn};
use crate::highlight::{Highlighter, HlLine};
use crate::store;

/// Which pane has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Focus {
    Tree,
    Diff,
}

/// A single rendered row of the diff pane.
pub(crate) enum Row {
    /// A `@@ … @@` hunk header.
    Hunk(String),
    /// A content line (context / addition / deletion), with its syntax-highlight
    /// colour runs when the file's language is recognised (`None` otherwise —
    /// the row then renders in the plain per-kind colour).
    Line(DiffLine, Option<HlLine>),
    /// An informational placeholder (binary file, empty diff).
    Notice(String),
}

/// The rows for one opened file, plus the row indices where hunks begin (for
/// `}`/`{` hunk navigation). Built lazily and cached per selected file.
pub(crate) struct FileView {
    pub(crate) rows: Vec<Row>,
    hunk_starts: Vec<usize>,
}

impl FileView {
    /// Build the row list for a file, parsing its hunks on demand.
    fn build(file: &FileDiff) -> FileView {
        let mut rows = Vec::new();
        let mut hunk_starts = Vec::new();

        if file.is_binary {
            rows.push(Row::Notice("Binary file — no textual diff to show".into()));
            return FileView { rows, hunk_starts };
        }

        // Resolve the language once for the whole file; `None` (unknown
        // extension) means every line falls back to plain per-kind colouring.
        // Highlighting happens here, when a file is *opened*, so the cost tracks
        // the opened file rather than the whole 1000-file diff.
        let highlighter = Highlighter::for_path(file.display_path());

        for hunk in file.hunks() {
            hunk_starts.push(rows.len());
            rows.push(Row::Hunk(format!(
                "@@ -{},{} +{},{} @@{}",
                hunk.old_start,
                hunk.old_count,
                hunk.new_start,
                hunk.new_count,
                if hunk.section.is_empty() {
                    String::new()
                } else {
                    format!(" {}", hunk.section)
                }
            )));
            // Highlight the hunk's bodies in one pass (parse state is per-hunk;
            // see the `highlight` module), then pair each line with its runs.
            let highlights = highlighter.as_ref().map(|hl| {
                let texts: Vec<&str> = hunk.lines.iter().map(|l| l.content.as_str()).collect();
                hl.hunk(&texts)
            });
            for (i, line) in hunk.lines.into_iter().enumerate() {
                let hl = highlights.as_ref().map(|h| h[i].clone());
                rows.push(Row::Line(line, hl));
            }
        }

        if rows.is_empty() {
            // Mode-only or pure-rename change: no hunks, but still worth showing.
            rows.push(Row::Notice("No line changes (metadata-only change)".into()));
        }

        FileView { rows, hunk_starts }
    }
}

/// The whole viewer's state.
pub(crate) struct App {
    pub(crate) files: Vec<FileDiff>,
    pub(crate) target: Target,
    /// Index into `files` of the file currently shown in the diff pane.
    pub(crate) selected: usize,
    pub(crate) focus: Focus,
    /// Cached rows for the selected file.
    pub(crate) view: FileView,
    /// Top visible row of the diff pane.
    pub(crate) scroll: usize,
    /// Top visible file row of the tree, so the selection stays on screen.
    pub(crate) tree_scroll: usize,
    pub(crate) show_help: bool,
    /// Whether the annotations side panel is shown.
    pub(crate) show_panel: bool,
    /// Annotations loaded from the store (both authors, every status).
    pub(crate) annotations: Vec<Annotation>,
    /// The turn-protocol block from the store, kept in sync on reload so the
    /// status bar can surface "agent is waiting" and `r` can release correctly.
    pub(crate) turn: Turn,
    /// Path to the annotation store, when one was resolved; drives live reload.
    pub(crate) store_path: Option<PathBuf>,
    /// Diff-pane inner height from the last render, used for paging/clamping.
    pub(crate) diff_height: usize,
    /// File-tree inner height from the last render, used to keep selection visible.
    pub(crate) tree_height: usize,
    /// Set by the Lua `quit` verb; checked by `run_loop` after each dispatch.
    pub(crate) should_quit: bool,
    /// Latest message from the scripting engine (a `print` or a config error),
    /// mirrored here each frame so the status bar can surface it.
    pub(crate) status_msg: Option<String>,
}

impl App {
    pub(crate) fn new(files: Vec<FileDiff>, target: Target) -> App {
        let view = FileView::build(&files[0]);
        App {
            files,
            target,
            selected: 0,
            focus: Focus::Tree,
            view,
            scroll: 0,
            tree_scroll: 0,
            show_help: false,
            show_panel: false,
            annotations: Vec::new(),
            turn: Turn::default(),
            store_path: None,
            diff_height: 1,
            tree_height: 1,
            should_quit: false,
            status_msg: None,
        }
    }

    /// Attach a resolved store path and its current state (annotations + turn).
    /// An absent state leaves the empty defaults in place. The store directory is
    /// watched separately in [`run_loop`], which calls [`App::reload`] on a write.
    pub(crate) fn attach_store(&mut self, path: PathBuf, state: Option<StateFile>) {
        self.store_path = Some(path);
        if let Some(state) = state {
            self.annotations = state.annotations;
            self.turn = state.turn;
        }
    }

    /// Reload annotations and turn state from the store, picking up another
    /// process's writes (the agent's comments, or our own turn release). Silent on
    /// errors so a transient read race never disturbs browsing; a no-op without a
    /// store. Triggered by a store-watch tick in [`run_loop`].
    pub(crate) fn reload(&mut self) {
        let Some(path) = &self.store_path else { return };
        if let Ok(Some(state)) = store::load(path) {
            self.annotations = state.annotations;
            self.turn = state.turn;
        }
    }

    /// Release the turn back to the agent (PLAN.md §6): bump `seq`, take
    /// ownership, clear the waiting flag, and record approval (the human's first
    /// release doubles as first-contact approval). The atomic store write is what
    /// wakes an agent blocked in `agent wait`. A no-op when no store is attached.
    pub(crate) fn release_turn(&mut self) {
        let Some(path) = self.store_path.clone() else {
            return;
        };
        let updated = store::update(&path, &self.target, |s| {
            s.turn.seq += 1;
            s.turn.owner = Author::Agent;
            s.turn.agent_waiting = false;
            s.turn.approved = true;
            s.turn.clone()
        });
        if let Ok(turn) = updated {
            self.turn = turn;
        }
        // The watch will also tick on our own write and trigger a harmless
        // reload of what we just stored; the in-memory update above keeps the
        // status bar correct in the meantime.
    }

    /// Whether an agent is making first contact: it is blocked in `agent wait`
    /// (so it has written to this session and is expecting a turn) but the human
    /// has not yet approved. While this holds the TUI surfaces an approval prompt,
    /// and the human's first turn-release (`r`) doubles as approval (PLAN.md §6).
    /// Once approved a session stays approved, so this is only ever true at the
    /// very start.
    pub(crate) fn awaiting_approval(&self) -> bool {
        self.turn.agent_waiting && !self.turn.approved
    }

    /// Annotations anchored to the file currently open in the diff pane.
    pub(crate) fn current_file_annotations(&self) -> Vec<&Annotation> {
        let path = self.current().display_path();
        self.annotations.iter().filter(|a| a.file == path).collect()
    }

    /// A `(side, line) -> severity` map of gutter marks for the current file,
    /// keeping the most severe annotation when several anchor to one line.
    /// `Severity` is `Ord`, so `max` picks the colour the gutter should show.
    pub(crate) fn line_marks(&self) -> HashMap<(Side, u32), Severity> {
        let path = self.current().display_path();
        let mut marks: HashMap<(Side, u32), Severity> = HashMap::new();
        for a in self.annotations.iter().filter(|a| a.file == path) {
            marks
                .entry((a.side, a.line))
                .and_modify(|s| *s = (*s).max(a.severity))
                .or_insert(a.severity);
        }
        marks
    }

    /// Open file `idx`, rebuilding the cached view and resetting the scroll.
    pub(crate) fn select(&mut self, idx: usize) {
        let idx = idx.min(self.files.len() - 1);
        if idx != self.selected {
            self.selected = idx;
            self.view = FileView::build(&self.files[idx]);
            self.scroll = 0;
        }
    }

    /// Open a file by its 1-based index (the numbering Lua sees). Values below 1
    /// clamp to the first file; `select` clamps the upper end.
    pub(crate) fn select_file(&mut self, index: i64) {
        let idx = if index < 1 { 0 } else { (index - 1) as usize };
        self.select(idx);
    }

    pub(crate) fn max_scroll(&self) -> usize {
        self.view.rows.len().saturating_sub(self.diff_height)
    }

    pub(crate) fn scroll_by(&mut self, delta: isize) {
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, self.max_scroll() as isize) as usize;
    }

    /// Set the diff scroll to an absolute row, clamped to `[0, max_scroll]`. A
    /// large value (used by `G`) lands exactly on the bottom.
    pub(crate) fn set_scroll(&mut self, n: i64) {
        let max = self.max_scroll() as i64;
        self.scroll = n.clamp(0, max) as usize;
    }

    /// Request quit; `run_loop` honours it after the dispatch returns.
    pub(crate) fn quit(&mut self) {
        self.should_quit = true;
    }

    /// Focus a pane by name (`"tree"`/`"diff"`); an unknown name is ignored.
    pub(crate) fn set_focus(&mut self, pane: &str) {
        match pane {
            "tree" => self.focus = Focus::Tree,
            "diff" => self.focus = Focus::Diff,
            _ => {}
        }
    }

    /// Toggle the help overlay.
    pub(crate) fn toggle_help(&mut self) {
        self.show_help = !self.show_help;
    }

    /// Toggle the annotations side panel.
    pub(crate) fn toggle_panel(&mut self) {
        self.show_panel = !self.show_panel;
    }

    pub(crate) fn next_hunk(&mut self) {
        if let Some(&s) = self.view.hunk_starts.iter().find(|&&s| s > self.scroll) {
            self.scroll = s.min(self.max_scroll());
        }
    }

    pub(crate) fn prev_hunk(&mut self) {
        if let Some(&s) = self
            .view
            .hunk_starts
            .iter()
            .rev()
            .find(|&&s| s < self.scroll)
        {
            self.scroll = s;
        }
    }

    pub(crate) fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Diff,
            Focus::Diff => Focus::Tree,
        };
    }

    pub(crate) fn current(&self) -> &FileDiff {
        &self.files[self.selected]
    }
}

#[cfg(test)]
use crate::lua::keys::KeyChord;
#[cfg(test)]
use crate::lua::LuaEngine;
#[cfg(test)]
use ratatui::crossterm::event::KeyEvent;

#[cfg(test)]
impl App {
    /// Test helper: route one key press through a fresh core-only [`LuaEngine`],
    /// exactly as `run_loop` does, and report whether it asked to quit. This is
    /// what lets the layer-1 snapshot/behaviour tests drive the real keymap (now
    /// living in `core.luau`) rather than a hand-coded match. A fresh engine per
    /// call keeps each press independent and is cheap enough for these tests.
    pub(crate) fn handle_key(&mut self, ev: KeyEvent) -> bool {
        let engine = LuaEngine::new(None).expect("core.luau loads");
        if let Some(chord) = KeyChord::from_event(&ev) {
            engine.dispatch(self, chord).expect("dispatch");
        }
        self.should_quit
    }
}
