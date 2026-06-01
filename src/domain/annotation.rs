//! The [`Annotation`] type and its associated enums.

use std::str::FromStr;

use jiff::Timestamp;
use nanoid::nanoid;
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Alphanumeric id alphabet. We avoid nanoid's default `-`/`_` so an id can
/// never start with `-` and be mistaken for a CLI flag (e.g. `--id -X…`).
const ID_ALPHABET: [char; 62] = [
    'A', 'B', 'C', 'D', 'E', 'F', 'G', 'H', 'I', 'J', 'K', 'L', 'M', 'N', 'O', 'P', 'Q', 'R', 'S',
    'T', 'U', 'V', 'W', 'X', 'Y', 'Z', 'a', 'b', 'c', 'd', 'e', 'f', 'g', 'h', 'i', 'j', 'k', 'l',
    'm', 'n', 'o', 'p', 'q', 'r', 's', 't', 'u', 'v', 'w', 'x', 'y', 'z', '0', '1', '2', '3', '4',
    '5', '6', '7', '8', '9',
];

/// Failure to parse one of the annotation enums from a CLI string.
///
/// Carries the offending value and the set of accepted spellings so the agent
/// gets an actionable message (its only feedback channel is the CLI).
#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid {kind} `{value}` (expected one of: {expected})")]
pub struct ParseEnumError {
    /// Which enum was being parsed (e.g. `severity`).
    pub kind: &'static str,
    /// The string that failed to parse.
    pub value: String,
    /// Comma-separated list of accepted spellings.
    pub expected: &'static str,
}

/// Who authored an annotation (or owns the current turn).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Author {
    Agent,
    Human,
}

/// How significant an annotation is.
///
/// Ordering is meaningful: `Info < Suggestion < Warning < Blocker`, so the most
/// pressing items sort last (or first, when reversed) in any UI listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Info,
    Suggestion,
    Warning,
    Blocker,
}

/// An optional one-character intent marker on an annotation.
///
/// Serializes to the literal symbol (`?`, `!`, `>`); absence is represented by
/// `Option::None` at the field level, matching the `tag: null` JSON shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Tag {
    /// `?` — a question.
    #[serde(rename = "?")]
    Question,
    /// `!` — a concern.
    #[serde(rename = "!")]
    Concern,
    /// `>` — a direction / instruction.
    #[serde(rename = ">")]
    Direction,
}

/// Lifecycle state of an annotation.
///
/// `Withdrawn` is a soft retraction: the agent retracted an annotation that the
/// human had already replied to, so the thread stays coherent (PLAN.md §7).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Status {
    Open,
    Resolved,
    Wontfix,
    Withdrawn,
}

/// Which side of the diff a line lives on, used purely for anchoring.
///
/// `Right` is the added/new side; `Left` is the removed/old side.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Right,
    Left,
}

/// What an annotation is anchored to.
///
/// `Line` (the default, and the only shape before this field existed) anchors to
/// a line or whole-line region on one [`Side`]. `File` anchors to the whole file;
/// its `line`/`end_line`/`side` are then not meaningful (kept as written, ignored
/// on display).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AnchorScope {
    #[default]
    Line,
    File,
}

impl FromStr for Author {
    type Err = ParseEnumError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "agent" => Ok(Author::Agent),
            "human" => Ok(Author::Human),
            _ => Err(ParseEnumError {
                kind: "author",
                value: s.to_string(),
                expected: "agent, human",
            }),
        }
    }
}

impl FromStr for Severity {
    type Err = ParseEnumError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "info" => Ok(Severity::Info),
            "suggestion" => Ok(Severity::Suggestion),
            "warning" => Ok(Severity::Warning),
            "blocker" => Ok(Severity::Blocker),
            _ => Err(ParseEnumError {
                kind: "severity",
                value: s.to_string(),
                expected: "info, suggestion, warning, blocker",
            }),
        }
    }
}

