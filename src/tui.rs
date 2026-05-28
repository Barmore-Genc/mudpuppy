//! The ratatui application: file tree, diff pane, status bar, and side panel,
//! with virtualized rendering and live reload from the store (PLAN.md §9).
//!
//! Not built yet. The renderer (ratatui + crossterm), filesystem watch
//! (`notify`), and syntax highlighting (`syntect`) dependencies land with this
//! milestone.

use anyhow::{bail, Result};

/// Launch the interactive review UI.
///
/// `pr` selects a pull-request target (`owner/repo#123` or a URL) when present;
/// otherwise the review targets local changes. `base` overrides the inferred
/// base ref for local reviews.
pub fn launch(_pr: Option<String>, _base: Option<String>) -> Result<()> {
    bail!("the TUI is not implemented yet");
}
