Pure schema types — the cross-process on-disk contract between the TUI and the agent. No behavior beyond (de)serialization and small invariants.

- `mod.rs`: module wiring and the public re-exports.
- `annotation.rs`: the `Annotation` type and its enums (`Author`, `Severity`, `Tag`, `Status`, `Side`); CLI string parsing (`FromStr`) and `ParseEnumError` live here.
- `state.rs`: `StateFile` (versioned store root), `Target`, `Turn` (turn-protocol block), `SCHEMA_VERSION`; upsert/remove/merge-by-id helpers.
