//! `mudpuppy debug colors` — a static swatch page for eyeballing every colour
//! the viewer paints, drawn on the same themed background the real UI uses. It
//! exists so colour/contrast can be judged visually in *your* terminal (the one
//! place named ANSI colours actually resolve) rather than guessed at from RGB
//! values. Each row is labelled with where that colour is used in the UI.

use anyhow::{Context, Result};
use ratatui::crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use super::palette;
use crate::highlight::Highlighter;

/// Open the preview, draw it, and block until the user quits (`q`/`Esc`/Ctrl-C).
/// Scroll with the arrow keys / `j`/`k` / PageUp/PageDown when it overflows.
pub fn run() -> Result<()> {
    let mut terminal = ratatui::init();
    let result = draw_loop(&mut terminal);
    ratatui::restore();
    result
}

fn draw_loop(terminal: &mut ratatui::DefaultTerminal) -> Result<()> {
    let lines = swatches();
    let mut scroll: u16 = 0;
    loop {
        let mut max_scroll = 0u16;
        terminal
            .draw(|frame| max_scroll = render(frame, &lines, scroll))
            .context("drawing the colour preview")?;
        match event::read().context("reading a key")? {
            Event::Key(k) if k.kind == KeyEventKind::Press => {
                let ctrl_c =
                    k.modifiers.contains(KeyModifiers::CONTROL) && k.code == KeyCode::Char('c');
                match k.code {
                    KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
                    _ if ctrl_c => return Ok(()),
                    KeyCode::Down | KeyCode::Char('j') => scroll = scroll.saturating_add(1),
                    KeyCode::Up | KeyCode::Char('k') => scroll = scroll.saturating_sub(1),
                    KeyCode::PageDown => scroll = scroll.saturating_add(10),
                    KeyCode::PageUp => scroll = scroll.saturating_sub(10),
                    _ => {}
                }
                scroll = scroll.min(max_scroll);
            }
            _ => {}
        }
    }
}

/// Draw one frame; returns the maximum useful scroll offset so the loop can clamp.
fn render(frame: &mut Frame, lines: &[Line<'static>], scroll: u16) -> u16 {
    let area = frame.area();
    // Same whole-app themed fill the real viewer paints, so swatches sit on the
    // exact background the UI uses.
    frame.render_widget(
        Block::default().style(Style::default().bg(palette::BG)),
        area,
    );

    let footer_h = 1;
    let body = Rect {
        height: area.height.saturating_sub(footer_h),
        ..area
    };
    let inner_h = body.height.saturating_sub(2); // borders
    let max_scroll = (lines.len() as u16).saturating_sub(inner_h);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan))
        .title(" mudpuppy debug colors ");
    frame.render_widget(
        Paragraph::new(lines.to_vec())
            .block(block)
            .style(Style::default().bg(palette::BG))
            .scroll((scroll, 0)),
        body,
    );

    let footer = Rect {
        x: area.x,
        y: area.y + area.height.saturating_sub(footer_h),
        width: area.width,
        height: footer_h,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            "  j/k or ↑/↓ scroll · PgUp/PgDn · q/Esc quit",
            Style::default().fg(palette::FG_DIM),
        )))
        .style(Style::default().bg(palette::BG)),
        footer,
    );

    max_scroll
}

/// A labelled line: the sample text styled as `style`, padded, followed by a dim
/// description of where the colour is used in the real UI.
fn swatch(sample: &str, style: Style, usage: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {sample:<22}"), style),
        Span::styled(
            usage.to_string(),
            Style::default()
                .fg(palette::FG_DIM)
                .add_modifier(Modifier::ITALIC),
        ),
    ])
}

fn heading(text: &str) -> Line<'static> {
    Line::from(Span::styled(
        format!("▌ {text}"),
        Style::default()
            .fg(Color::White)
            .add_modifier(Modifier::BOLD),
    ))
}

