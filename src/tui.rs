//! The ratatui diff viewer (PLAN.md §9), milestone 1: a read-only, keyboard-
//! driven browser over the parsed diff. File tree on the left, diff pane in the
//! center, status bar along the bottom, and a `?` help overlay.
//!
//! Rendering is **virtualized**: only the rows currently in the viewport are
//! turned into styled spans, and a file's hunks are parsed lazily the first
//! time it is opened (and cached), so a 50k-line diff never gets materialized in
//! full.
//!
//! As of milestone 2 the viewer also **reads** the annotation store: it draws
//! severity-coloured gutter markers on annotated lines, lists annotations in a
//! toggleable side panel, and live-reloads when the store changes on disk so an
//! agent's comments appear while the TUI is open. That reload rides the same
//! `notify` coordination bus `agent wait` uses (PLAN.md §9): the event loop
//! watches the store directory and refreshes in place when a write lands.
//!
//! Milestone 3 adds the human's half of the turn protocol (PLAN.md §6): when an
//! agent is blocked in `agent wait`, the store's `turn.agent_waiting` flag is
//! set and the status bar surfaces it; pressing `r` **releases the turn** —
//! bumping `turn.seq`, handing ownership back to the agent, and (on first
//! contact) recording approval. That store write is what wakes the waiting
//! agent.
//!
//! The diff pane is **syntax-highlighted** via [`crate::highlight`] (syntect):
//! each opened file's hunks are coloured in place, under the gutter and
//! annotation overlays. Authoring annotations from inside the TUI is still to
//! come.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::crossterm::event::{
    Event, EventStream, KeyCode, KeyEvent, KeyEventKind, KeyModifiers,
};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::StreamExt;

use crate::diff::{parse_diff, DiffLine, FileDiff, FileStatus, LineKind};
use crate::domain::{Annotation, Author, Severity, Side, StateFile, Status, Target, Turn};
use crate::highlight::{Highlighter, HlLine};
use crate::session::Session;
use crate::{source, store};

/// The glyph drawn in the annotation gutter column on an annotated line.
const MARK: &str = "●";

/// Launch the interactive review UI.
///
/// `pr` selects a pull-request target (`owner/repo#123` or a URL) when present;
/// otherwise the review targets local changes. `base` overrides the inferred
/// base ref for local reviews.
pub fn launch(pr: Option<String>, base: Option<String>) -> Result<()> {
    let loaded = source::load(pr.as_deref(), base.as_deref())?;
    let files = parse_diff(&loaded.raw);

    if files.is_empty() {
        // Nothing to render — say so on the normal terminal rather than flashing
        // an empty alternate screen.
        println!("No changes to review ({}).", target_desc(&loaded.target));
        return Ok(());
    }

    // Resolve where this review's annotations live and load any that exist.
    // Resolution failure shouldn't block browsing the diff, so degrade to an
    // empty, store-less view rather than aborting.
    let mut app = App::new(files, loaded.target.clone());
    if let Ok(session) = Session::resolve(loaded.target) {
        let state = store::load(&session.store_path)?;
        app.attach_store(session.store_path, state);
    }

    // `ratatui::init` enters the alternate screen, turns on raw mode, and
    // installs a panic hook that restores the terminal; `restore` undoes it.
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// The draw → await-event → handle loop. Returns when the user quits.
///
/// Runs on a small current-thread Tokio runtime — the same async foundation
/// `agent wait` uses (agent.rs) — so the two halves of the `notify` coordination
/// bus share one model. A `tokio::select!` multiplexes the two wake sources with
/// no busy poll: crossterm's async [`EventStream`] for terminal input, and a
/// channel the store-directory watcher ticks on every write. A store tick reloads
/// in place — the live-reload half of the bus that wakes the agent (PLAN.md
/// §6, §9).
fn run_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the TUI runtime")?;

    runtime.block_on(async move {
        // Store-change ticks: the watcher fires these from notify's own thread;
        // the loop only cares that *something* changed and re-reads to decide.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        // Watch the store directory if one was resolved. The watcher must outlive
        // the loop, so it's bound here; dropping it would stop delivery. Live
        // reload is best-effort: if the watch can't be set up we still browse the
        // diff, just without picking up the agent's writes (mirrors the store-less
        // degrade in `launch`).
        let _watcher = app
            .store_path
            .as_deref()
            .and_then(Path::parent)
            .and_then(|dir| watch_store_dir(dir, tx).ok());

        let mut events = EventStream::new();

        loop {
            terminal.draw(|frame| render(frame, app))?;
            tokio::select! {
                // Terminal input. `EventStream::next` is cancel-safe, so the
                // dropped future on a store tick loses nothing.
                maybe_event = events.next() => match maybe_event {
                    // Only act on key *presses*; on Windows crossterm also emits
                    // release and repeat events that would double every keystroke.
                    Some(Ok(Event::Key(key))) => {
                        if key.kind == KeyEventKind::Press && app.handle_key(key) {
                            return Ok(());
                        }
                    }
                    // Resize and other terminal events just need the redraw above.
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e).context("reading terminal input"),
                    // The input stream ended (terminal closed): nothing to wait on.
                    None => return Ok(()),
                },
                // A store write landed (the agent's comments, or our own release).
                // `None` means the watcher was dropped/never attached; keep going
                // on input alone rather than spinning, since `recv` then stays
                // pending.
                Some(()) = rx.recv() => app.reload(),
            }
        }
    })
}

/// Start watching `dir` for changes, forwarding a `()` tick on every filesystem
/// event. The returned watcher must be kept alive for as long as ticks are
/// wanted. Mirrors `agent wait`'s watch (non-recursive on the store directory,
/// since atomic writes land as a temp file + rename within it).
///
/// The store directory may not exist yet when the human opens the TUI before any
/// annotation is written, so it's created first — both so the watch can attach
/// and so it's already in place to catch the agent's very first write.
fn watch_store_dir(dir: &Path, tx: UnboundedSender<()>) -> Result<RecommendedWatcher> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating the store directory {} to watch", dir.display()))?;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // We don't care which event fired, only that something changed; the loop
        // re-reads the store to decide. Drop the tick if the receiver is gone.
        if res.is_ok() {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}

