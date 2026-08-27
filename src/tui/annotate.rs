//! The user's annotation mutations, authored from inside the viewer.
//!
//! Each verb maps the cursor (or the visual selection) to an anchor, then writes
//! through [`store::update`] — the only safe path, since it reloads inside the
//! lock and merges by id (PLAN.md §4) — and calls [`App::reload`] to refresh in
//! place. Authoring mirrors the agent's semantics in `agent.rs`, but stamps
//! [`Author::User`]. The composer (`composer.rs`) drives the create/edit verbs;
//! delete and status changes act on the annotation anchored to the cursor line.

use anyhow::{bail, Context, Result};
use jiff::Timestamp;

use super::app::{App, Row};
use crate::diff::LineKind;
use crate::domain::{AnchorScope, Annotation, Author, Severity, Side, StateFile, Status, Tag};
use crate::store;

/// Whether annotation `a`'s line anchor covers a cursor row with old/new line
/// numbers `old`/`new` — exact line, or within a `line..=end_line` region.
fn anchor_covers(a: &Annotation, old: Option<u32>, new: Option<u32>) -> bool {
    let target = match a.side {
        Side::Right => new,
        Side::Left => old,
    };
    let Some(target) = target else {
        return false;
    };
    let end = a.end_line.unwrap_or(a.line);
    a.line.min(end) <= target && target <= a.line.max(end)
}

impl App {
    /// The annotation that the inline comment row at `idx` belongs to, looked up
    /// by the id the row carries. `None` for non-comment rows or a dangling id.
    fn row_comment_annotation(&self, idx: usize) -> Option<&Annotation> {
        let Row::Comment(c) = self.view.rows.get(idx)? else {
            return None;
        };
        self.annotations.iter().find(|a| a.id == c.id)
    }

    /// The cursor row's `(old_lineno, new_lineno)`. A row of an inline comment
    /// resolves to the line its comment anchors to, so cursor-targeted verbs work
    /// while sitting on a comment body. `None` on a hunk header, expander, or
    /// notice.
    fn cursor_line_numbers(&self) -> Option<(Option<u32>, Option<u32>)> {
        match self.view.rows.get(self.cursor)? {
            Row::Line(l, _) => Some((l.old_lineno, l.new_lineno)),
            Row::Comment(_) => {
                let a = self.row_comment_annotation(self.cursor)?;
                Some(match a.side {
                    Side::Left => (Some(a.line), None),
                    Side::Right => (None, Some(a.line)),
                })
            }
            _ => None,
        }
    }

    /// Map row `idx` to its `(side, line)` anchor: an addition/context anchors on
    /// the new (Right) side, a deletion on the old (Left) side. `None` for a
    /// non-line row.
    fn row_line_anchor(&self, idx: usize) -> Option<(Side, u32)> {
        match self.view.rows.get(idx)? {
            Row::Line(l, _) => match l.kind {
                LineKind::Addition | LineKind::Context => l.new_lineno.map(|n| (Side::Right, n)),
                LineKind::Deletion => l.old_lineno.map(|n| (Side::Left, n)),
            },
            _ => None,
        }
    }

    /// The line number row `idx` carries on `side`, if it is a line row present
    /// on that side.
    fn row_lineno_on_side(&self, idx: usize, side: Side) -> Option<u32> {
        match self.view.rows.get(idx)? {
            Row::Line(l, _) => match side {
                Side::Right => l.new_lineno,
                Side::Left => l.old_lineno,
            },
            _ => None,
        }
    }

    /// Resolve the anchor for a new comment: a single `(side, line)` from the
    /// cursor, or a whole-line `(side, start, end)` region from the visual
    /// selection. With the cursor on an inline comment row, the new comment lands
    /// on the same line(s) as that comment. `None` when no diff line is in range
    /// (cursor on a hunk header, expander, or notice).
    pub(crate) fn anchor_for_comment(&self) -> Option<(Side, u32, Option<u32>)> {
        match self.selection_span() {
            Some((lo, hi)) => {
                // Side comes from the first line row in the span; the extent is
                // the min/max line number on that side across the selection.
                let (side, _) = (lo..=hi).find_map(|i| self.row_line_anchor(i))?;
                let nums: Vec<u32> = (lo..=hi)
                    .filter_map(|i| self.row_lineno_on_side(i, side))
                    .collect();
                let start = *nums.iter().min()?;
                let end = *nums.iter().max()?;
                let end_line = (end > start).then_some(end);
                Some((side, start, end_line))
            }
            None => match self.view.rows.get(self.cursor)? {
                Row::Comment(_) => {
                    let a = self.row_comment_annotation(self.cursor)?;
                    Some((a.side, a.line, a.end_line))
                }
                _ => {
                    let (side, line) = self.row_line_anchor(self.cursor)?;
                    Some((side, line, None))
                }
            },
        }
    }