/// Build every swatch line. Grouped to mirror how colours are used; the goal is
/// that a reader can spot any row whose sample text is hard to read against the
/// themed background and tell us which usage it maps to.
fn swatches() -> Vec<Line<'static>> {
    let mut out: Vec<Line<'static>> = Vec::new();
    let bg = match palette::BG {
        Color::Rgb(r, g, b) => format!("#{r:02x}{g:02x}{b:02x}"),
        other => format!("{other:?}"),
    };

    out.push(Line::raw(""));
    out.push(Line::from(Span::styled(
        format!("  themed background: {bg}  (everything below is painted on it)"),
        Style::default().fg(Color::Gray),
    )));
    out.push(Line::raw(""));

    out.push(heading("Foreground text on the themed background"));
    let fg = |c: Color| Style::default().fg(c);
    out.push(swatch(
        "the quick brown fox",
        fg(Color::Reset),
        "plain / context diff content (terminal default)",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(palette::FG_DIM),
        "line numbers, help text, metadata, separators",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(Color::Gray),
        "comment bodies, annotation preview, tag labels",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(Color::White).add_modifier(Modifier::BOLD),
        "selected file, annotation file header (bold)",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(Color::Cyan),
        "focused border, command names, NORMAL mode, caret",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(Color::Green),
        "additions (+N), added line marker, INSERT mode",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(Color::Red),
        "deletions (-N), removed line marker",
    ));
    out.push(swatch(
        "the quick brown fox",
        fg(Color::Yellow),
        "keybinding hints, intent tags",
    ));
    out.push(Line::raw(""));

    out.push(heading("Severity colours (annotation marks & headers)"));
    out.push(swatch("● info", fg(Color::Blue), "Severity::Info"));
    out.push(swatch(
        "● suggestion",
        fg(Color::Cyan),
        "Severity::Suggestion",
    ));
    out.push(swatch("● warning", fg(Color::Yellow), "Severity::Warning"));
    out.push(swatch("● blocker", fg(Color::Red), "Severity::Blocker"));
    out.push(Line::raw(""));

    out.push(heading("Row background tints (sample text over each)"));
    let on = |bg: Color| Style::default().bg(bg);
    out.push(swatch(
        "+ added line",
        Style::default().bg(palette::BG_ADDED).fg(Color::Green),
        "diff: added line band",
    ));
    out.push(swatch(
        "- removed line",
        Style::default().bg(palette::BG_REMOVED).fg(Color::Red),
        "diff: removed line band",
    ));
    out.push(swatch(
        "selected file row",
        on(palette::BG_SELECTED_FILE),
        "tree: highlighted file",
    ));
    out.push(swatch(
        "visual selection",
        on(palette::BG_SELECTION),
        "diff: selected line span / annotation row",
    ));
    out.push(swatch(
        "cursor row",
        on(palette::BG_CURSOR),
        "diff: the cursor line",
    ));
    out.push(swatch(
        "status bar",
        Style::default().bg(Color::Rgb(30, 33, 40)).fg(Color::Gray),
        "bottom status bar",
    ));
    out.push(Line::raw(""));

    out.push(heading("Chips (foreground on a coloured background)"));
    let chip = |fg: Color, bg: Color| Style::default().fg(fg).bg(bg).add_modifier(Modifier::BOLD);
    out.push(swatch(
        " Save ",
        chip(Color::Black, Color::Green),
        "composer save button",
    ));
    out.push(swatch(
        " Cancel ",
        chip(Color::White, Color::Red),
        "composer cancel button",
    ));
    out.push(swatch(
        " key ",
        chip(Color::Black, Color::Cyan),
        "status-bar key chip",
    ));
    out.push(swatch(
        " key ",
        chip(Color::Black, Color::Yellow),
        "status-bar key chip",
    ));
    out.push(Line::raw(""));

    out.push(heading(
        "Syntax highlighting (real tokens on the themed background)",
    ));
    out.extend(syntax_sample());
    out.push(Line::raw(""));

    out
}

/// Highlight a small Rust snippet the same way the diff pane does, so every
/// token colour the theme emits is visible on the themed background.
fn syntax_sample() -> Vec<Line<'static>> {
    let code = [
        "// a comment explaining why, not what",
        "use std::collections::HashMap;",
        "pub fn greet(name: &str, count: u32) -> String {",
        "    let mut msg = String::new();",
        "    for i in 0..count {",
        "        msg.push_str(&format!(\"{i}: hello, {name}!\\n\"));",
        "    }",
        "    msg",
        "}",
    ];
    let Some(hl) = Highlighter::for_path("sample.rs", None) else {
        return vec![Line::raw("  (no Rust syntax available)")];
    };
    hl.hunk(&code)
        .into_iter()
        .map(|runs| {
            let mut spans = vec![Span::raw("  ")];
            for (color, text) in runs {
                spans.push(Span::styled(text, Style::default().fg(color)));
            }
            Line::from(spans)
        })
        .collect()
}
