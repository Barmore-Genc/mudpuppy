//! Key chords and editor modes — the lookup key for a binding.
//!
//! A [`KeyChord`] is a non-modifier [`Key`] plus the CTRL/ALT modifiers; SHIFT is
//! deliberately *not* part of a chord — for a character key the shifted glyph
//! already carries the case (`G` vs `g`), matching how the old hard-coded matcher
//! keyed off `KeyCode::Char`. [`parse`](KeyChord::parse) turns a config string
//! like `"ctrl-d"`, `"tab"`, or `"G"` into a chord; [`from_event`](KeyChord::from_event)
//! turns a crossterm [`KeyEvent`] into one. The two are inverses up to spelling,
//! which the round-trip test pins down.

use std::fmt;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// A non-modifier key: either a character or one of the named special keys we
/// can bind. Character keys keep their glyph as-is (case included).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Key {
    Char(char),
    Tab,
    BackTab,
    Enter,
    Esc,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Backspace,
    Delete,
}

/// A key plus its CTRL/ALT modifiers — the thing a binding is keyed on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyChord {
    pub key: Key,
    pub ctrl: bool,
    pub alt: bool,
}

/// A sequence of chords — the lookup key for a binding. A single-key binding is
/// a length-1 sequence, so the old one-chord-per-binding model is the common
/// special case. `Vec<KeyChord>` derives `Hash`/`Eq` from `KeyChord`, so it works
/// directly as a registry key.
pub type KeySeq = Vec<KeyChord>;

impl KeyChord {
    /// A chord with no modifiers.
    pub(crate) fn plain(key: Key) -> KeyChord {
        KeyChord {
            key,
            ctrl: false,
            alt: false,
        }
    }

    /// Parse a binding spelling into a chord *sequence*: whitespace-separated
    /// tokens, each parsed with [`parse`](KeyChord::parse), with the `<leader>`
    /// token expanded to `leader`. A single token (no whitespace) yields a
    /// length-1 sequence, so existing single-key configs are unchanged. Returns
    /// `None` if any token is unparseable or the string is empty.
    pub fn parse_sequence(s: &str, leader: KeyChord) -> Option<KeySeq> {
        let mut seq = KeySeq::new();
        for token in s.split_ascii_whitespace() {
            if token == "<leader>" {
                seq.push(leader);
            } else {
                seq.push(KeyChord::parse(token)?);
            }
        }
        if seq.is_empty() {
            return None;
        }
        Some(seq)
    }

    /// Whether this chord is an unmodified ASCII digit — a candidate count prefix
    /// when no sequence is in flight. The `0`-only-with-a-pending-count rule
    /// (vim's, so a leading `0` stays a normal key) is applied by the caller.
    pub(crate) fn count_digit(&self) -> Option<u32> {
        match self.key {
            Key::Char(c) if !self.ctrl && !self.alt && c.is_ascii_digit() => c.to_digit(10),
            _ => None,
        }
    }

    /// Parse a config spelling into a chord. Accepts `ctrl-`/`c-`/`control-` and
    /// `alt-`/`a-`/`m-`/`meta-` modifier prefixes (and ignores a `shift-` prefix,
    /// which is folded into the character itself). The key name is either a
    /// single character (kept case-sensitive, so `"G"` is shift-g) or one of the
    /// named special keys (`"tab"`, `"pageup"`, `"esc"`, …). Returns `None` for
    /// an unrecognised spelling.
    pub fn parse(s: &str) -> Option<KeyChord> {
        // The lone "-" key has no modifiers and would confuse the split below.
        if s == "-" {
            return Some(KeyChord::plain(Key::Char('-')));
        }

        let parts: Vec<&str> = s.split('-').collect();
        let (mods, last) = parts.split_at(parts.len() - 1);
        let mut chord = KeyChord::plain(parse_key_name(last[0])?);
        for m in mods {
            match m.to_ascii_lowercase().as_str() {
                "c" | "ctrl" | "control" => chord.ctrl = true,
                "a" | "alt" | "m" | "meta" => chord.alt = true,
                "s" | "shift" => {} // folded into the character's case
                _ => return None,
            }
        }
        Some(chord)
    }

