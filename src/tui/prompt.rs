//! The modal yes/no/… prompt overlay.
//!
//! A [`Prompt`] is a question plus a fixed set of labelled options the user picks
//! between. It is opened from Lua via `mudpuppy.prompt(message, options)`: the
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

/// An open prompt: the question, the option labels, and which one is highlighted.
pub(crate) struct Prompt {
    pub(crate) message: String,
    pub(crate) options: Vec<String>,
    /// Highlighted option index, moved with the arrow / h-l / j-k keys.
    pub(crate) selected: usize,
}

impl App {
    /// Open a prompt with `message` and the given option `labels`. An empty label
    /// list is ignored (the engine guarantees at least one).
    pub(crate) fn open_prompt(&mut self, message: String, labels: Vec<String>) {
        if labels.is_empty() {
            return;
        }
        self.prompt = Some(Prompt {
            message,
            options: labels,
            selected: 0,
        });
    }

    /// Feed one key event to the open prompt. Returns `Some(index)` when the user
    /// commits to an option (Enter on the highlighted one, or a `1`–`9` digit
    /// selecting directly) — the caller runs that option's callback through the
    /// engine. Returns `None` while the prompt stays open, when it is dismissed
    /// with Esc (no callback runs), or when no prompt is open.
    pub(crate) fn handle_prompt_key(&mut self, ev: KeyEvent) -> Option<usize> {
        let prompt = self.prompt.as_mut()?;
        let n = prompt.options.len();
        match ev.code {
            // Dismiss without choosing — the script's "do nothing" path.
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let chosen = prompt.selected;
                self.prompt = None;
                return Some(chosen);
            }
            KeyCode::Left | KeyCode::Up | KeyCode::Char('h') | KeyCode::Char('k') => {
                prompt.selected = prompt.selected.saturating_sub(1);
            }
            KeyCode::Right
            | KeyCode::Down
            | KeyCode::Tab
            | KeyCode::Char('l')
            | KeyCode::Char('j') => {
                if prompt.selected + 1 < n {
                    prompt.selected += 1;
                }
            }
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
