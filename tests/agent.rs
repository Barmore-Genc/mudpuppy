//! End-to-end tests for the `mudpuppy agent` command surface.
//!
//! These drive the *real* binary (plain captured stdout/stderr, no PTY) against
//! a throwaway git repo, with `MUDPUPPY_DATA_DIR` pointed at a temp directory so
//! the host's real store is never touched. They exercise the whole vertical
//! slice the milestone adds: target resolution → store path → atomic+locked
//! merge-by-id writes → readback. This is the agent half of the proof-of-concept
//! collaboration loop.

mod common;

use std::path::{Path, PathBuf};
use std::process::Command;

use common::repo_with_changes;

/// Run `mudpuppy <args>` inside `repo`, with the store redirected to `data`.
/// Returns `(stdout, stderr, success)`.
fn run(repo: &Path, data: &Path, args: &[&str]) -> (String, String, bool) {
    let out = Command::new(env!("CARGO_BIN_EXE_mudpuppy"))
        .args(args)
        .current_dir(repo)
        .env("MUDPUPPY_DATA_DIR", data)
        // Match the fixture's git isolation so resolution is host-independent.
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

/// Locate the single `annotations.json` written somewhere under `data`.
fn find_store(data: &Path) -> Option<PathBuf> {
    fn walk(dir: &Path) -> Option<PathBuf> {
        for entry in std::fs::read_dir(dir).ok()?.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if let Some(found) = walk(&path) {
                    return Some(found);
                }
            } else if path.file_name().is_some_and(|n| n == "annotations.json") {
                return Some(path);
            }
        }
        None
    }
    walk(data)
}

#[test]
fn add_list_resolve_round_trip() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    // Add an agent comment; stdout is the new id.
    let (stdout, stderr, ok) = run(
        repo,
        data,
        &[
            "agent",
            "comment",
            "add",
            "--file",
            "a_app.rs",
            "--line",
            "1",
            "--severity",
            "blocker",
            "--body",
            "first comment",
        ],
    );
    assert!(ok, "add failed: {stderr}");
    let id = stdout.trim().to_string();
    assert_eq!(id.len(), 8, "expected an 8-char nanoid, got {id:?}");

    // The store lands under the redirected data dir.
    let store = find_store(data).expect("store file written");
    let json = std::fs::read_to_string(&store).unwrap();
    assert!(json.contains("\"schema_version\""));
    assert!(json.contains("first comment"));

    // List shows it.
    let (stdout, stderr, ok) = run(repo, data, &["agent", "comment", "list"]);
    assert!(ok, "list failed: {stderr}");
    assert!(stdout.contains(&id), "list missing id: {stdout}");
    assert!(stdout.contains("blocker"));
    assert!(stdout.contains("first comment"));
    assert!(stdout.contains("a_app.rs:1 (right)"));

    // Resolve it, then it drops out of the --open list.
    let (_, stderr, ok) = run(repo, data, &["agent", "comment", "resolve", "--id", &id]);
    assert!(ok, "resolve failed: {stderr}");
    let (stdout, _, ok) = run(repo, data, &["agent", "comment", "list", "--open"]);
    assert!(ok);
    assert!(
        stdout.contains("no matching annotations"),
        "resolved comment should not be open: {stdout}"
    );
}

