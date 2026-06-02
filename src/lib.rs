//! mudpuppy — collaborative, turn-based code review between a user and an AI
//! agent, mediated entirely by an on-disk annotation store.
//!
//! This crate is the testable core behind the `mudpuppy` binary. The binary
//! (`src/main.rs`) is a thin shell that parses the [`cli`] command tree and
//! dispatches into these modules. There is deliberately **no AI** anywhere in
//! here and **no network** beyond shelling out to `git` (local diffs) and `gh`
//! (read-only PR diffs); see [`AGENTS.md`] for the hard requirements.
//!
//! [`AGENTS.md`]: https://github.com/kaangenc/mudpuppy/blob/main/AGENTS.md
//!
//! # Module map
//!
//! The layout mirrors PLAN.md §3. Modules are introduced as their milestone
//! lands; several are still skeletons during bootstrap.
//!
//! - [`domain`] — pure schema types ([`Annotation`](domain::Annotation),
//!   [`StateFile`](domain::StateFile), and friends). The cross-process
//!   contract; the most heavily tested module.
//! - [`source`] — diff-source providers (local `git`, PR `gh`).
//! - [`blob`] — full file-content provider (working tree / `git show` / `gh api`).
//! - `command` — the `:command` palette state + fuzzy filtering over names.
//! - [`diff`] — hand-rolled unified-diff parser + anchoring/staleness.
//! - [`highlight`] — syntect syntax highlighting for the diff pane.
//! - [`lua`] — embedded Luau sandbox: the configurable keymap and event hooks.
//! - [`store`] — load / merge-by-id / atomic+locked save; the turn protocol.
//! - [`session`] — repo + target resolution, store-path derivation, resume.
//! - [`tui`] — the ratatui application.
//! - [`agent`] — implementation of the `agent` subcommands.
//! - [`install`] — `install claude`: writes Claude Code skills to disk.
//! - [`logging`] — file-logging facility (env-gated) with a test-mockable sink.
//! - [`cli`] — the clap command tree and top-level dispatch.

pub mod agent;
pub mod blob;
pub mod cli;
mod command;
pub mod diff;
pub mod domain;
pub mod highlight;
pub mod install;
pub mod logging;
pub mod lua;
mod picker;
pub mod session;
pub mod source;
pub mod store;
pub mod tui;
