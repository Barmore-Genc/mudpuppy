//! The composer's vim-like editing engine.
//!
//! [`super::Composer`] owns the buffer and the modal state; this module is the
//! `impl` block that turns normal-mode key presses into motions, operators and
//! edits, plus the insert-mode buffer edits the caller drives directly. The
//! public surface used by the caller is small — `normal_key` (one normal-mode
//! key), the insert-mode edits (`insert_char`/`insert_newline`/`backspace`/
//! `leave_insert`), `redo`, and the pending-state helpers (`has_pending`/
//! `clear_pending`) — everything else is internal.
//!
//! ## Model
//!
//! A *motion* ([`Motion`]) names a target cell and whether an operator should
//! treat the span as inclusive of that cell or as whole lines. Normal-mode keys
//! split into motions, the operators `d`/`c`/`y` (which consume the next motion,
//! or repeat for the line-wise `dd`/`cc`/`yy`), and the standalone edits. Counts
//! (`3w`, `d2j`) are accumulated as a prefix and multiplied across an operator
//! and its motion. Every buffer mutation first snapshots for `u`.

use ratatui::crossterm::event::{KeyCode, KeyEvent};

use super::{Composer, Mode};

/// A normal-mode operator awaiting a motion (or a repeat of itself for the
/// line-wise `dd`/`cc`/`yy`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Operator {
    Delete,
    Change,
    Yank,
}

/// A pending `f`/`F`/`t`/`T` awaiting its target character.
#[derive(Clone, Copy)]
pub(super) struct PendingFind {
    /// `F`/`T` search left rather than right.
    backward: bool,
    /// `t`/`T` stop one cell short of the target.
    till: bool,
}

/// A remembered `f`/`t` search, replayed by `;` (same direction) and `,`
/// (reversed).
#[derive(Clone, Copy)]
pub(super) struct LastFind {
    target: char,
    backward: bool,
    till: bool,
}

/// The yank/delete register: the captured text plus whether it was taken
/// line-wise (which decides how `p`/`P` re-insert it).
#[derive(Clone, Default)]
pub(super) struct Register {
    lines: Vec<String>,
    linewise: bool,
}

/// A buffer + cursor snapshot for `u`/`Ctrl-R`.
#[derive(Clone)]
pub(super) struct Snapshot {
    lines: Vec<String>,
    row: usize,
    col: usize,
}

/// The character class used for word motions; `big` (`W`/`B`/`E`) collapses
/// `Word` and `Punct` so only whitespace separates WORDs.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Class {
    Blank,
    Word,
    Punct,
}

fn classify(c: char, big: bool) -> Class {
    if c.is_whitespace() {
        Class::Blank
    } else if big || c.is_alphanumeric() || c == '_' {
        Class::Word
    } else {
        Class::Punct
    }
}

/// Byte offset of char index `col` within `line` (its byte length when `col` is
/// at or past the end).
fn byte_index(line: &str, col: usize) -> usize {
    line.char_indices()
        .nth(col)
        .map(|(b, _)| b)
        .unwrap_or(line.len())
}

/// The kinds of word motion, dispatched by `Composer::word_motion`.
#[derive(Clone, Copy)]
enum Word {
    /// `w`/`W` — start of the next word.
    Forward,
    /// `b`/`B` — start of the current/previous word.
    Backward,
    /// `e`/`E` — end of the current/next word.
    End,
}

/// A resolved motion: where the cursor lands and how an operator treats the
/// span between the cursor and that cell.
struct Motion {
    row: usize,
    col: usize,
    /// Char-wise inclusive: the target cell is part of an operator's span (`e`,
    /// `f`, `t`).
    inclusive: bool,
    /// Whole-line motion: operators act on the rows `row..=target`, ignoring the
    /// columns (`j`, `k`, `G`, `gg`).
    linewise: bool,
}

impl Composer {
    // --- Insert-mode edits (driven directly by the caller) ---

    pub(super) fn insert_char(&mut self, ch: char) {
        let at = self.byte_at(self.col);
        self.lines[self.row].insert(at, ch);
        self.col += 1;
    }

