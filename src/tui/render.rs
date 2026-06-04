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
        let area = frame.area();
        let outer = render_help(frame, area, app);
        app.hits.help_outer = Some(outer);
    }
    if let Some(picker) = app.picker.as_ref() {
        let area = frame.area();
        let (rect, inner, offset) = render_picker(frame, area, picker);
        app.hits.picker_outer = Some(rect);
        app.hits.picker_inner = Some(inner);
        app.hits.picker_offset = offset;
    }
    if let Some(palette) = app.palette.as_ref() {
        let area = frame.area();
        let (rect, inner, offset) = render_palette(frame, area, palette);
        app.hits.palette_outer = Some(rect);
        app.hits.palette_inner = Some(inner);
        app.hits.palette_offset = offset;
    }
    if let Some(composer) = app.composer.as_ref() {
        let area = frame.area();
        let (rect, save, cancel) = render_composer(frame, area, composer);
        app.hits.composer_outer = Some(rect);
        app.hits.composer_save_span = Some(save);
        app.hits.composer_cancel_span = Some(cancel);
    }
    // A modal prompt sits on top of everything (only one overlay is open at a
    // time in practice, but draw it last so it is unambiguously topmost).
    if let Some(prompt) = app.prompt.as_mut() {
        let area = frame.area();
        render_prompt(frame, area, prompt);
    }
}

