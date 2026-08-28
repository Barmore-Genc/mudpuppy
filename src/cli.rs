//! The clap command tree and top-level dispatch.
//!
//! The derived `--help` output is a product surface, not just developer
//! ergonomics: `mudpuppy agent --help` is the *agent's* entry point and must be
//! genuinely self-documenting (PLAN.md §7). Keep the `about`/`long_about` text
//! here accurate as the command surface firms up.

use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Args, Parser, Subcommand};

/// Terminal UI for collaborative, turn-based code review between a user and an
/// AI agent.
///
/// With no subcommand, opens the TUI on the current repository. Pass a PR
/// reference (`owner/repo#123` or a full URL) to review a pull request instead
/// of local changes. Agent-driven commands live under `mudpuppy agent`.
///
/// Run `mudpuppy help config` for the configuration & scripting (Luau) reference
/// — how to rebind keys and hook events in `~/.config/mudpuppy/mudpuppy.luau`.
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

    /// Run as if mudpuppy was started in `<DIR>` instead of the current working
    /// directory (like `git -C`). Applies to both the TUI and `agent` commands.
    #[arg(short = 'C', value_name = "DIR", global = true)]
    pub dir: Option<PathBuf>,

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

    /// Install editor/agent integrations (e.g. Claude Code skills).
    Install {
        #[command(subcommand)]
        command: InstallCommand,
    },

    /// Developer diagnostics for inspecting the viewer's rendering.
    #[command(hide = true)]
    Debug {
        #[command(subcommand)]
        command: DebugCommand,
    },
}

/// Visual diagnostics under `mudpuppy debug`.
#[derive(Debug, Subcommand)]
pub enum DebugCommand {
    /// Open a swatch page of every colour the viewer uses, drawn on the themed
    /// background, to judge contrast in your own terminal.
    Colors,
}

