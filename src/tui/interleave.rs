//! Splicing inline comment threads (and the open composer) into the built diff
//! rows.
//!
//! The cached structure [`base_view`](super::app::App) stays annotation-free;
//! this pass runs after it in the view rebuild, cloning its rows and walking
//! them to emit
//! single-line [`Row::Comment`] rows under each annotated diff line plus a
//! [`Row::Composer`] placeholder at the open composer's target. Bodies are
//! pre-wrapped to single visual lines so the "1 row = 1 visual line" invariant
//! (scroll slicing, `follow_cursor`, the mouse `diff_row_at`) still holds, and
//! `hunk_starts` is recomputed because the splice shifts row indices.

use std::collections::HashMap;

use crate::diff::{DiffLine, LineKind};
use crate::domain::{AnchorScope, Annotation, Side, Status};
use crate::tui::app::{App, CommentLine, CommentMeta, Row};
use crate::tui::composer::ComposerTarget;

/// A parent annotation plus its (creation-ordered) replies — one inline thread.
struct Thread<'a> {
    parent: &'a Annotation,
    replies: Vec<&'a Annotation>,
}

/// Where the open composer splices into the inline flow.
enum ComposerSlot {
    /// New comment on `(side, line)`: after that line's existing thread, or
    /// directly under the line when it has none.
    Line(Side, u32),
    /// Reply under the thread whose parent id is this: after its last comment.
    Reply(String),
    /// Edit of the comment with this id: replaces that comment's rows.
    Edit(String),
    /// Whole-file comment: keeps the centered modal, no inline placeholder.
    File,
}

impl App {
    /// Splice inline comment threads (and the open composer) into the freshly
    /// built code rows, recomputing `hunk_starts`.
    pub(super) fn interleave_annotations(&mut self) {
        // The line-number and marker columns are fixed; comment text wraps to
        // what's left. Fall back to a sane width before the first render
        // measures the pane.
        let inner_width = if self.comment_width == 0 {
            80
        } else {
            self.comment_width
        };

        let path = self.current().display_path().to_string();
        // Group line-scoped threads by their (side, line) anchor. Orphaned
        // anchors flipped to File scope are left out (they ride the file-level
        // header).
        let mut threads: HashMap<(Side, u32), Vec<Thread<'_>>> = HashMap::new();
        for a in &self.annotations {
            if a.file != path || a.scope != AnchorScope::Line || a.is_reply() {
                continue;
            }
            let mut replies: Vec<&Annotation> = self
                .annotations
                .iter()
                .filter(|r| r.reply_to.as_deref() == Some(a.id.as_str()))
                .collect();
            replies.sort_by_key(|x| x.created_at);
            threads
                .entry((a.side, a.line))
                .or_default()
                .push(Thread { parent: a, replies });
        }
        // Stable order among several parents on one line: by creation time.
        for v in threads.values_mut() {
            v.sort_by_key(|x| x.parent.created_at);
        }

        // Where the open composer wants to splice, plus the placeholder height.
        let composer = self.composer.as_ref().map(|c| {
            let slot = match &c.target {
                ComposerTarget::Line { side, line, .. } => ComposerSlot::Line(*side, *line),
                ComposerTarget::Reply { parent } => ComposerSlot::Reply(parent.clone()),
                ComposerTarget::Edit { id } => ComposerSlot::Edit(id.clone()),
                ComposerTarget::File => ComposerSlot::File,
            };
            (slot, composer_reserved_rows(c.lines.len()))
        });

        // Interleave from a fresh clone of the annotation-free structure cache so
        // `base_view` stays comment-free (and keeps its background highlight
        // fills) for the next pass.
        let base = self.base_view.rows.clone();
        let mut rows: Vec<Row> = Vec::with_capacity(base.len());
        let mut hunk_starts: Vec<usize> = Vec::new();

        for row in base {
            let anchor = match &row {
                Row::Hunk(_) => {
                    hunk_starts.push(rows.len());
                    None
                }
                Row::Line(l, _) => line_anchor(l),
                _ => None,
            };
            rows.push(row);

            let Some(key) = anchor else { continue };
            if let Some(line_threads) = threads.get(&key) {
                for thread in line_threads {
                    emit_thread(&mut rows, thread, inner_width, composer.as_ref());
                }
            }
            // A line-target composer with no existing thread on this line splices
            // directly under the line.
            if let Some((ComposerSlot::Line(side, line), n)) = &composer {
                if (*side, *line) == key && !threads.contains_key(&key) {
                    rows.push(Row::Composer { rows: *n });
                }
            }
        }

        self.view.max_width = super::app::FileView::rows_width(&rows);
        self.view.rows = rows;
        self.view.hunk_starts = hunk_starts;
    }
}

/// The `(side, line)` a diff line anchors a comment to: additions/context on the
/// new (Right) side, deletions on the old (Left) side. `None` for a line present
/// on neither side number.
fn line_anchor(l: &DiffLine) -> Option<(Side, u32)> {
    match l.kind {
        LineKind::Addition | LineKind::Context => l.new_lineno.map(|n| (Side::Right, n)),
        LineKind::Deletion => l.old_lineno.map(|n| (Side::Left, n)),
    }
}

/// The placeholder-row count the inline composer reserves for a body of
/// `body_lines` lines: a bordered box (two border rows), three header rows
/// (anchor, chips, mode), a blank spacer and a footer row, clamped so it always
/// reserves room for at least one body line.
fn composer_reserved_rows(body_lines: usize) -> u16 {
    // border(2) + anchor + chips + mode + body + blank spacer + footer.
    (body_lines.max(1) as u16) + 7
}

