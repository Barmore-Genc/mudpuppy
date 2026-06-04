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

use super::app::{App, Focus, Hits, Row, Sidebar};
use super::composer::{Composer, Mode};
use crate::command::CommandPalette;
use crate::diff::{DiffLine, FileStatus, LineKind};
use crate::domain::{Author, Severity, Side, Status, Tag, Target};
use crate::highlight::HlLine;
use crate::picker::{fuzzy_match, Picker};

/// The glyph drawn in the annotation gutter column on an annotated line.
pub(crate) const MARK: &str = "●";

/// Draw the whole UI for one frame and record viewport heights for the next
/// key-handling pass.
pub(crate) fn render(frame: &mut Frame, app: &mut App) {
    // Reset last frame's interactive regions; each render path repopulates
    // anything it draws (issue #29 mouse support). Overlays leave their span
    // fields `None` when closed, so handlers know the affordance isn't on
    // screen.
    app.hits = Hits::default();
    // On first contact an approval banner claims the top row. It only appears
    // until the user approves, so an established session lays out exactly as
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

    // The left sidebar hosts both tabs. The annotations list needs more room to
    // read than the file tree, so it claims a wider column when it is showing.
    let sidebar_pct = match app.sidebar {
        Sidebar::Files => 28,
        Sidebar::Annotations => 40,
    };
    let [side, diff] =
        Layout::horizontal([Constraint::Percentage(sidebar_pct), Constraint::Min(0)]).areas(main);

    // Inner heights (minus the one-row borders top and bottom) drive paging and
    // keep the tree/list selection visible.
    app.tree_height = side.height.saturating_sub(2) as usize;
    app.diff_height = diff.height.saturating_sub(2) as usize;
    app.scroll = app.scroll.min(app.max_scroll());

    app.hits.sidebar_outer = Some(side);
    app.hits.sidebar_inner = Some(inner_rect(side));
    app.hits.diff_outer = Some(diff);
    app.hits.diff_inner = Some(inner_rect(diff));
    app.hits.status = Some(status);

    match app.sidebar {
        Sidebar::Files => render_tree(frame, side, app),
        Sidebar::Annotations => render_annotations(frame, side, app),
    }
    render_diff(frame, diff, app);
    render_status(frame, status, app);

    if app.show_help {
        render_help(frame, frame.area());
    }
    if let Some(picker) = app.picker.as_ref() {
        let area = frame.area();
        let rect = render_picker(frame, area, picker);
        app.hits.picker_outer = Some(rect);
    }
    if let Some(palette) = app.palette.as_ref() {
        let area = frame.area();
        let rect = render_palette(frame, area, palette);
        app.hits.palette_outer = Some(rect);
    }
    if let Some(composer) = app.composer.as_ref() {
        let area = frame.area();
        let (rect, save, cancel) = render_composer(frame, area, composer);
        app.hits.composer_outer = Some(rect);
        app.hits.composer_save_span = Some(save);
        app.hits.composer_cancel_span = Some(cancel);
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
    // Click-y mapping needs to know how many non-row header lines come first.
    app.hits.diff_header_rows = header.len() as u16;

    let mut lines: Vec<Line> = header;
    for (offset, row) in app.view.rows[app.scroll..end].iter().enumerate() {
        let idx = app.scroll + offset;
        let mut line = row_to_line(row, gutter, &marks);
        // Selection span first, then the cursor row on top, so the cursor stays
        // distinct inside a highlighted region.
        if selection.is_some_and(|(lo, hi)| lo <= idx && idx <= hi) {
            line = line.style(Style::default().bg(Color::Rgb(48, 54, 78)));
        }
        // Highlight the cursor row when the diff is focused, or when the
        // annotations tab is driving it (so the previewed line is visible even
        // though focus stays on the list).
        if (focused || app.sidebar == Sidebar::Annotations) && idx == app.cursor {
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

/// The annotations sidebar tab: every annotation in the store across all files,
/// grouped under a bold file header, each row a severity-coloured mark with its
/// anchor, author, optional tag, status, and a one-line body preview. Threaded
/// replies are indented under their parent. The selected row is highlighted and
/// the list scrolls to keep it visible. Replaces the file tree when toggled on.
fn render_annotations(frame: &mut Frame, area: Rect, app: &mut App) {
    let focused = app.focus == Focus::Tree;
    let height = (area.height.saturating_sub(2) as usize).max(1);
    let list = app.annotation_list();
    let total = list.len();

    if total == 0 {
        app.annotation_scroll = 0;
        let line = Line::from(Span::styled(
            "No annotations yet.",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
        frame.render_widget(
            Paragraph::new(line).block(bordered(" Annotations (0) ", focused)),
            area,
        );
        return;
    }

    let selected = app.annotation_selected.min(total - 1);
    let sel_bg = Color::Rgb(48, 54, 78);

    // Build the flat line list: a dim bold header before each new file's run,
    // then a header + preview pair per annotation. `block_start[i]` records where
    // annotation `i` begins so the selection can be scrolled into view.
    let mut lines: Vec<Line> = Vec::new();
    let mut block_start: Vec<usize> = Vec::with_capacity(total);
    let mut current_file: Option<&str> = None;
    for (i, a) in list.iter().enumerate() {
        if current_file != Some(a.file.as_str()) {
            current_file = Some(a.file.as_str());
            if !lines.is_empty() {
                lines.push(Line::raw(""));
            }
            lines.push(Line::from(Span::styled(
                a.file.clone(),
                Style::default()
                    .fg(Color::White)
                    .add_modifier(Modifier::BOLD),
            )));
        }

        block_start.push(lines.len());
        let sel = i == selected;
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
        let mut header_style = Style::default().fg(severity_color(a.severity));
        if sel {
            header_style = header_style.bg(sel_bg).add_modifier(Modifier::BOLD);
        }
        lines.push(Line::from(Span::styled(header, header_style)));

        // First body line as a preview; the list stays scannable.
        let preview = a.body.lines().next().unwrap_or_default();
        let mut preview_style = Style::default().fg(Color::Gray);
        if sel {
            preview_style = preview_style.bg(sel_bg);
        }
        lines.push(Line::from(Span::styled(
            format!("{indent}  {preview}"),
            preview_style,
        )));
    }

    // Keep the two-line selected block within the viewport.
    let start = block_start[selected];
    let end = start + 2;
    let mut scroll = app.annotation_scroll;
    if start < scroll {
        scroll = start;
    } else if end > scroll + height {
        scroll = end - height;
    }
    scroll = scroll.min(lines.len().saturating_sub(height));
    app.annotation_scroll = scroll;
    // Record the per-annotation start lines (in unscrolled coordinates) and the
    // scroll offset so a click can map y → annotation index.
    app.hits.annotation_block_starts = block_start;
    app.hits.annotation_scroll = scroll;

    let visible = lines.split_off(scroll);
    let visible: Vec<Line> = visible.into_iter().take(height).collect();

    let title = format!(" Annotations ({total}) ");
    frame.render_widget(
        Paragraph::new(visible).block(bordered(&title, focused)),
        area,
    );
}

/// The first-contact approval banner (PLAN.md §6): a full-width highlighted row
/// telling the user an agent wants to collaborate and that releasing the turn
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
fn render_status(frame: &mut Frame, area: Rect, app: &mut App) {
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

    // A pending count and/or partial key sequence (`5`, `g`, `Space c`), so the
    // user sees a multi-key binding building up the way vim's command line does.
    if let Some(hint) = pending_hint(app) {
        spans.push(Span::raw("  "));
        spans.push(Span::styled(
            format!(" {hint} "),
            Style::default()
                .bg(Color::Blue)
                .fg(Color::White)
                .add_modifier(Modifier::BOLD),
        ));
    }

    // Visual-mode selection size and a pending delete confirmation, so the user
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
        // Record the clickable span over "r release"/"r approve" so a mouse
        // click on the call-to-action triggers release_turn (issue #29).
        let x_start = area.x + span_total_width(&spans).min(area.width);
        spans.push(Span::styled("r", Style::default().fg(Color::Yellow)));
        let action = if app.turn.approved {
            " release  "
        } else {
            " approve  "
        };
        spans.push(Span::raw(action));
        let label_chars = 1 + action.trim().chars().count();
        let x_end = (x_start + label_chars as u16).min(area.x + area.width);
        app.hits.release_span = Some((area.y, x_start, x_end));
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
fn render_picker(frame: &mut Frame, area: Rect, picker: &Picker) -> Rect {
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
    area
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

/// Modal overlay for the `:command` palette: a `:`-prefixed query line atop a
/// fuzzy-ranked list of command names. Mirrors [`render_picker`]; matched
/// characters and the cursor row are highlighted, and the list scrolls to keep
/// the selection visible.
fn render_palette(frame: &mut Frame, area: Rect, palette: &CommandPalette) -> Rect {
    let area = centered_rect(60, (area.height * 6 / 10).max(6), area);
    let block = bordered(" Command ", true);
    let inner = block.inner(area);
    let list_height = inner.height.saturating_sub(1) as usize;
    let offset = palette
        .selected
        .saturating_sub(list_height.saturating_sub(1));

    let mut lines: Vec<Line> = Vec::with_capacity(inner.height as usize);
    lines.push(Line::from(vec![
        Span::styled(":", Style::default().fg(Color::Yellow)),
        Span::raw(palette.query.clone()),
    ]));
    for (row, &cand) in palette
        .filtered
        .iter()
        .enumerate()
        .skip(offset)
        .take(list_height)
    {
        let name = &palette.all[cand];
        let positions = fuzzy_match(&palette.query, name)
            .map(|m| m.positions)
            .unwrap_or_default();
        lines.push(picker_row(name, &positions, row == palette.selected));
    }

    frame.render_widget(Clear, area);
    frame.render_widget(Paragraph::new(lines).block(block), area);
    area
}

/// The modal comment composer: the target anchor, severity + tag chips, the
/// body with a visible caret, and a key-hint footer.
fn render_composer(
    frame: &mut Frame,
    area: Rect,
    composer: &Composer,
) -> (Rect, super::app::Hitspan, super::app::Hitspan) {
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
    // Mode indicator, vim-style.
    let (mode_label, mode_color) = match composer.mode {
        Mode::Insert => ("-- INSERT --", Color::Green),
        Mode::Normal => ("-- NORMAL --", Color::Cyan),
    };
    lines.push(Line::from(Span::styled(
        mode_label,
        Style::default().fg(mode_color).add_modifier(Modifier::BOLD),
    )));

    // Body, with a caret on the cursor cell.
    let caret = Style::default().bg(Color::Cyan).fg(Color::Black);
    for (i, text) in composer.lines.iter().enumerate() {
        if i != composer.row {
            lines.push(Line::raw(text.clone()));
            continue;
        }
        let chars: Vec<char> = text.chars().collect();
        let before: String = chars[..composer.col.min(chars.len())].iter().collect();
        let after: String = chars
            .get(composer.col + 1..)
            .map(|s| s.iter().collect())
            .unwrap_or_default();
        let at = match chars.get(composer.col) {
            // Reverse-video the character under the cursor.
            Some(c) => Span::styled(c.to_string(), caret),
            // Past the end of the line: a block glyph, *not* a reverse-video
            // space — a whitespace-only line renders as two visual rows under
            // ratatui's `Wrap`, which would split an empty body's caret onto its
            // own row away from where typing lands.
            None => Span::styled("█", Style::default().fg(Color::Cyan)),
        };
        lines.push(Line::from(vec![Span::raw(before), at, Span::raw(after)]));
    }

    lines.push(Line::raw(""));
    let footer = match composer.mode {
        Mode::Insert => "Esc normal  ·  Enter newline  ·  Ctrl-S save  ·  Ctrl-E severity  ·  Ctrl-T tag",
        Mode::Normal => "Enter save  ·  i/a/o insert  ·  x/dd delete  ·  Esc cancel  ·  Ctrl-E severity  ·  Ctrl-T tag",
    };
    let footer_line_idx = lines.len();
    lines.push(Line::from(Span::styled(
        footer,
        Style::default().fg(Color::DarkGray),
    )));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    // Hit regions for the footer's save / cancel labels (issue #29). Assumes
    // none of the lines above the footer wrapped — true when the body fits in
    // the composer's 72-column inner width, which it nearly always does for the
    // short messages this UI is for. A wrapped body just misses; the keyboard
    // shortcuts still work.
    let inner_x = area.x + 1;
    let footer_y = area.y + 1 + footer_line_idx as u16;
    let (save_word, cancel_word) = match composer.mode {
        Mode::Insert => ("Ctrl-S save", "Esc normal"),
        Mode::Normal => ("Enter save", "Esc cancel"),
    };
    let span_for = |needle: &str| -> super::app::Hitspan {
        match footer.find(needle) {
            Some(off) => {
                let x0 = inner_x + off as u16;
                (footer_y, x0, x0 + needle.chars().count() as u16)
            }
            None => (footer_y, inner_x, inner_x),
        }
    };
    (area, span_for(save_word), span_for(cancel_word))
}

/// The centered help overlay listing every keybinding.
///
/// Kept compact (cyan section headers, no blank separators) so the whole list —
/// including the closing hint — fits within a short, 24-row terminal.
///
/// TODO: this list is hand-written and only describes the *default* keymap, so it
/// silently drifts once a user rebinds anything in their config. We need to
/// generate it from the live binding registry instead — likely by attaching an
/// optional description to each `m.map`/`m.command` and rendering those. Labelled
/// "default keymap" in the meantime so it doesn't claim to be authoritative.
fn render_help(frame: &mut Frame, area: Rect) {
    let section =
        |name: &'static str| Line::from(Span::styled(name, Style::default().fg(Color::Cyan)));
    let text = vec![
        Line::from(Span::styled(
            "mudpuppy — default keymap",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        section("Navigation"),
        Line::raw("  j / k         move cursor (diff) / selection (tree)"),
        Line::raw("  ctrl-d/u/f/b  half / full page down / up"),
        Line::raw("  g g / G       top / bottom (cursor in diff, first/last file)"),
        Line::raw("  ]h / [h  } {  next / prev hunk"),
        Line::raw("  J / K         next / prev file (from the diff pane)"),
        Line::raw("  5j  100G      a number prefixes a count"),
        section("Focus"),
        Line::raw("  Tab / l / h   toggle · tree → diff · diff → tree"),
        section("Annotations  (Space is the leader)"),
        Line::raw("  Space a       annotations tab (all files) ↔ file tree"),
        Line::raw("  v / V  Esc    whole-line selection (diff) · clear"),
        Line::raw("  Space c c/f/r comment line/selection · file · reply"),
        Line::raw("  Space c e/d/s edit · delete (confirm y) · cycle status"),
        section("More"),
        Line::raw("  Space t r     release the turn; first release approves"),
        Line::raw("  Space f       add any file · Space e    expand context"),
        Line::raw("  :             command palette"),
        Line::raw("  ? q Ctrl-c    help · quit"),
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
        Author::User => "user",
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

/// The pending count + partial key sequence as a compact string (`5`, `g`,
/// `5 g g`, `Space c`), or `None` when nothing is in flight.
fn pending_hint(app: &App) -> Option<String> {
    if app.pending_count.is_none() && app.pending_seq.is_empty() {
        return None;
    }
    let mut parts: Vec<String> = Vec::new();
    if let Some(n) = app.pending_count {
        parts.push(n.to_string());
    }
    // KeyChord's Display spells space as "space"; capitalize it so the leader
    // reads as "Space" in the status line.
    parts.extend(app.pending_seq.iter().map(|c| {
        let s = c.to_string();
        if s == "space" {
            "Space".to_string()
        } else {
            s
        }
    }));
    Some(parts.join(" "))
}

/// Short description of what's under review, for the status bar.
pub(crate) fn target_desc(target: &Target) -> String {
    match target {
        Target::Local { base, .. } => format!("local vs {base}"),
        Target::Pr { pr, .. } => format!("PR {pr}"),
    }
}

/// Sum of the char counts of every span — the approximate rendered width when
/// the content is plain ASCII / single-cell glyphs. Used by the status bar to
/// figure out where the next span will land so the release "button" hit region
/// matches what the user sees. Wide CJK glyphs would skew this, but the status
/// bar only renders ASCII/box-drawing.
fn span_total_width(spans: &[Span<'_>]) -> u16 {
    spans.iter().map(|s| s.content.chars().count() as u16).sum()
}

/// Shrink `area` by one cell on every side — the body inside a bordered block.
/// Used to record the pane's clickable interior so a click on the body lands on
/// the correct content row regardless of the surrounding border.
fn inner_rect(area: Rect) -> Rect {
    Rect {
        x: area.x + 1,
        y: area.y + 1,
        width: area.width.saturating_sub(2),
        height: area.height.saturating_sub(2),
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
