//! The top-level [`StateFile`] — the versioned on-disk root of the store.

use nanoid::nanoid;
use serde::{Deserialize, Serialize};

use super::{Annotation, Author};

/// The schema version this build reads and writes. Bump on any
/// backwards-incompatible change to the on-disk shape.
pub const SCHEMA_VERSION: u32 = 1;

/// What is being reviewed, recorded so a stored review can be matched back to
/// its diff (PLAN.md §4). `head_sha` is the anchor point for staleness checks.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum Target {
    /// The user's local `git` changes against `base`.
    Local { base: String, head_sha: String },
    /// A GitHub pull request, identified as `owner/repo#123`.
    Pr { pr: String, head_sha: String },
}

impl Target {
    /// The diff's head commit SHA, regardless of source.
    pub fn head_sha(&self) -> &str {
        match self {
            Target::Local { head_sha, .. } | Target::Pr { head_sha, .. } => head_sha,
        }
    }
}

/// The turn-protocol block (PLAN.md §6). Coordinates the user/agent handoff
/// over the filesystem with no daemon: `agent wait` records `seq`, flips
/// `agent_waiting`, and blocks until the user bumps `seq` and flips `owner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// Whose turn it currently is.
    pub owner: Author,
    /// Monotonic turn counter; incremented when a turn is released.
    pub seq: u64,
    /// Set by `agent wait` while it is blocked on the user.
    pub agent_waiting: bool,
    /// First-contact approval; the user's first turn-release sets it.
    pub approved: bool,
}

impl Default for Turn {
    fn default() -> Self {
        // A fresh session belongs to the agent (it comments first), is not yet
        // waiting, and has not yet been approved by the user.
        Turn {
            owner: Author::Agent,
            seq: 0,
            agent_waiting: false,
            approved: false,
        }
    }
}

/// A fresh random salt for the debug-log hash (see [`crate::logging::hash`]).
/// Length 16 over the same alphabet as annotation ids; only needs to be stable
/// within a session and unguessable, not cryptographically strong.
fn fresh_log_seed() -> String {
    nanoid!(16)
}

/// The versioned root of the annotation store: one JSON file per
/// `(repo, target)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateFile {
    /// On-disk schema version (see [`SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// What is under review.
    pub target: Target,
    /// Turn-protocol coordination block.
    #[serde(default)]
    pub turn: Turn,
    /// Per-session salt for hashed debug-log labels, rotated on [`StateFile::clear`]
    /// so a reset's logs don't correlate with the previous round's. Defaulted for
    /// stores written before the field existed.
    #[serde(default = "fresh_log_seed")]
    pub log_seed: String,
    /// All annotations, both authors, every status.
    #[serde(default)]
    pub annotations: Vec<Annotation>,
}

impl StateFile {
    /// Create an empty store for `target` at the current schema version.
    pub fn new(target: Target) -> Self {
        StateFile {
            schema_version: SCHEMA_VERSION,
            target,
            turn: Turn::default(),
            log_seed: fresh_log_seed(),
            annotations: Vec::new(),
        }
    }

    /// Find an annotation by id.
    pub fn get(&self, id: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
    }

    /// Find a mutable annotation by id.
    pub fn get_mut(&mut self, id: &str) -> Option<&mut Annotation> {
        self.annotations.iter_mut().find(|a| a.id == id)
    }

    /// Merge one annotation in **by id**: replace an existing entry with the same
    /// `id`, or append it if new. This is the per-record half of the store's
    /// merge-by-id contract (PLAN.md §4); the store reloads current state before
    /// calling it so a concurrent writer's other records are preserved.
    ///
    /// Returns `true` if an existing annotation was replaced.
    pub fn upsert(&mut self, annotation: Annotation) -> bool {
        match self.get_mut(&annotation.id) {
            Some(existing) => {
                *existing = annotation;
                true
            }
            None => {
                self.annotations.push(annotation);
                false
            }
        }
    }

    /// Remove an annotation by id, returning it if it was present.
    pub fn remove(&mut self, id: &str) -> Option<Annotation> {
        let idx = self.annotations.iter().position(|a| a.id == id)?;
        Some(self.annotations.remove(idx))
    }

    /// Drop every annotation, returning how many were removed. Unlike
    /// [`StateFile::remove`], this deliberately discards the whole list — the
    /// clean-slate "reset" action — so it is the one place that does not honour
    /// the merge-by-id contract; callers gate it behind a confirmation. Also
    /// rotates [`StateFile::log_seed`] so a new round's hashed logs don't
    /// correlate with the previous one's.
    pub fn clear(&mut self) -> usize {
        let n = self.annotations.len();
        self.annotations.clear();
        self.log_seed = fresh_log_seed();
        n
    }

