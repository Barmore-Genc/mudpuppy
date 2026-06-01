//! The modal comment composer overlay.
//!
//! [`Composer`] is the human's authoring surface: a small text buffer plus a
//! severity and an optional tag, bound to a [`ComposerTarget`] captured when it
//! was opened (a line, a region, the whole file, a reply, or an edit). Key input
//! is captured in Rust before it reaches Lua — the same precedence the picker
//! uses (see [`App::handle_composer_key`]). On save it calls the matching
//! `annotate` method; on cancel it just closes.
//!
//! The composer-opening verbs (`add_comment`, `comment_file`, `reply`,
//! `edit_comment`) live here too, since they construct the [`Composer`] from the
//! cursor/selection anchor.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use super::app::App;
use crate::domain::{Severity, Side, Tag};

/// What a [`Composer`] will create or modify when saved.
pub(crate) enum ComposerTarget {
    /// A new line or whole-line region comment anchored at `(side, line)` (with
    /// `end_line` set for a region).
    Line {
        side: Side,
        line: u32,
        end_line: Option<u32>,
    },
    /// A new comment about the whole file.
    File,
    /// A threaded reply under an existing annotation `id`.
    Reply { parent: String },
    /// An in-place edit of the human's own annotation `id`.
    Edit { id: String },
}

/// The composer overlay's state.
pub(crate) struct Composer {
    /// The markdown body being authored (multi-line; `enter` inserts a newline).
    pub(crate) body: String,
    pub(crate) severity: Severity,
    pub(crate) tag: Option<Tag>,
    /// What gets written on save.
    pub(crate) target: ComposerTarget,
    /// The file the comment lives on, for the overlay header.
    pub(crate) file: String,
}

impl Composer {
    /// A fresh composer for `target` on `file`, defaulting to a `Suggestion` with
    /// no tag and an empty body.
    pub(crate) fn new(target: ComposerTarget, file: String) -> Composer {
        Composer {
            body: String::new(),
            severity: Severity::Suggestion,
            tag: None,
            target,
            file,
        }
    }

    /// A one-line summary of what the composer is anchored to, for the header.
    pub(crate) fn anchor_label(&self) -> String {
        match &self.target {
            ComposerTarget::Line {
                line,
                end_line: Some(end),
                ..
            } if end != line => format!("L{line}–{end}"),
            ComposerTarget::Line { line, .. } => format!("L{line}"),
            ComposerTarget::File => "(whole file)".to_string(),
            ComposerTarget::Reply { parent } => format!("reply to {parent}"),
            ComposerTarget::Edit { id } => format!("edit {id}"),
        }
    }

    fn cycle_severity(&mut self) {
        self.severity = match self.severity {
            Severity::Info => Severity::Suggestion,
            Severity::Suggestion => Severity::Warning,
            Severity::Warning => Severity::Blocker,
            Severity::Blocker => Severity::Info,
        };
    }

    fn cycle_tag(&mut self) {
        self.tag = match self.tag {
            None => Some(Tag::Question),
            Some(Tag::Question) => Some(Tag::Concern),
            Some(Tag::Concern) => Some(Tag::Direction),
            Some(Tag::Direction) => None,
        };
    }
}

impl App {
    /// Feed one key event to the open composer. Returns `true` if it was consumed
    /// (the composer was open); `false` lets the caller route the key elsewhere.
    ///
    /// Mirrors `handle_picker_key`: captured in Rust before Lua. `Esc` cancels,
    /// `Ctrl-S` saves, `Ctrl-E`/`Ctrl-T` cycle severity/tag, everything else
    /// edits the body.
    pub(crate) fn handle_composer_key(&mut self, ev: KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        if self.composer.is_none() {
            return false;
        }
        match ev.code {
            KeyCode::Esc => self.composer = None,
            KeyCode::Char('s') if ctrl => self.save_composer(),
            KeyCode::Char('e') if ctrl => {
                if let Some(c) = self.composer.as_mut() {
                    c.cycle_severity();
                }
            }
            KeyCode::Char('t') if ctrl => {
                if let Some(c) = self.composer.as_mut() {
                    c.cycle_tag();
                }
            }
            KeyCode::Enter => {
                if let Some(c) = self.composer.as_mut() {
                    c.body.push('\n');
                }
            }
            KeyCode::Backspace => {
                if let Some(c) = self.composer.as_mut() {
                    c.body.pop();
                }
            }
            KeyCode::Char(ch) if !ctrl => {
                if let Some(c) = self.composer.as_mut() {
                    c.body.push(ch);
                }
            }
            _ => {}
        }
        true
    }

    /// Persist the open composer through the matching `annotate` method, then
    /// close it. A blank body is treated as a cancel so an accidental open leaves
    /// nothing behind.
    fn save_composer(&mut self) {
        let Some(composer) = self.composer.take() else {
            return;
        };
        if composer.body.trim().is_empty() {
            self.notice = Some("empty comment discarded".to_string());
            return;
        }
        let Composer {
            body,
            severity,
            tag,
            target,
            ..
        } = composer;
        match target {
            ComposerTarget::Line {
                side,
                line,
                end_line,
            } => self.add_annotation(side, line, end_line, severity, tag, body),
            ComposerTarget::File => self.write_file_comment(severity, tag, body),
            ComposerTarget::Reply { parent } => self.write_reply(parent, severity, tag, body),
            ComposerTarget::Edit { id } => self.edit_annotation(id, body, severity, tag),
        }
    }

    /// Open the composer on the cursor line (or the visual selection, as a
    /// whole-line region). A no-op with a hint when the cursor is on a non-line
    /// row (a hunk header, an expander, or a notice).
    pub(crate) fn add_comment(&mut self) {
        match self.anchor_for_comment() {
            Some((side, line, end_line)) => {
                let file = self.current().display_path().to_string();
                self.composer = Some(Composer::new(
                    ComposerTarget::Line {
                        side,
                        line,
                        end_line,
                    },
                    file,
                ));
                self.selection_anchor = None;
            }
            None => self.notice = Some("no diff line under the cursor to comment on".to_string()),
        }
    }

    /// Open the composer for a whole-file comment.
    pub(crate) fn comment_file(&mut self) {
        let file = self.current().display_path().to_string();
        self.composer = Some(Composer::new(ComposerTarget::File, file));
        self.selection_anchor = None;
    }

    /// Open the composer as a reply under the annotation on the cursor line. A
    /// no-op with a hint when no annotation is anchored there.
    pub(crate) fn reply(&mut self) {
        match self.annotation_id_at_cursor() {
            Some(parent) => {
                let file = self.current().display_path().to_string();
                self.composer = Some(Composer::new(ComposerTarget::Reply { parent }, file));
            }
            None => self.notice = Some("no annotation on this line to reply to".to_string()),
        }
    }

    /// Open the composer to edit the human's own annotation on the cursor line,
    /// prefilled with its current body/severity/tag. A no-op (with a hint) when
    /// there is no such annotation or it is the agent's.
    pub(crate) fn edit_comment(&mut self) {
        let Some(id) = self.annotation_id_at_cursor() else {
            self.notice = Some("no annotation on this line to edit".to_string());
            return;
        };
        let Some(a) = self.annotations.iter().find(|a| a.id == id) else {
            return;
        };
        if a.author != crate::domain::Author::Human {
            self.notice = Some("can only edit your own annotations".to_string());
            return;
        }
        let file = self.current().display_path().to_string();
        let mut composer = Composer::new(ComposerTarget::Edit { id }, file);
        composer.body = a.body.clone();
        composer.severity = a.severity;
        composer.tag = a.tag;
        self.composer = Some(composer);
    }
}
