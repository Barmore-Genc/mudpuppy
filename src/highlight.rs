//! Syntax highlighting for the diff pane (PLAN.md §2).
//!
//! mudpuppy highlights diff content **in process** with [`syntect`] so it keeps
//! full control of the gutter/annotation overlay the TUI draws on top — no
//! external `delta`/`difftastic` renderer, no second pass over the text.
//!
//! The public surface is deliberately small and pull-based, so the caller keeps
//! the performance budget (AGENTS.md "performance is a feature"; 1000+ files):
//!
//! - [`Highlighter::for_path`] resolves a language once per *opened* file, or
//!   returns `None` for an unknown/unsupported extension so the caller can fall
//!   back to its plain per-kind colouring.
//! - [`Highlighter::hunk`] highlights one hunk's lines, returning per-line colour
//!   runs the TUI turns into spans. Parse state is **reset per hunk on purpose**:
//!   a unified diff only carries hunk bodies, never the lines between them, so
//!   there is no continuous context to thread a multi-line string or block
//!   comment through across the gap. Restarting per hunk keeps each hunk's
//!   highlighting self-consistent and bounds the work to what's on screen.
//!
//! The syntax/theme assets load once, lazily, behind a [`OnceLock`] — the first
//! highlighted file pays for it, an all-binary or unsupported-language diff never
//! does.

use std::path::Path;
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::{Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// One highlighted line: contiguous `(colour, text)` runs whose concatenated
/// text is exactly the original line (so the rendered characters never change,
/// only their colour — see TESTING.md on why that keeps the `.snap` grid stable).
pub type HlLine = Vec<(Color, String)>;

/// The loaded syntect assets: the default syntax definitions and the one theme
/// we colour with. Built once and shared.
struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        // `_newlines` is the variant whose rules expect a trailing `\n`, which is
        // what `hunk` feeds in for correct end-of-line tokenization.
        let syntaxes = SyntaxSet::load_defaults_newlines();
        // A dark theme that reads well on the TUI's dark background; this is one
        // of the defaults syntect ships, so no asset files to vendor.
        let theme = ThemeSet::load_defaults().themes["base16-ocean.dark"].clone();
        Assets { syntaxes, theme }
    })
}

/// A language-resolved highlighter for a single file. Cheap to hold (it borrows
/// the shared assets); build one per opened file via [`Highlighter::for_path`].
pub struct Highlighter {
    syntax: &'static SyntaxReference,
}

impl Highlighter {
    /// Resolve the language for `path` by its extension, or `None` when nothing
    /// matches — the caller then renders the file without highlighting.
    pub fn for_path(path: &str) -> Option<Highlighter> {
        let assets = assets();
        let ext = Path::new(path).extension()?.to_str()?;
        let syntax = assets.syntaxes.find_syntax_by_extension(ext)?;
        Some(Highlighter { syntax })
    }

    /// Highlight one hunk's content lines (without their `+`/`-`/space markers),
    /// returning one [`HlLine`] per input line, aligned 1:1. Parse state is
    /// local to this call — see the module docs on why it resets per hunk.
    pub fn hunk(&self, lines: &[&str]) -> Vec<HlLine> {
        let assets = assets();
        let mut hl = HighlightLines::new(self.syntax, &assets.theme);
        lines
            .iter()
            .map(|line| {
                // syntect's `_newlines` syntaxes want the terminating newline;
                // we strip it back off the rendered text so spans hold no stray
                // glyph.
                let owned = format!("{line}\n");
                match hl.highlight_line(&owned, &assets.syntaxes) {
                    Ok(runs) => runs
                        .into_iter()
                        .filter_map(|(style, text)| {
                            let text = text.trim_end_matches('\n');
                            (!text.is_empty())
                                .then(|| (to_color(style.foreground), text.to_string()))
                        })
                        .collect(),
                    // A highlighting failure must never break rendering: fall back
                    // to a single unstyled run carrying the original text.
                    Err(_) => vec![(Color::Reset, (*line).to_string())],
                }
            })
            .collect()
    }
}

/// Map a syntect RGBA colour to a ratatui truecolor. The alpha channel is the
/// theme's selection/foreground blend hint, irrelevant to opaque terminal text.
fn to_color(c: syntect::highlighting::Color) -> Color {
    Color::Rgb(c.r, c.g, c.b)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unknown_extension_has_no_highlighter() {
        assert!(Highlighter::for_path("notes.unknownext").is_none());
        assert!(Highlighter::for_path("Makefile-with-no-ext").is_none());
    }

    #[test]
    fn rust_keyword_gets_a_distinct_colour() {
        let hl = Highlighter::for_path("src/lib.rs").expect("rust syntax");
        let line = hl.hunk(&["let x = 1;"]).pop().unwrap();

        // The runs reconstruct the original line exactly — only colour is added.
        let text: String = line.iter().map(|(_, t)| t.as_str()).collect();
        assert_eq!(text, "let x = 1;");

        // The `let` keyword is coloured, and not every run shares one colour
        // (i.e. the keyword and the rest are distinguished).
        let colours: std::collections::HashSet<_> = line.iter().map(|(c, _)| *c).collect();
        assert!(
            colours.len() > 1,
            "expected multiple colours, got {colours:?}"
        );
        assert!(
            line.iter().all(|(c, _)| matches!(c, Color::Rgb(..))),
            "every run should carry a concrete truecolor"
        );
    }

    #[test]
    fn highlighting_preserves_every_line_and_its_text() {
        let hl = Highlighter::for_path("a.rs").unwrap();
        let lines = ["fn main() {", "    let s = \"hi\";", "}"];
        let out = hl.hunk(&lines);
        assert_eq!(out.len(), lines.len());
        for (orig, runs) in lines.iter().zip(&out) {
            let text: String = runs.iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(&text, orig);
        }
    }
}
