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

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::App;

/// An open prompt: the question, the option labels, and which one is highlighted.
pub(crate) struct Prompt {
    pub(crate) message: String,
    pub(crate) options: Vec<String>,
    /// Highlighted option index, moved with the arrow / h-l keys (and j-k when
    /// there is no scrollable body).
    pub(crate) selected: usize,
    /// An optional scrollable body shown above the options (e.g. an update's
    /// changelog), rendered help-style. When present, `j`/`k` scroll it.
    pub(crate) body: Option<String>,
    /// First visible body line, paged like the help overlay.
    pub(crate) body_scroll: usize,
    /// Body line count and viewport height from the last render, used to clamp
    /// `body_scroll` and detect the bottom. Set by the renderer.
    pub(crate) body_total: usize,
    pub(crate) body_height: usize,
}

impl Prompt {
    /// Whether this prompt carries a scrollable body (which changes the key map:
    /// `j`/`k` scroll the body instead of moving between options).
    pub(crate) fn has_body(&self) -> bool {
        self.body.is_some()
    }

    /// The last body scroll position that still fills the viewport.
    pub(crate) fn max_body_scroll(&self) -> usize {
        self.body_total.saturating_sub(self.body_height)
    }

    fn scroll_body(&mut self, delta: isize) {
        let next = self.body_scroll as isize + delta;
        self.body_scroll = next.clamp(0, self.max_body_scroll() as isize) as usize;
    }
}

impl App {
    /// Open a prompt with `message` and the given option `labels`, optionally with
    /// a scrollable `body` (help-style) above the options — the update flow uses the
    /// body to show the release changelog. An empty label list is ignored (the
    /// engine guarantees at least one).
    pub(crate) fn open_prompt_with_body(
        &mut self,
        message: String,
        labels: Vec<String>,
        body: Option<String>,
    ) {
        if labels.is_empty() {
            return;
        }
        self.prompt = Some(Prompt {
            message,
            options: labels,
            selected: 0,
            body,
            body_scroll: 0,
            body_total: 0,
            body_height: 1,
        });
    }

    /// Feed one key event to the open prompt. Returns `Some(index)` when the user
    /// commits to an option (Enter on the highlighted one, or a `1`–`9` digit
    /// selecting directly) — the caller runs that option's callback through the
    /// engine. Returns `None` while the prompt stays open, when it is dismissed
    /// with Esc (no callback runs), or when no prompt is open.
    ///
    /// When the prompt has a scrollable body, `j`/`k`/`PgUp`/`PgDn`/`ctrl-d/u`/`g`/`G`
    /// scroll it (mirroring the help overlay) and option selection moves with the
    /// arrows / `h`-`l`; without a body, `j`/`k` move the selection as before.
    pub(crate) fn handle_prompt_key(&mut self, ev: KeyEvent) -> Option<usize> {
        let prompt = self.prompt.as_mut()?;
        let n = prompt.options.len();
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let body = prompt.has_body();
        match ev.code {
            // Dismiss without choosing — the script's "do nothing" path.
            KeyCode::Esc => self.prompt = None,
            KeyCode::Enter => {
                let chosen = prompt.selected;
                self.prompt = None;
                return Some(chosen);
            }
            // Body scrolling (only when there is a body).
            KeyCode::Char('j') | KeyCode::Down if body => prompt.scroll_body(1),
            KeyCode::Char('k') | KeyCode::Up if body => prompt.scroll_body(-1),
            KeyCode::PageDown | KeyCode::Char(' ') if body => {
                prompt.scroll_body(prompt.body_height as isize);
            }
            KeyCode::PageUp if body => prompt.scroll_body(-(prompt.body_height as isize)),
            KeyCode::Char('d') if body && ctrl => {
                prompt.scroll_body((prompt.body_height / 2) as isize);
            }
            KeyCode::Char('u') if body && ctrl => {
                prompt.scroll_body(-((prompt.body_height / 2) as isize));
            }
            KeyCode::Char('g') if body => prompt.body_scroll = 0,
            KeyCode::Char('G') if body => prompt.body_scroll = prompt.max_body_scroll(),
            // Option selection. `j`/`k` only move the selection when there is no
            // body (otherwise they scroll, handled above).
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