/// Which pane has keyboard focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Focus {
    Tree,
    Diff,
}

/// A single rendered row of the diff pane.
enum Row {
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
struct FileView {
    rows: Vec<Row>,
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
struct App {
    files: Vec<FileDiff>,
    target: Target,
    /// Index into `files` of the file currently shown in the diff pane.
    selected: usize,
    focus: Focus,
    /// Cached rows for the selected file.
    view: FileView,
    /// Top visible row of the diff pane.
    scroll: usize,
    /// Top visible file row of the tree, so the selection stays on screen.
    tree_scroll: usize,
    show_help: bool,
    /// Whether the annotations side panel is shown.
    show_panel: bool,
    /// Annotations loaded from the store (both authors, every status).
    annotations: Vec<Annotation>,
    /// The turn-protocol block from the store, kept in sync on reload so the
    /// status bar can surface "agent is waiting" and `r` can release correctly.
    turn: Turn,
    /// Path to the annotation store, when one was resolved; drives live reload.
    store_path: Option<PathBuf>,
    /// Diff-pane inner height from the last render, used for paging/clamping.
    diff_height: usize,
    /// File-tree inner height from the last render, used to keep selection visible.
    tree_height: usize,
}

impl App {
    fn new(files: Vec<FileDiff>, target: Target) -> App {
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
        }
    }

    /// Attach a resolved store path and its current state (annotations + turn).
    /// An absent state leaves the empty defaults in place. The store directory is
    /// watched separately in [`run_loop`], which calls [`App::reload`] on a write.
    fn attach_store(&mut self, path: PathBuf, state: Option<StateFile>) {
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
    fn reload(&mut self) {
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
    fn release_turn(&mut self) {
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

    /// Annotations anchored to the file currently open in the diff pane.
    fn current_file_annotations(&self) -> Vec<&Annotation> {
        let path = self.current().display_path();
        self.annotations.iter().filter(|a| a.file == path).collect()
    }

    /// A `(side, line) -> severity` map of gutter marks for the current file,
    /// keeping the most severe annotation when several anchor to one line.
    /// `Severity` is `Ord`, so `max` picks the colour the gutter should show.
    fn line_marks(&self) -> HashMap<(Side, u32), Severity> {
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
    fn select(&mut self, idx: usize) {
        let idx = idx.min(self.files.len() - 1);
        if idx != self.selected {
            self.selected = idx;
            self.view = FileView::build(&self.files[idx]);
            self.scroll = 0;
        }
    }

    fn max_scroll(&self) -> usize {
        self.view.rows.len().saturating_sub(self.diff_height)
    }

    fn scroll_by(&mut self, delta: isize) {
        let next = self.scroll as isize + delta;
        self.scroll = next.clamp(0, self.max_scroll() as isize) as usize;
    }

    fn next_hunk(&mut self) {
        if let Some(&s) = self.view.hunk_starts.iter().find(|&&s| s > self.scroll) {
            self.scroll = s.min(self.max_scroll());
        }
    }

    fn prev_hunk(&mut self) {
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

    /// Handle one key press. Returns `true` when the app should quit.
    fn handle_key(&mut self, key: KeyEvent) -> bool {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);

        // Help overlay swallows everything except its own dismissal.
        if self.show_help {
            match key.code {
                KeyCode::Char('?') | KeyCode::Esc | KeyCode::Char('q') => self.show_help = false,
                _ => {}
            }
            return false;
        }

        match key.code {
            // Global.
            KeyCode::Char('q') => return true,
            KeyCode::Char('c') if ctrl => return true,
            KeyCode::Char('?') => self.show_help = true,
            KeyCode::Char('a') => self.show_panel = !self.show_panel,
            KeyCode::Char('r') => self.release_turn(),
            KeyCode::Tab | KeyCode::BackTab => self.toggle_focus(),

            // Half / full page, regardless of focus, scroll the diff.
            KeyCode::Char('d') if ctrl => self.scroll_by(self.diff_height as isize / 2),
            KeyCode::Char('u') if ctrl => self.scroll_by(-(self.diff_height as isize / 2)),
            KeyCode::Char('f') if ctrl => self.scroll_by(self.diff_height as isize),
            KeyCode::Char('b') if ctrl => self.scroll_by(-(self.diff_height as isize)),
            KeyCode::PageDown => self.scroll_by(self.diff_height as isize),
            KeyCode::PageUp => self.scroll_by(-(self.diff_height as isize)),

            _ => match self.focus {
                Focus::Tree => self.handle_tree_key(key),
                Focus::Diff => self.handle_diff_key(key),
            },
        }
        false
    }

    fn handle_tree_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.select(self.selected + 1),
            KeyCode::Char('k') | KeyCode::Up => self.select(self.selected.saturating_sub(1)),
            KeyCode::Char('g') | KeyCode::Home => self.select(0),
            KeyCode::Char('G') | KeyCode::End => self.select(self.files.len() - 1),
            KeyCode::Enter | KeyCode::Char('l') | KeyCode::Right => self.focus = Focus::Diff,
            _ => {}
        }
    }

    fn handle_diff_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down => self.scroll_by(1),
            KeyCode::Char('k') | KeyCode::Up => self.scroll_by(-1),
            KeyCode::Char('g') | KeyCode::Home => self.scroll = 0,
            KeyCode::Char('G') | KeyCode::End => self.scroll = self.max_scroll(),
            KeyCode::Char('}') | KeyCode::Char('n') => self.next_hunk(),
            KeyCode::Char('{') | KeyCode::Char('N') => self.prev_hunk(),
            // Jump between files without leaving the diff pane.
            KeyCode::Char('J') => self.select(self.selected + 1),
            KeyCode::Char('K') => self.select(self.selected.saturating_sub(1)),
            KeyCode::Char('h') | KeyCode::Left => self.focus = Focus::Tree,
            _ => {}
        }
    }

