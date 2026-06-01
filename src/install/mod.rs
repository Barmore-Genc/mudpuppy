//! `mudpuppy install claude` — write the Claude Code skills to disk.
//!
//! mudpuppy adds no AI to the binary (AGENTS.md): this command only *writes
//! skill files*. The two skills teach a separate Claude Code agent the mudpuppy
//! review loop and make it aware mudpuppy is set up, so it offers it as an
//! option. Their bodies are embedded markdown (`pr_review.md`,
//! `implementation_review.md`) and lean on `mudpuppy agent --help` as the
//! canonical surface rather than restating flags that may drift.
//!
//! The user picks the install scope (issue #13):
//! - [`Location::Project`] — `<repo>/.claude/skills/`, committed and shared.
//! - [`Location::Local`] — same path, but git-ignored via `.git/info/exclude`
//!   so it stays on this machine only.
//! - [`Location::User`] — `~/.claude/skills/`, available across all repos.
//!
//! With no `--location` we prompt interactively; non-interactively a missing
//! location (or an existing file without `--force`) is an error rather than a
//! silent guess or clobber.

use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};

use crate::cli::InstallCommand;
use crate::source;

/// One skill we install: the directory name under `skills/` and its `SKILL.md`
/// contents, embedded at build time.
struct Skill {
    name: &'static str,
    body: &'static str,
}

/// The skills installed by `mudpuppy install claude`, in install order.
const SKILLS: &[Skill] = &[
    Skill {
        name: "mudpuppy-pr-review",
        body: include_str!("pr_review.md"),
    },
    Skill {
        name: "mudpuppy-implementation-review",
        body: include_str!("implementation_review.md"),
    },
];

/// Where the skills get written. Derives both the `skills/` directory and
/// whether the choice is git-ignored locally.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Location {
    /// `.claude/skills/` in this repo — committed and shared with the team.
    Project,
    /// `.claude/skills/` in this repo, git-ignored so it stays on your machine.
    Local,
    /// `~/.claude/skills/` — available across all your repos.
    User,
}

/// Route an `install` subcommand to its handler.
pub fn dispatch(command: InstallCommand) -> Result<()> {
    match command {
        InstallCommand::Claude { location, force } => claude(location, force),
    }
}

/// `install claude [--location …] [--force]` — write both skills at the chosen
/// scope, prompting for the scope and before any overwrite when interactive.
fn claude(location: Option<Location>, force: bool) -> Result<()> {
    let location = match location {
        Some(loc) => loc,
        None => prompt_location()?,
    };

    let skills_dir = skills_dir(location)?;
    for skill in SKILLS {
        install_skill(skill, &skills_dir, force)?;
    }

    // A local install must not be committed; exclude the skill dirs through the
    // repo-local `.git/info/exclude`, which is itself never checked in.
    if location == Location::Local {
        exclude_locally(&repo_root()?)?;
    }
    Ok(())
}

/// The `skills/` directory for `location`. Project/local resolve against the git
/// repo root; user resolves against the home directory.
fn skills_dir(location: Location) -> Result<PathBuf> {
    let base = match location {
        Location::Project | Location::Local => repo_root()?,
        Location::User => home_dir()?,
    };
    Ok(base.join(".claude").join("skills"))
}

/// The canonical git repo root for the current directory.
fn repo_root() -> Result<PathBuf> {
    let out = source::git(&["rev-parse", "--show-toplevel"]).context(
        "not inside a git repository (project/local installs need one; use --location user instead)",
    )?;
    Ok(PathBuf::from(out.trim()))
}

/// The user's home directory, for a user-level install.
fn home_dir() -> Result<PathBuf> {
    directories::BaseDirs::new()
        .map(|d| d.home_dir().to_path_buf())
        .context("could not determine your home directory for a user-level install")
}

/// Write one skill's `SKILL.md`, creating its directory. Prompts (interactive)
/// or errors (non-interactive) before overwriting an existing file, unless
/// `--force` was passed.
fn install_skill(skill: &Skill, skills_dir: &Path, force: bool) -> Result<()> {
    let dir = skills_dir.join(skill.name);
    let file = dir.join("SKILL.md");

    if file.exists() && !force && !confirm_overwrite(&file)? {
        println!("kept existing {}", file.display());
        return Ok(());
    }

    std::fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
    std::fs::write(&file, skill.body).with_context(|| format!("writing {}", file.display()))?;
    println!("installed {}", file.display());
    Ok(())
}

/// Add each skill dir to the repo's `.git/info/exclude` so a local install never
/// shows up as an untracked change. Idempotent: re-running adds nothing.
fn exclude_locally(repo_root: &Path) -> Result<()> {
    let exclude = repo_root.join(".git").join("info").join("exclude");
    let existing = std::fs::read_to_string(&exclude).unwrap_or_default();
    let names: Vec<&str> = SKILLS.iter().map(|s| s.name).collect();

    let Some(updated) = add_exclusions(&existing, &names) else {
        return Ok(()); // already excluded
    };

    if let Some(parent) = exclude.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&exclude, updated).with_context(|| format!("updating {}", exclude.display()))?;
    println!("git-ignored locally via {}", exclude.display());
    Ok(())
}