    /// Whether any annotation threads as a reply under `id`.
    pub fn has_replies(&self, id: &str) -> bool {
        self.annotations
            .iter()
            .any(|a| a.reply_to.as_deref() == Some(id))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_store_round_trips() {
        let s = StateFile::new(Target::Local {
            base: "main".to_string(),
            head_sha: "abc123".to_string(),
        });
        let json = serde_json::to_string(&s).unwrap();
        let back: StateFile = serde_json::from_str(&json).unwrap();
        assert_eq!(s, back);
    }

    #[test]
    fn target_is_internally_tagged() {
        let local = serde_json::to_value(Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        })
        .unwrap();
        assert_eq!(local["kind"], "local");
        assert_eq!(local["base"], "main");

        let pr = serde_json::to_value(Target::Pr {
            pr: "owner/repo#1".to_string(),
            head_sha: "def".to_string(),
        })
        .unwrap();
        assert_eq!(pr["kind"], "pr");
        assert_eq!(pr["pr"], "owner/repo#1");
    }

    #[test]
    fn head_sha_reads_from_either_variant() {
        let local = Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        };
        let pr = Target::Pr {
            pr: "o/r#1".to_string(),
            head_sha: "def".to_string(),
        };
        assert_eq!(local.head_sha(), "abc");
        assert_eq!(pr.head_sha(), "def");
    }

    #[test]
    fn fresh_turn_defaults_to_unapproved_agent() {
        let t = Turn::default();
        assert_eq!(t.owner, Author::Agent);
        assert_eq!(t.seq, 0);
        assert!(!t.agent_waiting);
        assert!(!t.approved);
    }

    fn ann(id: &str, reply_to: Option<&str>) -> Annotation {
        Annotation {
            id: id.to_string(),
            author: Author::Agent,
            file: "src/lib.rs".to_string(),
            line: 1,
            end_line: None,
            side: super::super::Side::Right,
            scope: super::super::AnchorScope::Line,
            signature: None,
            severity: super::super::Severity::Info,
            tag: None,
            status: super::super::Status::Open,
            body: "b".to_string(),
            reply_to: reply_to.map(str::to_string),
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn upsert_appends_then_replaces_by_id() {
        let mut s = StateFile::new(Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        });
        assert!(!s.upsert(ann("aaa", None)), "first insert is an append");
        assert_eq!(s.annotations.len(), 1);

        let mut updated = ann("aaa", None);
        updated.body = "edited".to_string();
        assert!(s.upsert(updated), "same id replaces in place");
        assert_eq!(s.annotations.len(), 1);
        assert_eq!(s.get("aaa").unwrap().body, "edited");
    }

    #[test]
    fn remove_and_has_replies() {
        let mut s = StateFile::new(Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        });
        s.upsert(ann("parent", None));
        s.upsert(ann("child", Some("parent")));
        assert!(s.has_replies("parent"));
        assert!(!s.has_replies("child"));
        assert_eq!(s.remove("child").unwrap().id, "child");
        assert!(!s.has_replies("parent"));
        assert!(s.remove("missing").is_none());
    }

    #[test]
    fn clear_drops_everything_and_returns_the_count() {
        let mut s = StateFile::new(Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        });
        s.upsert(ann("a", None));
        s.upsert(ann("b", None));
        assert_eq!(s.clear(), 2, "reports how many it removed");
        assert!(s.annotations.is_empty());
        assert_eq!(s.clear(), 0, "clearing an empty store removes nothing");
    }

    #[test]
    fn clear_rotates_the_log_seed() {
        let mut s = StateFile::new(Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        });
        let before = s.log_seed.clone();
        s.clear();
        assert_ne!(s.log_seed, before, "reset rotates the debug-log salt");
    }

    #[test]
    fn log_seed_defaults_when_absent_on_load() {
        // A store written before the seed field existed still loads, getting a
        // fresh non-empty seed.
        let json = r#"{
            "schema_version": 1,
            "target": { "kind": "local", "base": "main", "head_sha": "abc" }
        }"#;
        let s: StateFile = serde_json::from_str(json).unwrap();
        assert!(!s.log_seed.is_empty());
    }

    #[test]
    fn turn_block_is_optional_on_load() {
        // A store written before the turn block existed must still load.
        let json = r#"{
            "schema_version": 1,
            "target": { "kind": "local", "base": "main", "head_sha": "abc" }
        }"#;
        let s: StateFile = serde_json::from_str(json).unwrap();
        assert_eq!(s.turn, Turn::default());
        assert!(s.annotations.is_empty());
    }
}
