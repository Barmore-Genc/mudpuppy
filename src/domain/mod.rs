//! Pure schema types for the annotation store.
//!
//! These types *are* the cross-process contract between the TUI and any
//! external agent (PLAN.md §4). They carry no behavior beyond (de)serialization
//! and small invariants, and they own the on-disk JSON shape — keep them
//! versioned and forward-compatible. This is the most heavily tested module.

mod annotation;
mod state;

pub use annotation::{
    AnchorScope, Annotation, Author, ParseEnumError, Severity, Side, Status, Tag,
};
pub use state::{StateFile, Target, Turn, SCHEMA_VERSION};