/// Integrations `mudpuppy install` can set up.
#[derive(Debug, Subcommand)]
pub enum InstallCommand {
    /// Install two Claude Code skills that teach an agent the mudpuppy review
    /// loop and make it aware mudpuppy is available.
    Claude {
        /// Where to install: `project` (committed), `local` (git-ignored), or
        /// `user` (`~/.claude/skills/`). Prompts interactively if omitted.
        #[arg(long, value_name = "WHERE")]
        location: Option<crate::install::Location>,
        /// Overwrite existing skill files without prompting.
        #[arg(long)]
        force: bool,
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

    /// Block until the user releases the turn, then print everything they
    /// changed since this call. The synchronization primitive of the review
    /// loop (PLAN.md §6).
    Wait {
        /// Give up after this many seconds instead of blocking indefinitely.
        #[arg(long, value_name = "SECONDS")]
        timeout: Option<u64>,
        #[command(flatten)]
        context: ContextArgs,
    },

    /// Clear the current session's annotations and start a fresh round.
    ///
    /// `--base` / `--pr` also record what's under review into the session store,
    /// so the agent's later commands and the user's open TUI both resolve the
    /// same diff. Omit both to keep the current target and only clear.
    Reset {
        /// Review the local changes against this base ref (e.g. the branch this
        /// work forks off). Mutually exclusive with `--pr`.
        #[arg(long, value_name = "REF", conflicts_with = "pr")]
        base: Option<String>,
        /// Review this pull request instead of the local changes; the diff comes
        /// from `gh pr diff`. Takes `owner/repo#123`, a number, or a URL.
        #[arg(long, value_name = "PR")]
        pr: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum CommentCommand {
    /// Leave a new annotation on a line.
    Add(AddArgs),

    /// List annotations, including the user's replies.
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
        #[command(flatten)]
        context: ContextArgs,
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
    /// replies; soft-retracts it to `withdrawn` if the user already replied.
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
    /// File the line belongs to. Not needed with `--reply-to`: a reply takes its
    /// parent's anchor.
    #[arg(long, value_name = "FILE", required_unless_present = "reply_to")]
    pub file: Option<String>,
    /// Line number to anchor to (the region start when `--end-line` is given).
    /// Not needed with `--reply-to`: a reply takes its parent's anchor.
    #[arg(long, value_name = "N", required_unless_present = "reply_to")]
    pub line: Option<u32>,
    /// Inclusive end line for a whole-line region; omit for a single line.
    #[arg(long, value_name = "N")]
    pub end_line: Option<u32>,
    /// Anchor to the whole file instead of a line (ignores `--line`/`--side`).
    #[arg(long)]
    pub whole_file: bool,
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
    /// Markdown body, inline. Use `-` to read it from stdin. For multi-line
    /// bodies prefer `--body-file -` with a heredoc, which avoids shell ANSI-C
    /// (`$'…\n…'`) quoting. Exactly one of `--body`/`--body-file` is required.
    #[arg(long, value_name = "TEXT")]
    pub body: Option<String>,
    /// Read the Markdown body from a file, or from stdin when the path is `-`.
    /// The heredoc form `--body-file - <<'EOF' … EOF` is the canonical way to
    /// pass a multi-line body without ANSI-C shell quoting.
    #[arg(long, value_name = "PATH")]
    pub body_file: Option<String>,
}

/// Width of the `annotated code` excerpt printed beneath each annotation by
/// `comment list` and `wait`, shared via `#[command(flatten)]`. Modeled on
/// grep's `-A`/`-B`/`-C`, except `-C` is the global change-directory flag, so
/// the both-sides knob is the long-only `--context`. The excerpt is on by
/// default; `--context 0` (or any negative width) turns it off, so the agent
/// gets the surrounding source for free but can opt out or widen it.
#[derive(Debug, Args)]
pub struct ContextArgs {
    /// Lines of code to show *after* the annotated line(s). Defaults to the
    /// `--context` width; 0 or negative shows none below.
    #[arg(short = 'A', long = "after", value_name = "N")]
    pub after: Option<i64>,
    /// Lines of code to show *before* the annotated line(s). Defaults to the
    /// `--context` width; 0 or negative shows none above.
    #[arg(short = 'B', long = "before", value_name = "N")]
    pub before: Option<i64>,
    /// Lines of code to show on *both* sides of the annotated line(s). 0 or
    /// negative suppresses the excerpt entirely.
    #[arg(long = "context", value_name = "N")]
    pub context: Option<i64>,
}

/// Parse the process arguments and dispatch.
pub fn run() -> Result<()> {
    let args: Vec<String> = std::env::args().collect();
    // `mudpuppy help config` is our own documentation topic, not a clap
    // subcommand — intercept it before clap, which would reject `config` as an
    // unknown command. Everything else (including clap's own `help` and
    // `help agent`) falls through to the normal command tree.
    if wants_config_help(&args) {
        print!("{}", crate::lua::config_help());
        return Ok(());
    }
    dispatch(Cli::parse())
}

/// Whether the invocation is `mudpuppy help config` (the scripting reference).
fn wants_config_help(args: &[String]) -> bool {
    args.len() == 3 && args[1] == "help" && args[2] == "config"
}

/// Open this process's debug log if the user config enabled it with
/// `mudpuppy.debug_log = true`. The location is fixed (`<data-dir>/logs/`, see
/// `crate::session::log_dir`) rather than config-supplied, so a hostile config
/// can't aim log writes at an arbitrary path; the file is
/// `<data-dir>/logs/mudpuppy-<role>.log`, with `role` distinguishing the TUI from
/// the headless agent so the two never interleave. Best-effort throughout: no
/// config, no setting, or any failure (building the engine, resolving the data
/// dir, creating the dir, opening the file) just leaves logging off.
/// `MUDPUPPY_LOG` (opened in `main`) still wins as an explicit override since the
/// global sink is install-once.
fn init_debug_log(role: &str) {
    let Some(config) = crate::lua::config_path() else {
        return;
    };
    if !config.exists() {
        return;
    }
    let Ok(engine) = crate::lua::LuaEngine::new(Some(config)) else {
        return;
    };
    if !engine.debug_log() {
        return;
    }
    // The config only flips logging on; we pick the (fixed) location so a config
    // can never aim log writes at an arbitrary path. Best-effort: give up quietly
    // if the data dir can't be resolved or the logs dir can't be made.
    let Ok(dir) = crate::session::log_dir() else {
        return;
    };
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let path = dir.join(format!("mudpuppy-{role}.log"));
    let _ = crate::logging::init_file(&path);
}

fn dispatch(cli: Cli) -> Result<()> {
    // `-C <DIR>`: change the working directory up front so every downstream
    // `git`/`gh` invocation (and store-path resolution) runs against that repo.
    if let Some(dir) = &cli.dir {
        std::env::set_current_dir(dir)
            .with_context(|| format!("changing directory to {}", dir.display()))?;
    }
    // Open this process's debug log before any work, so the diff/base resolution
    // is captured. The TUI and the headless `agent` get separate files so their
    // logs don't interleave; the switch is the user config, not a flag.
    match &cli.command {
        Some(Command::Agent { .. }) => init_debug_log("agent"),
        None => init_debug_log("tui"),
        _ => {}
    }
    match cli.command {
        Some(Command::Agent { command }) => crate::agent::dispatch(command),
        Some(Command::Install { command }) => crate::install::dispatch(command),
        Some(Command::Debug { command }) => match command {
            DebugCommand::Colors => crate::tui::debug::run(),
        },
        None => crate::tui::launch(cli.pr, cli.base),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;
    use std::path::Path;

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
        assert_eq!(args.file.as_deref(), Some("src/lib.rs"));
        assert_eq!(args.line, Some(10));
        assert_eq!(args.side, "right"); // default
        assert_eq!(args.body.as_deref(), Some("hello"));
        assert_eq!(args.body_file, None);
        assert_eq!(args.end_line, None);
        assert!(!args.whole_file);
    }

    #[test]
    fn a_reply_needs_no_file_or_line() {
        let cli = Cli::try_parse_from([
            "mudpuppy",
            "agent",
            "comment",
            "add",
            "--reply-to",
            "abcd1234",
            "--body",
            "on it",
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
        assert_eq!(args.file, None);
        assert_eq!(args.line, None);
        assert_eq!(args.reply_to.as_deref(), Some("abcd1234"));

        // Without a reply target they are still required.
        assert!(
            Cli::try_parse_from(["mudpuppy", "agent", "comment", "add", "--body", "hi"]).is_err()
        );
    }

    #[test]
    fn parses_region_and_whole_file_flags() {
        let cli = Cli::try_parse_from([
            "mudpuppy",
            "agent",
            "comment",
            "add",
            "--file",
            "src/lib.rs",
            "--line",
            "10",
            "--end-line",
            "20",
            "--whole-file",
            "--body",
            "hi",
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
        assert_eq!(args.end_line, Some(20));
        assert!(args.whole_file);
    }

    #[test]
    fn bare_invocation_targets_local_tui() {
        let cli = Cli::try_parse_from(["mudpuppy"]).unwrap();
        assert!(cli.command.is_none());
        assert!(cli.pr.is_none());
    }

    #[test]
    fn accepts_dash_c_before_and_after_the_subcommand() {
        // Before any subcommand (bare TUI invocation), like `git -C`.
        let cli = Cli::try_parse_from(["mudpuppy", "-C", "/tmp/repo"]).unwrap();
        assert_eq!(cli.dir.as_deref(), Some(Path::new("/tmp/repo")));

        // And as a global flag after a subcommand.
        let cli = Cli::try_parse_from(["mudpuppy", "-C", "/tmp/repo", "agent", "reset"]).unwrap();
        assert_eq!(cli.dir.as_deref(), Some(Path::new("/tmp/repo")));
    }

    #[test]
    fn accepts_pr_positional() {
        let cli = Cli::try_parse_from(["mudpuppy", "owner/repo#123"]).unwrap();
        assert_eq!(cli.pr.as_deref(), Some("owner/repo#123"));
    }

    #[test]
    fn recognizes_the_config_help_topic() {
        let yes = ["mudpuppy", "help", "config"].map(String::from);
        assert!(wants_config_help(&yes));
        // Adjacent invocations are *not* the config topic and must reach clap.
        for args in [
            vec!["mudpuppy", "help"],
            vec!["mudpuppy", "help", "agent"],
            vec!["mudpuppy", "config"],
            vec!["mudpuppy", "help", "config", "extra"],
        ] {
            let owned: Vec<String> = args.into_iter().map(String::from).collect();
            assert!(!wants_config_help(&owned), "should not match {owned:?}");
        }
    }

    #[test]
    fn config_help_documents_the_api() {
        let help = crate::lua::config_help();
        // It names the language, the registration verbs, and a worked example.
        assert!(help.contains("Luau"));
        assert!(help.contains("mudpuppy.map"));
        assert!(help.contains("mudpuppy.unmap"));
        assert!(help.contains("mudpuppy.on"));
    }
}