    /// Translate a crossterm key press into a chord, or `None` for a key we don't
    /// model (function keys, media keys, …). SHIFT is dropped (the character
    /// already reflects it); CTRL and ALT are honoured.
    pub fn from_event(ev: &KeyEvent) -> Option<KeyChord> {
        let key = match ev.code {
            KeyCode::Char(c) => Key::Char(c),
            KeyCode::Tab => Key::Tab,
            KeyCode::BackTab => Key::BackTab,
            KeyCode::Enter => Key::Enter,
            KeyCode::Esc => Key::Esc,
            KeyCode::Up => Key::Up,
            KeyCode::Down => Key::Down,
            KeyCode::Left => Key::Left,
            KeyCode::Right => Key::Right,
            KeyCode::Home => Key::Home,
            KeyCode::End => Key::End,
            KeyCode::PageUp => Key::PageUp,
            KeyCode::PageDown => Key::PageDown,
            KeyCode::Backspace => Key::Backspace,
            KeyCode::Delete => Key::Delete,
            _ => return None,
        };
        Some(KeyChord {
            key,
            ctrl: ev.modifiers.contains(KeyModifiers::CONTROL),
            alt: ev.modifiers.contains(KeyModifiers::ALT),
        })
    }
}

/// Map a key-name token to a [`Key`]. Single-character tokens keep their case so
/// `"G"` is distinct from `"g"`; everything else is matched case-insensitively.
fn parse_key_name(s: &str) -> Option<Key> {
    Some(match s.to_ascii_lowercase().as_str() {
        "tab" => Key::Tab,
        "backtab" => Key::BackTab,
        "enter" | "return" | "cr" => Key::Enter,
        "esc" | "escape" => Key::Esc,
        "up" => Key::Up,
        "down" => Key::Down,
        "left" => Key::Left,
        "right" => Key::Right,
        "home" => Key::Home,
        "end" => Key::End,
        "pageup" | "pgup" => Key::PageUp,
        "pagedown" | "pgdn" => Key::PageDown,
        "space" => Key::Char(' '),
        "backspace" | "bs" => Key::Backspace,
        "delete" | "del" => Key::Delete,
        _ => {
            // A single character (taken from the original, case-preserving token).
            let mut chars = s.chars();
            let c = chars.next()?;
            if chars.next().is_some() {
                return None;
            }
            Key::Char(c)
        }
    })
}

impl fmt::Display for Key {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Key::Char(' ') => f.write_str("space"),
            Key::Char(c) => write!(f, "{c}"),
            Key::Tab => f.write_str("tab"),
            Key::BackTab => f.write_str("backtab"),
            Key::Enter => f.write_str("enter"),
            Key::Esc => f.write_str("esc"),
            Key::Up => f.write_str("up"),
            Key::Down => f.write_str("down"),
            Key::Left => f.write_str("left"),
            Key::Right => f.write_str("right"),
            Key::Home => f.write_str("home"),
            Key::End => f.write_str("end"),
            Key::PageUp => f.write_str("pageup"),
            Key::PageDown => f.write_str("pagedown"),
            Key::Backspace => f.write_str("backspace"),
            Key::Delete => f.write_str("delete"),
        }
    }
}

impl fmt::Display for KeyChord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.ctrl {
            f.write_str("ctrl-")?;
        }
        if self.alt {
            f.write_str("alt-")?;
        }
        write!(f, "{}", self.key)
    }
}

/// Which keymap is consulted for a key press. The active mode is derived from UI
/// state: whichever modal overlay is open, else the focused pane.
///
/// The two *pane* modes fall back to [`Global`](Mode::Global) on a miss. The
/// overlay modes are exclusive — they never fall back to `Global`, so an overlay
/// can't be quit or scrolled by a stray pane binding — except the two composer
/// sub-modes, which fall back to [`Composer`](Mode::Composer) for the chords
/// that mean the same thing in both.
///
/// A key that no binding in the active chain claims is handed back to the
/// overlay's Rust fallback (typing into the picker query or the composer buffer,
/// the composer's vim engine, cancelling a delete confirmation). That is why the
/// overlay modes only need to bind the *commands*, not every printable key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Global,
    Tree,
    Diff,
    Help,
    /// The fuzzy "add any file" picker overlay.
    Picker,
    /// The `:command` palette overlay.
    Palette,
    /// A modal question opened by `mudpuppy.prompt`.
    Prompt,
    /// The delete-confirmation armed by `mudpuppy.delete_comment()`.
    DeleteConfirm,
    /// Composer chords shared by both of its editing modes (save, cycle
    /// severity/tag, insert a newline).
    Composer,
    /// The composer while typing.
    ComposerInsert,
    /// The composer's vim-like normal mode.
    ComposerNormal,
}

