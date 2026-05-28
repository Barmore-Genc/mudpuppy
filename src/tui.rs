//! The ratatui diff viewer (PLAN.md §9), milestone 1: a read-only, keyboard-
//! driven browser over the parsed diff. File tree on the left, diff pane in the
//! center, status bar along the bottom, and a `?` help overlay.
//!
//! Rendering is **virtualized**: only the rows currently in the viewport are
//! turned into styled spans, and a file's hunks are parsed lazily the first
//! time it is opened (and cached), so a 50k-line diff never gets materialized in
//! full. Annotations, syntax highlighting, and live reload arrive in later
//! milestones; this module deliberately neither reads nor writes the store.

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::{DefaultTerminal, Frame};

use crate::diff::{parse_diff, DiffLine, FileDiff, FileStatus, LineKind};
use crate::domain::Target;
use crate::source;

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

    let mut app = App::new(files, loaded.target);

    // `ratatui::init` enters the alternate screen, turns on raw mode, and
    // installs a panic hook that restores the terminal; `restore` undoes it.
    let mut terminal = ratatui::init();
    let result = run_loop(&mut terminal, &mut app);
    ratatui::restore();
    result
}

/// The draw → read-event → handle loop. Returns when the user quits.
fn run_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    loop {
        terminal.draw(|frame| render(frame, app))?;
        // Only act on key *presses*; on Windows crossterm also emits release and
        // repeat events that would otherwise double every keystroke.
        if let Event::Key(key) = event::read()? {
            if key.kind == KeyEventKind::Press && app.handle_key(key) {
                return Ok(());
            }
        }
    }
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
    /// A content line (context / addition / deletion).
    Line(DiffLine),
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
            for line in hunk.lines {
                rows.push(Row::Line(line));
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
            diff_height: 1,
            tree_height: 1,
        }
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
    let [tree, diff] =
        Layout::horizontal([Constraint::Percentage(28), Constraint::Min(0)]).areas(main);

    // Inner heights (minus the one-row borders top and bottom) drive paging and
    // keep the tree selection visible.
    app.tree_height = tree.height.saturating_sub(2) as usize;
    app.diff_height = diff.height.saturating_sub(2) as usize;
    app.scroll = app.scroll.min(app.max_scroll());

    render_tree(frame, tree, app);
    render_diff(frame, diff, app);
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
fn render_diff(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Diff;
    let file = app.current();
    let height = app.diff_height.max(1);
    let end = (app.scroll + height).min(app.view.rows.len());

    let lines: Vec<Line> = app.view.rows[app.scroll..end]
        .iter()
        .map(row_to_line)
        .collect();

    let title = format!(
        " {} [{}] {}/{} ",
        file.display_path(),
        status_word(&file.status),
        app.selected + 1,
        app.files.len()
    );
    frame.render_widget(Paragraph::new(lines).block(bordered(&title, focused)), area);
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

    let left = format!(
        " {}  ·  file {}/{}  ·  +{} -{}  ·  [{}] {}",
        target_desc(&app.target),
        app.selected + 1,
        app.files.len(),
        file.additions,
        file.deletions,
        focus,
        scroll_pct,
    );

    let line = Line::from(vec![
        Span::raw(left),
        Span::raw("  "),
        Span::styled("?", Style::default().fg(Color::Yellow)),
        Span::raw(" help  "),
        Span::styled("q", Style::default().fg(Color::Yellow)),
        Span::raw(" quit"),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(Style::default().bg(Color::Rgb(30, 33, 40)).fg(Color::Gray)),
        area,
    );
}

/// The centered help overlay listing every keybinding.
fn render_help(frame: &mut Frame, area: Rect) {
    let text = vec![
        Line::from(Span::styled(
            "mudpuppy — diff viewer",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(Span::styled("Navigation", Style::default().fg(Color::Cyan))),
        Line::raw("  j / k        down / up (scroll diff, move file selection)"),
        Line::raw("  Ctrl-d / -u  half page down / up"),
        Line::raw("  Ctrl-f / -b  full page down / up"),
        Line::raw("  g / G        top / bottom (or first / last file)"),
        Line::raw("  } / {        next / prev hunk   (also n / N)"),
        Line::raw("  J / K        next / prev file (from the diff pane)"),
        Line::raw(""),
        Line::from(Span::styled("Focus", Style::default().fg(Color::Cyan))),
        Line::raw("  Tab          switch between file tree and diff"),
        Line::raw("  l / Enter    file tree → diff"),
        Line::raw("  h            diff → file tree"),
        Line::raw(""),
        Line::from(Span::styled("Other", Style::default().fg(Color::Cyan))),
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

/// Turn one diff row into a styled line.
fn row_to_line(row: &Row) -> Line<'static> {
    match row {
        Row::Hunk(text) => Line::from(Span::styled(
            text.clone(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Row::Notice(text) => Line::from(Span::styled(
            text.clone(),
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        )),
        Row::Line(line) => diff_line(line),
    }
}

/// Format a content line: a `old new` gutter, a `+`/`-`/space marker, then the
/// text, coloured by kind.
fn diff_line(line: &DiffLine) -> Line<'static> {
    let (marker, color) = match line.kind {
        LineKind::Addition => ('+', Color::Green),
        LineKind::Deletion => ('-', Color::Red),
        LineKind::Context => (' ', Color::Reset),
    };

    let old = line.old_lineno.map(|n| n.to_string()).unwrap_or_default();
    let new = line.new_lineno.map(|n| n.to_string()).unwrap_or_default();
    let gutter = format!("{old:>5} {new:>5} ");

    // Tabs render unpredictably across terminals; expand to a 4-space stop.
    let content = line.content.replace('\t', "    ");

    let text_style = match line.kind {
        LineKind::Context => Style::default(),
        _ => Style::default().fg(color),
    };

    Line::from(vec![
        Span::styled(gutter, Style::default().fg(Color::DarkGray)),
        Span::styled(format!("{marker} "), Style::default().fg(color)),
        Span::styled(content, text_style),
    ])
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
