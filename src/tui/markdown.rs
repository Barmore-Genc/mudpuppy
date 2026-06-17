//! Lightweight Markdown styling for the prompt's scrollable details body.
//!
//! The only Markdown we render is GitHub release-note changelogs shown in the
//! self-update prompt, so this is deliberately small: it understands ATX
//! headings, bullet/ordered lists, blockquotes, fenced code blocks, and the
//! common inline spans (`code`, `**bold**`, `*italic*`, `[text](url)` links).
//! It is not a CommonMark implementation — anything it doesn't recognise falls
//! through as plain dim text, which is exactly what the body used to be.
//!
//! [`render`] returns already-wrapped styled [`Line`]s so the prompt keeps
//! scrolling in real display-line units (see `render::render_detailed_prompt`),
//! the same contract the old plain `wrap_text` path had.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use super::palette;

/// Bright accent for headings, list bullets, and links — matches the option
/// chips' cyan so the overlay reads as one piece.
const ACCENT: Color = Color::Cyan;
/// Inline `code` and fenced code blocks: a warm tone that stays legible on the
/// dark background without competing with the cyan accents.
const CODE: Color = Color::Rgb(230, 200, 140);
/// Emphasised (`**bold**`) text: a step brighter than the dim body so it lifts.
const STRONG: Color = Color::Rgb(210, 217, 232);

/// A styled text fragment produced by inline parsing, before word-wrapping.
type Token = (String, Style);

fn base_style() -> Style {
    Style::default().fg(palette::FG_DIM)
}

/// Render `text` as styled, word-wrapped lines no wider than `width` columns.
/// Always yields at least one (possibly blank) line so an empty body still has
/// height to scroll/clear against.
pub(crate) fn render(text: &str, width: usize) -> Vec<Line<'static>> {
    let width = width.max(1);
    let mut out: Vec<Line<'static>> = Vec::new();
    let mut in_code = false;
    for raw in text.split('\n') {
        let line = raw.strip_suffix('\r').unwrap_or(raw);
        let trimmed = line.trim_start();

        // Fenced code blocks: the ``` / ~~~ fence lines are markers, not content,
        // so they're consumed without emitting a row.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            in_code = !in_code;
            continue;
        }
        if in_code {
            let style = Style::default().fg(CODE);
            wrap_into(
                &mut out,
                Vec::new(),
                0,
                vec![(line.to_string(), style)],
                width,
            );
            continue;
        }

        if trimmed.is_empty() {
            out.push(Line::raw(""));
            continue;
        }

        if let Some(line_out) = heading(trimmed) {
            wrap_into(&mut out, Vec::new(), 0, line_out, width);
            continue;
        }

        if let Some((prefix, hang, rest)) = list_item(line) {
            wrap_into(
                &mut out,
                prefix,
                hang,
                parse_inline(rest, base_style()),
                width,
            );
            continue;
        }

        if let Some(rest) = trimmed.strip_prefix('>') {
            let rest = rest.strip_prefix(' ').unwrap_or(rest);
            let quote = Style::default()
                .fg(palette::FG_DIM)
                .add_modifier(Modifier::ITALIC);
            let prefix = vec![("▎ ".to_string(), Style::default().fg(ACCENT))];
            wrap_into(&mut out, prefix, 2, parse_inline(rest, quote), width);
            continue;
        }

        wrap_into(
            &mut out,
            Vec::new(),
            0,
            parse_inline(line, base_style()),
            width,
        );
    }
    if out.is_empty() {
        out.push(Line::raw(""));
    }
    out
}