    /// Split the current line at the cursor; the tail becomes a new line below.
    pub(super) fn insert_newline(&mut self) {
        let at = self.byte_at(self.col);
        let tail = self.lines[self.row].split_off(at);
        self.lines.insert(self.row + 1, tail);
        self.row += 1;
        self.col = 0;
    }

    /// Delete the char before the cursor, joining with the previous line at a
    /// line start.
    pub(super) fn backspace(&mut self) {
        if self.col > 0 {
            let at = self.byte_at(self.col - 1);
            self.lines[self.row].remove(at);
            self.col -= 1;
        } else if self.row > 0 {
            let line = self.lines.remove(self.row);
            self.row -= 1;
            self.col = self.line_len();
            self.lines[self.row].push_str(&line);
        }
    }

    /// `Esc` in insert mode: drop to normal and pull the cursor back onto the
    /// last typed char, as vim does.
    pub(super) fn leave_insert(&mut self) {
        self.mode = Mode::Normal;
        self.col = self.col.saturating_sub(1);
    }

    // --- Pending-state helpers (used by the caller for Enter/Esc routing) ---

    /// Whether a multi-key command is mid-flight (an operator, an awaited
    /// argument, or a count prefix), so `Enter`/`Esc` should feed the engine
    /// rather than save/cancel the composer.
    pub(super) fn has_pending(&self) -> bool {
        self.pending_op.is_some()
            || self.pending_find.is_some()
            || self.pending_g
            || self.pending_replace
            || self.count.is_some()
            || self.op_count.is_some()
    }

    /// Abandon any in-flight command and its count.
    pub(super) fn clear_pending(&mut self) {
        self.pending_op = None;
        self.pending_find = None;
        self.pending_g = false;
        self.pending_replace = false;
        self.count = None;
        self.op_count = None;
    }

    // --- Normal-mode dispatch ---

    /// Handle one key in [`Mode::Normal`]. `Enter` (save) and `Esc` (cancel) are
    /// handled by the caller, which routes them here only while [`has_pending`]
    /// is set.
    ///
    /// [`has_pending`]: Composer::has_pending
    pub(super) fn normal_key(&mut self, ev: KeyEvent) {
        // Resolve an awaited second key first.
        if self.pending_replace {
            self.pending_replace = false;
            if let KeyCode::Char(ch) = ev.code {
                self.replace_char(ch, self.eff_count());
            }
            self.clear_pending();
            return;
        }
        if let Some(find) = self.pending_find.take() {
            self.resolve_find(ev, find);
            return;
        }
        if self.pending_g {
            self.pending_g = false;
            self.resolve_g(ev);
            return;
        }

        // A count prefix: digits accumulate, except a leading `0` is the motion.
        if let KeyCode::Char(c) = ev.code {
            if c.is_ascii_digit() && !(c == '0' && self.count.is_none()) {
                let d = c.to_digit(10).unwrap() as usize;
                self.count = Some(self.count.unwrap_or(0) * 10 + d);
                return;
            }
        }

        // A motion either moves the cursor or supplies an operator's span.
        // `cw`/`cW` on a non-blank is the well-known exception: it acts like
        // `ce`/`cE`, changing to the word's end without eating trailing space.
        let key = self.cw_as_ce(ev);
        if let Some(m) = self.motion(key, self.eff_count()) {
            self.apply_motion(m);
            return;
        }

        // Operators consume the next motion, or repeat for a line-wise edit.
        match ev.code {
            KeyCode::Char('d') => return self.set_operator(Operator::Delete),
            KeyCode::Char('c') => return self.set_operator(Operator::Change),
            KeyCode::Char('y') => return self.set_operator(Operator::Yank),
            _ => {}
        }

        // Keys that begin a two-key command keep any pending operator/count.
        match ev.code {
            KeyCode::Char('f') => {
                self.pending_find = Some(PendingFind {
                    backward: false,
                    till: false,
                });
                return;
            }
            KeyCode::Char('F') => {
                self.pending_find = Some(PendingFind {
                    backward: true,
                    till: false,
                });
                return;
            }
            KeyCode::Char('t') => {
                self.pending_find = Some(PendingFind {
                    backward: false,
                    till: true,
                });
                return;
            }
            KeyCode::Char('T') => {
                self.pending_find = Some(PendingFind {
                    backward: true,
                    till: true,
                });
                return;
            }
            KeyCode::Char('g') => {
                self.pending_g = true;
                return;
            }
            KeyCode::Char('r') => {
                self.pending_replace = true;
                return;
            }
            _ => {}
        }

        // Any other key with an operator pending abandons the operator.
        if self.pending_op.is_some() {
            self.clear_pending();
            return;
        }

        self.command(ev);
        self.clear_pending();
    }

