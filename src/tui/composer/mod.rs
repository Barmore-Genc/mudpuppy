//! The modal comment composer overlay.
//!
//! [`Composer`] is the user's authoring surface: a small text buffer plus a
//! severity and an optional tag, bound to a [`ComposerTarget`] captured when it
//! was opened (a line, a region, the whole file, a reply, or an edit). While it
//! is open the keymap switches to the `composer-insert` / `composer-normal` modes
//! (both falling back to `composer`), so every command key is rebindable from the
//! config; keys no binding claims fall through to `composer_fallback_key`. On save
//! it calls the matching `annotate` method; on cancel it just closes.
//!
//! Editing is vim-like and modal. The composer opens in [`Mode::Insert`] (cursor
//! at the end of any prefilled body) so typing works immediately; `Esc` drops to
//! [`Mode::Normal`], where `Enter` saves and a broad subset of vim drives the
//! buffer — motions (`h`/`j`/`k`/`l`, `w`/`b`/`e`/`W`/`B`/`E`, `0`/`^`/`$`,
//! `gg`/`G`, `f`/`F`/`t`/`T` and `;`/`,`, with count prefixes), operators
//! (`d`/`c`/`y` over any motion, plus `dd`/`cc`/`yy` and `D`/`C`/`Y`/`S`),
//! single-key edits (`x`/`X`/`s`/`r`/`~`/`J`/`p`/`P`) and undo/redo (`u`,
//! `Ctrl-R`). The vim engine itself lives in the `vim` submodule, and owns every
//! key the keymap leaves unbound. `Ctrl-S` (save), `Ctrl-E`/`Ctrl-T`
//! (severity/tag) and `Ctrl-J` (newline) are bound in `composer`, so they work in
//! either mode.
//!
//! The composer-opening verbs (`add_comment`, `comment_file`, `reply`,
//! `edit_comment`) live here too, since they construct the [`Composer`] from the
//! cursor/selection anchor.

mod vim;

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
    /// An in-place edit of the user's own annotation `id`.
    Edit { id: String },
}

/// Which vim-like editing mode the composer is in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Mode {
    /// Keys type into the buffer; `Esc` drops to [`Mode::Normal`].
    Insert,
    /// Keys are motions/operators; `Enter` saves, `i`/`a`/`o` re-enter insert.
    Normal,
}

/// The composer overlay's state.
pub(crate) struct Composer {
    /// The markdown body, split into lines (always at least one, possibly empty);
    /// joined with `\n` by [`Composer::body`] on save.
    pub(crate) lines: Vec<String>,
    /// Cursor row into `lines`.
    pub(crate) row: usize,
    /// Cursor column as a *char* offset within `lines[row]` (may equal the line
    /// length, i.e. one past the last char).
    pub(crate) col: usize,
    pub(crate) mode: Mode,
    /// A pending normal-mode operator (`d`/`c`/`y`) awaiting its motion, or a
    /// repeat of itself for the line-wise `dd`/`cc`/`yy`.
    pending_op: Option<vim::Operator>,
    /// A pending `f`/`F`/`t`/`T` awaiting the character to search for.
    pending_find: Option<vim::PendingFind>,
    /// Saw a leading `g`, awaiting the second key (e.g. `gg`).
    pending_g: bool,
    /// Saw `r`, awaiting the replacement character.
    pending_replace: bool,
    /// The count prefix accumulated for the next motion/operator (`3w`, `d2j`).
    count: Option<usize>,
    /// The count captured when an operator was pressed; multiplied with `count`
    /// so `2d3w` deletes six words.
    op_count: Option<usize>,
    /// The last `f`/`t` search, for `;`/`,` repeats.
    last_find: Option<vim::LastFind>,
    /// The yank/delete register, replayed by `p`/`P`.
    register: vim::Register,
    /// Buffer snapshots for `u`; cleared on a fresh edit.
    undo: Vec<vim::Snapshot>,
    /// Snapshots popped by `u`, replayed by `Ctrl-R`.
    redo: Vec<vim::Snapshot>,
    pub(crate) severity: Severity,
    pub(crate) tag: Option<Tag>,
    /// What gets written on save.
    pub(crate) target: ComposerTarget,
    /// The file the comment lives on, for the overlay header.
    pub(crate) file: String,
}

