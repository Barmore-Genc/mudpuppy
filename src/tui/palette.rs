//! The viewer's core colour palette, kept in one place so the app background,
//! its highlight tints, and the dim text colour stay consistent across panes and
//! match the `mudpuppy debug colors` preview.
//!
//! These are the colours we fully control as truecolor RGB (as opposed to the
//! per-token syntax colours, which come from the syntax theme in `highlight`,
//! and the bright accents like `Color::Green`, which stay ANSI). Painting an
//! explicit background here is what lets the rest of the UI read predictably:
//! foregrounds no longer depend on whatever the terminal's default background
//! happens to be.

use ratatui::style::Color;

/// App background. Deliberately much darker than the syntax theme's own
/// background (`#2b303b`): the syntax tokens are mostly light, so a near-black
/// base gives them more contrast, and it opens headroom below the foreground for
/// the highlight tints to sit in without having to be bright.
pub(crate) const BG: Color = Color::Rgb(13, 16, 23);

/// Dim text: line numbers, metadata, help, separators, unfocused borders.
/// Replaces `Color::DarkGray`, whose terminal-palette value sat too close to the
/// background to read.
pub(crate) const FG_DIM: Color = Color::Rgb(140, 149, 168);

/// Subtle tints behind added / removed diff lines. The `+`/`-` marker is the
/// primary cue, but a faint coloured band makes additions and deletions
/// scannable without having to read the gutter. Kept dim so the syntax-coloured
/// text on top stays the focus; distinct in hue from the blue selection tints.
pub(crate) const BG_ADDED: Color = Color::Rgb(18, 36, 26);
pub(crate) const BG_REMOVED: Color = Color::Rgb(40, 20, 24);

/// The tree row under the cursor.
pub(crate) const BG_SELECTED_FILE: Color = Color::Rgb(26, 31, 43);
/// A line inside the visual selection span, and the selected annotation row.
pub(crate) const BG_SELECTION: Color = Color::Rgb(33, 41, 61);
/// The diff cursor line (drawn over a selection, so it stays a step brighter).
pub(crate) const BG_CURSOR: Color = Color::Rgb(45, 55, 82);