    /// Rewrite `w`/`W` to `e`/`E` when a `c` operator is pending over a non-blank
    /// cell, so `cw` behaves like `ce` (vim's long-standing special case).
    fn cw_as_ce(&self, ev: KeyEvent) -> KeyEvent {
        if self.pending_op != Some(Operator::Change) {
            return ev;
        }
        if self.class_at((self.row, self.col), false) == Class::Blank {
            return ev;
        }
        let mut ev = ev;
        ev.code = match ev.code {
            KeyCode::Char('w') => KeyCode::Char('e'),
            KeyCode::Char('W') => KeyCode::Char('E'),
            other => other,
        };
        ev
    }

    /// Standalone normal-mode commands (no operator, no pending second key).
    fn command(&mut self, ev: KeyEvent) {
        let count = self.eff_count();
        match ev.code {
            KeyCode::Char('i') => self.enter_insert(),
            KeyCode::Char('a') => self.enter_insert_after(),
            KeyCode::Char('A') => {
                self.col = self.line_len();
                self.enter_insert();
            }
            KeyCode::Char('I') => {
                self.col = self.first_non_blank(self.row);
                self.enter_insert();
            }
            KeyCode::Char('o') => self.open_line(true),
            KeyCode::Char('O') => self.open_line(false),
            KeyCode::Char('x') => self.delete_chars(count),
            KeyCode::Char('X') => self.delete_chars_before(count),
            KeyCode::Char('s') => {
                self.delete_chars(count);
                self.enter_insert();
            }
            KeyCode::Char('S') => self.change_lines(count),
            KeyCode::Char('D') => self.delete_to_eol(),
            KeyCode::Char('C') => {
                self.delete_to_eol();
                self.enter_insert();
            }
            KeyCode::Char('Y') => self.yank_lines(count),
            KeyCode::Char('~') => self.toggle_case(count),
            KeyCode::Char('J') => self.join_lines(count),
            KeyCode::Char('p') => self.paste(true, count),
            KeyCode::Char('P') => self.paste(false, count),
            KeyCode::Char('u') => self.undo(),
            _ => {}
        }
    }

    /// The effective repeat count: the operator's count times the motion's
    /// count, so `2d3w` yields six.
    fn eff_count(&self) -> usize {
        self.count.unwrap_or(1) * self.op_count.unwrap_or(1)
    }

    /// The count explicitly typed (if any), for motions like `G`/`gg` whose
    /// meaning differs between "no count" and "1".
    fn explicit_count(&self) -> Option<usize> {
        self.count.or(self.op_count)
    }

    // --- Operators ---

    /// Press an operator: stash it (capturing the count), or—if it is already
    /// pending and repeated—run the line-wise form (`dd`/`cc`/`yy`).
    fn set_operator(&mut self, op: Operator) {
        match self.pending_op {
            Some(p) if p == op => {
                let lines = self.eff_count();
                let hi = (self.row + lines - 1).min(self.lines.len() - 1);
                self.push_undo();
                self.operate_lines(op, self.row, hi);
                self.clear_pending();
            }
            _ => {
                self.pending_op = Some(op);
                self.op_count = self.count.take();
            }
        }
    }