    fn toggle_focus(&mut self) {
        self.focus = match self.focus {
            Focus::Tree => Focus::Diff,
            Focus::Diff => Focus::Tree,
        };
    }

    fn current(&self) -> &FileDiff {
        &self.files[self.selected]
    }
}

/// Draw the whole UI for one frame and record viewport heights for the next
/// key-handling pass.
fn render(frame: &mut Frame, app: &mut App) {
    let [main, status] =
        Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(frame.area());

    // The annotations panel claims a right-hand column when toggled on.
    let (tree, diff, panel) = if app.show_panel {
        let [tree, diff, panel] = Layout::horizontal([
            Constraint::Percentage(24),
            Constraint::Min(0),
            Constraint::Length(40),
        ])
        .areas(main);
        (tree, diff, Some(panel))
    } else {
        let [tree, diff] =
            Layout::horizontal([Constraint::Percentage(28), Constraint::Min(0)]).areas(main);
        (tree, diff, None)
    };

    // Inner heights (minus the one-row borders top and bottom) drive paging and
    // keep the tree selection visible.
    app.tree_height = tree.height.saturating_sub(2) as usize;
    app.diff_height = diff.height.saturating_sub(2) as usize;
    app.scroll = app.scroll.min(app.max_scroll());

    render_tree(frame, tree, app);
    render_diff(frame, diff, app);
    if let Some(panel) = panel {
        render_panel(frame, panel, app);
    }
    render_status(frame, status, app);

    if app.show_help {
        render_help(frame, frame.area());
    }
}

/// The left-hand file tree, with status markers and `+`/`-` counts.
fn render_tree(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Tree;
    let height = app.tree_height.max(1);

    // Scroll the tree so the selection is always on screen.
    if app.selected < app.tree_scroll {
        app.tree_scroll = app.selected;
    } else if app.selected >= app.tree_scroll + height {
        app.tree_scroll = app.selected + 1 - height;
    }

    let mut lines = Vec::new();
    let end = (app.tree_scroll + height).min(app.files.len());
    for idx in app.tree_scroll..end {
        let file = &app.files[idx];
        let (marker, color) = status_marker(&file.status);
        let selected = idx == app.selected;

        let path_style = if selected {
            Style::default()
                .fg(Color::White)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default()
        };

        let spans = vec![
            Span::styled(format!("{marker} "), Style::default().fg(color)),
            Span::styled(file.display_path().to_string(), path_style),
            Span::raw("  "),
            Span::styled(
                format!("+{}", file.additions),
                Style::default().fg(Color::Green),
            ),
            Span::raw(" "),
            Span::styled(
                format!("-{}", file.deletions),
                Style::default().fg(Color::Red),
            ),
        ];

        let mut line = Line::from(spans);
        if selected {
            line = line.style(Style::default().bg(Color::Rgb(40, 44, 52)));
        }
        lines.push(line);
    }

    let title = format!(" Files ({}) ", app.files.len());
    frame.render_widget(Paragraph::new(lines).block(bordered(&title, focused)), area);
}

/// The center diff pane: only the visible window of rows is built into spans.
/// Lines carrying annotations get a severity-coloured gutter mark; when the file
/// has any annotations every row reserves the mark column so text stays aligned.
fn render_diff(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Diff;
    let marks = app.line_marks();
    let gutter = !marks.is_empty();
    let file = app.current();
    let height = app.diff_height.max(1);
    let end = (app.scroll + height).min(app.view.rows.len());

    let lines: Vec<Line> = app.view.rows[app.scroll..end]
        .iter()
        .map(|row| row_to_line(row, gutter, &marks))
        .collect();

    let count = marks.len();
    let title = if count == 0 {
        format!(
            " {} [{}] {}/{} ",
            file.display_path(),
            status_word(&file.status),
            app.selected + 1,
            app.files.len()
        )
    } else {
        format!(
            " {} [{}] {}/{}  {MARK}{count} ",
            file.display_path(),
            status_word(&file.status),
            app.selected + 1,
            app.files.len(),
        )
    };
    frame.render_widget(Paragraph::new(lines).block(bordered(&title, focused)), area);
}

/// The right-hand annotations panel: every annotation on the current file, each
/// with a severity-coloured mark, its anchor line, author, optional tag, status,
/// and a one-line body preview. Threaded replies are indented under their parent.
fn render_panel(frame: &mut Frame, area: Rect, app: &App) {
    let path = app.current().display_path();
    let here = app.current_file_annotations();

    let mut lines: Vec<Line> = Vec::new();
    if here.is_empty() {
        lines.push(Line::from(Span::styled(
            "No annotations on this file.",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )));
        let elsewhere = app.annotations.len();
        if elsewhere > 0 {
            lines.push(Line::raw(""));
            lines.push(Line::from(Span::styled(
                format!("{elsewhere} on other files."),
                Style::default().fg(Color::DarkGray),
            )));
        }
    } else {
        for a in &here {
            let indent = if a.is_reply() { "  " } else { "" };
            let tag = a
                .tag
                .map(|t| format!(" {}", tag_symbol(t)))
                .unwrap_or_default();
            let header = format!(
                "{indent}{MARK} L{} {}{}  {}",
                a.line,
                author_word(a.author),
                tag,
                ann_status(a.status),
            );
            lines.push(Line::from(vec![Span::styled(
                header,
                Style::default().fg(severity_color(a.severity)),
            )]));
            // First body line as a preview; the panel stays scannable.
            if let Some(preview) = a.body.lines().next() {
                lines.push(Line::from(Span::styled(
                    format!("{indent}  {preview}"),
                    Style::default().fg(Color::Gray),
                )));
            }
            lines.push(Line::raw(""));
        }
    }

    let title = format!(" Annotations · {} ({}) ", path, here.len());
    frame.render_widget(
        Paragraph::new(lines)
            .block(bordered(&title, false))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The bottom status bar: target, position, counts, focus, and the help hint.
fn render_status(frame: &mut Frame, area: Rect, app: &App) {
    let file = app.current();
    let scroll_pct = if app.max_scroll() == 0 {
        "ALL".to_string()
    } else if app.scroll == 0 {
        "TOP".to_string()
    } else if app.scroll >= app.max_scroll() {
        "BOT".to_string()
    } else {
        format!("{}%", app.scroll * 100 / app.max_scroll())
    };

    let focus = match app.focus {
        Focus::Tree => "tree",
        Focus::Diff => "diff",
    };

    // The annotation segment is omitted entirely when there are none, so an
    // unannotated review reads exactly as the milestone-1 viewer did.
    let annotations = if app.annotations.is_empty() {
        String::new()
    } else {
        let open = app.annotations.iter().filter(|a| a.is_open()).count();
        format!("  ·  {MARK} {} ({open} open)", app.annotations.len())
    };

    let left = format!(
        " {}  ·  file {}/{}  ·  +{} -{}{}  ·  [{}] {}",
        target_desc(&app.target),
        app.selected + 1,
        app.files.len(),
        file.additions,
        file.deletions,
        annotations,
        focus,
        scroll_pct,
    );

    let mut spans = vec![Span::raw(left)];
    // When an agent is blocked in `agent wait`, make it impossible to miss and
    // advertise the release key (PLAN.md §6).
    if app.turn.agent_waiting {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            " agent waiting ",
            Style::default()
                .bg(Color::Yellow)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("  "));
        spans.push(Span::styled("r", Style::default().fg(Color::Yellow)));
        spans.push(Span::raw(" release  "));
    } else {
        spans.push(Span::raw("  "));
    }
    spans.extend([
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);
    let line = Line::from(spans);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Rgb(30, 33, 40)).fg(Color::Gray)),
        area,
    );
}

