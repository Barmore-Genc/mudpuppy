Pure schema types — the cross-process on-disk contract between the TUI and the agent. No behavior beyond (de)serialization and small invariants.

- `mod.rs`: module wiring and the public re-exports.
- `annotation.rs`: the `Annotation` type and its enums (`Author`, `Severity`, `Tag`, `Status`, `Side`, `AnchorScope`); `end_line` (region) + `scope` (line/whole-file) + `signature` (relocation anchor) fields, `Annotation::new_id`, CLI string parsing (`FromStr`) and `ParseEnumError`.
- `state.rs`: `StateFile` (versioned store root), `Target`, `Turn` (turn-protocol block), `SCHEMA_VERSION`; upsert/remove/clear/merge-by-id helpers.