    /// Apply a resolved motion: move the cursor, or—if an operator is pending—run
    /// it over the span and clear the pending state.
    fn apply_motion(&mut self, m: Motion) {
        match self.pending_op.take() {
            None => {
                self.row = m.row.min(self.lines.len() - 1);
                self.col = m.col.min(self.line_len_at(self.row));
            }
            Some(op) => self.apply_operator(op, m),
        }
        self.clear_pending();
    }

    /// Run `op` over the span from the cursor to motion `m`.
    fn apply_operator(&mut self, op: Operator, m: Motion) {
        self.push_undo();
        if m.linewise {
            let lo = self.row.min(m.row);
            let hi = self.row.max(m.row);
            self.operate_lines(op, lo, hi);
            return;
        }
        let cur = (self.row, self.col);
        let tgt = (m.row, m.col);
        let (from, to) = if tgt <= cur {
            (tgt, cur)
        } else if m.inclusive {
            (cur, (m.row, m.col + 1))
        } else if tgt.0 > cur.0 && tgt.1 == 0 {
            // An exclusive motion landing in column 0 of a later line (e.g. `dw`
            // off the end of a line) backs up to the end of the previous line, so
            // the operator stays on this line rather than swallowing the break.
            (cur, (tgt.0 - 1, self.line_len_at(tgt.0 - 1)))
        } else {
            (cur, tgt)
        };
        match op {
            Operator::Yank => {
                self.register = Register {
                    lines: self.copy_span(from, to),
                    linewise: false,
                };
                self.row = from.0;
                self.col = from.1.min(self.line_len_at(from.0));
            }
            Operator::Delete => {
                self.register = Register {
                    lines: self.delete_span(from, to),
                    linewise: false,
                };
            }
            Operator::Change => {
                self.register = Register {
                    lines: self.delete_span(from, to),
                    linewise: false,
                };
                self.mode = Mode::Insert;
            }
        }
    }

    /// Run `op` over whole rows `lo..=hi`.
    fn operate_lines(&mut self, op: Operator, lo: usize, hi: usize) {
        let block = self.lines[lo..=hi].to_vec();
        match op {
            Operator::Yank => {
                self.register = Register {
                    lines: block,
                    linewise: true,
                };
                self.row = lo;
                self.col = self.col.min(self.line_len_at(lo));
            }
            Operator::Delete => {
                self.register = Register {
                    lines: block,
                    linewise: true,
                };
                self.lines.drain(lo..=hi);
                if self.lines.is_empty() {
                    self.lines.push(String::new());
                }
                self.row = lo.min(self.lines.len() - 1);
                self.col = self.first_non_blank(self.row);
            }
            Operator::Change => {
                self.register = Register {
                    lines: block,
                    linewise: true,
                };
                self.lines.splice(lo..=hi, std::iter::once(String::new()));
                self.row = lo;
                self.col = 0;
                self.mode = Mode::Insert;
            }
        }
    }

    // --- Standalone edits ---

    /// `x` — delete `count` chars from the cursor rightward, within the line.
    fn delete_chars(&mut self, count: usize) {
        let end = (self.col + count).min(self.line_len());
        if end > self.col {
            self.push_undo();
            self.register = Register {
                lines: self.delete_span((self.row, self.col), (self.row, end)),
                linewise: false,
            };
            self.clamp_col();
        }
    }

    /// `X` — delete `count` chars before the cursor, within the line.
    fn delete_chars_before(&mut self, count: usize) {
        let start = self.col.saturating_sub(count);
        if start < self.col {
            self.push_undo();
            self.register = Register {
                lines: self.delete_span((self.row, start), (self.row, self.col)),
                linewise: false,
            };
        }
    }

    /// `D` — delete from the cursor to the end of the line.
    fn delete_to_eol(&mut self) {
        let end = self.line_len();
        if end > self.col {
            self.push_undo();
            self.register = Register {
                lines: self.delete_span((self.row, self.col), (self.row, end)),
                linewise: false,
            };
            self.clamp_col();
        }
    }

    /// `Y` — yank `count` whole lines from the cursor down.
    fn yank_lines(&mut self, count: usize) {
        let hi = (self.row + count - 1).min(self.lines.len() - 1);
        self.register = Register {
            lines: self.lines[self.row..=hi].to_vec(),
            linewise: true,
        };
    }

