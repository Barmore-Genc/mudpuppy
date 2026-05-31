//! The viewer's state and verbs. `App` holds everything a frame needs — the
//! parsed files, the selected file and its lazily-built `FileView`, scroll and
//! focus, the loaded annotations and turn block — and exposes the mutating verbs
//! the Lua keymap drives (scroll, select, focus, release the turn, …). The Lua
//! engine reads `&App` and calls these through a per-dispatch `&mut` borrow.
//!
//! `FileView` is the per-file render model: a file's hunks are parsed and
//! syntax-highlighted into `Row`s the first time it is opened, then cached, so a
//! huge diff is never materialized in full.

use std::collections::{HashMap, HashSet};
use std::ops::Range;
use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::blob::{self, BlobSide};
use crate::diff::{DiffLine, FileDiff, GapPos, LineKind};
use crate::domain::{Annotation, Author, Severity, Side, StateFile, Target, Turn};
use crate::highlight::{Highlighter, HlLine};
use crate::picker::Picker;
use crate::{source, store};

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
    /// A "show more" affordance covering a run of still-hidden context lines.
    /// `can_up`/`can_down` say whether more lines remain above/below this gap.
    Expander {
        old: Range<u32>,
        new: Range<u32>,
        can_up: bool,
        can_down: bool,
    },
}

/// How far one gap of hidden context has been revealed from each edge.
#[derive(Clone, Default)]
pub(crate) struct GapExpansion {
    pub(crate) from_top: usize,
    pub(crate) from_bottom: usize,
}

/// The reveal state of every gap in a file's diff, indexed in the order
/// [`crate::diff::gaps`] returns them. Empty until the user expands anything.
#[derive(Clone, Default)]
pub(crate) struct ViewPlan {
    pub(crate) expanded: Vec<GapExpansion>,
}

/// The rows for one opened file, plus the row indices where hunks begin (for
/// `}`/`{` hunk navigation). Built lazily and cached per selected file.
pub(crate) struct FileView {
    pub(crate) rows: Vec<Row>,
    hunk_starts: Vec<usize>,
}

