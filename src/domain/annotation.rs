//! The [`Annotation`] type and its associated enums.

use jiff::Timestamp;
use serde::{Deserialize, Serialize};

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
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum Side {
    Right,
    Left,
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
    /// Anchored line number on `side`.
    pub line: u32,
    /// Which side of the diff the line is on.
    pub side: Side,
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
            side: Side::Right,
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
    fn severity_orders_info_to_blocker() {
        assert!(Severity::Info < Severity::Suggestion);
        assert!(Severity::Suggestion < Severity::Warning);
        assert!(Severity::Warning < Severity::Blocker);
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