impl FromStr for Tag {
    type Err = ParseEnumError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        // Accept the wire symbols and the spelled-out names interchangeably; the
        // agent may reach for either.
        match s.to_ascii_lowercase().as_str() {
            "?" | "question" => Ok(Tag::Question),
            "!" | "concern" => Ok(Tag::Concern),
            ">" | "direction" => Ok(Tag::Direction),
            _ => Err(ParseEnumError {
                kind: "tag",
                value: s.to_string(),
                expected: "?, !, >",
            }),
        }
    }
}

impl FromStr for Status {
    type Err = ParseEnumError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "open" => Ok(Status::Open),
            "resolved" => Ok(Status::Resolved),
            "wontfix" => Ok(Status::Wontfix),
            "withdrawn" => Ok(Status::Withdrawn),
            _ => Err(ParseEnumError {
                kind: "status",
                value: s.to_string(),
                expected: "open, resolved, wontfix, withdrawn",
            }),
        }
    }
}

impl FromStr for Side {
    type Err = ParseEnumError;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_ascii_lowercase().as_str() {
            "right" => Ok(Side::Right),
            "left" => Ok(Side::Left),
            _ => Err(ParseEnumError {
                kind: "side",
                value: s.to_string(),
                expected: "right, left",
            }),
        }
    }
}

/// A single annotation on a line of the diff under review.
///
/// See PLAN.md §4 for field semantics. `id` is a short [nanoid] assigned on
/// creation and deduped against the store; `reply_to` threads a reply under a
/// parent annotation's `id`.
///
/// [nanoid]: https://github.com/ai/nanoid
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Annotation {
    /// Stable short id (nanoid, length 8).
    pub id: String,
    /// Who wrote it.
    pub author: Author,
    /// Path within the diff.
    pub file: String,
    /// Anchored line number on `side` (the region start for a multi-line region).
    pub line: u32,
    /// Inclusive end line of a whole-line region on `side`; `None` is a single
    /// line (the only shape before regions existed). Added additively so old
    /// stores keep loading.
    #[serde(default)]
    pub end_line: Option<u32>,
    /// Which side of the diff the line is on.
    pub side: Side,
    /// What the annotation is anchored to. Absent in old stores → [`AnchorScope::Line`].
    #[serde(default)]
    pub scope: AnchorScope,
    /// Significance.
    pub severity: Severity,
    /// Optional intent marker; `None` serializes to `null`.
    #[serde(default)]
    pub tag: Option<Tag>,
    /// Lifecycle state.
    pub status: Status,
    /// Markdown body.
    pub body: String,
    /// Parent annotation id when this is a threaded reply; `None` otherwise.
    #[serde(default)]
    pub reply_to: Option<String>,
    /// Creation time.
    pub created_at: Timestamp,
    /// Last-modified time; bumped on any edit or status change.
    pub updated_at: Timestamp,
}

impl Annotation {
    /// Mint a fresh annotation id: a length-8 [nanoid] over `ID_ALPHABET`.
    /// Both the agent CLI and the TUI author through this, so the id shape stays
    /// in one place.
    ///
    /// [nanoid]: https://github.com/ai/nanoid
    pub fn new_id() -> String {
        nanoid!(8, &ID_ALPHABET)
    }

    /// Whether this annotation is still actionable (neither resolved, declined,
    /// nor withdrawn).
    pub fn is_open(&self) -> bool {
        matches!(self.status, Status::Open)
    }