/// Append exclude patterns for any skill `names` not already present in
/// `existing`. Returns the new file contents, or `None` if nothing changed.
fn add_exclusions(existing: &str, names: &[&str]) -> Option<String> {
    let patterns: Vec<String> = names
        .iter()
        .map(|n| format!("/.claude/skills/{n}/"))
        .filter(|p| !existing.lines().any(|l| l.trim() == p))
        .collect();
    if patterns.is_empty() {
        return None;
    }

    let mut out = existing.to_string();
    if !out.is_empty() && !out.ends_with('\n') {
        out.push('\n');
    }
    out.push_str("# mudpuppy: local-only Claude Code skills\n");
    for p in patterns {
        out.push_str(&p);
        out.push('\n');
    }
    Some(out)
}

// --- Interactive prompts ----------------------------------------------------

/// Ask where to install. Errors (rather than guessing) when not on a terminal.
fn prompt_location() -> Result<Location> {
    if !io::stdin().is_terminal() {
        bail!(
            "no --location given and not running interactively; pass --location project|local|user"
        );
    }
    println!("Where should the mudpuppy Claude Code skills be installed?");
    println!("  1) project — .claude/skills/ in this repo (committed, shared with the team)");
    println!("  2) local   — .claude/skills/ in this repo, git-ignored (just you)");
    println!("  3) user    — ~/.claude/skills/ (all your repos)");
    loop {
        print!("Choose [1-3]: ");
        io::stdout().flush().ok();
        match read_line()?.trim() {
            "1" | "project" => return Ok(Location::Project),
            "2" | "local" => return Ok(Location::Local),
            "3" | "user" => return Ok(Location::User),
            other => println!("'{other}' is not one of 1, 2, or 3."),
        }
    }
}

/// Ask whether to overwrite `file`. Errors (rather than clobbering) when not on a
/// terminal — `--force` is the non-interactive opt-in.
fn confirm_overwrite(file: &Path) -> Result<bool> {
    if !io::stdin().is_terminal() {
        bail!(
            "{} already exists; re-run with --force to overwrite",
            file.display()
        );
    }
    print!("{} already exists. Overwrite? [y/N]: ", file.display());
    io::stdout().flush().ok();
    Ok(matches!(
        read_line()?.trim().to_ascii_lowercase().as_str(),
        "y" | "yes"
    ))
}

/// Read one line from stdin, erroring on EOF so a closed pipe doesn't loop.
fn read_line() -> Result<String> {
    let mut line = String::new();
    let n = io::stdin()
        .read_line(&mut line)
        .context("reading from stdin")?;
    if n == 0 {
        bail!("input ended before a choice was entered");
    }
    Ok(line)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn skills_have_frontmatter_and_lean_on_help() {
        for skill in SKILLS {
            assert!(
                skill.body.starts_with("---\n"),
                "{} is missing YAML frontmatter",
                skill.name
            );
            assert!(
                skill.body.contains(&format!("name: {}", skill.name)),
                "{} frontmatter name must match its directory",
                skill.name
            );
            // The skills must point at the self-documenting CLI, not duplicate it.
            assert!(
                skill.body.contains("mudpuppy agent --help"),
                "{} should defer to `mudpuppy agent --help`",
                skill.name
            );
            // Issue #13: skills must never instruct the agent to write to GitHub.
            assert!(
                !skill.body.contains("gh pr review") || skill.body.contains("Do not run"),
                "{} must not tell the agent to post to GitHub",
                skill.name
            );
        }
    }

    #[test]
    fn add_exclusions_adds_missing_and_is_idempotent() {
        let names = ["mudpuppy-pr-review", "mudpuppy-implementation-review"];

        // From empty: both patterns appear.
        let first = add_exclusions("", &names).expect("should add to empty file");
        assert!(first.contains("/.claude/skills/mudpuppy-pr-review/"));
        assert!(first.contains("/.claude/skills/mudpuppy-implementation-review/"));

        // Re-running over the result is a no-op.
        assert!(
            add_exclusions(&first, &names).is_none(),
            "already-excluded names should change nothing"
        );

        // Pre-existing unrelated content is preserved, and a missing trailing
        // newline before our block doesn't glue lines together.
        let updated = add_exclusions("*.log", &names).expect("should append");
        assert!(updated.starts_with("*.log\n"));
        assert!(updated.contains("/.claude/skills/mudpuppy-pr-review/"));
    }

    #[test]
    fn add_exclusions_adds_only_the_missing_one() {
        let existing = "/.claude/skills/mudpuppy-pr-review/\n";
        let names = ["mudpuppy-pr-review", "mudpuppy-implementation-review"];
        let updated = add_exclusions(existing, &names).expect("one is still missing");
        assert!(updated.contains("mudpuppy-implementation-review"));
        // The already-present one isn't duplicated.
        assert_eq!(
            updated.matches("mudpuppy-pr-review/").count(),
            1,
            "existing pattern should not be repeated"
        );
    }
}
