//! Repo + target resolution, store-path derivation, resume, liveness, and reset
//! (PLAN.md §5). The session key is the canonical git repo root plus the review
//! target (`local` or `pr:<n>`); the store path encodes both so reopening in
//! the same repo reattaches automatically.
//!
//! Not implemented yet. The `directories` dependency (platform data dir) lands
//! with this milestone.
