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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use ratatui::style::Color;
use syntect::easy::HighlightLines;
use syntect::highlighting::Theme;
use syntect::parsing::{SyntaxReference, SyntaxSet};

/// One highlighted line: contiguous `(colour, text)` runs whose concatenated
/// text is exactly the original line (so the rendered characters never change,
/// only their colour — see TESTING.md on why that keeps the `.snap` grid stable).
pub type HlLine = Vec<(Color, String)>;

/// One contiguous block of lines to highlight off the UI thread, plus the
/// base-view row indices each highlighted line fills. Parse state is local to a
/// segment (the `hunk` reset boundary), so each block stands alone — a hunk
/// body, a revealed gap edge, or a synthetic file's whole content.
pub struct HlSegment {
    /// Indices into the view's rows of the `Row::Line` rows this segment's
    /// highlighted lines fill, aligned 1:1 with `texts`.
    pub row_indices: Vec<usize>,
    /// The raw line texts to highlight (without `+`/`-`/space markers).
    pub texts: Vec<String>,
}

/// A whole-file highlight job handed to the background worker: the path (and a
/// first line for shebang resolution) to pick the language, the segments to
/// colour, and the `generation` that requested it so a stale result can be
/// dropped once a newer structure supersedes it.
pub struct HighlightRequest {
    pub path: String,
    pub first_line: Option<String>,
    pub generation: u64,
    pub segments: Vec<HlSegment>,
}

/// The worker's output: the per-row colour fills to write into the base view,
/// tagged with the `generation` of the request that produced them.
pub struct HighlightResult {
    pub generation: u64,
    /// `(base row index, highlighted line)` pairs.
    pub fills: Vec<(usize, HlLine)>,
}

/// Highlight every segment of `req`, resolving the language once from its path.
/// Pure compute (no runtime), so it runs on a `spawn_blocking` worker and is
/// directly unit-testable. Returns empty `fills` when the language is unknown
/// (rows then stay in their plain per-kind colour). Between segments it checks
/// `cancel` — the latest wanted generation — and bails early when this job has
/// been superseded by a file switch or expansion.
pub fn run_request(req: &HighlightRequest, cancel: &AtomicU64) -> HighlightResult {
    let mut fills = Vec::new();
    let Some(hl) = Highlighter::for_path(&req.path, req.first_line.as_deref()) else {
        return HighlightResult {
            generation: req.generation,
            fills,
        };
    };
    for seg in &req.segments {
        if cancel.load(Ordering::Relaxed) != req.generation {
            break;
        }
        let texts: Vec<&str> = seg.texts.iter().map(String::as_str).collect();
        for (idx, line) in seg.row_indices.iter().zip(hl.hunk(&texts)) {
            fills.push((*idx, line));
        }
    }
    HighlightResult {
        generation: req.generation,
        fills,
    }
}

/// The loaded syntect assets: the syntax definitions and the one theme we colour
/// with. Built once and shared.
struct Assets {
    syntaxes: SyntaxSet,
    theme: Theme,
}

fn assets() -> &'static Assets {
    static ASSETS: OnceLock<Assets> = OnceLock::new();
    ASSETS.get_or_init(|| {
        // `two-face` bundles the Sublime/bat syntax and theme set, far broader
        // than syntect's small built-in defaults — so unfamiliar languages get
        // real highlighting instead of falling back to flat per-kind colour.
        // The `_newlines` variant's rules expect a trailing `\n`, which is what
        // `hunk` feeds in for correct end-of-line tokenization.
        let syntaxes = two_face::syntax::extra_newlines();
        // A dark theme that reads well on the TUI's dark background.
        let theme = two_face::theme::extra()
            .get(two_face::theme::EmbeddedThemeName::Base16OceanDark)
            .clone();
        Assets { syntaxes, theme }
    })
}

/// A language-resolved highlighter for a single file. Cheap to hold (it borrows
/// the shared assets); build one per opened file via [`Highlighter::for_path`].
pub struct Highlighter {
    syntax: &'static SyntaxReference,
}

