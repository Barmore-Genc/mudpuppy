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
}
