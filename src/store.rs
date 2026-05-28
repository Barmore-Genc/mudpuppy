//! Load / merge-by-id / save for the annotation store, with atomic
//! (temp + rename) and file-locked writes, plus the turn protocol (PLAN.md §4,
//! §6). Saves never clobber the whole file: they reload, apply this process's
//! changes by `id`, and write under an advisory lock so a live TUI and a
//! headless agent don't lose each other's work.
//!
//! Not implemented yet. The `tempfile` + `fs4` dependencies land with this
//! milestone.