/// The centered help overlay listing every keybinding.
///
/// Kept compact (cyan section headers, no blank separators) so the whole list —
/// including the closing hint — fits within a short, 24-row terminal.
fn render_help(frame: &mut Frame, area: Rect) {
    let section =
        |name: &'static str| Line::from(Span::styled(name, Style::default().fg(Color::Cyan)));
    let text = vec![
        Line::from(Span::styled(
            "mudpuppy — diff viewer",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        section("Navigation"),
        Line::raw("  j / k        down / up (scroll diff, move file selection)"),
        Line::raw("  Ctrl-d / -u  half page down / up"),
        Line::raw("  Ctrl-f / -b  full page down / up"),
        Line::raw("  g / G        top / bottom (or first / last file)"),
        Line::raw("  } / {        next / prev hunk   (also n / N)"),
        Line::raw("  J / K        next / prev file (from the diff pane)"),
        section("Focus"),
        Line::raw("  Tab          switch between file tree and diff"),
        Line::raw("  l / Enter    file tree → diff"),
        Line::raw("  h            diff → file tree"),
        section("Annotations"),
        Line::raw("  a            toggle the annotations panel"),
        section("Turn"),
        Line::raw("  r            release the turn back to the agent"),
        section("Other"),
        Line::raw("  ?            toggle this help"),
        Line::raw("  q / Ctrl-c   quit"),
        Line::raw(""),
        Line::from(Span::styled(
            "press ?, q, or Esc to close",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    let area = centered_rect(64, text.len() as u16 + 2, area);
    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(text)
            .block(bordered(" Help ", true))
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// Turn one diff row into a styled line. `gutter` reserves the annotation-mark
/// column (so non-content rows stay aligned with marked lines); `marks` supplies
/// the per-`(side, line)` severities to colour the mark.
fn row_to_line(row: &Row, gutter: bool, marks: &HashMap<(Side, u32), Severity>) -> Line<'static> {
    let pad = if gutter { " " } else { "" };
    match row {
        Row::Hunk(text) => Line::from(Span::styled(
            format!("{pad}{text}"),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Row::Notice(text) => Line::from(Span::styled(
            format!("{pad}{text}"),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        Row::Line(line, hl) => diff_line(line, hl.as_ref(), gutter, marks),
    }
}

/// Format a content line: an optional annotation mark, an `old new` gutter, a
/// `+`/`-`/space marker, then the text. The `+`/`-` marker keeps its kind colour
/// (green/red) so additions and deletions stay scannable at a glance; the text
/// itself is syntax-coloured when `hl` is present, falling back to the plain
/// per-kind colour otherwise.
fn diff_line(
    line: &DiffLine,
    hl: Option<&HlLine>,
    gutter: bool,
    marks: &HashMap<(Side, u32), Severity>,
) -> Line<'static> {
    let (marker, color) = match line.kind {
        LineKind::Addition => ('+', Color::Green),
        LineKind::Deletion => ('-', Color::Red),
        LineKind::Context => (' ', Color::Reset),
    };

    let old = line.old_lineno.map(|n| n.to_string()).unwrap_or_default();
    let new = line.new_lineno.map(|n| n.to_string()).unwrap_or_default();
    let numbers = format!("{old:>5} {new:>5} ");

    let mut spans = Vec::with_capacity(4);
    if gutter {
        // The mark sits on whichever side the line exists on (an addition only
        // on RIGHT, a deletion only on LEFT, context on either).
        let severity = line
            .new_lineno
            .and_then(|n| marks.get(&(Side::Right, n)))
            .or_else(|| line.old_lineno.and_then(|n| marks.get(&(Side::Left, n))));
        match severity {
            Some(sev) => spans.push(Span::styled(
                MARK,
                Style::default().fg(severity_color(*sev)),
            )),
            None => spans.push(Span::raw(" ")),
        }
    }
    spans.push(Span::styled(numbers, Style::default().fg(Color::DarkGray)));
    spans.push(Span::styled(
        format!("{marker} "),
        Style::default().fg(color),
    ));

    // Tabs render unpredictably across terminals; expand to a 4-space stop.
    // Expanding per highlight run is equivalent to expanding the whole line, so
    // the rendered characters match the plain path exactly.
    match hl {
        Some(runs) => {
            for (run_color, text) in runs {
                spans.push(Span::styled(
                    text.replace('\t', "    "),
                    Style::default().fg(*run_color),
                ));
            }
        }
        None => {
            let text_style = match line.kind {
                LineKind::Context => Style::default(),
                _ => Style::default().fg(color),
            };
            spans.push(Span::styled(line.content.replace('\t', "    "), text_style));
        }
    }
    Line::from(spans)
}

/// A bordered block whose border brightens when its pane is focused.
fn bordered(title: &str, focused: bool) -> Block<'static> {
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };
    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title.to_string())
}

/// Single-letter status marker and its colour for the file tree.
fn status_marker(status: &FileStatus) -> (char, Color) {
    match status {
        FileStatus::Added => ('A', Color::Green),
        FileStatus::Deleted => ('D', Color::Red),
        FileStatus::Modified => ('M', Color::Yellow),
        FileStatus::Renamed => ('R', Color::Cyan),
    }
}

/// Human-readable status word for the diff-pane title.
fn status_word(status: &FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "added",
        FileStatus::Deleted => "deleted",
        FileStatus::Modified => "modified",
        FileStatus::Renamed => "renamed",
    }
}

/// The gutter/panel colour for an annotation's severity.
fn severity_color(severity: Severity) -> Color {
    match severity {
        Severity::Info => Color::Blue,
        Severity::Suggestion => Color::Cyan,
        Severity::Warning => Color::Yellow,
        Severity::Blocker => Color::Red,
    }
}

/// Single-word author label for the annotations panel.
fn author_word(author: Author) -> &'static str {
    match author {
        Author::Agent => "agent",
        Author::Human => "human",
    }
}

/// Single-word status label for an annotation (distinct from a file's status).
fn ann_status(status: Status) -> &'static str {
    match status {
        Status::Open => "open",
        Status::Resolved => "resolved",
        Status::Wontfix => "wontfix",
        Status::Withdrawn => "withdrawn",
    }
}

/// The one-character tag symbol shown in the panel.
fn tag_symbol(tag: crate::domain::Tag) -> &'static str {
    use crate::domain::Tag;
    match tag {
        Tag::Question => "?",
        Tag::Concern => "!",
        Tag::Direction => ">",
    }
}

/// Short description of what's under review, for the status bar.
fn target_desc(target: &Target) -> String {
    match target {
        Target::Local { base, .. } => format!("local vs {base}"),
        Target::Pr { pr, .. } => format!("PR {pr}"),
    }
}

/// A rectangle of `width`×`height` centered within `area` (clamped to fit).
fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let width = width.min(area.width);
    let height = height.min(area.height);
    Rect {
        x: area.x + (area.width - width) / 2,
        y: area.y + (area.height - height) / 2,
        width,
        height,
    }
}

