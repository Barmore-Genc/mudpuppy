//! The top-level [`StateFile`] — the versioned on-disk root of the store.

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

/// The turn-protocol block (PLAN.md §6). Coordinates the human/agent handoff
/// over the filesystem with no daemon: `agent wait` records `seq`, flips
/// `agent_waiting`, and blocks until the human bumps `seq` and flips `owner`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Turn {
    /// Whose turn it currently is.
    pub owner: Author,
    /// Monotonic turn counter; incremented when a turn is released.
    pub seq: u64,
    /// Set by `agent wait` while it is blocked on the human.
    pub agent_waiting: bool,
    /// First-contact approval; the human's first turn-release sets it.
    pub approved: bool,
}

impl Default for Turn {
    fn default() -> Self {
        // A fresh session belongs to the agent (it comments first), is not yet
        // waiting, and has not yet been approved by the human.
        Turn {
            owner: Author::Agent,
            seq: 0,
            agent_waiting: false,
            approved: false,
        }
    }
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
            annotations: Vec::new(),
        }
    }

    /// Find an annotation by id.
    pub fn get(&self, id: &str) -> Option<&Annotation> {
        self.annotations.iter().find(|a| a.id == id)
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