impl Highlighter {
    /// Resolve the language for `path`, or `None` when nothing matches — the
    /// caller then renders the file without highlighting. Tries the file
    /// extension first, then `first_line`'s content (e.g. a `#!` shebang) so a
    /// known interpreter still highlights when the extension is missing or
    /// unrecognized.
    pub fn for_path(path: &str, first_line: Option<&str>) -> Option<Highlighter> {
        let assets = assets();
        let syntax = Path::new(path)
            .extension()
            .and_then(|ext| ext.to_str())
            .and_then(|ext| assets.syntaxes.find_syntax_by_extension(ext))
            .or_else(|| first_line.and_then(|l| assets.syntaxes.find_syntax_by_first_line(l)))?;
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
        assert!(Highlighter::for_path("notes.unknownext", None).is_none());
        assert!(Highlighter::for_path("Makefile-with-no-ext", None).is_none());
    }

    #[test]
    fn first_line_resolves_when_extension_is_unknown() {
        // No usable extension, but a `#!` shebang names the interpreter.
        let hl = Highlighter::for_path("script", Some("#!/usr/bin/env python3"));
        assert!(hl.is_some(), "shebang should resolve a syntax");
    }

    #[test]
    fn common_non_rust_languages_resolve() {
        // two-face's broad set covers languages syntect's defaults miss; these
        // are the kind of files that used to render flat green/red.
        for path in ["main.go", "app.py", "lib.rb", "index.ts", "config.toml"] {
            assert!(
                Highlighter::for_path(path, None).is_some(),
                "{path} should resolve to a syntax"
            );
        }
    }

    #[test]
    fn rust_keyword_gets_a_distinct_colour() {
        let hl = Highlighter::for_path("src/lib.rs", None).expect("rust syntax");
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
    fn run_request_fills_rust_segments_with_truecolor() {
        // A two-line Rust segment mapped onto base rows 3 and 4. The worker
        // resolves the language from the path and returns one fill per line.
        let req = HighlightRequest {
            path: "src/lib.rs".to_string(),
            first_line: None,
            generation: 7,
            segments: vec![HlSegment {
                row_indices: vec![3, 4],
                texts: vec!["fn main() {".to_string(), "    let x = 1;".to_string()],
            }],
        };
        let cancel = AtomicU64::new(7);
        let result = run_request(&req, &cancel);
        assert_eq!(result.generation, 7);
        // One fill per input line, mapped to the requested base rows.
        let idxs: Vec<usize> = result.fills.iter().map(|(i, _)| *i).collect();
        assert_eq!(idxs, vec![3, 4]);
        // Every run carries a concrete truecolor (syntect output, not a fallback).
        assert!(result
            .fills
            .iter()
            .flat_map(|(_, line)| line)
            .all(|(c, _)| matches!(c, Color::Rgb(..))));
    }

    #[test]
    fn run_request_bails_when_superseded() {
        // A request whose generation no longer matches `cancel` yields nothing.
        let req = HighlightRequest {
            path: "src/lib.rs".to_string(),
            first_line: None,
            generation: 1,
            segments: vec![HlSegment {
                row_indices: vec![0],
                texts: vec!["let x = 1;".to_string()],
            }],
        };
        let cancel = AtomicU64::new(2);
        let result = run_request(&req, &cancel);
        assert!(result.fills.is_empty());
    }

    #[test]
    fn run_request_unknown_language_yields_no_fills() {
        let req = HighlightRequest {
            path: "notes.unknownext".to_string(),
            first_line: None,
            generation: 0,
            segments: vec![HlSegment {
                row_indices: vec![0],
                texts: vec!["plain text".to_string()],
            }],
        };
        let cancel = AtomicU64::new(0);
        assert!(run_request(&req, &cancel).fills.is_empty());
    }

    #[test]
    fn highlighting_preserves_every_line_and_its_text() {
        let hl = Highlighter::for_path("a.rs", None).unwrap();
        let lines = ["fn main() {", "    let s = \"hi\";", "}"];
        let out = hl.hunk(&lines);
        assert_eq!(out.len(), lines.len());
        for (orig, runs) in lines.iter().zip(&out) {
            let text: String = runs.iter().map(|(_, t)| t.as_str()).collect();
            assert_eq!(&text, orig);
        }
    }
}