impl Mode {
    /// Parse the mode name used in `mudpuppy.map(mode, …)`.
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s.to_ascii_lowercase().as_str() {
            "global" => Mode::Global,
            "tree" => Mode::Tree,
            "diff" => Mode::Diff,
            "help" => Mode::Help,
            "picker" => Mode::Picker,
            "palette" => Mode::Palette,
            "prompt" => Mode::Prompt,
            "delete-confirm" => Mode::DeleteConfirm,
            "composer" => Mode::Composer,
            "composer-insert" => Mode::ComposerInsert,
            "composer-normal" => Mode::ComposerNormal,
            _ => return None,
        })
    }

    /// The modes a lookup walks, in order, when this one is active.
    pub(crate) fn chain(self) -> &'static [Mode] {
        match self {
            Mode::Global => &[Mode::Global],
            Mode::Tree => &[Mode::Tree, Mode::Global],
            Mode::Diff => &[Mode::Diff, Mode::Global],
            Mode::Help => &[Mode::Help],
            Mode::Picker => &[Mode::Picker],
            Mode::Palette => &[Mode::Palette],
            Mode::Prompt => &[Mode::Prompt],
            Mode::DeleteConfirm => &[Mode::DeleteConfirm],
            Mode::Composer => &[Mode::Composer],
            Mode::ComposerInsert => &[Mode::ComposerInsert, Mode::Composer],
            Mode::ComposerNormal => &[Mode::ComposerNormal, Mode::Composer],
        }
    }

    /// Whether a leading digit is an ambient count in this mode. Only the pane
    /// modes take counts; in an overlay a digit is text to type or an option to
    /// pick, so it must reach the binding (or the fallback) unchanged.
    pub(crate) fn takes_count(self) -> bool {
        matches!(self, Mode::Global | Mode::Tree | Mode::Diff)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_modifiers_and_named_keys() {
        assert_eq!(
            KeyChord::parse("ctrl-d"),
            Some(KeyChord {
                key: Key::Char('d'),
                ctrl: true,
                alt: false
            })
        );
        // `C-d` is the same chord spelled the wezterm/vim way.
        assert_eq!(KeyChord::parse("C-d"), KeyChord::parse("ctrl-d"));
        assert_eq!(KeyChord::parse("tab"), Some(KeyChord::plain(Key::Tab)));
        assert_eq!(
            KeyChord::parse("pageup"),
            Some(KeyChord::plain(Key::PageUp))
        );
        assert_eq!(KeyChord::parse("?"), Some(KeyChord::plain(Key::Char('?'))));
        // Case is preserved for single-character keys.
        assert_eq!(KeyChord::parse("G"), Some(KeyChord::plain(Key::Char('G'))));
        assert_ne!(KeyChord::parse("G"), KeyChord::parse("g"));
        assert_eq!(KeyChord::parse("-"), Some(KeyChord::plain(Key::Char('-'))));
    }

    #[test]
    fn rejects_unknown_spellings() {
        assert_eq!(KeyChord::parse("hyper-x"), None);
        assert_eq!(KeyChord::parse("nonsense"), None);
    }

    #[test]
    fn round_trips_through_display() {
        for s in ["ctrl-d", "tab", "G", "?", "pageup", "alt-x", "esc", "space"] {
            let chord = KeyChord::parse(s).unwrap();
            let printed = chord.to_string();
            assert_eq!(
                KeyChord::parse(&printed),
                Some(chord),
                "round trip via {printed:?}"
            );
        }
    }

    #[test]
    fn parses_sequences_and_expands_leader() {
        let space = KeyChord::plain(Key::Char(' '));
        // A single token is a length-1 sequence.
        assert_eq!(
            KeyChord::parse_sequence("g", space),
            Some(vec![KeyChord::plain(Key::Char('g'))])
        );
        // Whitespace-separated tokens become a multi-chord sequence.
        assert_eq!(
            KeyChord::parse_sequence("g g", space),
            Some(vec![
                KeyChord::plain(Key::Char('g')),
                KeyChord::plain(Key::Char('g'))
            ])
        );
        // `<leader>` expands to the configured leader chord.
        assert_eq!(
            KeyChord::parse_sequence("<leader> t r", space),
            Some(vec![
                space,
                KeyChord::plain(Key::Char('t')),
                KeyChord::plain(Key::Char('r')),
            ])
        );
        // An unparseable token fails the whole sequence; empty input is None.
        assert_eq!(KeyChord::parse_sequence("g hyper-x", space), None);
        assert_eq!(KeyChord::parse_sequence("   ", space), None);
    }

    #[test]
    fn count_digit_recognizes_unmodified_digits() {
        assert_eq!(KeyChord::plain(Key::Char('5')).count_digit(), Some(5));
        assert_eq!(KeyChord::plain(Key::Char('0')).count_digit(), Some(0));
        assert_eq!(KeyChord::plain(Key::Char('g')).count_digit(), None);
        // A modified digit is a chord, not a count.
        assert_eq!(KeyChord::parse("ctrl-5").unwrap().count_digit(), None);
    }

    #[test]
    fn from_event_ignores_shift_but_honors_ctrl() {
        let ev = KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT);
        assert_eq!(
            KeyChord::from_event(&ev),
            Some(KeyChord::plain(Key::Char('G')))
        );
        let ev = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert_eq!(KeyChord::from_event(&ev), KeyChord::parse("ctrl-d"));
    }
}