#[test]
fn replies_thread_and_cancel_hard_deletes() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    let (parent, _, ok) = run(
        repo,
        data,
        &[
            "agent",
            "comment",
            "add",
            "--file",
            "a_app.rs",
            "--line",
            "2",
            "--body",
            "parent body",
        ],
    );
    assert!(ok);
    let parent = parent.trim().to_string();

    let (reply, stderr, ok) = run(
        repo,
        data,
        &[
            "agent",
            "comment",
            "add",
            "--file",
            "a_app.rs",
            "--line",
            "2",
            "--reply-to",
            &parent,
            "--body",
            "reply body",
        ],
    );
    assert!(ok, "reply failed: {stderr}");
    let reply = reply.trim().to_string();

    // A reply to a missing id is rejected.
    let (_, stderr, ok) = run(
        repo,
        data,
        &[
            "agent",
            "comment",
            "add",
            "--file",
            "a_app.rs",
            "--line",
            "2",
            "--reply-to",
            "deadbeef",
            "--body",
            "orphan",
        ],
    );
    assert!(!ok, "reply to a missing parent should fail");
    assert!(stderr.contains("reply target"), "stderr: {stderr}");

    // Cancelling the childless reply hard-deletes it.
    let (stdout, _, ok) = run(repo, data, &["agent", "comment", "cancel", "--id", &reply]);
    assert!(ok);
    assert!(stdout.contains("deleted"), "stdout: {stdout}");

    // Cancelling the parent (which had a reply, now gone) also hard-deletes it.
    let (stdout, _, ok) = run(repo, data, &["agent", "comment", "cancel", "--id", &parent]);
    assert!(ok);
    assert!(stdout.contains("deleted"));

    let (stdout, _, _) = run(repo, data, &["agent", "comment", "list"]);
    assert!(stdout.contains("no annotations yet") || stdout.contains("no matching"));
}

#[test]
fn agent_cannot_cancel_the_humans_annotation() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    // Create the store via a normal agent add, then inject a *human* annotation
    // directly — standing in for one the TUI wrote.
    let (_, _, ok) = run(
        repo,
        data,
        &[
            "agent",
            "comment",
            "add",
            "--file",
            "a_app.rs",
            "--line",
            "1",
            "--body",
            "agent note",
        ],
    );
    assert!(ok);
    let store = find_store(data).unwrap();
    let mut state: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&store).unwrap()).unwrap();
    state["annotations"]
        .as_array_mut()
        .unwrap()
        .push(serde_json::json!({
            "id": "human001",
            "author": "human",
            "file": "a_app.rs",
            "line": 1,
            "side": "RIGHT",
            "severity": "warning",
            "tag": null,
            "status": "open",
            "body": "human note",
            "reply_to": null,
            "created_at": "2026-05-28T12:00:00Z",
            "updated_at": "2026-05-28T12:00:00Z"
        }));
    std::fs::write(&store, serde_json::to_string_pretty(&state).unwrap()).unwrap();

    // The agent may not cancel or edit it.
    let (_, stderr, ok) = run(
        repo,
        data,
        &["agent", "comment", "cancel", "--id", "human001"],
    );
    assert!(!ok, "agent must not cancel the human's annotation");
    assert!(stderr.contains("human's annotation"), "stderr: {stderr}");

    // But a status change (resolve) is allowed on either author's annotation.
    let (_, _, ok) = run(
        repo,
        data,
        &["agent", "comment", "resolve", "--id", "human001"],
    );
    assert!(ok, "resolving any annotation is allowed");
}

#[test]
fn diff_prints_the_file_under_review() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    let (stdout, stderr, ok) = run(repo, data, &["agent", "diff"]);
    assert!(ok, "diff failed: {stderr}");
    assert!(stdout.contains("diff --git"), "stdout: {stdout}");
    assert!(stdout.contains("a_app.rs"));

    // --file narrows to one section.
    let (stdout, _, ok) = run(repo, data, &["agent", "diff", "--file", "b_notes.txt"]);
    assert!(ok);
    assert!(stdout.contains("b_notes.txt"));
    assert!(
        !stdout.contains("a_app.rs"),
        "only the requested file: {stdout}"
    );
}

#[test]
fn reset_clears_the_session() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    run(
        repo,
        data,
        &[
            "agent", "comment", "add", "--file", "a_app.rs", "--line", "1", "--body", "x",
        ],
    );
    let (stdout, _, ok) = run(repo, data, &["agent", "reset"]);
    assert!(ok);
    assert!(stdout.contains("cleared 1"), "stdout: {stdout}");

    let (stdout, _, _) = run(repo, data, &["agent", "comment", "list"]);
    assert!(stdout.contains("no annotations") || stdout.contains("no matching"));
}