/// Append one thread's wrapped comment rows (parent then replies), splicing the
/// composer placeholder at the matching slot (reply after the last comment, edit
/// in place of the edited comment).
fn emit_thread(
    rows: &mut Vec<Row>,
    thread: &Thread<'_>,
    inner_width: u16,
    composer: Option<&(ComposerSlot, u16)>,
) {
    let (edit_id, reserved) = match composer {
        Some((ComposerSlot::Edit(id), n)) => (Some(id.as_str()), *n),
        Some((_, n)) => (None, *n),
        None => (None, 0),
    };

    // Parent comment (replaced by the composer when it is the edit target).
    if edit_id == Some(thread.parent.id.as_str()) {
        rows.push(Row::Composer { rows: reserved });
    } else {
        emit_comment(rows, thread.parent, 0, inner_width);
    }
    for reply in &thread.replies {
        if edit_id == Some(reply.id.as_str()) {
            rows.push(Row::Composer { rows: reserved });
        } else {
            emit_comment(rows, reply, 1, inner_width);
        }
    }
    // A reply-target composer sits after the thread's last comment.
    if let Some((ComposerSlot::Reply(parent), n)) = composer {
        if *parent == thread.parent.id {
            rows.push(Row::Composer { rows: *n });
        }
    }
}

/// Append one comment's wrapped visual lines: a header line carrying the
/// mark+author+tag+status, then continuation rows for each wrapped body line.
fn emit_comment(rows: &mut Vec<Row>, a: &Annotation, depth: usize, inner_width: u16) {
    let dimmed = matches!(a.status, Status::Resolved | Status::Withdrawn);
    let meta = || CommentMeta {
        author: a.author,
        severity: a.severity,
        tag: a.tag,
        status: a.status,
    };
    // Budget the body width against the *full* rendered prefix so wrapped lines
    // never spill past the pane edge and get clipped. Every inline comment row
    // carries the gutter pad (1) + the depth indent + the "▏ " thread bar (2);
    // see `comment_line` in `render`. The header line additionally shares its
    // row with the "● " mark (2) and the author/status label, while continuation
    // lines just align two spaces under the bar.
    let indent = depth * 2;
    let base = 1 + indent + 2;
    let cont_width = (inner_width as usize).saturating_sub(base + 2).max(4);
    let head_width = (inner_width as usize)
        .saturating_sub(base + 2 + super::render::comment_label(&meta()).chars().count())
        .max(4);
    for (i, line) in wrap_text_widths(&a.body, head_width, cont_width)
        .into_iter()
        .enumerate()
    {
        let header = i == 0;
        rows.push(Row::Comment(CommentLine {
            id: a.id.clone(),
            depth,
            header,
            text: line,
            meta: header.then(meta),
            dimmed,
        }));
    }
}

/// Greedy word-wrap of `text` to `width` columns, breaking on whitespace and
/// hard-splitting any single word longer than `width`. Always yields at least
/// one line per input line so a header still renders for an empty body. Shared
/// with the prompt overlay's scrollable details body and the all-annotations
/// sidebar list, which pre-wraps to keep its scroll math in rendered-row units.
pub(crate) fn wrap_text(text: &str, width: usize) -> Vec<String> {
    wrap_text_widths(text, width, width)
}

/// Like [`wrap_text`] but the first emitted visual line wraps to `first_width`
/// and every later line to `rest_width`. Inline comments use this because the
/// header line shares its row with the author/status label and so has less room
/// than the indented continuation lines beneath it.
fn wrap_text_widths(text: &str, first_width: usize, rest_width: usize) -> Vec<String> {
    let first_width = first_width.max(1);
    let rest_width = rest_width.max(1);
    // The first output line is the header; once any line is emitted the rest use
    // the (wider) continuation budget.
    let width_for = |out: &[String]| {
        if out.is_empty() {
            first_width
        } else {
            rest_width
        }
    };
    let mut out: Vec<String> = Vec::new();
    for raw in text.split('\n') {
        let mut line = String::new();
        let mut line_len = 0usize;
        for word in raw.split_whitespace() {
            let width = width_for(&out);
            let wlen = word.chars().count();
            if wlen > width {
                // Flush the current line, then hard-split the long word, picking
                // up the (possibly wider) continuation width once it spills over.
                if line_len > 0 {
                    out.push(std::mem::take(&mut line));
                }
                let mut chunk = String::new();
                for ch in word.chars() {
                    if chunk.chars().count() == width_for(&out) {
                        out.push(std::mem::take(&mut chunk));
                    }
                    chunk.push(ch);
                }
                line = chunk;
                line_len = line.chars().count();
                continue;
            }
            let extra = if line_len == 0 { wlen } else { wlen + 1 };
            if line_len + extra > width {
                out.push(std::mem::take(&mut line));
                line.push_str(word);
                line_len = wlen;
            } else {
                if line_len > 0 {
                    line.push(' ');
                    line_len += 1;
                }
                line.push_str(word);
                line_len += wlen;
            }
        }
        out.push(line);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{composer_reserved_rows, wrap_text};

    #[test]
    fn wrap_breaks_on_whitespace() {
        assert_eq!(wrap_text("one two three", 7), vec!["one two", "three"]);
    }

    #[test]
    fn wrap_hard_splits_an_overlong_word() {
        assert_eq!(wrap_text("abcdefgh", 3), vec!["abc", "def", "gh"]);
    }

    #[test]
    fn wrap_keeps_blank_lines() {
        // A blank body still yields one (empty) line so a header renders.
        assert_eq!(wrap_text("", 10), vec![""]);
    }

    #[test]
    fn reserved_rows_grow_with_the_body() {
        assert_eq!(composer_reserved_rows(1), 8);
        assert_eq!(composer_reserved_rows(3), 10);
        // An empty buffer still reserves room for one body line.
        assert_eq!(composer_reserved_rows(0), 8);
    }
}
