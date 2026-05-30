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

impl KeyChord {
    /// A chord with no modifiers.
    fn plain(key: Key) -> KeyChord {
        KeyChord {
            key,
            ctrl: false,
            alt: false,
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
/// state ([`Help`](Mode::Help) when the overlay is open, else the focused pane);
/// a miss in the active mode falls back to [`Global`](Mode::Global) — except in
/// `Help`, which is exclusive (no fallback) so the overlay swallows other keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Mode {
    Global,
    Tree,
    Diff,
    Help,
}

impl Mode {
    /// Parse the mode name used in `mudpuppy.map(mode, …)`.
    pub fn parse(s: &str) -> Option<Mode> {
        Some(match s.to_ascii_lowercase().as_str() {
            "global" => Mode::Global,
            "tree" => Mode::Tree,
            "diff" => Mode::Diff,
            "help" => Mode::Help,
            _ => return None,
        })
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