    /// The id of the first annotation (store order) anchored to the cursor line —
    /// the target of reply/edit/delete/status. Multiple-per-line cycling is
    /// future polish.
    pub(crate) fn annotation_id_at_cursor(&self) -> Option<String> {
        let path = self.current().display_path().to_string();
        let (old, new) = self.cursor_line_numbers()?;
        self.annotations
            .iter()
            .find(|a| a.file == path && a.scope == AnchorScope::Line && anchor_covers(a, old, new))
            .map(|a| a.id.clone())
    }

    /// Run `f` against the store under the lock, then reload in place. Surfaces a
    /// store-less view or any write error as a transient status-bar notice rather
    /// than failing — authoring must never crash the viewer.
    fn mutate_store<T>(&mut self, f: impl FnOnce(&mut StateFile) -> Result<T>) -> Option<T> {
        let Some(path) = self.store_path.clone() else {
            self.notice = Some("no annotation store for this review".to_string());
            return None;
        };
        match store::update(&path, &self.target, f) {
            Ok(Ok(value)) => {
                self.reload();
                Some(value)
            }
            Ok(Err(e)) => {
                self.notice = Some(e.to_string());
                None
            }
            Err(e) => {
                self.notice = Some(e.to_string());
                None
            }
        }
    }

    /// Create a line or region annotation authored by the user.
    pub(crate) fn add_annotation(
        &mut self,
        side: Side,
        line: u32,
        end_line: Option<u32>,
        severity: Severity,
        tag: Option<Tag>,
        body: String,
    ) {
        let file = self.current().display_path().to_string();
        let signature = self.capture_signature(&file, line, side);
        let now = Timestamp::now();
        let annotation = Annotation {
            id: Annotation::new_id(),
            author: Author::User,
            file,
            line,
            end_line,
            side,
            scope: AnchorScope::Line,
            signature,
            severity,
            tag,
            status: Status::Open,
            body,
            reply_to: None,
            created_at: now,
            updated_at: now,
        };
        self.mutate_store(move |s| {
            s.upsert(annotation);
            Ok(())
        });
    }

    /// Create a whole-file annotation (`scope = File`); its line/side are not
    /// meaningful, so they are left at neutral defaults.
    pub(crate) fn write_file_comment(
        &mut self,
        severity: Severity,
        tag: Option<Tag>,
        body: String,
    ) {
        let file = self.current().display_path().to_string();
        let now = Timestamp::now();
        let annotation = Annotation {
            id: Annotation::new_id(),
            author: Author::User,
            file,
            line: 0,
            end_line: None,
            side: Side::Right,
            scope: AnchorScope::File,
            signature: None,
            severity,
            tag,
            status: Status::Open,
            body,
            reply_to: None,
            created_at: now,
            updated_at: now,
        };
        self.mutate_store(move |s| {
            s.upsert(annotation);
            Ok(())
        });
    }

    /// Thread a reply under `parent`, inheriting its anchor so the reply shows on
    /// the same line.
    pub(crate) fn write_reply(
        &mut self,
        parent: String,
        severity: Severity,
        tag: Option<Tag>,
        body: String,
    ) {
        let now = Timestamp::now();
        self.mutate_store(move |s| {
            let p = s
                .get(&parent)
                .with_context(|| format!("reply target `{parent}` not found"))?;
            let annotation = Annotation {
                id: Annotation::new_id(),
                author: Author::User,
                file: p.file.clone(),
                line: p.line,
                end_line: p.end_line,
                side: p.side,
                scope: p.scope,
                // Inherit the parent's signature so a reply relocates in lockstep
                // with the comment it threads under.
                signature: p.signature.clone(),
                severity,
                tag,
                status: Status::Open,
                body,
                reply_to: Some(parent.clone()),
                created_at: now,
                updated_at: now,
            };
            s.upsert(annotation);
            Ok(())
        });
    }