// Layer 1 of TESTING.md: the dense correctness layer. We drive the *decoupled*
// `update(state, event)` transition (`App::handle_key`) with real crossterm
// `KeyEvent`s, render `App` into a ratatui `TestBackend`, and snapshot the
// resulting cell grid with `insta` — plus a few explicit style assertions,
// because the buffer carries fg/bg/modifiers and "did it render *green*" is part
// of correctness. No real terminal, no PTY, no async: fully deterministic.
//
// Everything layer 1 deliberately *cannot* see (real binary, raw-mode
// setup/teardown, tty byte decoding, the `git`/`gh` subprocesses) is layer 2's
// job and lives in `tests/e2e.rs`.
#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    /// A deterministic multi-file diff exercising every status the tree renders
    /// (modified, added, deleted, renamed, binary) plus a metadata-only rename
    /// and a file long enough to scroll/page through. Byte-stable: no clock, no
    /// ids — the viewer neither reads nor writes the store yet.
    const FIXTURE: &str = r#"diff --git a/src/alpha.rs b/src/alpha.rs
index 1111111..2222222 100644
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1,4 +1,5 @@ fn alpha() {
 use std::io;
-let x = 1;
+let x = 2;
+let y = 3;
 println!("{x}");
 done
@@ -20,2 +21,2 @@ fn beta() {
 let beta = compute();
-old beta
+new beta
diff --git a/src/long.rs b/src/long.rs
index 3333333..4444444 100644
--- a/src/long.rs
+++ b/src/long.rs
@@ -1,19 +1,20 @@ impl Long {
 line 01
 line 02
 line 03
 line 04
 line 05
 line 06
 line 07
 line 08
-line 09 old
+line 09 new
 line 10
 line 11
 line 12
 line 13
 line 14
 line 15
 line 16
 line 17
 line 18
+line 19 added
 line 20
@@ -40,2 +41,3 @@ fn far() {
 far context
-far old
+far new
+far extra
diff --git a/README.md b/README.md
index 5555555..6666666 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
-# Old Title
+# New Title
 intro line
diff --git a/src/new_module.rs b/src/new_module.rs
new file mode 100644
index 0000000..7777777
--- /dev/null
+++ b/src/new_module.rs
@@ -0,0 +1,3 @@
+pub fn hello() {}
+pub fn world() {}
+// brand new
diff --git a/old.txt b/old.txt
deleted file mode 100644
index 8888888..0000000
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-gone one
-gone two
diff --git a/assets/logo.png b/assets/logo.png
new file mode 100644
index 0000000..9999999
Binary files /dev/null and b/assets/logo.png differ
diff --git a/src/moved.rs b/src/relocated.rs
similarity index 100%
rename from src/moved.rs
rename to src/relocated.rs
"#;

    fn app() -> App {
        App::new(
            parse_diff(FIXTURE),
            Target::Local {
                base: "main".to_string(),
                head_sha: "deadbeefcafe".to_string(),
            },
        )
    }

    /// Build an annotation with byte-stable fields (fixed clock/id) for snapshots.
    fn note(
        id: &str,
        author: Author,
        file: &str,
        side: Side,
        line: u32,
        sev: Severity,
    ) -> Annotation {
        Annotation {
            id: id.to_string(),
            author,
            file: file.to_string(),
            line,
            side,
            severity: sev,
            tag: None,
            status: Status::Open,
            body: format!("body for {id}"),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        }
    }

    /// An app pre-loaded with annotations (no store path; reload is not exercised).
    fn annotated_app(annotations: Vec<Annotation>) -> App {
        let mut a = app();
        a.annotations = annotations;
        a
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ctrl(c: char) -> KeyEvent {
        KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
    }

    /// Render `app` into a fresh `TestBackend`, mirroring the real loop: draw
    /// once (which records the viewport heights `handle_key` relies on for
    /// paging/clamping), then for each key apply the transition and redraw.
    fn drive(app: &mut App, w: u16, h: u16, keys: &[KeyEvent]) -> Terminal<TestBackend> {
        let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
        term.draw(|f| render(f, app)).unwrap();
        for &k in keys {
            app.handle_key(k);
            term.draw(|f| render(f, app)).unwrap();
        }
        term
    }

    fn screen(term: &Terminal<TestBackend>) -> String {
        term.backend().to_string()
    }

    /// Style of the first cell (row-major) whose symbol equals `sym` and whose
    /// column is `>= min_x` (use `min_x >= 29` to stay inside the diff pane,
    /// past the 28-column tree and its border).
    fn style_of(term: &Terminal<TestBackend>, sym: &str, min_x: u16) -> Style {
        let buf = term.backend().buffer();
        for y in 0..buf.area.height {
            for x in min_x..buf.area.width {
                if let Some(c) = buf.cell((x, y)) {
                    if c.symbol() == sym {
                        return c.style();
                    }
                }
            }
        }
        panic!("no cell with symbol {sym:?} at column >= {min_x}");
    }

    fn any_cell_has_bg(term: &Terminal<TestBackend>, bg: Color) -> bool {
        let buf = term.backend().buffer();
        (0..buf.area.height).any(|y| {
            (0..buf.area.width).any(|x| buf.cell((x, y)).map(|c| c.style().bg) == Some(Some(bg)))
        })
    }

    /// Whether any cell in `rows` (inclusive y range) renders `sym` with `fg`.
    /// Used to find a gutter mark in the diff *body* while ignoring the same
    /// glyph in the pane title (y == 0) and the status bar (bottom row).
    fn has_fg_symbol(term: &Terminal<TestBackend>, sym: &str, fg: Color, rows: (u16, u16)) -> bool {
        let buf = term.backend().buffer();
        (rows.0..=rows.1).any(|y| {
            (0..buf.area.width).any(|x| {
                buf.cell((x, y))
                    .is_some_and(|c| c.symbol() == sym && c.style().fg == Some(fg))
            })
        })
    }

    // ---- snapshots: key -> state -> render ---------------------------------

    #[test]
    fn initial_view() {
        // Tree focused, first file (alpha.rs) open in the diff pane.
        let term = drive(&mut app(), 100, 24, &[]);
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn tree_selection_moves_to_added_file() {
        // j x3 walks the tree alpha -> long -> README -> new_module (an add).
        let mut a = app();
        let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('j')); 3]);
        assert_eq!(a.selected, 3);
        assert_eq!(a.current().display_path(), "src/new_module.rs");
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn diff_pane_focused_and_scrolled() {
        // Select the long file, focus the diff (l), scroll down three rows.
        let mut a = app();
        let keys = [
            key(KeyCode::Char('j')),
            key(KeyCode::Char('l')),
            key(KeyCode::Char('j')),
            key(KeyCode::Char('j')),
            key(KeyCode::Char('j')),
        ];
        let term = drive(&mut a, 100, 24, &keys);
        assert_eq!(a.focus, Focus::Diff);
        assert_eq!(a.scroll, 3);
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn status_bar_shows_bottom() {
        // G in the diff pane pins to the last row; status reads BOT.
        let mut a = app();
        let keys = [
            key(KeyCode::Char('j')),
            key(KeyCode::Char('l')),
            key(KeyCode::Char('G')),
        ];
        let term = drive(&mut a, 100, 24, &keys);
        assert_eq!(a.scroll, a.max_scroll());
        assert!(screen(&term).contains("BOT"));
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn help_overlay() {
        let term = drive(&mut app(), 100, 24, &[key(KeyCode::Char('?'))]);
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn binary_file_notice() {
        // j x5: alpha -> long -> README -> new_module -> old.txt -> logo.png.
        let mut a = app();
        let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('j')); 5]);
        assert_eq!(a.current().display_path(), "assets/logo.png");
        assert!(screen(&term).contains("Binary file"));
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn metadata_only_rename_notice() {
        // G jumps to the last file: a 100%-similarity rename with no hunks.
        let mut a = app();
        let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('G'))]);
        assert_eq!(a.current().display_path(), "src/relocated.rs");
        assert!(screen(&term).contains("metadata-only"));
        insta::assert_snapshot!(screen(&term));
    }

    // ---- behavior: the keymap and viewport math ----------------------------

    #[test]
    fn vim_jk_scrolls_diff_and_clamps_at_top() {
        let mut a = app();
        // Long file, diff focus.
        drive(
            &mut a,
            100,
            24,
            &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
        );
        for _ in 0..4 {
            a.handle_key(key(KeyCode::Char('j')));
        }
        assert_eq!(a.scroll, 4);
        for _ in 0..10 {
            a.handle_key(key(KeyCode::Char('k')));
        }
        assert_eq!(a.scroll, 0, "k clamps at the top");
    }

    #[test]
    fn hunk_navigation_jumps_between_hunks() {
        let mut a = app();
        drive(
            &mut a,
            100,
            24,
            &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
        );
        // Two hunks: the second starts well past the viewport, so `}` lands at
        // the clamped bottom; `{` returns to the first hunk at the top.
        a.handle_key(key(KeyCode::Char('}')));
        assert_eq!(a.scroll, a.max_scroll());
        a.handle_key(key(KeyCode::Char('{')));
        assert_eq!(a.scroll, 0);
        // n / N are aliases.
        a.handle_key(key(KeyCode::Char('n')));
        assert_eq!(a.scroll, a.max_scroll());
        a.handle_key(key(KeyCode::Char('N')));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn g_and_capital_g_jump_top_and_bottom() {
        let mut a = app();
        drive(
            &mut a,
            100,
            24,
            &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
        );
        a.handle_key(key(KeyCode::Char('G')));
        assert_eq!(a.scroll, a.max_scroll());
        a.handle_key(key(KeyCode::Char('g')));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn ctrl_d_and_u_half_page() {
        let mut a = app();
        drive(
            &mut a,
            100,
            24,
            &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
        );
        let half = a.diff_height / 2;
        a.handle_key(ctrl('d'));
        assert_eq!(a.scroll, half.min(a.max_scroll()));
        a.handle_key(ctrl('u'));
        assert_eq!(a.scroll, 0);
    }

    #[test]
    fn tab_toggles_focus() {
        let mut a = app();
        assert_eq!(a.focus, Focus::Tree);
        a.handle_key(key(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Diff);
        a.handle_key(key(KeyCode::Tab));
        assert_eq!(a.focus, Focus::Tree);
    }

    #[test]
    fn capital_j_k_switch_files_from_the_diff_pane() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('l'))); // focus diff
        assert_eq!(a.selected, 0);
        a.handle_key(key(KeyCode::Char('J')));
        assert_eq!(a.selected, 1);
        assert_eq!(a.focus, Focus::Diff, "stays in the diff pane");
        a.handle_key(key(KeyCode::Char('K')));
        assert_eq!(a.selected, 0);
    }

    #[test]
    fn quit_keys_signal_exit() {
        assert!(app().handle_key(key(KeyCode::Char('q'))), "q quits");
        assert!(app().handle_key(ctrl('c')), "Ctrl-c quits");
        assert!(
            !app().handle_key(key(KeyCode::Char('j'))),
            "ordinary keys do not"
        );
    }

    #[test]
    fn help_overlay_swallows_navigation() {
        let mut a = app();
        a.handle_key(key(KeyCode::Char('l'))); // focus diff
        a.handle_key(key(KeyCode::Char('?'))); // open help
        assert!(a.show_help);
        a.handle_key(key(KeyCode::Char('j'))); // swallowed
        assert_eq!(a.scroll, 0, "help eats navigation keys");
        a.handle_key(key(KeyCode::Esc)); // Esc closes it
        assert!(!a.show_help);
    }

    // ---- styles: the buffer carries colour + modifiers ---------------------

    #[test]
    fn diff_rows_are_styled_by_kind() {
        // alpha.rs is open: a cyan-bold hunk header, green additions, red
        // deletions. Restrict to the diff pane (x >= 29) so the tree's own
        // +N/-N counts don't satisfy the marker lookups.
        let term = drive(&mut app(), 100, 24, &[]);

        let hunk = style_of(&term, "@", 29);
        assert_eq!(hunk.fg, Some(Color::Cyan));
        assert!(hunk.add_modifier.contains(Modifier::BOLD));

        // The +/- *markers* sit in a fixed gutter column (~41), well past the
        // +N,M / -N,M ranges inside the cyan hunk header, so min_x = 40 picks
        // the addition/deletion lines rather than the header digits.
        assert_eq!(style_of(&term, "+", 40).fg, Some(Color::Green));
        assert_eq!(style_of(&term, "-", 40).fg, Some(Color::Red));
    }

    #[test]
    fn diff_content_is_syntax_highlighted() {
        // alpha.rs (Rust) is open at launch. The diff body otherwise only ever
        // uses *named* colours (cyan header, green/red markers, dark-gray line
        // numbers), so a truecolor `Rgb` foreground anywhere in the body can
        // only have come from syntect — proof the highlighter is wired through
        // rendering, not just unit-tested in isolation.
        let term = drive(&mut app(), 100, 24, &[]);
        let buf = term.backend().buffer();
        // Body rows only: 0 is the pane title, the last row is the status bar.
        let highlighted = (1..23).any(|y| {
            (29..buf.area.width).any(|x| {
                buf.cell((x, y))
                    .is_some_and(|c| matches!(c.style().fg, Some(Color::Rgb(..))))
            })
        });
        assert!(highlighted, "expected syntect truecolor in the diff body");
    }

    #[test]
    fn selection_and_status_bar_have_background_fills() {
        let term = drive(&mut app(), 100, 24, &[]);
        assert!(
            any_cell_has_bg(&term, Color::Rgb(40, 44, 52)),
            "selected tree row is highlighted"
        );
        assert!(
            any_cell_has_bg(&term, Color::Rgb(30, 33, 40)),
            "status bar has its background fill"
        );
    }

    // ---- annotations: gutter marks, panel, and the lookup map --------------

    /// Two annotations on alpha.rs (the file open at launch): a blocker on the
    /// `let x = 2;` addition (RIGHT line 2) and an info on a context line.
    fn alpha_notes() -> Vec<Annotation> {
        vec![
            note(
                "blk00001",
                Author::Agent,
                "src/alpha.rs",
                Side::Right,
                2,
                Severity::Blocker,
            ),
            note(
                "inf00002",
                Author::Human,
                "src/alpha.rs",
                Side::Right,
                4,
                Severity::Info,
            ),
        ]
    }

    #[test]
    fn gutter_marks_render_on_annotated_lines() {
        let term = drive(&mut annotated_app(alpha_notes()), 100, 24, &[]);
        // A severity-coloured mark appears in the diff body (rows 1..22, between
        // the pane title and the status bar). The blocker is red; the info, blue.
        assert!(has_fg_symbol(&term, MARK, Color::Red, (1, 22)));
        assert!(has_fg_symbol(&term, MARK, Color::Blue, (1, 22)));
        insta::assert_snapshot!(screen(&term));
    }

    #[test]
    fn unannotated_file_has_no_gutter_column() {
        // README.md (file index 2) carries no annotations, so its diff body has
        // no reserved mark column — even while other files do.
        let mut a = annotated_app(alpha_notes());
        let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('j')); 2]);
        assert_eq!(a.current().display_path(), "README.md");
        assert!(a.line_marks().is_empty());
        // No mark in the diff body; the status bar's total-count glyph (bottom
        // row) is expected and excluded by the row range.
        assert!(!has_fg_symbol(&term, MARK, Color::Red, (1, 22)));
        assert!(!has_fg_symbol(&term, MARK, Color::Blue, (1, 22)));
    }

    #[test]
    fn annotations_panel_lists_current_file() {
        // `a` toggles the panel; it lists alpha.rs's two annotations.
        let mut a = annotated_app(alpha_notes());
        let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('a'))]);
        assert!(a.show_panel);
        let text = screen(&term);
        assert!(text.contains("Annotations"));
        assert!(text.contains("L2 agent"));
        assert!(text.contains("L4 human"));
        insta::assert_snapshot!(text);
    }

    #[test]
    fn status_bar_shows_annotation_count() {
        let term = drive(&mut annotated_app(alpha_notes()), 100, 24, &[]);
        // Two annotations, both open.
        assert!(screen(&term).contains("2 (2 open)"));
    }

    #[test]
    fn line_marks_keep_the_most_severe_per_line() {
        // A warning and a blocker collide on the same (side, line): blocker wins.
        let a = annotated_app(vec![
            note(
                "w0000001",
                Author::Agent,
                "src/alpha.rs",
                Side::Right,
                2,
                Severity::Warning,
            ),
            note(
                "b0000002",
                Author::Agent,
                "src/alpha.rs",
                Side::Right,
                2,
                Severity::Blocker,
            ),
        ]);
        let marks = a.line_marks();
        assert_eq!(marks.get(&(Side::Right, 2)), Some(&Severity::Blocker));
    }

    #[test]
    fn current_file_annotations_filter_by_path() {
        let a = annotated_app(vec![
            note(
                "a0000001",
                Author::Agent,
                "src/alpha.rs",
                Side::Right,
                2,
                Severity::Info,
            ),
            note(
                "o0000002",
                Author::Agent,
                "README.md",
                Side::Right,
                1,
                Severity::Info,
            ),
        ]);
        // alpha.rs is selected at launch.
        let here = a.current_file_annotations();
        assert_eq!(here.len(), 1);
        assert_eq!(here[0].id, "a0000001");
    }

    // ---- turn protocol: the human's release (PLAN.md §6) -------------------

    #[test]
    fn r_releases_the_turn_back_to_the_agent() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotations.json");
        let target = Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        };

        // Seed a store where an agent is blocked waiting on the human at seq 3.
        let mut seed = StateFile::new(target.clone());
        seed.turn.agent_waiting = true;
        seed.turn.owner = Author::Human;
        seed.turn.seq = 3;
        store::save(&path, &seed).unwrap();

        let mut a = App::new(parse_diff(FIXTURE), target);
        a.attach_store(path.clone(), store::load(&path).unwrap());
        assert!(a.turn.agent_waiting, "attach loads the turn block");

        a.handle_key(key(KeyCode::Char('r')));

        // In memory: ownership handed back, waiting cleared.
        assert_eq!(a.turn.owner, Author::Agent);
        assert!(!a.turn.agent_waiting);
        // On disk: seq bumped past what the waiter recorded, and first-contact
        // approval is now set — this write is what unblocks `agent wait`.
        let saved = store::load(&path).unwrap().unwrap();
        assert_eq!(saved.turn.seq, 4);
        assert_eq!(saved.turn.owner, Author::Agent);
        assert!(!saved.turn.agent_waiting);
        assert!(saved.turn.approved);
    }

    #[test]
    fn r_without_a_store_is_a_harmless_noop() {
        // No store attached (resolution failed / store-less view): `r` must not
        // panic and leaves the default turn untouched.
        let mut a = app();
        a.handle_key(key(KeyCode::Char('r')));
        assert_eq!(a.turn, Turn::default());
    }

    #[test]
    fn status_bar_surfaces_a_waiting_agent() {
        let mut a = app();
        a.turn.agent_waiting = true;
        let term = drive(&mut a, 100, 24, &[]);
        assert!(
            screen(&term).contains("agent waiting"),
            "status bar should advertise the waiting agent:\n{}",
            screen(&term)
        );
    }

    // ---- live reload: the data path a notify tick drives (PLAN.md §9) -------

    #[test]
    fn reload_picks_up_another_processs_writes() {
        // The notify watch only decides *when* to reload; `reload` does the work
        // of re-reading the store, so drive it directly to prove a TUI started on
        // an empty store sees an agent's later annotation and turn change.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotations.json");
        let target = Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        };

        let mut a = App::new(parse_diff(FIXTURE), target.clone());
        a.attach_store(path.clone(), store::load(&path).unwrap());
        assert!(a.annotations.is_empty(), "starts with an empty store");

        // Another process (the agent) writes a comment and takes the turn.
        store::update(&path, &target, |s| {
            s.annotations.push(note(
                "agent001",
                Author::Agent,
                "src/alpha.rs",
                Side::Right,
                2,
                Severity::Warning,
            ));
            s.turn.agent_waiting = true;
        })
        .unwrap();

        a.reload();
        assert_eq!(a.annotations.len(), 1, "reload picks up the new annotation");
        assert_eq!(a.annotations[0].id, "agent001");
        assert!(a.turn.agent_waiting, "and the refreshed turn state");
    }

    #[test]
    fn reload_without_a_store_is_a_harmless_noop() {
        // Store-less view (resolution failed): reload must not panic and leaves
        // the empty defaults in place.
        let mut a = app();
        a.reload();
        assert!(a.annotations.is_empty());
        assert_eq!(a.turn, Turn::default());
    }
}