impl Composer {
    /// A fresh composer for `target` on `file`, defaulting to a `Suggestion` with
    /// no tag and an empty body, ready to type into ([`Mode::Insert`]).
    pub(crate) fn new(target: ComposerTarget, file: String) -> Composer {
        Composer {
            lines: vec![String::new()],
            row: 0,
            col: 0,
            mode: Mode::Insert,
            pending_op: None,
            pending_find: None,
            pending_g: false,
            pending_replace: false,
            count: None,
            op_count: None,
            last_find: None,
            register: vim::Register::default(),
            undo: Vec::new(),
            redo: Vec::new(),
            severity: Severity::Suggestion,
            tag: None,
            target,
            file,
        }
    }

    /// Replace the buffer with `body`, placing the cursor at the very end. Used to
    /// prefill an edit so further typing appends, as the old single-`String`
    /// composer did.
    fn set_body(&mut self, body: &str) {
        self.lines = body.split('\n').map(str::to_string).collect();
        if self.lines.is_empty() {
            self.lines.push(String::new());
        }
        self.row = self.lines.len() - 1;
        self.col = self.lines[self.row].chars().count();
    }

    /// The buffer as a single string, for saving.
    pub(crate) fn body(&self) -> String {
        self.lines.join("\n")
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

    /// Char count of the cursor's line.
    fn line_len(&self) -> usize {
        self.lines[self.row].chars().count()
    }

    /// Byte offset of char index `col` within the cursor's line (its byte length
    /// when `col` is at or past the end).
    fn byte_at(&self, col: usize) -> usize {
        let line = &self.lines[self.row];
        line.char_indices()
            .nth(col)
            .map(|(b, _)| b)
            .unwrap_or(line.len())
    }
}

impl App {
    /// An edit may have grown or shrunk the body, so after any key that reached
    /// the composer, rebuild the view (the inline placeholder reserves height for
    /// the box) and scroll the box fully into view. A no-op once the composer has
    /// closed — save and cancel rebuild on their own.
    pub(crate) fn refresh_composer_view(&mut self) {
        if self.composer.is_some() {
            self.rebuild_view();
            self.focus_composer_row();
        }
    }

    /// The Rust fallback for a composer key no binding claimed: in insert mode a
    /// printable character is typed into the buffer; in normal mode the key goes
    /// to the vim engine (motions, operators, `f`/`r`/`g` arguments, counts).
    pub(super) fn composer_fallback_key(&mut self, ev: KeyEvent) -> bool {
        let ctrl = ev.modifiers.contains(KeyModifiers::CONTROL);
        let Some(composer) = self.composer.as_mut() else {
            return false;
        };
        match composer.mode {
            Mode::Insert => match ev.code {
                KeyCode::Char(ch) if !ctrl => composer.insert_char(ch),
                _ => return false,
            },
            Mode::Normal => composer.normal_key(ev),
        }
        true
    }

    /// The keymap mode for the open composer, or `None` when it is closed. Drives
    /// `lua::active_mode`, so a binding can be scoped to insert or normal mode
    /// (or to `composer`, the fallback shared by both).
    pub(crate) fn composer_mode(&self) -> Option<crate::lua::keys::Mode> {
        use crate::lua::keys::Mode as KeyMode;
        Some(match self.composer.as_ref()?.mode {
            Mode::Insert => KeyMode::ComposerInsert,
            Mode::Normal => KeyMode::ComposerNormal,
        })
    }

    /// Whether the composer is mid-way through a multi-key normal-mode command
    /// (an operator, an `f`/`r`/`g` awaiting its argument, or a count). Exposed to
    /// Lua so the `enter`/`esc` bindings can leave those keys to the in-flight
    /// command instead of saving/cancelling.
    pub(crate) fn composer_pending(&self) -> bool {
        self.composer.as_ref().is_some_and(|c| c.has_pending())
    }

    /// Abandon an in-flight normal-mode command, keeping the composer open.
    pub(crate) fn composer_clear_pending(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.clear_pending();
        }
    }