    /// `S`/`cc` — change `count` whole lines from the cursor down.
    fn change_lines(&mut self, count: usize) {
        let hi = (self.row + count - 1).min(self.lines.len() - 1);
        self.push_undo();
        self.operate_lines(Operator::Change, self.row, hi);
    }

    /// `r` — replace `count` chars under the cursor with `ch`.
    fn replace_char(&mut self, ch: char, count: usize) {
        let mut chars: Vec<char> = self.lines[self.row].chars().collect();
        let n = count.min(chars.len().saturating_sub(self.col));
        if n == 0 {
            return;
        }
        self.push_undo();
        for c in chars.iter_mut().skip(self.col).take(n) {
            *c = ch;
        }
        self.lines[self.row] = chars.into_iter().collect();
        self.col += n - 1;
    }

    /// `~` — toggle the case of `count` chars under the cursor, advancing past
    /// them.
    fn toggle_case(&mut self, count: usize) {
        let mut chars: Vec<char> = self.lines[self.row].chars().collect();
        let n = count.min(chars.len().saturating_sub(self.col));
        if n == 0 {
            return;
        }
        self.push_undo();
        for c in chars.iter_mut().skip(self.col).take(n) {
            *c = if c.is_uppercase() {
                c.to_lowercase().next().unwrap_or(*c)
            } else {
                c.to_uppercase().next().unwrap_or(*c)
            };
        }
        self.lines[self.row] = chars.into_iter().collect();
        self.col = (self.col + n).min(self.line_len());
    }

    /// `J` — join the current line with `count - 1` following lines, separated by
    /// a single space, leaving the cursor at the join.
    fn join_lines(&mut self, count: usize) {
        let joins = count.max(2) - 1;
        let mut joined_any = false;
        for _ in 0..joins {
            if self.row + 1 >= self.lines.len() {
                break;
            }
            if !joined_any {
                self.push_undo();
                joined_any = true;
            }
            let next = self.lines.remove(self.row + 1);
            let tail = next.trim_start();
            let join_col = self.lines[self.row].chars().count();
            let needs_space = !self.lines[self.row].is_empty()
                && !self.lines[self.row].ends_with(' ')
                && !tail.is_empty();
            if needs_space {
                self.lines[self.row].push(' ');
            }
            self.lines[self.row].push_str(tail);
            self.col = join_col;
        }
    }

    /// `p`/`P` — re-insert the register `count` times, after/before the cursor.
    fn paste(&mut self, after: bool, count: usize) {
        if self.register.lines.is_empty() {
            return;
        }
        self.push_undo();
        if self.register.linewise {
            let at = if after { self.row + 1 } else { self.row };
            let block: Vec<String> = (0..count)
                .flat_map(|_| self.register.lines.iter().cloned())
                .collect();
            let row = at;
            self.lines.splice(at..at, block);
            self.row = row;
            self.col = self.first_non_blank(self.row);
        } else {
            let text = self.register.lines.join("\n").repeat(count);
            let col = if after && self.line_len() > 0 {
                self.col + 1
            } else {
                self.col
            };
            self.insert_text_at(self.row, col, &text);
        }
    }

    /// Splice `text` (which may contain newlines) into the buffer at `(row,
    /// col)`, leaving the cursor on the last inserted char.
    fn insert_text_at(&mut self, row: usize, col: usize, text: &str) {
        let byte = byte_index(&self.lines[row], col);
        let tail = self.lines[row].split_off(byte);
        let mut parts = text.split('\n');
        let first = parts.next().unwrap_or("");
        self.lines[row].push_str(first);
        let mut r = row;
        let mut last_len = col + first.chars().count();
        for part in parts {
            r += 1;
            self.lines.insert(r, part.to_string());
            last_len = part.chars().count();
        }
        self.lines[r].push_str(&tail);
        self.row = r;
        self.col = last_len.saturating_sub(1);
    }

    // --- Resolving awaited second keys ---

