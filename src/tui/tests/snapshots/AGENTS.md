Generated `insta` snapshot files (`.snap`) — Layer-1 character-grid baselines for the `tui` tests. Not source code.

- Each file is named `mudpuppy__tui__tests__<group>__<test_name>.snap` (the group is the test submodule, e.g. `rendering`) and captures the expected text grid (no color) for that test.
- These are produced/updated by `cargo insta` (review with `cargo insta review`); don't hand-edit them.