/// The modal prompt overlay (opened by `mudpuppy.prompt`): a wrapped question
/// above a row of labelled option chips, the highlighted one styled like a button.
/// A footer spells out the keys. The matching callbacks run in the scripting
/// engine when the user confirms.
fn render_prompt(frame: &mut Frame, area: Rect, prompt: &mut super::prompt::Prompt) {
    // A prompt with a scrollable body (e.g. an update changelog) renders help-sized
    // so the notes have room; a plain prompt stays compact.
    if prompt.body.is_some() {
        render_prompt_with_body(frame, area, prompt);
        return;
    }

    let area = centered_rect(64, (area.height * 4 / 10).max(7), area);
    let block = bordered(" mudpuppy ", true);

    let lines: Vec<Line> = vec![
        Line::from(Span::styled(
            prompt.message.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        Line::from(option_chips(prompt)),
        Line::raw(""),
        Line::from(Span::styled(
            "←/→ choose  ·  1-9 pick  ·  Enter confirm  ·  Esc dismiss",
            Style::default().fg(Color::DarkGray),
        )),
    ];

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

/// The option row: one numbered chip per option, the highlighted one styled like a
/// button. Shared by the compact and bodied prompt layouts.
fn option_chips(prompt: &super::prompt::Prompt) -> Vec<Span<'static>> {
    let mut chips: Vec<Span> = Vec::new();
    for (i, label) in prompt.options.iter().enumerate() {
        if i > 0 {
            chips.push(Span::raw("  "));
        }
        let text = format!(" {} {} ", i + 1, label);
        let style = if i == prompt.selected {
            Style::default()
                .bg(Color::Cyan)
                .fg(Color::Black)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(Color::Gray).add_modifier(Modifier::DIM)
        };
        chips.push(Span::styled(text, style));
    }
    chips
}

/// A prompt with a scrollable body: the question headline, the body (e.g. a
/// changelog) in a help-style scroll region with a "press j or scroll" hint, then
/// the option chips and a key footer.
fn render_prompt_with_body(frame: &mut Frame, area: Rect, prompt: &mut super::prompt::Prompt) {
    // Match the help overlay's sizing so the overlay family feels uniform.
    let height = (area.height * 7 / 10).max(10);
    let outer = centered_rect(72, height, area);

    let body_lines: Vec<Line> = prompt
        .body
        .as_deref()
        .unwrap_or("")
        .split('\n')
        .map(|t| Line::raw(t.to_string()))
        .collect();
    prompt.body_total = body_lines.len();

    // Fixed rows around the scrollable body: headline + blank on top, blank + chips
    // + footer below. The body gets the rest, less one row for the scroll hint when
    // there is more content than fits.
    const FIXED_ROWS: u16 = 5;
    let inner_height = outer.height.saturating_sub(2);
    let body_room = inner_height.saturating_sub(FIXED_ROWS).max(1) as usize;
    let has_more_hint = prompt.body_total > body_room;
    prompt.body_height = if has_more_hint {
        body_room.saturating_sub(1).max(1)
    } else {
        body_room
    };
    if prompt.body_scroll > prompt.max_body_scroll() {
        prompt.body_scroll = prompt.max_body_scroll();
    }

    let max_scroll = prompt.max_body_scroll();
    let at_bottom = prompt.body_scroll >= max_scroll;
    let pct = if max_scroll == 0 {
        "ALL".to_string()
    } else if prompt.body_scroll == 0 {
        "TOP".to_string()
    } else if at_bottom {
        "BOT".to_string()
    } else {
        format!("{}%", prompt.body_scroll * 100 / max_scroll)
    };
    let title = format!(" mudpuppy · {pct} · j/k scroll ");
    let block = bordered(&title, true);
    let inner = block.inner(outer);
    frame.render_widget(Clear, outer);
    frame.render_widget(block, outer);

    let body_constraint = prompt.body_height as u16 + u16::from(has_more_hint);
    let [headline, _gap, body_area, blank, chips_area, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(body_constraint),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
    ])
    .areas(inner);
    let _ = blank;

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            prompt.message.clone(),
            Style::default().add_modifier(Modifier::BOLD),
        )))
        .wrap(Wrap { trim: false }),
        headline,
    );

    if has_more_hint && !at_bottom {
        let [content, hint] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(body_area);
        frame.render_widget(
            Paragraph::new(body_lines)
                .wrap(Wrap { trim: false })
                .scroll((prompt.body_scroll as u16, 0)),
            content,
        );
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                "  ↓ press j or scroll for more ",
                Style::default()
                    .fg(Color::DarkGray)
                    .add_modifier(Modifier::ITALIC),
            ))),
            hint,
        );
    } else {
        frame.render_widget(
            Paragraph::new(body_lines)
                .wrap(Wrap { trim: false })
                .scroll((prompt.body_scroll as u16, 0)),
            body_area,
        );
    }

    frame.render_widget(Paragraph::new(Line::from(option_chips(prompt))), chips_area);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "←/→ choose · 1-9 pick · Enter confirm · j/k scroll · Esc dismiss",
            Style::default().fg(Color::DarkGray),
        ))),
        footer,
    );
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

    let block = sidebar_tabs_block(area, app, focused);
    frame.render_widget(Paragraph::new(lines).block(block), area);
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
        let block = sidebar_tabs_block(area, app, focused);
        frame.render_widget(Paragraph::new(line).block(block), area);
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

    let block = sidebar_tabs_block(area, app, focused);
    frame.render_widget(Paragraph::new(visible).block(block), area);
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
fn render_picker(frame: &mut Frame, area: Rect, picker: &Picker) -> (Rect, Rect, usize) {
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
    (area, inner, offset)
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
fn render_palette(frame: &mut Frame, area: Rect, palette: &CommandPalette) -> (Rect, Rect, usize) {
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
    (area, inner, offset)
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
    // The save / cancel labels are styled as button-like chips (green/red on
    // black, bold) so they read as something clickable rather than just an
    // informational footer (issue #29). The rest of the footer keeps the
    // existing dim hint look. Layout below the chips records their column
    // ranges so the mouse can hit them.
    let dim = Style::default().fg(Color::DarkGray);
    let save_chip_text = " save ";
    let cancel_chip_text = " cancel ";
    let save_chip_style = Style::default()
        .bg(Color::Green)
        .fg(Color::Black)
        .add_modifier(Modifier::BOLD);
    let cancel_chip_style = Style::default()
        .bg(Color::Red)
        .fg(Color::White)
        .add_modifier(Modifier::BOLD);
    let save_hint = match composer.mode {
        Mode::Insert => " (Ctrl-S)",
        Mode::Normal => " (Enter)",
    };
    let footer_line_idx = lines.len();
    let inner_x = area.x + 1;
    let footer_y = area.y + 1 + footer_line_idx as u16;
    let save_x0 = inner_x;
    let save_x1 = save_x0 + save_chip_text.chars().count() as u16;
    let hint_after_save = save_x1 + save_hint.chars().count() as u16;
    let pad = "  ";
    let cancel_x0 = hint_after_save + pad.chars().count() as u16;
    let cancel_x1 = cancel_x0 + cancel_chip_text.chars().count() as u16;
    let cancel_hint = match composer.mode {
        Mode::Insert => " (Esc → normal)",
        Mode::Normal => " (Esc)",
    };
    lines.push(Line::from(vec![
        Span::styled(save_chip_text, save_chip_style),
        Span::styled(save_hint, dim),
        Span::raw(pad),
        Span::styled(cancel_chip_text, cancel_chip_style),
        Span::styled(cancel_hint, dim),
        Span::raw(pad),
        Span::styled("Ctrl-E severity  ·  Ctrl-T tag  ·  Ctrl-J newline", dim),
    ]));

    frame.render_widget(Clear, area);
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );

    let save_span: super::app::Hitspan = (footer_y, save_x0, save_x1);
    let cancel_span: super::app::Hitspan = (footer_y, cancel_x0, cancel_x1);
    (area, save_span, cancel_span)
}

