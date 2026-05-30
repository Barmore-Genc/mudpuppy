//! End-to-end tests for `mudpuppy install claude`.
//!
//! These drive the *real* binary against a throwaway git repo, asserting that
//! both skills land at the chosen scope, that the local scope git-ignores them,
//! and that re-installing is idempotent (it neither clobbers nor errors silently
//! without `--force`). stdin is the inherited (non-tty) pipe, so the prompt paths
//! resolve to their non-interactive errors rather than hanging.

mod common;

use std::path::Path;
use std::process::Command;

use common::repo_clean;

/// Run `mudpuppy install <args>` inside `repo` with `HOME` redirected to `home`
/// (so a user-level install can't touch the real `~/.claude`). Returns
/// `(stdout, stderr, success)`.
fn run(repo: &Path, home: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_mudpuppy"))
        .args(args)
        .current_dir(repo)
        .env("HOME", home)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .expect("run mudpuppy");
    (
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
        out.status.success(),
    )
}

const SKILLS: [&str; 2] = ["mudpuppy-pr-review", "mudpuppy-implementation-review"];

#[test]
fn project_install_writes_both_skills_committed() {
    let repo = repo_clean();
    let home = tempfile::tempdir().unwrap();
    let (repo, home) = (repo.path(), home.path());

    let (out, err, ok) = run(repo, home, &["install", "claude", "--location", "project"]);
    assert!(ok, "install failed: {err}");

    for name in SKILLS {
        let skill = repo.join(".claude/skills").join(name).join("SKILL.md");
        assert!(skill.exists(), "missing {}\n{out}", skill.display());
        let body = std::fs::read_to_string(&skill).unwrap();
        assert!(body.contains(&format!("name: {name}")), "bad frontmatter");
    }

    // Project scope is committed: it must *not* be added to the local excludes.
    let exclude = repo.join(".git/info/exclude");
    let excludes = std::fs::read_to_string(&exclude).unwrap_or_default();
    assert!(
        !excludes.contains(".claude/skills"),
        "project install should not git-ignore the skills"
    );
}

#[test]
fn local_install_git_ignores_the_skills() {
    let repo = repo_clean();
    let home = tempfile::tempdir().unwrap();
    let (repo, home) = (repo.path(), home.path());

    let (_out, err, ok) = run(repo, home, &["install", "claude", "--location", "local"]);
    assert!(ok, "install failed: {err}");

    let excludes = std::fs::read_to_string(repo.join(".git/info/exclude")).unwrap();
    for name in SKILLS {
        assert!(
            excludes.contains(&format!("/.claude/skills/{name}/")),
            "local install should exclude {name}, got:\n{excludes}"
        );
    }

    // git agrees the working tree is clean (the skills are ignored, not untracked).
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .current_dir(repo)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .output()
        .unwrap();
    let status = String::from_utf8_lossy(&status.stdout);
    assert!(
        !status.contains(".claude"),
        "skills should be git-ignored, but git sees:\n{status}"
    );
}

#[test]
fn user_install_writes_under_home() {
    let repo = repo_clean();
    let home = tempfile::tempdir().unwrap();
    let (repo, home) = (repo.path(), home.path());

    let (_out, err, ok) = run(repo, home, &["install", "claude", "--location", "user"]);
    assert!(ok, "install failed: {err}");

    for name in SKILLS {
        let skill = home.join(".claude/skills").join(name).join("SKILL.md");
        assert!(skill.exists(), "missing {}", skill.display());
    }
}

#[test]
fn reinstall_is_idempotent_with_force_and_refuses_without() {
    let repo = repo_clean();
    let home = tempfile::tempdir().unwrap();
    let (repo, home) = (repo.path(), home.path());

    let (_o, e, ok) = run(repo, home, &["install", "claude", "--location", "project"]);
    assert!(ok, "first install failed: {e}");

    // Non-interactive re-install over existing files refuses rather than clobber.
    let (_o, err, ok) = run(repo, home, &["install", "claude", "--location", "project"]);
    assert!(!ok, "a second install without --force should fail");
    assert!(err.contains("--force"), "should mention --force: {err}");

    // With --force it overwrites cleanly.
    let (_o, e, ok) = run(
        repo,
        home,
        &["install", "claude", "--location", "project", "--force"],
    );
    assert!(ok, "forced re-install failed: {e}");
}

#[test]
fn omitting_location_non_interactively_is_an_error() {
    let repo = repo_clean();
    let home = tempfile::tempdir().unwrap();
    let (repo, home) = (repo.path(), home.path());

    let (_o, err, ok) = run(repo, home, &["install", "claude"]);
    assert!(!ok, "missing location with no tty should fail");
    assert!(
        err.contains("--location"),
        "should mention --location: {err}"
    );
}
