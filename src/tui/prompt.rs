//! The modal yes/no/… prompt overlay.
//!
//! A [`Prompt`] is a question plus a fixed set of labelled options the user picks
//! between, and an optional scrollable `details` body (e.g. a release changelog).
//! It is opened from Lua via `mudpuppy.prompt(message, options[, details])`: the
//! labels render here, while the matching callbacks live in the scripting engine
//! and run when the user chooses (see [`crate::lua::LuaEngine::run_prompt`]). The
//! auto-update flow in `core.luau` is the first caller, but the overlay is a
//! general-purpose primitive scripts can reuse.
//!
//! Like the composer, picker, and `:command` palette, the prompt has its own
//! keymap mode (`prompt`), so every key it responds to is rebindable from Lua.
//! Choosing an option stages the index on `App` (see `App::choose_prompt`) for
//! the event loop to hand back to the engine, since the engine is already
//! borrowed while the binding that chose runs.

use super::app::App;

/// An open prompt: the question, an optional details body, the option labels, and
/// which one is highlighted.
pub(crate) struct Prompt {
    pub(crate) message: String,
    /// Optional body shown between the question and the options (Markdown text,
    /// styled and wrapped by `super::markdown`). Scrolled with up/down when it
    /// overflows.
    pub(crate) details: Option<String>,
    /// Scroll offset (in wrapped display lines) into `details`. The renderer
    /// clamps the effective offset to the real content height each frame.
    pub(crate) scroll: u16,
    pub(crate) options: Vec<String>,
    /// Highlighted option index, moved by the `prompt` mode's bindings.
    pub(crate) selected: usize,
}

impl Prompt {
    /// Whether this prompt has a details body (which makes up/down scroll it
    /// instead of moving the option selection).
    pub(super) fn has_details(&self) -> bool {
        self.details.is_some()
    }
}

impl App {
    /// Open a prompt with `message`, the given option `labels`, and an optional
    /// `details` body. An empty label list is ignored (the engine guarantees at
    /// least one).
    pub(crate) fn open_prompt(
        &mut self,
        message: String,
        labels: Vec<String>,
        details: Option<String>,
    ) {
        if labels.is_empty() {
            return;
        }
        self.prompt = Some(Prompt {
            message,
            details,
            scroll: 0,
            options: labels,
            selected: 0,
        });
    }

    /// Close the prompt without choosing — the script's "do nothing" path.
    pub(crate) fn close_prompt(&mut self) {
        self.prompt = None;
    }

    /// Move the highlighted option by `delta`, clamped to the option list.
    pub(crate) fn prompt_move(&mut self, delta: i64) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        let last = prompt.options.len().saturating_sub(1) as i64;
        let next = (prompt.selected as i64 + delta).clamp(0, last);
        prompt.selected = next as usize;
    }

    /// Scroll the prompt's details body by `delta` lines. A no-op when the prompt
    /// has no body; the renderer clamps the offset to the real content height.
    pub(crate) fn prompt_scroll(&mut self, delta: i64) {
        let Some(prompt) = self.prompt.as_mut() else {
            return;
        };
        if !prompt.has_details() {
            return;
        }
        let next = (prompt.scroll as i64 + delta).max(0);
        prompt.scroll = next.min(u16::MAX as i64) as u16;
    }

    /// The highlighted option's index, or `None` when no prompt is open.
    pub(crate) fn prompt_selected(&self) -> Option<usize> {
        self.prompt.as_ref().map(|p| p.selected)
    }

    /// How many options the open prompt offers (`0` when none is open).
    pub(crate) fn prompt_len(&self) -> usize {
        self.prompt.as_ref().map_or(0, |p| p.options.len())
    }

    /// Whether the open prompt has a scrollable details body.
    pub(crate) fn prompt_has_details(&self) -> bool {
        self.prompt.as_ref().is_some_and(Prompt::has_details)
    }
}