/// The centered help overlay listing every keybinding. The overlay is
/// scrollable (issue #29): the body that doesn't fit in the viewport can be
/// paged with `j`/`k`/`PageUp`/`PageDown`/`g`/`G` or the mouse wheel, and the
/// title shows the current scroll-position percentage so the user knows there's
/// more below.
///
/// Returns the outer rect so the caller can record it for mouse hit-testing.
///
/// TODO: this list is hand-written and only describes the *default* keymap, so it
/// silently drifts once a user rebinds anything in their config. We need to
/// generate it from the live binding registry instead — likely by attaching an
/// optional description to each `m.map`/`m.command` and rendering those. Labelled
/// "default keymap" in the meantime so it doesn't claim to be authoritative.
fn render_help(frame: &mut Frame, area: Rect, app: &mut App) -> Rect {
    let section =
        |name: &'static str| Line::from(Span::styled(name, Style::default().fg(Color::Cyan)));
    let text = vec![
        Line::from(Span::styled(
            "mudpuppy — default keymap",
            Style::default().add_modifier(Modifier::BOLD),
        )),
        Line::raw(""),
        section("Navigation"),
        Line::raw("  j / k         move cursor (diff) / selection (tree)"),
        Line::raw("  ctrl-d/u/f/b  half / full page down / up"),
        Line::raw("  g g / G       top / bottom (cursor in diff, first/last file)"),
        Line::raw("  ]h / [h  } {  next / prev hunk"),
        Line::raw("  J / K         next / prev file (from the diff pane)"),
        Line::raw("  5j  100G      a number prefixes a count"),
        Line::raw(""),
        section("Focus"),
        Line::raw("  Tab / l / h   toggle · tree → diff · diff → tree"),
        Line::raw(""),
        section("Annotations  (Space is the leader)"),
        Line::raw("  Space a       annotations tab (all files) ↔ file tree"),
        Line::raw("  v / V  Esc    whole-line selection (diff) · clear"),
        Line::raw("  Space c c/f/r comment line/selection · file · reply"),
        Line::raw("  Space c e/d/s edit · delete (confirm y) · cycle status"),
        Line::raw(""),
        section("More"),
        Line::raw("  Space t r     release the turn; first release approves"),
        Line::raw("  Space f       add any file · Space e    expand context"),
        Line::raw("  :             command palette"),
        Line::raw("  ? q Ctrl-c    help · quit"),
        Line::raw(""),
        section("Mouse"),
        Line::raw("  wheel         scroll the pane / overlay under the cursor"),
        Line::raw("  click         focus a pane; pick a file / annotation row"),
        Line::raw("  click title   switch sidebar tabs (Files ↔ Annotations)"),
        Line::raw("  click r       release / approve the turn (status bar)"),
        Line::raw("  drag (diff)   enter visual mode and select lines"),
        Line::raw("  dbl-click     open file (tree) · comment line (diff)"),
        Line::raw(""),
        section("Help overlay"),
        Line::raw("  j / k         scroll down / up one line"),
        Line::raw("  PgDn / PgUp   page down / up"),
        Line::raw("  ctrl-d / u    half-page down / up"),
        Line::raw("  g / G         jump to top / bottom"),
        Line::raw("  ? / q / Esc   close"),
    ];

    // Match the picker's vertical sizing so the overlay family feels uniform
    // — 70% of the terminal height, floored at 8 rows. Width stays at 64 so
    // the keymap lines (built to fit there) don't wrap.
    let height = ((area.height * 7 / 10).max(8)).min(text.len() as u16 + 2);
    let outer = centered_rect(64, height, area);
    let inner_height = outer.height.saturating_sub(2) as usize;

    // Record metrics so the key handler can clamp/page correctly. When there's
    // more content below the viewport we steal the last row for a "scroll for
    // more" hint, so the effective viewport (the height clamping uses) shrinks
    // by one.
    app.help_total = text.len();
    let has_more_hint_reserved = text.len() > inner_height;
    let effective_height = if has_more_hint_reserved {
        inner_height.saturating_sub(1).max(1)
    } else {
        inner_height
    };
    app.help_height = effective_height;
    if app.help_scroll > app.max_help_scroll() {
        app.help_scroll = app.max_help_scroll();
    }

    let at_bottom = app.help_scroll >= app.max_help_scroll();
    let show_more_hint = has_more_hint_reserved && !at_bottom;

    let pct = if app.max_help_scroll() == 0 {
        "ALL".to_string()
    } else if app.help_scroll == 0 {
        "TOP".to_string()
    } else if at_bottom {
        "BOT".to_string()
    } else {
        format!("{}%", app.help_scroll * 100 / app.max_help_scroll())
    };
    let title = format!(" Help · {pct} · j/k · ? close ");

    let block = bordered(&title, true);
    let inner_area = block.inner(outer);
    frame.render_widget(Clear, outer);
    frame.render_widget(block, outer);

    if show_more_hint {
        let [content_area, hint_area] =
            Layout::vertical([Constraint::Min(0), Constraint::Length(1)]).areas(inner_area);
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.help_scroll as u16, 0)),
            content_area,
        );
        let hint = Line::from(Span::styled(
            "  ↓ press j or scroll for more ",
            Style::default()
                .fg(Color::DarkGray)
                .add_modifier(Modifier::ITALIC),
        ));
        frame.render_widget(Paragraph::new(hint), hint_area);
    } else {
        frame.render_widget(
            Paragraph::new(text)
                .wrap(Wrap { trim: false })
                .scroll((app.help_scroll as u16, 0)),
            inner_area,
        );
    }
    outer
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

