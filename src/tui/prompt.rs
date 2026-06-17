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
//! Like the composer, picker, and `:command` palette, the prompt captures key
//! input ahead of the Lua keymap while it is open; [`App::handle_prompt_key`]
//! returns the chosen option index (which the event loop hands back to the engine)
//! or `None` while it stays open or is dismissed.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

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
    /// Highlighted option index, moved with the left/right (and Tab / h-l) keys.
    pub(crate) selected: usize,
}

impl Prompt {
    /// Whether this prompt has a details body (which makes up/down scroll it
    /// instead of moving the option selection).
    fn has_details(&self) -> bool {
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

    /// Feed one key event to the open prompt. Returns `Some(index)` when the user
    /// commits to an option (Enter on the highlighted one, or a `1`–`9` digit
    /// selecting directly) — the caller runs that option's callback through the
    /// engine. Returns `None` while the prompt stays open, when it is dismissed
    /// with Esc (no callback runs), or when no prompt is open.
    ///
    /// When the prompt has a details body, up/down (and `j`/`k`, PageUp/PageDown)
    /// scroll it; left/right (and Tab, `h`/`l`) always move the option selection.
    /// Without a body, up/down move the selection too, preserving the plain
    /// yes/no prompt's behavior.
    pub(crate) fn handle_prompt_key(&mut self, ev: KeyEvent) -> Option<usize> {
        let prompt = self.prompt.as_mut()?;
        let n = prompt.options.len();
        let has_details = prompt.has_details();
        match ev.code {
            // Dismiss without choosing — the script's "do nothing" path.
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let chosen = prompt.selected;
                self.prompt = None;
                return Some(chosen);
            }
            KeyCode::Left | KeyCode::Char('h') => {
                prompt.selected = prompt.selected.saturating_sub(1);
            }
            KeyCode::Right | KeyCode::Tab | KeyCode::Char('l') => {
                if prompt.selected + 1 < n {
                    prompt.selected += 1;
                }
            }
            // Up/down (and j/k) scroll the body when there is one; otherwise they
            // fall back to moving the option selection.
            KeyCode::Up | KeyCode::Char('k') => {
                if has_details {
                    prompt.scroll = prompt.scroll.saturating_sub(1);
                } else {
                    prompt.selected = prompt.selected.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if has_details {
                    prompt.scroll = prompt.scroll.saturating_add(1);
                } else if prompt.selected + 1 < n {
                    prompt.selected += 1;
                }
            }
            KeyCode::PageUp => prompt.scroll = prompt.scroll.saturating_sub(PROMPT_PAGE),
            KeyCode::PageDown => prompt.scroll = prompt.scroll.saturating_add(PROMPT_PAGE),
            // A digit picks (and commits to) that option directly: `1` is the
            // first option. `0` is left alone (there is no zeroth option).
            KeyCode::Char(c @ '1'..='9') => {
                let idx = c as usize - '1' as usize;
                if idx < n {
                    self.prompt = None;
                    return Some(idx);
                }
            }
            _ => {}
        }
        None
    }
}

/// Lines the body scrolls by on PageUp/PageDown. A coarse step; the renderer
/// clamps the result to the real content height so over-scrolling is harmless.
const PROMPT_PAGE: u16 = 10;