    /// Whether this annotation is a threaded reply to another.
    pub fn is_reply(&self) -> bool {
        self.reply_to.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Annotation {
        Annotation {
            id: "V1StGXR8".to_string(),
            author: Author::Agent,
            file: "src/auth.rs".to_string(),
            line: 42,
            end_line: None,
            side: Side::Right,
            scope: AnchorScope::Line,
            severity: Severity::Suggestion,
            tag: Some(Tag::Question),
            status: Status::Open,
            body: "Is this branch reachable?".to_string(),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn round_trips_through_json() {
        let a = sample();
        let json = serde_json::to_string(&a).unwrap();
        let back: Annotation = serde_json::from_str(&json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn enums_use_wire_spellings() {
        let json = serde_json::to_value(sample()).unwrap();
        assert_eq!(json["author"], "agent");
        assert_eq!(json["side"], "RIGHT");
        assert_eq!(json["severity"], "suggestion");
        assert_eq!(json["status"], "open");
        assert_eq!(json["tag"], "?");
    }

    #[test]
    fn absent_tag_is_null() {
        let mut a = sample();
        a.tag = None;
        let json = serde_json::to_value(&a).unwrap();
        assert!(json["tag"].is_null());
        // ...and a missing `tag` key deserializes back to None.
        let back: Annotation = serde_json::from_value(json).unwrap();
        assert_eq!(back.tag, None);
    }

    #[test]
    fn absent_end_line_and_scope_default_for_back_compat() {
        // A store written before regions/whole-file existed has neither key; both
        // must default so old stores and the existing agent CLI keep working.
        let json = r#"{
            "id": "V1StGXR8",
            "author": "agent",
            "file": "src/auth.rs",
            "line": 42,
            "side": "RIGHT",
            "severity": "suggestion",
            "tag": null,
            "status": "open",
            "body": "hi",
            "created_at": "2026-05-28T12:00:00Z",
            "updated_at": "2026-05-28T12:00:00Z"
        }"#;
        let a: Annotation = serde_json::from_str(json).unwrap();
        assert_eq!(a.end_line, None);
        assert_eq!(a.scope, AnchorScope::Line);
    }

    #[test]
    fn region_and_whole_file_round_trip() {
        let mut a = sample();
        a.end_line = Some(50);
        a.scope = AnchorScope::File;
        let json = serde_json::to_value(&a).unwrap();
        assert_eq!(json["end_line"], 50);
        assert_eq!(json["scope"], "file");
        let back: Annotation = serde_json::from_value(json).unwrap();
        assert_eq!(a, back);
    }

    #[test]
    fn new_id_is_eight_url_safe_chars() {
        let id = Annotation::new_id();
        assert_eq!(id.len(), 8);
        assert!(id.chars().all(|c| c.is_ascii_alphanumeric()));
    }

    #[test]
    fn severity_orders_info_to_blocker() {
        assert!(Severity::Info < Severity::Suggestion);
        assert!(Severity::Suggestion < Severity::Warning);
        assert!(Severity::Warning < Severity::Blocker);
    }

    #[test]
    fn enums_parse_from_cli_strings_case_insensitively() {
        assert_eq!("agent".parse::<Author>().unwrap(), Author::Agent);
        assert_eq!("HUMAN".parse::<Author>().unwrap(), Author::Human);
        assert_eq!("blocker".parse::<Severity>().unwrap(), Severity::Blocker);
        assert_eq!("Left".parse::<Side>().unwrap(), Side::Left);
        assert_eq!("wontfix".parse::<Status>().unwrap(), Status::Wontfix);
        // Tags accept both the wire symbol and the spelled-out name.
        assert_eq!("?".parse::<Tag>().unwrap(), Tag::Question);
        assert_eq!("concern".parse::<Tag>().unwrap(), Tag::Concern);
    }

    #[test]
    fn unknown_enum_value_reports_accepted_set() {
        let err = "critical".parse::<Severity>().unwrap_err();
        assert_eq!(err.kind, "severity");
        assert_eq!(err.value, "critical");
        let msg = err.to_string();
        assert!(msg.contains("info, suggestion, warning, blocker"), "{msg}");
    }

    #[test]
    fn open_and_reply_helpers() {
        let mut a = sample();
        assert!(a.is_open());
        assert!(!a.is_reply());
        a.status = Status::Resolved;
        a.reply_to = Some("parent01".to_string());
        assert!(!a.is_open());
        assert!(a.is_reply());
    }
}
