//! Repo + target resolution and store-path derivation (PLAN.md §5).
//!
//! The session key is the canonical git repo root plus the review target
//! (`local` or a PR). The store path encodes both —
//! `<data-dir>/mudpuppy/<repo-slug>/<target>/annotations.json` — so reopening in
//! the same repo, for the same target, reattaches to the same store
//! automatically. That repo-keyed path (not a process id) is what makes resume
//! free: same repo + same target → same file.
//!
//! `<repo-slug>` is the remote's `owner/repo` when there is one, else the
//! sanitized canonical repo path (so repos with no remote still work).
//!
//! The data dir comes from the platform convention (via `directories`), but the
//! `MUDPUPPY_DATA_DIR` environment variable overrides it — handy for tests and
//! for users who want the store somewhere explicit.
//!
//! The blocking turn protocol, liveness/pidfile, and reset (the rest of §5/§6)
//! arrive with milestone 3; this module currently covers path resolution only.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::ProjectDirs;

use crate::domain::Target;
use crate::source;

/// Environment variable that overrides the platform data directory.
pub const DATA_DIR_ENV: &str = "MUDPUPPY_DATA_DIR";

/// The basename of the store file within a session directory.
const STORE_FILE: &str = "annotations.json";

/// A resolved review session: where its annotation store lives on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Canonical git repo root the review is anchored to.
    pub repo_root: PathBuf,
    /// What is under review (local changes or a PR).
    pub target: Target,
    /// Absolute path to this session's `annotations.json`.
    pub store_path: PathBuf,
}

impl Session {
    /// Resolve the session for `target` from the current working directory's
    /// git repo, deriving the store path. Does not create any files.
    pub fn resolve(target: Target) -> Result<Session> {
        let repo_root = repo_root().context("resolving the git repository root")?;
        let remote = remote_url();
        let slug = repo_slug(&repo_root, remote.as_deref());
        let store_path = store_path(&data_dir()?, &slug, &target);
        Ok(Session {
            repo_root,
            target,
            store_path,
        })
    }
}

/// The canonical git repo root for the current working directory.
fn repo_root() -> Result<PathBuf> {
    let out = source::git(&["rev-parse", "--show-toplevel"])
        .context("not inside a git repository (mudpuppy reviews a repo's changes)")?;
    Ok(PathBuf::from(out.trim()))
}