    /// Leave insert mode for normal mode.
    pub(crate) fn composer_leave_insert(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.leave_insert();
        }
    }

    /// Insert a line break at the cursor (works in either mode).
    pub(crate) fn composer_newline(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.insert_newline();
        }
    }

    /// Delete the character before the cursor.
    pub(crate) fn composer_backspace(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.backspace();
        }
    }

    /// Step the composer's severity to the next value.
    pub(crate) fn composer_cycle_severity(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.cycle_severity();
        }
    }

    /// Step the composer's tag to the next value.
    pub(crate) fn composer_cycle_tag(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.cycle_tag();
        }
    }

    /// Redo the last undone edit.
    pub(crate) fn composer_redo(&mut self) {
        if let Some(c) = self.composer.as_mut() {
            c.redo();
        }
    }

    /// Persist the open composer through the matching `annotate` method, then
    /// close it. A blank body is treated as a cancel so an accidental open leaves
    /// nothing behind.
    pub(crate) fn save_composer(&mut self) {
        let Some(composer) = self.composer.take() else {
            return;
        };
        let body = composer.body();
        if body.trim().is_empty() {
            self.notice = Some("empty comment discarded".to_string());
            // Drop the inline placeholder rows the discarded composer reserved
            // (it was already taken out of `self.composer` above).
            self.rebuild_view();
            return;
        }
        let Composer {
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
                self.rebuild_view();
                self.focus_composer_row();
            }
            None => self.notice = Some("no diff line under the cursor to comment on".to_string()),
        }
    }

    /// Open the composer for a whole-file comment. The whole-file composer keeps
    /// its centered modal, so no inline placeholder is spliced.
    pub(crate) fn comment_file(&mut self) {
        let file = self.current().display_path().to_string();
        self.composer = Some(Composer::new(ComposerTarget::File, file));
        self.selection_anchor = None;
    }

    /// Open the composer as a reply under the annotation on the cursor line. A
    /// no-op with a hint when no annotation is anchored there.
    pub(crate) fn reply(&mut self) {
        match self.annotation_id_at_cursor() {
            Some(parent) => self.open_reply(parent),
            None => self.notice = Some("no annotation on this line to reply to".to_string()),
        }
    }

    /// Open a reply composer under annotation `parent` and scroll it into view.
    /// A reply to a reply threads under their shared parent, so the composer
    /// lands at the end of the thread and the conversation stays one level deep.
    pub(crate) fn open_reply(&mut self, parent: String) {
        let parent = self.thread_root(&parent);
        let file = self.current().display_path().to_string();
        self.composer = Some(Composer::new(ComposerTarget::Reply { parent }, file));
        self.rebuild_view();
        self.focus_composer_row();
    }

    /// Open the composer to edit the user's own annotation on the cursor line,
    /// prefilled with its current body/severity/tag. A no-op (with a hint) when
    /// there is no such annotation or it is the agent's.
    pub(crate) fn edit_comment(&mut self) {
        let Some(id) = self.annotation_id_at_cursor() else {
            self.notice = Some("no annotation on this line to edit".to_string());
            return;
        };
        self.open_edit(id);
    }

    /// Open the edit composer for annotation `id`, prefilled. Guards against
    /// editing the agent's annotations.
    pub(crate) fn open_edit(&mut self, id: String) {
        let Some(a) = self.annotations.iter().find(|a| a.id == id) else {
            return;
        };
        if a.author != crate::domain::Author::User {
            self.notice = Some("can only edit your own annotations".to_string());
            return;
        }
        let (body, severity, tag) = (a.body.clone(), a.severity, a.tag);
        let file = self.current().display_path().to_string();
        let mut composer = Composer::new(ComposerTarget::Edit { id }, file);
        composer.set_body(&body);
        composer.severity = severity;
        composer.tag = tag;
        self.composer = Some(composer);
        self.rebuild_view();
        self.focus_composer_row();
    }

    /// A double-click on a comment row: reply to it, or edit it when it is the
    /// user's own. Drives the mouse path in `dispatch_click`.
    pub(crate) fn reply_or_edit(&mut self, id: String) {
        let own = self
            .annotations
            .iter()
            .find(|a| a.id == id)
            .is_some_and(|a| a.author == crate::domain::Author::User);
        if own {
            self.open_edit(id);
        } else {
            self.open_reply(id);
        }
    }
}
