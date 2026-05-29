//! Diff-source providers (PLAN.md §3, §5).
//!
//! Produces the raw unified diff under review and the [`Target`] that pins it to
//! a head SHA. Two sources:
//!
//! - **local** — shells out to `git`, comparing the working tree (uncommitted
//!   edits included) against the merge-base of the base ref and `HEAD`. On the
//!   default branch the merge-base *is* `HEAD`, so this naturally degrades to
//!   "uncommitted changes vs HEAD".
//! - **pr** — shells out to `gh pr diff` (read-only), per the no-writes-to-
//!   GitHub rule.
//!
//! Everything here is a subprocess call; mudpuppy makes no network calls of its
//! own. `git`/`gh` failures surface as clear, actionable errors.

use std::io::ErrorKind;
use std::process::Command;

use anyhow::{bail, Context, Result};

use crate::domain::Target;

/// A resolved diff plus the target that identifies it.
#[derive(Debug, Clone)]
pub struct LoadedDiff {
    /// What is being reviewed (records the head SHA for staleness checks).
    pub target: Target,
    /// The raw unified diff text, ready for [`crate::diff::parse_diff`].
    pub raw: String,
}

/// Load the diff under review.
///
/// `pr` selects a pull-request target (anything `gh pr diff` accepts: a number,
/// `owner/repo#123`, or a URL); otherwise the review targets local changes,
/// with `base` overriding the inferred base ref.
pub fn load(pr: Option<&str>, base: Option<&str>) -> Result<LoadedDiff> {
    match pr {
        Some(pr) => load_pr(pr),
        None => load_local(base),
    }
}

/// Load local `git` changes against `base` (or the inferred default branch).
fn load_local(base: Option<&str>) -> Result<LoadedDiff> {
    // Doubles as the "are we in a git repo?" check.
    git(&["rev-parse", "--show-toplevel"])
        .context("not inside a git repository (mudpuppy reviews a repo's changes)")?;

    let resolved_base = match base {
        Some(b) => Some(b.to_string()),
        None => default_branch()?,
    };

    let raw = match &resolved_base {
        // Compare the working tree against the merge-base of <base> and HEAD, so
        // the diff is exactly this branch's work including uncommitted edits.
        Some(base_ref) => git(&["diff", "--merge-base", base_ref])
            .with_context(|| format!("computing diff against base `{base_ref}`"))?,
        // No base to anchor to (no default branch found, none given): fall back
        // to uncommitted changes against HEAD.
        None => git(&["diff", "HEAD"]).context("computing diff against HEAD")?,
    };

    let head_sha = git(&["rev-parse", "HEAD"])
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    Ok(LoadedDiff {
        target: Target::Local {
            base: resolved_base.unwrap_or_else(|| "HEAD".to_string()),
            head_sha,
        },
        raw,
    })
}

/// Load a GitHub pull request's diff via `gh` (read-only).
fn load_pr(pr: &str) -> Result<LoadedDiff> {
    // `gh pr diff` accepts a number or URL directly, but not the documented
    // `owner/repo#123` form — translate that into `<n> --repo owner/repo`.
    let selector = gh_pr_selector(pr);
    let mut diff_args = vec!["pr", "diff"];
    diff_args.extend(selector.iter().map(String::as_str));
    let raw = gh(&diff_args).with_context(|| format!("fetching diff for PR `{pr}`"))?;

    // Best-effort head SHA; the diff is still usable without it.
    let mut view_args = vec!["pr", "view"];
    view_args.extend(selector.iter().map(String::as_str));
    view_args.extend(["--json", "headRefOid", "-q", ".headRefOid"]);
    let head_sha = gh(&view_args)
        .map(|s| s.trim().to_string())
        .unwrap_or_default();

    Ok(LoadedDiff {
        target: Target::Pr {
            pr: pr.to_string(),
            head_sha,
        },
        raw,
    })
}

/// Build the `gh` PR selector arguments for a user-supplied PR reference.
///
/// `owner/repo#123` expands to `["123", "--repo", "owner/repo"]`; anything else
/// (a bare number or a full URL, both of which `gh` accepts as-is) passes
/// through unchanged.
fn gh_pr_selector(pr: &str) -> Vec<String> {
    if let Some((repo, number)) = pr.split_once('#') {
        if repo.contains('/') && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()) {
            return vec![number.to_string(), "--repo".to_string(), repo.to_string()];
        }
    }
    vec![pr.to_string()]
}

/// Determine the repository's default branch as a ref usable for `git diff`.
///
/// Prefers the remote's advertised default (`origin/HEAD`), then falls back to
/// the conventional branch names. Returns `None` when none resolve, letting the
/// caller diff against `HEAD` instead of failing.
fn default_branch() -> Result<Option<String>> {
    // `origin/HEAD` -> "refs/remotes/origin/main"; trim to "origin/main".
    if let Ok(symref) = git(&["symbolic-ref", "--quiet", "refs/remotes/origin/HEAD"]) {
        if let Some(name) = symref.trim().strip_prefix("refs/remotes/") {
            return Ok(Some(name.to_string()));
        }
    }

    for candidate in ["origin/main", "origin/master", "main", "master"] {
        if git(&["rev-parse", "--verify", "--quiet", candidate]).is_ok() {
            return Ok(Some(candidate.to_string()));
        }
    }

    Ok(None)
}

/// Run `git` with `args`, returning stdout on success.
fn git(args: &[&str]) -> Result<String> {
    run("git", args)
}

/// Run `gh` with `args`, returning stdout on success.
fn gh(args: &[&str]) -> Result<String> {
    run("gh", args)
}

/// Run a subprocess, mapping a missing binary and a non-zero exit to clear
/// errors. Returns captured stdout (lossily decoded) on success.
fn run(program: &str, args: &[&str]) -> Result<String> {
    let output = Command::new(program).args(args).output().map_err(|e| {
        if e.kind() == ErrorKind::NotFound {
            install_hint(program)
        } else {
            anyhow::Error::new(e).context(format!("failed to run `{program}`"))
        }
    })?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        let detail = stderr.trim();
        if detail.is_empty() {
            bail!("`{program} {}` failed", args.join(" "));
        }
        bail!("`{program} {}` failed: {detail}", args.join(" "));
    }

    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// A clear "install this" error for a missing required binary.
fn install_hint(program: &str) -> anyhow::Error {
    match program {
        "git" => {
            anyhow::anyhow!("`git` was not found on PATH; it is required to review local changes")
        }
        "gh" => anyhow::anyhow!(
            "`gh` (the GitHub CLI) was not found on PATH; it is required only to \
             review a pull request. Install it from https://cli.github.com/ and \
             run `gh auth login`."
        ),
        other => anyhow::anyhow!("`{other}` was not found on PATH"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn owner_repo_hash_number_expands_to_repo_flag() {
        assert_eq!(
            gh_pr_selector("octocat/hello#123"),
            vec!["123", "--repo", "octocat/hello"]
        );
    }

    #[test]
    fn bare_number_and_url_pass_through() {
        assert_eq!(gh_pr_selector("123"), vec!["123"]);
        let url = "https://github.com/octocat/hello/pull/7";
        assert_eq!(gh_pr_selector(url), vec![url]);
    }

    #[test]
    fn non_numeric_after_hash_is_not_treated_as_a_pr_number() {
        // A branch ref like `feature#foo` isn't `owner/repo#<n>`; leave it alone
        // and let `gh` decide what to do with it.
        assert_eq!(gh_pr_selector("feature#foo"), vec!["feature#foo"]);
    }
}