    /// Revise the user's own annotation in place. Guards against editing the
    /// agent's annotations.
    pub(crate) fn edit_annotation(
        &mut self,
        id: String,
        body: String,
        severity: Severity,
        tag: Option<Tag>,
    ) {
        let now = Timestamp::now();
        self.mutate_store(move |s| {
            let a = s
                .get_mut(&id)
                .with_context(|| format!("no annotation with id `{id}`"))?;
            if a.author != Author::User {
                bail!("`{id}` is the agent's annotation; you can only edit your own");
            }
            a.body = body;
            a.severity = severity;
            a.tag = tag;
            a.updated_at = now;
            Ok(())
        });
    }

    /// Delete the user's own annotation: hard-delete when nothing replies to it,
    /// else soft-retract to `withdrawn` so a thread the agent replied to stays
    /// coherent (the same rule as `agent comment cancel`).
    pub(crate) fn delete_annotation(&mut self, id: String) {
        let now = Timestamp::now();
        self.mutate_store(move |s| {
            match s.get(&id) {
                None => bail!("no annotation with id `{id}`"),
                Some(a) if a.author != Author::User => {
                    bail!("`{id}` is the agent's annotation; you can only delete your own")
                }
                Some(_) => {}
            }
            if s.has_replies(&id) {
                let a = s.get_mut(&id).expect("just checked it exists");
                a.status = Status::Withdrawn;
                a.updated_at = now;
            } else {
                s.remove(&id);
            }
            Ok(())
        });
    }

    /// Set an annotation's status (any author may resolve/reopen, matching the
    /// agent's `set_status`).
    pub(crate) fn set_annotation_status(&mut self, id: String, status: Status) {
        let now = Timestamp::now();
        self.mutate_store(move |s| {
            let a = s
                .get_mut(&id)
                .with_context(|| format!("no annotation with id `{id}`"))?;
            a.status = status;
            a.updated_at = now;
            Ok(())
        });
    }

    /// Clear every annotation in the store — the clean-slate "reset" before
    /// starting a fresh review. Drops the agent's annotations too (not just the
    /// user's), so the keymap gates it behind a confirmation prompt. Reports how
    /// many were removed in the status notice.
    pub(crate) fn reset_annotations(&mut self) {
        if let Some(n) = self.mutate_store(|s| Ok(s.clear())) {
            self.notice = Some(match n {
                0 => "no annotations to reset".to_string(),
                1 => "reset 1 annotation".to_string(),
                n => format!("reset {n} annotations"),
            });
        }
    }

    // --- cursor-targeted verbs the keymap drives ---------------------------

    /// Arm a delete confirmation for the annotation on the cursor line (resolved
    /// on a `y` press by the event loop). A no-op with a hint when there is none.
    pub(crate) fn request_delete(&mut self) {
        match self.annotation_id_at_cursor() {
            Some(id) => self.pending_delete = Some(id),
            None => self.notice = Some("no annotation on this line to delete".to_string()),
        }
    }

    /// Confirm the armed delete, if any.
    pub(crate) fn confirm_pending_delete(&mut self) {
        if let Some(id) = self.pending_delete.take() {
            self.delete_annotation(id);
        }
    }

    /// Disarm an armed delete without deleting. Also the `delete-confirm` mode's
    /// fallback, so any key that isn't bound to confirm cancels (as it always has).
    pub(crate) fn cancel_pending_delete(&mut self) {
        self.pending_delete = None;
    }

    /// Cycle the status of the annotation on the cursor line: open → resolved →
    /// wontfix → open. A no-op with a hint when there is none.
    pub(crate) fn cycle_annotation_status(&mut self) {
        let Some(id) = self.annotation_id_at_cursor() else {
            self.notice = Some("no annotation on this line to update".to_string());
            return;
        };
        let next = match self
            .annotations
            .iter()
            .find(|a| a.id == id)
            .map(|a| a.status)
        {
            Some(Status::Open) => Status::Resolved,
            Some(Status::Resolved) => Status::Wontfix,
            _ => Status::Open,
        };
        self.set_annotation_status(id, next);
    }
}
