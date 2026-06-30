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

/// The current version of the embedded skill bodies. Bump this whenever you
/// change a skill body (`pr_review.md` / `implementation_review.md`) so already-
/// installed copies are detected as stale and the user is offered a refresh; set
/// [`SKILL_UPDATE_MESSAGE`] to say what changed. The version is stamped into each
/// installed `SKILL.md`'s frontmatter as `mudpuppy-skill-version: N`.
pub const SKILL_VERSION: u32 = 3;

/// Human-readable "what changed" line for the refresh prompt. Rewrite this each
/// time you bump [`SKILL_VERSION`] to describe the new skill content.
pub const SKILL_UPDATE_MESSAGE: &str =
    "the review target is now recorded in one store shared by the agent and the \
     TUI: `agent reset --pr <ref>` points the whole review (diff, anchoring, the \
     user's UI) at a pull request, and `--base <ref>` at a local base. The \
     PR-review skill now declares the PR with `reset --pr` first.";

/// The frontmatter key the installed version is stamped under.
const VERSION_KEY: &str = "mudpuppy-skill-version";

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

/// What [`installed_skill_status`] found: the lowest version stamped across every
/// installed `SKILL.md` we located, plus those files' paths. A file with no stamp
/// counts as version `0`. "Lowest" so a mixed install (one refreshed, one not) is
/// still reported as stale until *all* are current.
pub struct InstalledSkills {
    /// The lowest version found across all located skill files.
    pub version: u32,
    /// Every `SKILL.md` we found, across all reachable install locations.
    pub paths: Vec<PathBuf>,
}

/// Scan the reachable install locations — `<repo>/.claude/skills/` (when in a
/// repo) and `~/.claude/skills/` — for installed skill files and report the
/// lowest stamped version among them. Returns `None` when no skill is installed
/// anywhere (nothing to refresh). Used by the launch-time staleness check.
pub fn installed_skill_status() -> Option<InstalledSkills> {
    // Project and Local share `<repo>/.claude/skills/`, so the two repo scopes
    // collapse to one directory; User is the home one. Missing locations (not in
    // a repo, no home) simply contribute nothing.
    let dirs: Vec<PathBuf> = [Location::Project, Location::User]
        .into_iter()
        .filter_map(|loc| skills_dir(loc).ok())
        .collect();
    status_in_dirs(&dirs)
}

/// The directory-scan core of [`installed_skill_status`], split out so tests can
/// point it at a temp tree instead of the real repo/home locations.
fn status_in_dirs(dirs: &[PathBuf]) -> Option<InstalledSkills> {
    let mut paths = Vec::new();
    let mut lowest = u32::MAX;
    for dir in dirs {
        for skill in SKILLS {
            let file = dir.join(skill.name).join("SKILL.md");
            let Ok(contents) = std::fs::read_to_string(&file) else {
                continue;
            };
            lowest = lowest.min(read_stamped_version(&contents));
            paths.push(file);
        }
    }

    (!paths.is_empty()).then_some(InstalledSkills {
        version: lowest,
        paths,
    })
}

/// Force-rewrite every installed skill file (re-stamping it to [`SKILL_VERSION`]),
/// so a stale install is brought current. Rewrites in place at each located path,
/// matching the skill body by the path's parent directory name.
pub fn refresh_installed_skills() -> Result<()> {
    let Some(installed) = installed_skill_status() else {
        return Ok(());
    };
    for path in installed.paths {
        let Some(skill) = skill_for_path(&path) else {
            continue;
        };
        std::fs::write(&path, stamp_version(skill.body, SKILL_VERSION))
            .with_context(|| format!("refreshing {}", path.display()))?;
    }
    Ok(())
}

/// Match an installed `SKILL.md` path back to its [`Skill`] by the directory name
/// it lives under (`…/<skill-name>/SKILL.md`).
fn skill_for_path(path: &Path) -> Option<&'static Skill> {
    let dir_name = path.parent()?.file_name()?.to_str()?;
    SKILLS.iter().find(|s| s.name == dir_name)
}

/// The file recording the skill version the user chose to skip, beside the
/// config dir (the parent of the resolved config path). Kept separate from the
/// Lua config so the skip is a plain integer, not folded into the keymap opt-out.
fn skip_file() -> Option<PathBuf> {
    crate::lua::config_path()?
        .parent()
        .map(|dir| dir.join("skills-skipped-version"))
}

/// The skill version the user last chose to skip, or `0` if none (no file, or
/// unreadable/garbage contents read as "skip nothing").
pub fn skipped_skill_version() -> u32 {
    skip_file()
        .and_then(|p| std::fs::read_to_string(p).ok())
        .and_then(|s| s.trim().parse::<u32>().ok())
        .unwrap_or(0)
}

/// Persist that the user skipped `version`, so the refresh prompt stays quiet
/// until a newer [`SKILL_VERSION`] ships. Creates the config dir if needed.
pub fn skip_skill_version(version: u32) -> Result<()> {
    let path = skip_file()
        .ok_or_else(|| anyhow::anyhow!("no config path to write the skip marker beside"))?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("creating {}", parent.display()))?;
    }
    std::fs::write(&path, format!("{version}\n"))
        .with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Whether the launch-time refresh prompt should fire: a stale install exists
