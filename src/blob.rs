//! Full file-content provider for context expansion.
//!
//! The diff under review only carries the changed hunks. To expand context
//! around a hunk — or to show an added / comment-only file in full — the TUI
//! needs the *whole* file at the target's revision. This module fetches that:
//! from the working tree for a local Head, via `git show` for the local Base,
//! and via `gh api` for a PR Head. Every source funnels through one pure
//! decoder ([`decode_blob`]) so the binary / size / newline rules are uniform
//! and unit-testable.
//!
//! Lookups are *tolerant*: an absent, binary, oversize, or otherwise
//! unresolvable file yields `Ok(None)`, never an error. Only a failure to spawn
//! the underlying `git`/`gh` process surfaces as `Err`.

use std::path::Path;

use anyhow::Result;

use crate::domain::Target;
use crate::source::{pr_owner_repo, run_optional};

/// Which side of the diff a content request is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum BlobSide {
    /// The new side: the file as it is at the target's head revision.
    Head,
    /// The old side: the file as it was at the diff's base (deletions / left
    /// context).
    Base,
}

/// Reject blobs larger than this many bytes; expanding context over a giant file
/// is pointless and we'd rather show nothing than stall the UI.
const MAX_BYTES: usize = 5 * 1024 * 1024;
/// Likewise cap the line count, guarding against many-but-short-lines files.
const MAX_LINES: usize = 200_000;
/// How far in we sniff for a NUL byte to decide a blob is binary.
const BINARY_SNIFF_BYTES: usize = 8192;

/// Full text of `path` at the target's revision, split into lines (content only:
/// no `+`/`-`/space diff marker, no trailing newline).
///
/// `Ok(None)` means the file is absent, binary, too large, or otherwise
/// unresolvable — all normal outcomes the caller renders as "no content". Only a
/// failure to spawn `git`/`gh` returns `Err`.
pub fn contents(
    target: &Target,
    repo_root: &Path,
    path: &str,
    side: BlobSide,
) -> Result<Option<Vec<String>>> {
    match (target, side) {
        (Target::Local { .. }, BlobSide::Head) => {
            // Working tree: read the file directly, no subprocess.
            match std::fs::read(repo_root.join(path)) {
                Ok(bytes) => Ok(decode_blob(&bytes)),
                Err(_) => Ok(None),
            }
        }
        (Target::Local { base, .. }, BlobSide::Base) => local_base(base, path),
        (Target::Pr { pr, head_sha }, BlobSide::Head) => pr_head(pr, head_sha, path),
        // PR base content is out of scope for v1.
        (Target::Pr { .. }, BlobSide::Base) => Ok(None),
    }
}

/// Local base side: resolve the merge-base of `base` and `HEAD` (or use `HEAD`
/// directly when that *is* the base), then read the file at that revision via
/// `git show`.
fn local_base(base: &str, path: &str) -> Result<Option<Vec<String>>> {
    let mergebase = if base == "HEAD" {
        "HEAD".to_string()
    } else {
        match run_optional("git", &["merge-base", base, "HEAD"])? {
            Some(out) => {
                let sha = out.trim();
                if sha.is_empty() {
                    return Ok(None);
                }
                sha.to_string()
            }
            None => return Ok(None),
        }
    };

    let spec = format!("{mergebase}:{path}");
    match run_optional("git", &["show", &spec])? {
        Some(out) => Ok(decode_blob(out.as_bytes())),
        None => Ok(None),
    }
}

/// PR head side: fetch the raw file at the PR head via `gh api`.
///
/// `{owner}/{repo}` come from the selector when it carries them, otherwise from
/// `gh pr view`. The ref is the head SHA, or the head branch name as a fallback;
/// with neither we give up (we never request an empty ref).
fn pr_head(pr: &str, head_sha: &str, path: &str) -> Result<Option<Vec<String>>> {
    let (owner, repo) = match pr_owner_repo(pr) {
        Some(pair) => pair,
        None => match pr_view_repo(pr)? {
            Some(pair) => pair,
            None => return Ok(None),
        },
    };

    let reference = if !head_sha.is_empty() {
        head_sha.to_string()
    } else {
        match pr_view_head_ref(pr)? {
            Some(r) if !r.is_empty() => r,
            _ => return Ok(None),
        }
    };

    let endpoint = format!("repos/{owner}/{repo}/contents/{path}");
    // Pass the ref via `-f` so it is never interpolated into the path.
    let ref_arg = format!("ref={reference}");
    match run_optional(
        "gh",
        &[
            "api",
            &endpoint,
            "-f",
            &ref_arg,
            "-H",
            "Accept: application/vnd.github.raw",
        ],
    )? {
        Some(out) => Ok(decode_blob(out.as_bytes())),
        None => Ok(None),
    }
}