/// The `origin` remote URL, if the repo has one. Best-effort: `None` on any
/// failure (no remote, detached config, etc.), which falls back to a path slug.
fn remote_url() -> Option<String> {
    source::git(&["config", "--get", "remote.origin.url"])
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

/// The base data directory: `MUDPUPPY_DATA_DIR` if set, else the platform
/// convention (e.g. `~/.local/share/mudpuppy` on Linux).
fn data_dir() -> Result<PathBuf> {
    if let Some(dir) = std::env::var_os(DATA_DIR_ENV) {
        return Ok(PathBuf::from(dir));
    }
    let dirs = ProjectDirs::from("", "", "mudpuppy")
        .context("could not determine a platform data directory for the annotation store")?;
    Ok(dirs.data_dir().to_path_buf())
}

/// Derive the on-disk slug for a repo: the remote's `owner/repo` when one is
/// present, else the sanitized canonical repo path so repos without a remote
/// still get a stable, collision-free directory.
fn repo_slug(repo_root: &Path, remote: Option<&str>) -> String {
    remote
        .and_then(slug_from_remote)
        .unwrap_or_else(|| sanitize_path(repo_root))
}

/// Extract `owner/repo` from a git remote URL, dropping any trailing `.git`.
///
/// Handles the two common forms: `git@host:owner/repo.git` (scp-like) and
/// `https://host/owner/repo.git`. Returns `None` for anything that doesn't yield
/// at least two trailing path segments, so the caller can fall back to a path slug.
fn slug_from_remote(remote: &str) -> Option<String> {
    // Normalize the scp-like form `git@host:owner/repo` to a `/`-delimited tail.
    let after_host = if let Some((_, rest)) = remote.split_once("://") {
        // https://host/owner/repo  ->  host/owner/repo  ->  drop the host
        rest.split_once('/').map(|(_, tail)| tail)?
    } else if let Some((_, rest)) = remote.split_once(':') {
        // git@host:owner/repo  ->  owner/repo
        rest
    } else {
        remote
    };

    let trimmed = after_host.trim_matches('/');
    let trimmed = trimmed.strip_suffix(".git").unwrap_or(trimmed);
    let mut segments = trimmed.rsplit('/');
    let repo = segments.next().filter(|s| !s.is_empty())?;
    let owner = segments.next().filter(|s| !s.is_empty())?;
    Some(format!("{owner}/{repo}"))
}

/// Sanitize an absolute path into a single filesystem-safe slug segment, so a
/// repo with no remote still maps to a stable directory.
fn sanitize_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let cleaned: String = raw
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    // Collapse runs of '-' and trim the edges for a tidy, stable name.
    let mut out = String::with_capacity(cleaned.len());
    let mut prev_dash = false;
    for c in cleaned.chars() {
        if c == '-' {
            if !prev_dash {
                out.push(c);
            }
            prev_dash = true;
        } else {
            out.push(c);
            prev_dash = false;
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "repo".to_string()
    } else {
        trimmed.to_string()
    }
}

/// The directory-name component for a target: `local`, or `pr/<sanitized>` so a
/// PR's review lives in its own subtree distinct from local changes.
fn target_dir(target: &Target) -> PathBuf {
    match target {
        Target::Local { .. } => PathBuf::from("local"),
        Target::Pr { pr, .. } => Path::new("pr").join(sanitize_segment(pr)),
    }
}

/// Sanitize an arbitrary string into one filesystem-safe path segment.
fn sanitize_segment(s: &str) -> String {
    let cleaned: String = s
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let trimmed = cleaned.trim_matches('-');
    if trimmed.is_empty() {
        "pr".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Assemble the full store path from the data dir, repo slug, and target.
fn store_path(data_dir: &Path, slug: &str, target: &Target) -> PathBuf {
    data_dir
        .join("mudpuppy")
        .join(slug)
        .join(target_dir(target))
        .join(STORE_FILE)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_from_https_and_ssh_remotes() {
        assert_eq!(
            slug_from_remote("https://github.com/octocat/hello.git").as_deref(),
            Some("octocat/hello")
        );
        assert_eq!(
            slug_from_remote("https://github.com/octocat/hello").as_deref(),
            Some("octocat/hello")
        );
        assert_eq!(
            slug_from_remote("git@github.com:octocat/hello.git").as_deref(),
            Some("octocat/hello")
        );
        // Nested groups (e.g. GitLab subgroups) keep the last two segments.
        assert_eq!(
            slug_from_remote("https://gitlab.com/group/sub/proj.git").as_deref(),
            Some("sub/proj")
        );
    }

    #[test]
    fn slug_from_remote_rejects_degenerate_urls() {
        assert_eq!(slug_from_remote("not-a-url"), None);
        assert_eq!(slug_from_remote("https://github.com/just-owner"), None);
    }

    #[test]
    fn path_slug_is_filesystem_safe_and_stable() {
        let slug = sanitize_path(Path::new("/Users/kaan/Code/my repo!"));
        assert_eq!(slug, "Users-kaan-Code-my-repo");
        assert!(!slug.contains('/'));
    }

    #[test]
    fn repo_slug_prefers_remote_then_falls_back_to_path() {
        let root = Path::new("/tmp/x");
        assert_eq!(
            repo_slug(root, Some("git@github.com:o/r.git")),
            "o/r".to_string()
        );
        assert_eq!(repo_slug(root, None), "tmp-x".to_string());
    }

    #[test]
    fn store_path_encodes_repo_and_target() {
        let data = Path::new("/data");
        let local = Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        };
        assert_eq!(
            store_path(data, "o/r", &local),
            PathBuf::from("/data/mudpuppy/o/r/local/annotations.json")
        );

        let pr = Target::Pr {
            pr: "o/r#123".to_string(),
            head_sha: "abc".to_string(),
        };
        assert_eq!(
            store_path(data, "o/r", &pr),
            PathBuf::from("/data/mudpuppy/o/r/pr/o-r-123/annotations.json")
        );
    }
}
