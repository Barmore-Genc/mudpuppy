//! Diff-source providers: a common trait plus a `local` provider (shells out to
//! `git`) and a `pr` provider (shells out to `gh`, read-only). Each resolves a
//! base ref and head SHA and produces a raw unified diff (PLAN.md §3, §5).
//!
//! Not implemented yet.
