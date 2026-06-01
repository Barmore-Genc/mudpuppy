//! Drawing the viewer: the per-frame `render` entry point, one function per pane
//! (file tree, diff, annotations panel, status bar, help overlay, approval
//! banner), the row → styled-line conversion, and the small style/format
//! helpers (status markers, severity colours, the bordered block, centering).
//!
//! Everything here reads `&App` (or `&mut App`, to record viewport heights) and
//! writes into a ratatui `Frame`; it holds no state of its own.

use std::collections::HashMap;

use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use super::app::{App, Focus, Row};
use super::composer::Composer;
use crate::diff::{DiffLine, FileStatus, LineKind};
use crate::domain::{Author, Severity, Side, Status, Tag, Target};
use crate::highlight::HlLine;
use crate::picker::{fuzzy_match, Picker};

/// The glyph drawn in the annotation gutter column on an annotated line.
pub(crate) const MARK: &str = "●";

/// Draw the whole UI for one frame and record viewport heights for the next
/// key-handling pass.
pub(crate) fn render(frame: &mut Frame, app: &mut App) {
    // On first contact an approval banner claims the top row. It only appears
    // until the human approves, so an established session lays out exactly as
    // before and its snapshots are unchanged.
    let body = if app.awaiting_approval() {
        let [banner, body] =
            Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(frame.area());
        render_approval_banner(frame, banner);
        body
    } else {
        frame.area()
    };

    let [main, status] = Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(body);

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
    if let Some(picker) = app.picker.as_ref() {
        let area = frame.area();
        render_picker(frame, area, picker);
    }
    if let Some(composer) = app.composer.as_ref() {
        let area = frame.area();
        render_composer(frame, area, composer);
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
    let height = app.diff_height.max(1);

    // File-scoped notes have no gutter line, so they ride a header row at the top
    // of the pane. It consumes one viewport row, so the diff window shrinks to fit.
    let file_level = app.file_level_annotations();
    let mut header: Vec<Line> = Vec::new();
    if !file_level.is_empty() {
        let sev = file_level
            .iter()
            .map(|a| a.severity)
            .max()
            .unwrap_or(Severity::Info);
        header.push(Line::from(Span::styled(
            format!("▌ file-level: {}", file_level.len()),
            Style::default()
                .fg(severity_color(sev))
                .add_modifier(Modifier::BOLD),
        )));
    }

    let avail = height.saturating_sub(header.len()).max(1);
    let end = (app.scroll + avail).min(app.view.rows.len());
    let selection = app.selection_span();

    let mut lines: Vec<Line> = header;
    for (offset, row) in app.view.rows[app.scroll..end].iter().enumerate() {
        let idx = app.scroll + offset;
        let mut line = row_to_line(row, gutter, &marks);
        // Selection span first, then the cursor row on top, so the cursor stays
        // distinct inside a highlighted region.
        if selection.is_some_and(|(lo, hi)| lo <= idx && idx <= hi) {
            line = line.style(Style::default().bg(Color::Rgb(48, 54, 78)));
        }
        if focused && idx == app.cursor {
            line = line.style(Style::default().bg(Color::Rgb(60, 66, 84)));
        }
        lines.push(line);
    }

    let file = app.current();
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
        let cursor_id = app.annotation_id_at_cursor();
        for a in &here {
            let indent = if a.is_reply() { "  " } else { "" };
            let tag = a
                .tag
                .map(|t| format!(" {}", tag_symbol(t)))
                .unwrap_or_default();
            let header = format!(
                "{indent}{MARK} {} {}{}  {}",
                panel_anchor(a),
                author_word(a.author),
                tag,
                ann_status(a.status),
            );
            // Highlight the annotation anchored to the cursor line so the panel
            // tracks what reply/edit/delete/status will act on.
            let mut style = Style::default().fg(severity_color(a.severity));
            if cursor_id.as_deref() == Some(a.id.as_str()) {
                style = style
                    .bg(Color::Rgb(48, 54, 78))
                    .add_modifier(Modifier::BOLD);
            }
            lines.push(Line::from(vec![Span::styled(header, style)]));
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

/// The first-contact approval banner (PLAN.md §6): a full-width highlighted row
/// telling the human an agent wants to collaborate and that releasing the turn
/// (`r`) approves it. Shown only while [`App::awaiting_approval`] holds.
fn render_approval_banner(frame: &mut Frame, area: Rect) {
    let text = " An agent wants to collaborate on this review — press r to approve and hand it the first turn ";
    let banner = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default()
            .bg(Color::Green)
            .fg(Color::Black)
            .add_modifier(Modifier::BOLD),
    )))
    .style(Style::default().bg(Color::Green));
    frame.render_widget(banner, area);
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

    // Visual-mode selection size and a pending delete confirmation, so the human
    // always sees the mode they're in and what a `y` will remove.
    if let Some((lo, hi)) = app.selection_span() {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" VISUAL {} lines ", hi - lo + 1),
            Style::default()
                .bg(Color::Magenta)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ));
    }
    if let Some(id) = &app.pending_delete {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" delete {id}? y/n "),
            Style::default()
                .bg(Color::Red)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    }
    // A transient authoring hint (e.g. "no diff line under the cursor").
    if let Some(notice) = &app.notice {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" {notice} "),
            Style::default().fg(Color::Black).bg(Color::Yellow),
        ));
    }

    // When an agent is blocked in `agent wait`, make it impossible to miss and
    // advertise the release key (PLAN.md §6). Before first-contact approval the
    // same `r` press approves, so the hint says so (the banner spells it out).
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
        let action = if app.turn.approved {
            " release  "
        } else {
            " approve  "
        };
        spans.push(Span::raw(action));
    } else {
        spans.push(Span::raw("  "));
    }
    // A scripting message (a Lua `print` or a config error) surfaces here; the
    // alternate screen has no usable stdout. Absent by default, so an
    // unconfigured session reads exactly as before.
    if let Some(msg) = &app.status_msg {
        spans.push(Span::styled(
            format!(" {msg} "),
            Style::default().fg(Color::Black).bg(Color::Cyan),
        ));
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

/// Modal overlay for the "add any file" picker: a query input line atop a
/// scrollable, fuzzy-ranked result list. Matched characters and the cursor row
/// are highlighted; the list scrolls to keep the selection visible.
fn render_picker(frame: &mut Frame, area: Rect, picker: &Picker) {
    let area = centered_rect(70, (area.height * 7 / 10).max(6), area);
    let block = bordered(" Add file ", true);
    let inner = block.inner(area);
    // The first inner row is the query input; the rest is the result list.
    let list_height = inner.height.saturating_sub(1) as usize;
    // Scroll so the selected row stays visible at the bottom edge.
    let offset = picker
        .selected
        .saturating_sub(list_height.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    lines.push(Line::from(vec![
        Span::styled("> ", Style::default().fg(Color::Yellow)),
        Span::raw(picker.query.clone()),
    ]));
    for (row, &cand) in picker
        .filtered
        .iter()
        .enumerate()
        .skip(offset)
        .take(list_height)
    {
        let path = &picker.all[cand];
        let positions = fuzzy_match(&picker.query, path)
            .map(|m| m.positions)
            .unwrap_or_default();
        lines.push(picker_row(path, &positions, row == picker.selected));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
}

/// One picker result row: bold the fuzzy-matched characters; highlight the whole
/// row when it is the cursor row.
fn picker_row(path: &str, positions: &[usize], selected: bool) -> Line<'static> {
    let base = if selected {
        Style::default().fg(Color::Black).bg(Color::Cyan)
    } else {
        Style::default()
    };
    let matched = base
        .fg(if selected {
            Color::Black
        } else {
            Color::Yellow
        })
        .add_modifier(Modifier::BOLD);
    let spans = path
        .char_indices()
        .map(|(byte, ch)| {
            let style = if positions.contains(&byte) {
                matched
            } else {
                base
            };
            Span::styled(ch.to_string(), style)
        })
        .collect::<Vec<_>>();
    Line::from(spans)
}

/// The modal comment composer: the target anchor, severity + tag chips, the
/// body with a visible caret, and a key-hint footer.
fn render_composer(frame: &mut Frame, area: Rect, composer: &Composer) {
    let area = centered_rect(72, (area.height * 6 / 10).max(8), area);
    let block = bordered(" Comment ", true);

    let mut lines: Vec<Line> = Vec::new();
    lines.push(Line::from(vec![
        Span::styled("on ", Style::default().fg(Color::DarkGray)),
        Span::raw(composer.file.clone()),
        Span::raw("  "),
        Span::styled(
            composer.anchor_label(),
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
    ]));

    // Severity and tag chips.
    let tag_label = match composer.tag {
        Some(t) => format!("tag {}", tag_symbol(t)),
        None => "no tag".to_string(),
    };
    lines.push(Line::from(vec![
        Span::styled(
            format!(" {} ", severity_word(composer.severity)),
            Style::default()
                .bg(severity_color(composer.severity))
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(tag_label, Style::default().fg(Color::Gray)),
    ]));
    lines.push(Line::raw(""));

    // Body, with a block caret on the last line so the insertion point is clear.
    let body_lines: Vec<&str> = composer.body.split('\n').collect();
    let last = body_lines.len() - 1;
    for (i, text) in body_lines.iter().enumerate() {
        let mut spans = vec![Span::raw((*text).to_string())];
        if i == last {
            spans.push(Span::styled("█", Style::default().fg(Color::Cyan)));
        }
        lines.push(Line::from(spans));
    }

    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "Ctrl-S save  ·  Esc cancel  ·  Ctrl-E severity  ·  Ctrl-T tag  ·  Enter newline",
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
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
        Line::raw("  j / k        move cursor (diff) / selection (tree)"),
        Line::raw("  Ctrl-d/u/f/b half / full page down / up"),
        Line::raw("  g / G        top / bottom (cursor in diff, first/last file)"),
        Line::raw("  } / {        next / prev hunk   (also n / N)"),
        Line::raw("  J / K        next / prev file (from the diff pane)"),
        section("Focus"),
        Line::raw("  Tab / l / h  toggle · tree → diff · diff → tree"),
        section("Annotations"),
        Line::raw("  a            toggle the annotations panel"),
        Line::raw("  v / V  Esc   whole-line selection (diff) · clear"),
        Line::raw("  c / F        comment line/selection · whole file"),
        Line::raw("  R / e / D    reply · edit · delete (D confirms with y)"),
        Line::raw("  s            cycle status (open → resolved → wontfix)"),
        section("More"),
        Line::raw("  r            release the turn; first release approves"),
        Line::raw("  Ctrl-p       add any file (fuzzy picker)"),
        Line::raw("  ? q Ctrl-c   help · quit"),
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
        Row::Expander {
            new,
            can_up,
            can_down,
            ..
        } => {
            let arrows = match (can_up, can_down) {
                (true, true) => "↕",
                (true, false) => "↑",
                (false, true) => "↓",
                (false, false) => "⋯",
            };
            let hidden = new.end - new.start;
            Line::from(Span::styled(
                format!("{pad}  {arrows} {hidden} hidden lines — show more"),
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ))
        }
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
        FileStatus::Unchanged => ('·', Color::DarkGray),
    }
}

/// Human-readable status word for the diff-pane title.
fn status_word(status: &FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "added",
        FileStatus::Deleted => "deleted",
        FileStatus::Modified => "modified",
        FileStatus::Renamed => "renamed",
        FileStatus::Unchanged => "unchanged",
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

/// Single-word severity label (composer chip).
fn severity_word(severity: Severity) -> &'static str {
    match severity {
        Severity::Info => "info",
        Severity::Suggestion => "suggestion",
        Severity::Warning => "warning",
        Severity::Blocker => "blocker",
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

/// The panel's anchor label for an annotation: `(whole file)`, `L42–50`, or
/// `L42`.
fn panel_anchor(a: &crate::domain::Annotation) -> String {
    use crate::domain::AnchorScope;
    if a.scope == AnchorScope::File {
        return "(whole file)".to_string();
    }
    match a.end_line {
        Some(end) if end != a.line => format!("L{}–{}", a.line, end),
        _ => format!("L{}", a.line),
    }
}

/// The one-character tag symbol shown in the panel.
fn tag_symbol(tag: Tag) -> &'static str {
    match tag {
        Tag::Question => "?",
        Tag::Concern => "!",
        Tag::Direction => ">",
    }
}

/// Short description of what's under review, for the status bar.
pub(crate) fn target_desc(target: &Target) -> String {
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