impl FileView {
    /// Build the row list for a file, parsing its hunks on demand.
    ///
    /// `blob` is the file's full new-side (Head) contents, used to reveal hidden
    /// context lines between/around hunks; `None` (a miss, or no expansion yet)
    /// means gaps still render a "show more" `Row::Expander` but no context.
    /// `plan` records how far each gap has been revealed.
    pub(crate) fn build(file: &FileDiff, blob: Option<&[String]>, plan: &ViewPlan) -> FileView {
        let mut rows = Vec::new();
        let mut hunk_starts = Vec::new();

        if file.is_binary {
            rows.push(Row::Notice("Binary file — no textual diff to show".into()));
            return FileView { rows, hunk_starts };
        }

        // Synthetic files have no diff: render their full current content as
        // context lines read from the Head blob.
        if file.synthetic && file.hunks().is_empty() {
            let Some(blob) = blob else {
                rows.push(Row::Notice(
                    "current content unavailable (deleted, binary, or too large)".into(),
                ));
                return FileView { rows, hunk_starts };
            };
            let highlighter = Highlighter::for_path(file.display_path());
            let texts: Vec<&str> = blob.iter().map(String::as_str).collect();
            let highlights = highlighter.as_ref().map(|hl| hl.hunk(&texts));
            for (idx, text) in texts.iter().enumerate() {
                let lineno = idx as u32 + 1;
                let hl = highlights.as_ref().map(|h| h[idx].clone());
                rows.push(Row::Line(
                    DiffLine {
                        kind: LineKind::Context,
                        content: (*text).to_string(),
                        old_lineno: Some(lineno),
                        new_lineno: Some(lineno),
                        no_newline: false,
                    },
                    hl,
                ));
            }
            return FileView { rows, hunk_starts };
        }

        // Resolve the language once for the whole file; `None` (unknown
        // extension) means every line falls back to plain per-kind colouring.
        // Highlighting happens here, when a file is *opened*, so the cost tracks
        // the opened file rather than the whole 1000-file diff.
        let highlighter = Highlighter::for_path(file.display_path());

        let hunks = file.hunks();
        let new_total = blob.map(|b| b.len() as u32);
        let gap_list = crate::diff::gaps(&hunks, new_total);

        // Emit one gap's revealed context (top edge, then the expander for what
        // stays hidden, then the bottom edge), pulling text from `blob`.
        let emit_gap = |rows: &mut Vec<Row>, gap: &crate::diff::Gap, gap_index: usize| {
            let total_hidden = (gap.new.end - gap.new.start) as usize;
            if total_hidden == 0 {
                return;
            }
            let exp = plan.expanded.get(gap_index).cloned().unwrap_or_default();
            // The two edges can never overlap or exceed the gap.
            let from_top = exp.from_top.min(total_hidden);
            let from_bottom = exp.from_bottom.min(total_hidden - from_top);

            // Within an all-context gap, old and new advance in lockstep, so the
            // old↔new offset is constant.
            let delta = gap.new.start as i64 - gap.old.start as i64;
            let reveal = |rows: &mut Vec<Row>, new_from: u32, new_to: u32| {
                let Some(b) = blob else { return };
                if new_from >= new_to {
                    return;
                }
                let texts: Vec<&str> = (new_from..new_to)
                    .map(|n| b.get(n as usize - 1).map(String::as_str).unwrap_or(""))
                    .collect();
                let highlights = highlighter.as_ref().map(|hl| hl.hunk(&texts));
                for (i, text) in texts.iter().enumerate() {
                    let new_lineno = new_from + i as u32;
                    let old_lineno = (new_lineno as i64 - delta) as u32;
                    let hl = highlights.as_ref().map(|h| h[i].clone());
                    rows.push(Row::Line(
                        DiffLine {
                            kind: LineKind::Context,
                            content: (*text).to_string(),
                            old_lineno: Some(old_lineno),
                            new_lineno: Some(new_lineno),
                            no_newline: false,
                        },
                        hl,
                    ));
                }
            };

            reveal(rows, gap.new.start, gap.new.start + from_top as u32);
            let hidden = total_hidden - from_top - from_bottom;
            if hidden > 0 {
                rows.push(Row::Expander {
                    old: (gap.old.start + from_top as u32)..(gap.old.end - from_bottom as u32),
                    new: (gap.new.start + from_top as u32)..(gap.new.end - from_bottom as u32),
                    can_up: matches!(gap.position, GapPos::BeforeFirst | GapPos::Between),
                    can_down: matches!(gap.position, GapPos::Between | GapPos::AfterLast),
                });
            }
            reveal(rows, gap.new.end - from_bottom as u32, gap.new.end);
        };

        // Walk gaps and hunks in line order: a BeforeFirst/Between gap renders
        // immediately before its hunk; the AfterLast gap renders at the end.
        let mut gaps_iter = gap_list.iter().enumerate().peekable();
        for hunk in &hunks {
            while let Some(&(gi, gap)) = gaps_iter.peek() {
                let renders_before = match gap.position {
                    GapPos::AfterLast => false,
                    // Both top and between gaps end where their hunk starts.
                    GapPos::BeforeFirst | GapPos::Between => gap.new.end == hunk.new_start,
                };
                if renders_before {
                    emit_gap(&mut rows, gap, gi);
                    gaps_iter.next();
                } else {
                    break;
                }
            }

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
            for (i, line) in hunk.lines.iter().enumerate() {
                let hl = highlights.as_ref().map(|h| h[i].clone());
                rows.push(Row::Line(line.clone(), hl));
            }
        }
        // Whatever gap remains is the trailing (after-last) one.
        for (gi, gap) in gaps_iter {
            emit_gap(&mut rows, gap, gi);
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
    /// Per-file context-reveal state, indexed parallel to `files`. Survives
    /// reloads so expanded regions stay open while the agent writes.
    plans: Vec<ViewPlan>,
    /// Lazily-fetched full file contents, keyed by `(path, side)`; a cached
    /// `None` records a known miss so we don't re-shell for it. Not invalidated
    /// on reload.
    pub(crate) blob_cache: HashMap<(String, BlobSide), Option<Vec<String>>>,
    /// Repository root, needed to read file blobs for context expansion.
    repo_root: Option<PathBuf>,
    /// The "add any file" picker overlay; `Some` while it is open. It captures
    /// all key input until dismissed.
    pub(crate) picker: Option<Picker>,
    /// Paths pulled in through the picker. Unlike comment-auto-pulled synthetic
    /// files, these are kept across reloads even with zero annotations.
    picker_added: HashSet<String>,
    /// The working-tree file universe (tracked + untracked, gitignore-respecting),
    /// loaded once on first picker open and cached for the session.
    file_universe: Option<Vec<String>>,
}

impl App {
    pub(crate) fn new(files: Vec<FileDiff>, target: Target) -> App {
        // The opening view has no blob yet (none fetched until expansion); gaps
        // still render their "show more" affordance.
        let view = FileView::build(&files[0], None, &ViewPlan::default());
        let plans = vec![ViewPlan::default(); files.len()];
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
            plans,
            blob_cache: HashMap::new(),
            repo_root: None,
            picker: None,
            picker_added: HashSet::new(),
            file_universe: None,
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
        self.merge_synthetic_files();
    }

    /// Reconcile synthetic entries with the loaded annotations so files outside
    /// the diff (but carrying comments) appear in the tree.
    ///
    /// Appends a synthetic [`FileDiff`] for each annotated path not already
    /// represented, and drops stale synthetic entries no annotation references.
    /// Existing entries are never reordered and `selected` keeps pointing at the
    /// same file.
    pub(crate) fn merge_synthetic_files(&mut self) {
        let selected_path = self.current().display_path().to_string();

        // Drop synthetic entries whose annotations have all vanished. Picker-added
        // entries are exempt: the user reached for them deliberately, so they stay
        // across reloads even with zero annotations.
        let annotated: Vec<String> = self.annotations.iter().map(|a| a.file.clone()).collect();
        self.files.retain(|f| {
            !f.synthetic
                || annotated.iter().any(|p| p == f.display_path())
                || self.picker_added.contains(f.display_path())
        });

        // Append a synthetic entry for each annotated path not already present
        // (deduped, so several comments on one file add only one entry).
        let mut present: Vec<String> = self
            .files
            .iter()
            .map(|f| f.display_path().to_string())
            .collect();
        for path in &annotated {
            if !present.iter().any(|p| p == path) {
                self.files.push(FileDiff::synthetic(path));
                present.push(path.clone());
            }
        }

        // Keep the selection on the same file across the retain/append.
        if let Some(idx) = self
            .files
            .iter()
            .position(|f| f.display_path() == selected_path)
        {
            self.selected = idx;
        } else if self.selected >= self.files.len() {
            self.selected = self.files.len().saturating_sub(1);
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
        self.merge_synthetic_files();
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
            self.scroll = 0;
            // Rebuild from the file's plan so any context revealed earlier in
            // this session is restored when returning to the file.
            self.rebuild_view();
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

    /// How many extra context lines from each edge of the nearest gap to reveal
    /// per keystroke.
    const CONTEXT_STEP: usize = 10;

    /// Record the repository root; needed before any blob lookup.
    pub(crate) fn set_repo_root(&mut self, root: PathBuf) {
        self.repo_root = Some(root);
    }

    /// Open the "add any file" picker, loading (and caching) the working-tree
    /// file universe on first use. A no-op without a known repo root.
    pub(crate) fn open_picker(&mut self) {
        let root = match &self.repo_root {
            Some(r) => r.clone(),
            None => return,
        };
        let all = self
            .file_universe
            .get_or_insert_with(|| source::list_files(&root))
            .clone();
        self.picker = Some(Picker::new(all));
    }

    /// Feed one key event to the open picker. Returns `true` if it was consumed
    /// (the picker was open); `false` lets the caller route the key elsewhere.
    pub(crate) fn handle_picker_key(&mut self, ev: KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let Some(picker) = self.picker.as_mut() else {
            return false;
        };
        match ev.code {
            KeyCode::Esc => self.picker = None,
            KeyCode::Enter => self.confirm_picker(),
            KeyCode::Backspace => {
                picker.query.pop();
                picker.refilter();
            }
            KeyCode::Down => picker.move_down(),
            KeyCode::Up => picker.move_up(),
            KeyCode::Char('n') if ctrl => picker.move_down(),
            // Ctrl-p moves the selection while open; it only *opens* the picker
            // when closed (via the Lua binding), so here it scrolls up.
            KeyCode::Char('p') if ctrl => picker.move_up(),
            KeyCode::Char(c) if !ctrl => {
                picker.query.push(c);
                picker.refilter();
            }
            _ => {}
        }
        true
    }

    /// Pull the highlighted path into the file list (selecting it if already
    /// present) and close the picker.
    fn confirm_picker(&mut self) {
        let path = self
            .picker
            .as_ref()
            .and_then(|p| p.current_path())
            .map(str::to_owned);
        self.picker = None;
        let Some(path) = path else { return };
        if let Some(idx) = self.files.iter().position(|f| f.display_path() == path) {
            self.select(idx);
            return;
        }
        self.files.push(FileDiff::synthetic(&path));
        self.plans.push(ViewPlan::default());
        self.picker_added.insert(path);
        self.select(self.files.len() - 1);
    }

    /// The current file's new/old-side path, preferring the new path.
    fn current_path(&self) -> Option<String> {
        let file = self.files.get(self.selected)?;
        file.new_path.clone().or_else(|| file.old_path.clone())
    }

    /// Fetch (and cache) a file's contents on `side`. A cached `None` records a
    /// known miss; the cache survives reloads. Without a known repo root (no
    /// session resolved) every lookup is a miss.
    fn blob_for(&mut self, path: &str, side: BlobSide) -> Option<Vec<String>> {
        let key = (path.to_string(), side);
        if let Some(cached) = self.blob_cache.get(&key) {
            return cached.clone();
        }
        let value = self.repo_root.as_ref().and_then(|root| {
            blob::contents(&self.target, root, path, side)
                .ok()
                .flatten()
        });
        self.blob_cache.insert(key, value.clone());
        value
    }

    /// Rebuild the current file's view from its plan, supplying the Head blob so
    /// revealed context lines can be drawn.
    pub(crate) fn rebuild_view(&mut self) {
        let blob = match self.current_path() {
            Some(path) => self.blob_for(&path, BlobSide::Head),
            None => None,
        };
        let plan = self.plans.get(self.selected).cloned().unwrap_or_default();
        if let Some(file) = self.files.get(self.selected) {
            self.view = FileView::build(file, blob.as_deref(), &plan);
        }
    }

    /// New-side total line count for the current file, if its Head blob is
    /// cached (drives the trailing-gap calculation).
    fn current_new_total(&self) -> Option<u32> {
        let path = self.current_path()?;
        self.blob_cache
            .get(&(path, BlobSide::Head))?
            .as_ref()
            .map(|b| b.len() as u32)
    }

    /// Gap index of the nearest `Row::Expander` at/below `scroll`, wrapping to
    /// the first if none is below.
    fn expander_below(&self) -> Option<usize> {
        let rows = &self.view.rows;
        rows.iter()
            .enumerate()
            .find(|(i, r)| *i >= self.scroll && matches!(r, Row::Expander { .. }))
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .find(|(_, r)| matches!(r, Row::Expander { .. }))
            })
            .and_then(|(i, _)| self.gap_index_at_row(i))
    }

    /// Gap index of the nearest `Row::Expander` above `scroll`, wrapping to
    /// the last if none is above.
    fn expander_above(&self) -> Option<usize> {
        let rows = &self.view.rows;
        rows.iter()
            .enumerate()
            .rev()
            .find(|(i, r)| *i < self.scroll && matches!(r, Row::Expander { .. }))
            .or_else(|| {
                rows.iter()
                    .enumerate()
                    .rev()
                    .find(|(_, r)| matches!(r, Row::Expander { .. }))
            })
            .and_then(|(i, _)| self.gap_index_at_row(i))
    }

    /// Map an expander row to its gap index (in [`crate::diff::gaps`] order) by
    /// matching the still-hidden range against the file's full gap list.
    fn gap_index_at_row(&self, row_index: usize) -> Option<usize> {
        let Some(Row::Expander { new, .. }) = self.view.rows.get(row_index) else {
            return None;
        };
        let file = self.files.get(self.selected)?;
        let all = crate::diff::gaps(&file.hunks(), self.current_new_total());
        all.iter()
            .position(|g| g.new.start <= new.start && new.end <= g.new.end)
    }

    pub(crate) fn expand_down(&mut self) {
        if let Some(gap) = self.expander_below() {
            self.grow_gap(gap, Self::CONTEXT_STEP, 0);
        }
    }

    pub(crate) fn expand_up(&mut self) {
        if let Some(gap) = self.expander_above() {
            let before = self.view.rows.len();
            self.grow_gap(gap, 0, Self::CONTEXT_STEP);
            // Revealing from the bottom edge pushes existing rows down; keep the
            // viewport on the same content by scrolling past the new rows.
            let revealed = self.view.rows.len().saturating_sub(before);
            self.scroll = (self.scroll + revealed).min(self.max_scroll());
        }
    }

    pub(crate) fn expand_all(&mut self) {
        if let Some(gap) = self.expander_below().or_else(|| self.expander_above()) {
            self.grow_gap(gap, usize::MAX, usize::MAX);
        }
    }

    /// Grow gap `gap`'s reveal by `top`/`bottom` lines (clamped on rebuild) and
    /// rebuild the current view from the updated plan.
    fn grow_gap(&mut self, gap: usize, top: usize, bottom: usize) {
        let selected = self.selected;
        if let Some(plan) = self.plans.get_mut(selected) {
            if plan.expanded.len() <= gap {
                plan.expanded.resize_with(gap + 1, GapExpansion::default);
            }
            let entry = &mut plan.expanded[gap];
            entry.from_top = entry.from_top.saturating_add(top);
            entry.from_bottom = entry.from_bottom.saturating_add(bottom);
        }
        self.rebuild_view();
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
impl App {
    /// Test helper: route one key press through a fresh core-only [`LuaEngine`],
    /// exactly as `run_loop` does, and report whether it asked to quit. This is
    /// what lets the layer-1 snapshot/behaviour tests drive the real keymap (now
    /// living in `core.luau`) rather than a hand-coded match. A fresh engine per
    /// call keeps each press independent and is cheap enough for these tests.
    pub(crate) fn handle_key(&mut self, ev: KeyEvent) -> bool {
        // Mirror `run_loop`: the picker captures keys before they reach Lua.
        if self.handle_picker_key(ev) {
            return self.should_quit;
        }
        let engine = LuaEngine::new(None).expect("core.luau loads");
        if let Some(chord) = KeyChord::from_event(&ev) {
            engine.dispatch(self, chord).expect("dispatch");
        }
        self.should_quit
    }
}