    /// The character after `f`/`F`/`t`/`T`: remember it for `;`/`,`, then move or
    /// drive the pending operator.
    fn resolve_find(&mut self, ev: KeyEvent, find: PendingFind) {
        if let KeyCode::Char(ch) = ev.code {
            self.last_find = Some(LastFind {
                target: ch,
                backward: find.backward,
                till: find.till,
            });
            if let Some(m) = self.find_char(ch, find.backward, find.till, self.eff_count()) {
                self.apply_motion(m);
                return;
            }
        }
        self.clear_pending();
    }

    /// The key after `g`. Only `gg` (to the first line, or line N with a count)
    /// is recognised.
    fn resolve_g(&mut self, ev: KeyEvent) {
        if let KeyCode::Char('g') = ev.code {
            let last = self.lines.len() - 1;
            let row = self
                .explicit_count()
                .map(|n| (n - 1).min(last))
                .unwrap_or(0);
            let m = Motion {
                row,
                col: self.first_non_blank(row),
                inclusive: false,
                linewise: true,
            };
            self.apply_motion(m);
        } else {
            self.clear_pending();
        }
    }

    // --- Motions ---

    /// Resolve a motion key (with repeat `count`) to a target, or `None` if the
    /// key is not a motion.
    fn motion(&self, ev: KeyEvent, count: usize) -> Option<Motion> {
        let charwise = |row, col, inclusive| {
            Some(Motion {
                row,
                col,
                inclusive,
                linewise: false,
            })
        };
        match ev.code {
            KeyCode::Char('h') | KeyCode::Left => {
                charwise(self.row, self.col.saturating_sub(count), false)
            }
            KeyCode::Char('l') | KeyCode::Right | KeyCode::Char(' ') => {
                charwise(self.row, (self.col + count).min(self.line_len()), false)
            }
            KeyCode::Char('j') | KeyCode::Down => {
                let row = (self.row + count).min(self.lines.len() - 1);
                Some(Motion {
                    row,
                    col: self.col,
                    inclusive: false,
                    linewise: true,
                })
            }
            KeyCode::Char('k') | KeyCode::Up => {
                let row = self.row.saturating_sub(count);
                Some(Motion {
                    row,
                    col: self.col,
                    inclusive: false,
                    linewise: true,
                })
            }
            KeyCode::Char('0') | KeyCode::Home => charwise(self.row, 0, false),
            KeyCode::Char('^') => charwise(self.row, self.first_non_blank(self.row), false),
            KeyCode::Char('$') | KeyCode::End => {
                let row = (self.row + count - 1).min(self.lines.len() - 1);
                charwise(row, self.line_len_at(row), false)
            }
            KeyCode::Char('w') => self.word_motion(Word::Forward, false, count),
            KeyCode::Char('W') => self.word_motion(Word::Forward, true, count),
            KeyCode::Char('b') => self.word_motion(Word::Backward, false, count),
            KeyCode::Char('B') => self.word_motion(Word::Backward, true, count),
            KeyCode::Char('e') => self.word_motion(Word::End, false, count),
            KeyCode::Char('E') => self.word_motion(Word::End, true, count),
            KeyCode::Char('G') => {
                let last = self.lines.len() - 1;
                let row = self
                    .explicit_count()
                    .map(|n| (n - 1).min(last))
                    .unwrap_or(last);
                Some(Motion {
                    row,
                    col: self.first_non_blank(row),
                    inclusive: false,
                    linewise: true,
                })
            }
            KeyCode::Char(';') => self.repeat_find(false, count),
            KeyCode::Char(',') => self.repeat_find(true, count),
            _ => None,
        }
    }

    /// A word motion (`w`/`b`/`e` and their `W`/`B`/`E` WORD forms), repeated
    /// `count` times. `e` is inclusive; the rest are exclusive.
    fn word_motion(&self, kind: Word, big: bool, count: usize) -> Option<Motion> {
        let mut pos = (self.row, self.col);
        for _ in 0..count {
            pos = match kind {
                Word::Forward => self.word_forward(pos, big),
                Word::Backward => self.word_backward(pos, big),
                Word::End => self.word_end(pos, big),
            };
        }
        Some(Motion {
            row: pos.0,
            col: pos.1,
            inclusive: matches!(kind, Word::End),
            linewise: false,
        })
    }

