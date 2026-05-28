//! The clap command tree and top-level dispatch.
//!
//! The derived `--help` output is a product surface, not just developer
//! ergonomics: `mudpuppy agent --help` is the *agent's* entry point and must be
//! genuinely self-documenting (PLAN.md §7). Keep the `about`/`long_about` text
//! here accurate as the command surface firms up.

use anyhow::Result;
use clap::{Args, Parser, Subcommand};

/// Terminal UI for collaborative, turn-based code review between a human and an
/// AI agent.
///
/// With no subcommand, opens the TUI on the current repository. Pass a PR
/// reference (`owner/repo#123` or a full URL) to review a pull request instead
/// of local changes. Agent-driven commands live under `mudpuppy agent`.
#[derive(Debug, Parser)]
#[command(name = "mudpuppy", version, about, long_about = None)]
pub struct Cli {
    /// PR to review: `owner/repo#123` or a full GitHub PR URL. Omit to review
    /// local changes.
    #[arg(value_name = "PR")]
    pub pr: Option<String>,

    /// Review local changes against an explicit base ref instead of the
    /// inferred default branch.
    #[arg(long, value_name = "REF")]
    pub base: Option<String>,

    #[command(subcommand)]
    pub command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Agent-facing commands. Run `mudpuppy agent --help` first — its output is
    /// the agent's instructions for the whole review loop.
    Agent {
        #[command(subcommand)]
        command: AgentCommand,
    },
}

/// The agent's command surface over the shared annotation store.
///
/// Severity: `info` < `suggestion` < `warning` < `blocker`.
/// Tag: `?` question · `!` concern · `>` direction.
/// Status: `open` · `resolved` · `wontfix` · `withdrawn`.
/// Anchoring binds an annotation to `(file, side, line)` against the diff's
/// current head; `side` is `right` (added/new) or `left` (removed/old).
#[derive(Debug, Subcommand)]
pub enum AgentCommand {
    /// Print the unified diff under review.
    Diff {
        /// Limit output to a single file.
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
    },

    /// Create, read, revise, and retract annotations.
    Comment {
        #[command(subcommand)]
        command: CommentCommand,
    },

    /// Block until the human releases the turn, then print everything they
    /// changed since this call. The synchronization primitive of the review
    /// loop (PLAN.md §6).
    Wait {
        /// Give up after this many seconds instead of blocking indefinitely.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
    },

    /// Clear the current session's annotations and start a fresh round.
    Reset,
}

#[derive(Debug, Subcommand)]
pub enum CommentCommand {
    /// Leave a new annotation on a line.
    Add(AddArgs),

    /// List annotations, including the human's replies.
    List {
        /// Only annotations that are still open.
        #[arg(long)]
        open: bool,
        /// Only annotations by this author.
        #[arg(long, value_name = "WHO")]
        author: Option<String>,
        /// Only annotations on this file.
        #[arg(long, value_name = "FILE")]
        file: Option<String>,
    },

    /// Revise one of the agent's own open annotations in place.
    Edit {
        /// Id of the annotation to edit.
        #[arg(long, value_name = "ID")]
        id: String,
        #[arg(long, value_name = "TEXT")]
        body: Option<String>,
        #[arg(long, value_name = "LEVEL")]
        severity: Option<String>,
        #[arg(long, value_name = "TAG")]
        tag: Option<String>,
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
    },

    /// Retract one of the agent's own annotations. Hard-deletes it if it has no
    /// replies; soft-retracts it to `withdrawn` if the human already replied.
    Cancel {
        #[arg(long, value_name = "ID")]
        id: String,
    },

    /// Mark an annotation resolved.
    Resolve {
        #[arg(long, value_name = "ID")]
        id: String,
    },

    /// Reopen a previously closed annotation.
    Reopen {
        #[arg(long, value_name = "ID")]
        id: String,
    },

    /// Mark an annotation as won't-fix.
    Wontfix {
        #[arg(long, value_name = "ID")]
        id: String,
    },
}

/// Arguments for `agent comment add`, split out so the long flag list stays
/// readable.
#[derive(Debug, Args)]
pub struct AddArgs {
    /// File the line belongs to.
    #[arg(long, value_name = "FILE")]
    pub file: String,
    /// Line number to anchor to.
    #[arg(long, value_name = "N")]
    pub line: u32,
    /// Which side of the diff: `right` (added/new) or `left` (removed/old).
    #[arg(long, value_name = "SIDE", default_value = "right")]
    pub side: String,
    /// Severity: `info`, `suggestion`, `warning`, or `blocker`.
    #[arg(long, value_name = "LEVEL", default_value = "suggestion")]
    pub severity: String,
    /// Optional intent tag: `?`, `!`, or `>`.
    #[arg(long, value_name = "TAG")]
    pub tag: Option<String>,
    /// Thread this comment as a reply under an existing annotation id.
    #[arg(long, value_name = "ID")]
    pub reply_to: Option<String>,
    /// Markdown body.
    #[arg(long, value_name = "TEXT")]
    pub body: String,
}

/// Parse the process arguments and dispatch.
///
/// During bootstrap the handlers are not wired up yet; this resolves the
/// command tree and reports what isn't implemented rather than doing work.
pub fn run() -> Result<()> {
    dispatch(Cli::parse())
}

fn dispatch(cli: Cli) -> Result<()> {
    match cli.command {
        Some(Command::Agent { command }) => crate::agent::dispatch(command),
        None => crate::tui::launch(cli.pr, cli.base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn command_tree_is_valid() {
        // clap asserts internal invariants (no duplicate flags, valid arg
        // combinations) when building the command.
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_agent_comment_add() {
        let cli = Cli::try_parse_from([
            "mudpuppy",
            "agent",
            "comment",
            "add",
            "--file",
            "src/lib.rs",
            "--line",
            "10",
            "--body",
            "hello",
        ])
        .unwrap();
        let Some(Command::Agent {
            command:
                AgentCommand::Comment {
                    command: CommentCommand::Add(args),
                },
        }) = cli.command
        else {
            panic!("expected agent comment add");
        };
        assert_eq!(args.file, "src/lib.rs");
        assert_eq!(args.line, 10);
        assert_eq!(args.side, "right"); // default
        assert_eq!(args.body, "hello");
    }

    #[test]
    fn bare_invocation_targets_local_tui() {
        let cli = Cli::try_parse_from(["mudpuppy"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.pr.is_none());
    }

    #[test]
    fn accepts_pr_positional() {
        let cli = Cli::try_parse_from(["mudpuppy", "owner/repo#123"]).unwrap();
        assert_eq!(cli.pr.as_deref(), Some("owner/repo#123"));
    }
}