/// Build the sidebar's bordered block with a two-tab title strip — `Files (N)`
/// and `Annotations (M)` separated by `│`, the active tab styled as a chip and
/// the inactive one dimmed. Records each chip's hit span on `app.hits` so a
/// mouse click on either tab switches to it directly (issue #29). Only the
/// active tab carries its count, keeping the strip narrow enough to fit inside
/// the 28-column sidebar of the Files mode.
fn sidebar_tabs_block(area: Rect, app: &mut App, focused: bool) -> Block<'static> {
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else {
        Style::default().fg(Color::DarkGray)
    };

    let active = Style::default()
        .fg(Color::Black)
        .bg(Color::Cyan)
        .add_modifier(Modifier::BOLD);
    let inactive = Style::default().fg(Color::DarkGray);
    let sep = Style::default().fg(Color::DarkGray);

    let files_text = match app.sidebar {
        Sidebar::Files => format!(" Files ({}) ", app.files.len()),
        Sidebar::Annotations => " Files ".to_string(),
    };
    let annot_text = match app.sidebar {
        Sidebar::Files => " Annotations ".to_string(),
        Sidebar::Annotations => format!(" Annotations ({}) ", app.annotations.len()),
    };

    let (files_style, annot_style) = match app.sidebar {
        Sidebar::Files => (active, inactive),
        Sidebar::Annotations => (inactive, active),
    };

    // Title text is left-aligned just after the top-left corner glyph at
    // `area.x`. Compute each chip's column span as we lay it out so the hit
    // tester lands exactly on what the user sees.
    let title_y = area.y;
    let files_x0 = area.x + 1;
    let files_x1 = files_x0 + files_text.chars().count() as u16;
    let sep_x1 = files_x1 + 1; // single `│` cell
    let annot_x0 = sep_x1;
    let annot_x1 = annot_x0 + annot_text.chars().count() as u16;

    app.hits.tab_files_span = Some((title_y, files_x0, files_x1));
    app.hits.tab_annot_span = Some((title_y, annot_x0, annot_x1));

    let title = Line::from(vec![
        Span::styled(files_text, files_style),
        Span::styled("│", sep),
        Span::styled(annot_text, annot_style),
    ]);

    Block::default()
        .borders(Borders::ALL)
        .border_style(border_style)
        .title(title)
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