    fn word_forward(&self, mut p: (usize, usize), big: bool) -> (usize, usize) {
        let start = self.class_at(p, big);
        // Step off the rest of the current word, landing on the next class.
        if start != Class::Blank {
            while let Some(n) = self.next_pos(p) {
                let same = self.class_at(n, big) == start;
                p = n;
                if !same {
                    break;
                }
            }
        }
        // Skip whitespace to the next word's first char.
        while self.class_at(p, big) == Class::Blank {
            match self.next_pos(p) {
                Some(n) => p = n,
                None => break,
            }
        }
        p
    }

    fn word_backward(&self, mut p: (usize, usize), big: bool) -> (usize, usize) {
        p = match self.prev_pos(p) {
            Some(n) => n,
            None => return p,
        };
        while self.class_at(p, big) == Class::Blank {
            p = match self.prev_pos(p) {
                Some(n) => n,
                None => return p,
            };
        }
        let cls = self.class_at(p, big);
        while let Some(n) = self.prev_pos(p) {
            if self.class_at(n, big) == cls {
                p = n;
            } else {
                break;
            }
        }
        p
    }

    fn word_end(&self, mut p: (usize, usize), big: bool) -> (usize, usize) {
        p = match self.next_pos(p) {
            Some(n) => n,
            None => return p,
        };
        while self.class_at(p, big) == Class::Blank {
            p = match self.next_pos(p) {
                Some(n) => n,
                None => return p,
            };
        }
        let cls = self.class_at(p, big);
        while let Some(n) = self.next_pos(p) {
            if self.class_at(n, big) == cls {
                p = n;
            } else {
                break;
            }
        }
        p
    }

    /// `f`/`F`/`t`/`T`: the `count`-th `target` on the current line, or `None` if
    /// absent. Forward finds are inclusive; backward finds are exclusive.
    fn find_char(&self, target: char, backward: bool, till: bool, count: usize) -> Option<Motion> {
        let chars: Vec<char> = self.lines[self.row].chars().collect();
        let mut remaining = count;
        let mut hit = None;
        if backward {
            let mut i = self.col;
            while i > 0 {
                i -= 1;
                if chars[i] == target {
                    remaining -= 1;
                    if remaining == 0 {
                        hit = Some(i);
                        break;
                    }
                }
            }
        } else {
            let mut i = self.col;
            while i + 1 < chars.len() {
                i += 1;
                if chars[i] == target {
                    remaining -= 1;
                    if remaining == 0 {
                        hit = Some(i);
                        break;
                    }
                }
            }
        }
        let i = hit?;
        let col = match (backward, till) {
            (false, false) => i,
            (false, true) => i.saturating_sub(1),
            (true, false) => i,
            (true, true) => i + 1,
        };
        Some(Motion {
            row: self.row,
            col,
            inclusive: !backward,
            linewise: false,
        })
    }

    /// `;`/`,`: replay the last `f`/`t` search, reversing direction for `,`.
    fn repeat_find(&self, reverse: bool, count: usize) -> Option<Motion> {
        let last = self.last_find?;
        self.find_char(last.target, last.backward ^ reverse, last.till, count)
    }

    // --- Position helpers (a cell is `(row, col)`; an end-of-line cell is the
    // line length and reads as blank) ---

    fn class_at(&self, (row, col): (usize, usize), big: bool) -> Class {
        match self.lines[row].chars().nth(col) {
            Some(c) => classify(c, big),
            None => Class::Blank,
        }
    }

    fn next_pos(&self, (row, col): (usize, usize)) -> Option<(usize, usize)> {
        if col < self.line_len_at(row) {
            Some((row, col + 1))
        } else if row + 1 < self.lines.len() {
            Some((row + 1, 0))
        } else {
            None
        }
    }

