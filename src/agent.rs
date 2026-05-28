//! Implementation of the `mudpuppy agent` subcommands over [`store`] and
//! [`session`].
//!
//! [`store`]: crate::store
//! [`session`]: crate::session
//!
//! Each verb reads or writes the shared annotation store and works whether or
//! not a TUI is running; only `wait` needs a live human to ever unblock
//! (PLAN.md §6, §7). Handlers are stubs during bootstrap.

use anyhow::{bail, Result};

use crate::cli::AgentCommand;

/// Route an `agent` subcommand to its handler.
pub fn dispatch(command: AgentCommand) -> Result<()> {
    match command {
        AgentCommand::Diff { .. } => bail!("`agent diff` is not implemented yet"),
        AgentCommand::Comment { .. } => bail!("`agent comment` is not implemented yet"),
        AgentCommand::Wait { .. } => bail!("`agent wait` is not implemented yet"),
        AgentCommand::Reset => bail!("`agent reset` is not implemented yet"),
    }
}