/// *and* the user hasn't already skipped this version (or a newer one).
pub fn should_prompt_skill_refresh() -> bool {
    installed_skill_status().is_some_and(|s| s.version < SKILL_VERSION)
        && skipped_skill_version() < SKILL_VERSION
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
    // `CLAUDE_SKILLS_HOME` overrides the home directory used to locate user-level
    // skills. When set, it wins outright (the real home is not consulted), which
    // lets tests point the user-scope skill scan at an isolated tree so the
    // host's real `~/.claude/skills` can't leak in — e.g. a stale installed
    // version triggering the TUI's refresh prompt mid-test.
    if let Some(dir) = std::env::var_os("CLAUDE_SKILLS_HOME") {
        return Ok(PathBuf::from(dir));
    }
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
    std::fs::write(&file, stamp_version(skill.body, SKILL_VERSION))
        .with_context(|| format!("writing {}", file.display()))?;
    println!("installed {}", file.display());
    Ok(())
}

/// Stamp `mudpuppy-skill-version: N` into a skill body's YAML frontmatter so the
/// installed file records which version it is. The body always opens with a
/// `---\n` frontmatter fence (asserted by a test); the stamp goes on the line
/// right after it.
fn stamp_version(body: &str, version: u32) -> String {
    let stamp = format!("{VERSION_KEY}: {version}\n");
    match body.strip_prefix("---\n") {
        Some(rest) => format!("---\n{stamp}{rest}"),
        // No frontmatter fence (shouldn't happen): prepend the stamp so the
        // version is at least recorded rather than silently dropped.
        None => format!("{stamp}{body}"),
    }
}

/// Read the stamped version from an installed `SKILL.md`'s frontmatter. A file
/// with no stamp (an older install, written before versioning) reads as `0`,
/// which is always stale.
fn read_stamped_version(contents: &str) -> u32 {
    contents
        .lines()
        // The frontmatter is the leading `---`…`---` block: skip the opening
        // fence, then read until the closing one, so a stray match in the body
        // can't be mistaken for the stamp.
        .skip(1)
        .take_while(|l| l.trim() != "---")
        .find_map(|l| {
            l.strip_prefix(VERSION_KEY)
                .and_then(|r| r.trim_start().strip_prefix(':'))
                .and_then(|n| n.trim().parse::<u32>().ok())
        })
        .unwrap_or(0)
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

    /// Write `<dir>/<skill>/SKILL.md` for every skill, each stamped at `version`
    /// (or unstamped when `None`), and return `dir`'s skills root.
    fn install_at(version: Option<u32>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        for skill in SKILLS {
            let file = dir.path().join(skill.name).join("SKILL.md");
            std::fs::create_dir_all(file.parent().unwrap()).unwrap();
            let body = match version {
                Some(v) => stamp_version(skill.body, v),
                // Strip any accidental stamp: an unstamped (legacy) file.
                None => skill.body.to_string(),
            };
            std::fs::write(&file, body).unwrap();
        }
        dir
    }

    #[test]
    fn stamp_and_read_round_trip() {
        let stamped = stamp_version(SKILLS[0].body, 7);
        assert!(stamped.starts_with("---\nmudpuppy-skill-version: 7\n"));
        assert_eq!(read_stamped_version(&stamped), 7);
        // The original (unstamped) body reads as version 0 = stale.
        assert_eq!(read_stamped_version(SKILLS[0].body), 0);
    }

    #[test]
    fn current_stamp_is_not_stale_but_older_and_unstamped_are() {
        // A current-version install is the latest, so not stale.
        let current = install_at(Some(SKILL_VERSION));
        let status = status_in_dirs(&[current.path().to_path_buf()]).expect("installed");
        assert_eq!(status.version, SKILL_VERSION);
        assert!(status.version >= SKILL_VERSION, "current is not stale");

        // An older install is stale.
        let older = install_at(Some(SKILL_VERSION.saturating_sub(1)));
        let status = status_in_dirs(&[older.path().to_path_buf()]).unwrap();
        assert!(status.version < SKILL_VERSION, "older install is stale");

        // An unstamped (legacy) file reads as 0 = stale.
        let legacy = install_at(None);
        let status = status_in_dirs(&[legacy.path().to_path_buf()]).unwrap();
        assert_eq!(status.version, 0, "an unstamped file is version 0");

        // No install anywhere reports nothing.
        let empty = tempfile::tempdir().unwrap();
        assert!(status_in_dirs(&[empty.path().to_path_buf()]).is_none());
    }

    #[test]
    fn lowest_version_wins_across_locations() {
        // A mixed install (one current dir, one stale dir) reports the lower one.
        let current = install_at(Some(SKILL_VERSION));
        let legacy = install_at(None);
        let status =
            status_in_dirs(&[current.path().to_path_buf(), legacy.path().to_path_buf()]).unwrap();
        assert_eq!(status.version, 0, "the stale location pins the report");
    }

    #[test]
    fn refresh_rewrites_and_re_stamps_stale_files() {
        let legacy = install_at(None);
        let dirs = [legacy.path().to_path_buf()];
        assert_eq!(status_in_dirs(&dirs).unwrap().version, 0);

        // Refresh each located file in place (the public refresh reuses this).
        for path in status_in_dirs(&dirs).unwrap().paths {
            let skill = skill_for_path(&path).unwrap();
            std::fs::write(&path, stamp_version(skill.body, SKILL_VERSION)).unwrap();
        }
        assert_eq!(
            status_in_dirs(&dirs).unwrap().version,
            SKILL_VERSION,
            "after refresh the install is current"
        );
    }

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