    fn prev_pos(&self, (row, col): (usize, usize)) -> Option<(usize, usize)> {
        if col > 0 {
            Some((row, col - 1))
        } else if row > 0 {
            Some((row - 1, self.line_len_at(row - 1)))
        } else {
            None
        }
    }

    fn first_non_blank(&self, row: usize) -> usize {
        self.lines[row]
            .chars()
            .position(|c| !c.is_whitespace())
            .unwrap_or(0)
    }

    fn line_len_at(&self, row: usize) -> usize {
        self.lines[row].chars().count()
    }

    /// Keep the cursor column within the current line after an edit.
    fn clamp_col(&mut self) {
        self.col = self.col.min(self.line_len());
    }

    // --- Span copy/delete (a half-open `from..to` over the buffer) ---

    /// Copy the text in `from..to` (exclusive of `to`) as one entry per row it
    /// spans, without mutating the buffer.
    fn copy_span(&self, from: (usize, usize), to: (usize, usize)) -> Vec<String> {
        let b_from = byte_index(&self.lines[from.0], from.1);
        if from.0 == to.0 {
            let b_to = byte_index(&self.lines[from.0], to.1);
            return vec![self.lines[from.0][b_from..b_to].to_string()];
        }
        let b_to = byte_index(&self.lines[to.0], to.1);
        let mut out = vec![self.lines[from.0][b_from..].to_string()];
        out.extend(self.lines[from.0 + 1..to.0].iter().cloned());
        out.push(self.lines[to.0][..b_to].to_string());
        out
    }

    /// Delete the text in `from..to`, join the ends, and leave the cursor at
    /// `from`. Returns the removed text (one entry per spanned row).
    fn delete_span(&mut self, from: (usize, usize), to: (usize, usize)) -> Vec<String> {
        let removed = self.copy_span(from, to);
        let b_from = byte_index(&self.lines[from.0], from.1);
        if from.0 == to.0 {
            let b_to = byte_index(&self.lines[from.0], to.1);
            self.lines[from.0].replace_range(b_from..b_to, "");
        } else {
            let b_to = byte_index(&self.lines[to.0], to.1);
            let head = self.lines[from.0][..b_from].to_string();
            let tail = self.lines[to.0][b_to..].to_string();
            self.lines
                .splice(from.0..=to.0, std::iter::once(head + &tail));
        }
        self.row = from.0;
        self.col = from.1;
        self.clamp_col();
        removed
    }

    // --- Insert-mode entry points ---

    fn enter_insert(&mut self) {
        self.push_undo();
        self.mode = Mode::Insert;
    }

    fn enter_insert_after(&mut self) {
        self.push_undo();
        if self.col < self.line_len() {
            self.col += 1;
        }
        self.mode = Mode::Insert;
    }

    /// `o` / `O` — open an empty line below / above and start inserting on it.
    fn open_line(&mut self, below: bool) {
        self.push_undo();
        let at = if below { self.row + 1 } else { self.row };
        self.lines.insert(at, String::new());
        self.row = at;
        self.col = 0;
        self.mode = Mode::Insert;
    }

    // --- Undo / redo ---

    fn snapshot(&self) -> Snapshot {
        Snapshot {
            lines: self.lines.clone(),
            row: self.row,
            col: self.col,
        }
    }

    /// Record the pre-edit state for `u`; a fresh edit invalidates the redo
    /// stack. Insert-mode typing is not snapshotted per key, so one `u` reverts a
    /// whole insert session along with the command that began it.
    fn push_undo(&mut self) {
        self.undo.push(self.snapshot());
        self.redo.clear();
    }

    fn restore(&mut self, snap: Snapshot) {
        self.lines = snap.lines;
        self.row = snap.row.min(self.lines.len() - 1);
        self.col = snap.col.min(self.line_len());
    }

    fn undo(&mut self) {
        if let Some(snap) = self.undo.pop() {
            let current = self.snapshot();
            self.restore(snap);
            self.redo.push(current);
        }
    }

    pub(super) fn redo(&mut self) {
        if let Some(snap) = self.redo.pop() {
            let current = self.snapshot();
            self.restore(snap);
            self.undo.push(current);
        }
    }
}