/// Resolve a PR's head `owner/repo` via `gh pr view` when the selector omits it.
fn pr_view_repo(pr: &str) -> Result<Option<(String, String)>> {
    let out = match run_optional(
        "gh",
        &[
            "pr",
            "view",
            pr,
            "--json",
            "headRepository,headRepositoryOwner",
        ],
    )? {
        Some(out) => out,
        None => return Ok(None),
    };

    let value: serde_json::Value = match serde_json::from_str(&out) {
        Ok(v) => v,
        Err(_) => return Ok(None),
    };
    let owner = value
        .get("headRepositoryOwner")
        .and_then(|o| o.get("login"))
        .and_then(|l| l.as_str());
    let name = value
        .get("headRepository")
        .and_then(|r| r.get("name"))
        .and_then(|n| n.as_str());
    match (owner, name) {
        (Some(owner), Some(name)) if !owner.is_empty() && !name.is_empty() => {
            Ok(Some((owner.to_string(), name.to_string())))
        }
        _ => Ok(None),
    }
}

/// Resolve a PR's head branch name via `gh pr view`, the fallback ref when the
/// head SHA is unknown.
fn pr_view_head_ref(pr: &str) -> Result<Option<String>> {
    let out = match run_optional(
        "gh",
        &[
            "pr",
            "view",
            pr,
            "--json",
            "headRefName",
            "-q",
            ".headRefName",
        ],
    )? {
        Some(out) => out,
        None => return Ok(None),
    };
    let trimmed = out.trim();
    if trimmed.is_empty() {
        Ok(None)
    } else {
        Ok(Some(trimmed.to_string()))
    }
}

/// Decode raw blob bytes into content lines, or `None` if the blob is binary or
/// over the size caps.
///
/// The single pure core every source routes through: NUL-sniff for binary, cap
/// on bytes and lines, lossy-UTF-8 decode, split on `\n`, strip one trailing
/// `\r` per line (CRLF), and drop the single empty trailing line produced by a
/// final newline.
fn decode_blob(bytes: &[u8]) -> Option<Vec<String>> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let sniff = &bytes[..bytes.len().min(BINARY_SNIFF_BYTES)];
    if sniff.contains(&0) {
        return None;
    }

    let text = String::from_utf8_lossy(bytes);
    let mut lines: Vec<String> = text
        .split('\n')
        .map(|line| line.strip_suffix('\r').unwrap_or(line).to_string())
        .collect();

    // A trailing newline yields a phantom empty final element; drop just that one.
    if lines.last().is_some_and(|l| l.is_empty()) {
        lines.pop();
    }

    if lines.len() > MAX_LINES {
        return None;
    }
    Some(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_newline() {
        assert_eq!(
            decode_blob(b"a\nb\nc"),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn strips_trailing_carriage_return_per_line() {
        assert_eq!(
            decode_blob(b"a\r\nb\r\nc"),
            Some(vec!["a".to_string(), "b".to_string(), "c".to_string()])
        );
    }

    #[test]
    fn trailing_newline_does_not_add_phantom_line() {
        assert_eq!(
            decode_blob(b"a\nb\n"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
        // CRLF terminator likewise leaves no empty last line.
        assert_eq!(
            decode_blob(b"a\r\nb\r\n"),
            Some(vec!["a".to_string(), "b".to_string()])
        );
    }

    #[test]
    fn empty_input_is_no_lines() {
        assert_eq!(decode_blob(b""), Some(vec![]));
    }

    #[test]
    fn nul_byte_in_sniff_window_is_binary() {
        assert_eq!(decode_blob(b"abc\0def"), None);
    }

    #[test]
    fn nul_byte_past_sniff_window_is_not_examined() {
        // A NUL only after the sniff window is not treated as binary.
        let mut bytes = vec![b'a'; BINARY_SNIFF_BYTES + 10];
        *bytes.last_mut().unwrap() = 0;
        assert!(decode_blob(&bytes).is_some());
    }

    #[test]
    fn oversize_by_bytes_is_rejected() {
        let bytes = vec![b'a'; MAX_BYTES + 1];
        assert_eq!(decode_blob(&bytes), None);
    }

    #[test]
    fn oversize_by_line_count_is_rejected() {
        // Many short lines: under the byte cap but over the line cap.
        let bytes = vec![b'\n'; MAX_LINES + 1];
        assert_eq!(decode_blob(&bytes), None);
    }
}