/// An ATX heading (`#`..`######` followed by a space). Returns the styled tokens
/// for the heading text, or `None` when the line isn't a heading.
fn heading(trimmed: &str) -> Option<Vec<Token>> {
    let hashes = trimmed.chars().take_while(|&c| c == '#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let after = &trimmed[hashes..];
    // Require the space so `#fragment`-style text isn't mistaken for a heading.
    let content = after.strip_prefix(' ')?;
    // Drop any closing run of `#` (the optional ATX-closing sequence).
    let content = content.trim_end_matches([' ', '#']);
    let mut style = Style::default().fg(ACCENT).add_modifier(Modifier::BOLD);
    if hashes == 1 {
        style = style.add_modifier(Modifier::UNDERLINED);
    }
    Some(parse_inline(content, style))
}

/// A bullet (`-`/`*`/`+`) or ordered (`1.`) list item. Returns the styled marker
/// prefix, the continuation indent (so wrapped lines hang under the text), and
/// the remaining item text — or `None` when the line isn't a list item.
fn list_item(line: &str) -> Option<(Vec<Token>, usize, &str)> {
    let indent = line.len() - line.trim_start().len();
    let body = &line[indent..];
    let pad = " ".repeat(indent);
    let accent = Style::default().fg(ACCENT);

    if let Some(rest) = body
        .strip_prefix("- ")
        .or_else(|| body.strip_prefix("* "))
        .or_else(|| body.strip_prefix("+ "))
    {
        let prefix = vec![(format!("{pad}• "), accent)];
        return Some((prefix, indent + 2, rest));
    }

    // Ordered: a run of digits then `. ` (e.g. `12. `).
    let digits = body.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 {
        if let Some(rest) = body[digits..].strip_prefix(". ") {
            let marker = format!("{pad}{}. ", &body[..digits]);
            let hang = indent + digits + 2;
            return Some((vec![(marker, accent)], hang, rest));
        }
    }
    None
}

/// Parse inline spans (`code`, `**bold**`, `*italic*`, `[text](url)`) out of `s`,
/// applying `base` to ordinary text. Single `_` is left alone so it doesn't
/// italicise the middle of `snake_case` identifiers; emphasis uses `*`.
fn parse_inline(s: &str, base: Style) -> Vec<Token> {
    let chars: Vec<char> = s.chars().collect();
    let mut out: Vec<Token> = Vec::new();
    let mut plain = String::new();
    let mut i = 0;
    let flush = |plain: &mut String, out: &mut Vec<Token>| {
        if !plain.is_empty() {
            out.push((std::mem::take(plain), base));
        }
    };
    while i < chars.len() {
        let c = chars[i];
        // `` `code` ``
        if c == '`' {
            if let Some(end) = find(&chars, i + 1, '`') {
                flush(&mut plain, &mut out);
                let text: String = chars[i + 1..end].iter().collect();
                out.push((text, Style::default().fg(CODE)));
                i = end + 1;
                continue;
            }
        }
        // `**bold**` / `__bold__`
        if (c == '*' || c == '_') && i + 1 < chars.len() && chars[i + 1] == c {
            if let Some(end) = find_run(&chars, i + 2, c, 2) {
                flush(&mut plain, &mut out);
                let inner: String = chars[i + 2..end].iter().collect();
                let strong = base.fg(STRONG).add_modifier(Modifier::BOLD);
                out.extend(parse_inline(&inner, strong));
                i = end + 2;
                continue;
            }
        }
        // `*italic*`
        if c == '*' {
            if let Some(end) = find(&chars, i + 1, '*') {
                flush(&mut plain, &mut out);
                let inner: String = chars[i + 1..end].iter().collect();
                out.extend(parse_inline(&inner, base.add_modifier(Modifier::ITALIC)));
                i = end + 1;
                continue;
            }
        }
        // `[text](url)` — show the text as an accented link, drop the URL.
        if c == '[' {
            if let Some(close) = find(&chars, i + 1, ']') {
                if chars.get(close + 1) == Some(&'(') {
                    if let Some(paren) = find(&chars, close + 2, ')') {
                        flush(&mut plain, &mut out);
                        let text: String = chars[i + 1..close].iter().collect();
                        let link = Style::default()
                            .fg(ACCENT)
                            .add_modifier(Modifier::UNDERLINED);
                        out.push((text, link));
                        i = paren + 1;
                        continue;
                    }
                }
            }
        }
        plain.push(c);
        i += 1;
    }
    flush(&mut plain, &mut out);
    out
}

/// Index of the next `target` in `chars` at or after `from`, or `None`.
fn find(chars: &[char], from: usize, target: char) -> Option<usize> {
    (from..chars.len()).find(|&j| chars[j] == target)
}

/// Index where a run of `len` copies of `target` starts, at or after `from`.
fn find_run(chars: &[char], from: usize, target: char, len: usize) -> Option<usize> {
    let mut j = from;
    while j + len <= chars.len() {
        if chars[j..j + len].iter().all(|&c| c == target) {
            return Some(j);
        }
        j += 1;
    }
    None
}

/// Greedy word-wrap of styled `tokens` into `out`, prefixed by `prefix` (the list
/// marker / quote bar, counted against the width) and hanging continuation lines
/// by `hang` columns. Mirrors `interleave::wrap_text`'s rules (break on
/// whitespace, hard-split overlong words) but carries per-span styles through.
fn wrap_into(
    out: &mut Vec<Line<'static>>,
    prefix: Vec<Token>,
    hang: usize,
    tokens: Vec<Token>,
    width: usize,
) {
    // Flatten tokens to styled words; whitespace is dropped and re-added as
    // single spaces during packing.
    let mut words: Vec<Token> = Vec::new();
    for (text, style) in tokens {
        for w in text.split_whitespace() {
            words.push((w.to_string(), style));
        }
    }

    let prefix_width: usize = prefix.iter().map(|(t, _)| t.chars().count()).sum();
    let mut spans: Vec<Span<'static>> = prefix
        .into_iter()
        .map(|(t, s)| Span::styled(t, s))
        .collect();
    let mut col = prefix_width;
    let mut has_word = false;

    let push_line = |out: &mut Vec<Line<'static>>, spans: &mut Vec<Span<'static>>| {
        out.push(Line::from(std::mem::take(spans)));
    };
    let new_continuation = |col: &mut usize, spans: &mut Vec<Span<'static>>| {
        if hang > 0 {
            spans.push(Span::raw(" ".repeat(hang)));
        }
        *col = hang;
    };

    for (word, style) in words {
        let wlen = word.chars().count();
        let avail = width.saturating_sub(if has_word { col + 1 } else { col });
        // Overlong word: flush the line, then hard-split across continuation
        // lines so it never overflows the body width.
        if wlen > width.saturating_sub(hang).max(1) {
            if has_word {
                push_line(out, &mut spans);
                new_continuation(&mut col, &mut spans);
                has_word = false;
            }
            for ch in word.chars() {
                if col >= width {
                    push_line(out, &mut spans);
                    new_continuation(&mut col, &mut spans);
                }
                spans.push(Span::styled(ch.to_string(), style));
                col += 1;
                has_word = true;
            }
            continue;
        }
        if has_word && wlen > avail {
            push_line(out, &mut spans);
            new_continuation(&mut col, &mut spans);
            has_word = false;
        }
        if has_word {
            spans.push(Span::raw(" "));
            col += 1;
        }
        spans.push(Span::styled(word, style));
        col += wlen;
        has_word = true;
    }
    push_line(out, &mut spans);
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The visible text of a rendered line, ignoring styling.
    fn text(line: &Line) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// The first span across all `lines` whose visible text equals `needle`.
    fn span<'a>(lines: &'a [Line], needle: &str) -> &'a Span<'a> {
        lines
            .iter()
            .flat_map(|l| l.spans.iter())
            .find(|s| s.content.as_ref() == needle)
            .unwrap_or_else(|| {
                panic!(
                    "no span {needle:?} in {:?}",
                    lines.iter().map(text).collect::<Vec<_>>()
                )
            })
    }

    /// One render exercising every feature this module styles: both heading
    /// levels, bullet and ordered lists (with hanging-indent wrap), a blockquote,
    /// a fenced code block, and all four inline spans — asserting both the text
    /// each produces and the style that sets it apart.
    #[test]
    fn showcase_renders_every_feature() {
        let md = "\
# mudpuppy 1.0
## Highlights
- first item that is long enough to wrap onto a second display line here
- uses `wrap_text` and **bold** and *italic* spans
1. ordered one
2. see [the PR](http://x.y)
> a quoted aside
```
let x = 1;
```";
        let lines = render(md, 24);
        let all: Vec<String> = lines.iter().map(text).collect();

        // Level-1 heading: bold + underlined accent, marker stripped. (The text
        // is word-wrapped into per-word spans, so check one word.)
        assert_eq!(all[0], "mudpuppy 1.0");
        let h1 = span(&lines, "mudpuppy");
        assert_eq!(h1.style.fg, Some(ACCENT));
        assert!(h1.style.add_modifier.contains(Modifier::BOLD));
        assert!(h1.style.add_modifier.contains(Modifier::UNDERLINED));

        // Level-2 heading: bold accent, not underlined.
        let h2 = span(&lines, "Highlights");
        assert!(h2.style.add_modifier.contains(Modifier::BOLD));
        assert!(!h2.style.add_modifier.contains(Modifier::UNDERLINED));

        // Bullet marker, and its long item wraps with a 2-col hanging indent.
        assert!(span(&lines, "• ").style.fg == Some(ACCENT));
        let bullet_idx = all.iter().position(|l| l.starts_with("• first")).unwrap();
        assert!(all[bullet_idx + 1].starts_with("  ")); // continuation hangs under text

        // Inline spans on the second bullet.
        assert_eq!(span(&lines, "wrap_text").style.fg, Some(CODE));
        let bold = span(&lines, "bold");
        assert_eq!(bold.style.fg, Some(STRONG));
        assert!(bold.style.add_modifier.contains(Modifier::BOLD));
        assert!(span(&lines, "italic")
            .style
            .add_modifier
            .contains(Modifier::ITALIC));

        // Ordered list keeps its numbering as an accented marker.
        assert_eq!(span(&lines, "1. ").style.fg, Some(ACCENT));
        assert!(all.iter().any(|l| l == "1. ordered one"));

        // Link shows the text (URL dropped) as an underlined accent. The label
        // is word-wrapped into per-word spans, so check one of its words.
        let link = span(&lines, "PR");
        assert_eq!(link.style.fg, Some(ACCENT));
        assert!(link.style.add_modifier.contains(Modifier::UNDERLINED));
        assert!(all.iter().all(|l| !l.contains("http")));

        // Blockquote: accent bar prefix, italic body.
        assert_eq!(span(&lines, "▎ ").style.fg, Some(ACCENT));
        assert!(span(&lines, "quoted")
            .style
            .add_modifier
            .contains(Modifier::ITALIC));

        // Fenced code: fence lines dropped, body in the code colour.
        assert!(all.iter().any(|l| l == "let x = 1;"));
        assert_eq!(span(&lines, "let").style.fg, Some(CODE));
        assert!(all.iter().all(|l| !l.contains("```")));
    }

    #[test]
    fn heading_is_bold_accented_and_stripped() {
        let lines = render("## What's new", 40);
        assert_eq!(text(&lines[0]), "What's new");
        let span = &lines[0].spans[0];
        assert_eq!(span.style.fg, Some(ACCENT));
        assert!(span.style.add_modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn bullets_get_a_marker_and_hang_indent() {
        let lines = render("- alpha beta gamma delta epsilon", 16);
        assert!(text(&lines[0]).starts_with("• alpha"));
        // Continuation wraps under the text, not the bullet.
        assert!(lines.len() > 1);
        assert!(text(&lines[1]).starts_with("  "));
    }

    #[test]
    fn inline_code_bold_and_links_render_text_only() {
        let lines = render("see `foo` and **bar** in [the docs](http://x.y)", 80);
        let t = text(&lines[0]);
        assert_eq!(t, "see foo and bar in the docs");
        // The code span carries the code colour.
        let code = lines[0]
            .spans
            .iter()
            .find(|s| s.content.as_ref() == "foo")
            .unwrap();
        assert_eq!(code.style.fg, Some(CODE));
    }

    #[test]
    fn snake_case_is_not_italicised() {
        let lines = render("call open_prompt now", 80);
        assert_eq!(text(&lines[0]), "call open_prompt now");
        // No span should have flipped to italic on the underscores.
        assert!(lines[0]
            .spans
            .iter()
            .all(|s| !s.style.add_modifier.contains(Modifier::ITALIC)));
    }

    #[test]
    fn fenced_code_drops_the_fence_lines() {
        let lines = render("```\nlet x = 1;\n```", 40);
        assert_eq!(lines.len(), 1);
        assert_eq!(text(&lines[0]), "let x = 1;");
        assert_eq!(lines[0].spans[0].style.fg, Some(CODE));
    }

    #[test]
    fn plain_text_keeps_the_dim_body_colour() {
        let lines = render("just a sentence", 40);
        assert_eq!(lines[0].spans[0].style.fg, Some(palette::FG_DIM));
    }

    #[test]
    fn blank_input_still_yields_a_line() {
        assert_eq!(render("", 40).len(), 1);
    }
}
