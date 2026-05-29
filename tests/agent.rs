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
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use common::{repo_with_changes, Session};

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

/// Spawn `mudpuppy <args>` detached, capturing stdout/stderr, so the test can
/// manipulate the store while the child blocks. (`run` blocks on `.output()`,
/// which can't interleave with `agent wait`.)
fn spawn(repo: &Path, data: &Path, args: &[&str]) -> std::process::Child {
    Command::new(env!("CARGO_BIN_EXE_mudpuppy"))
        .args(args)
        .current_dir(repo)
        .env("MUDPUPPY_DATA_DIR", data)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn mudpuppy")
}

/// Poll `f` until it returns `Some`, or give up after `timeout`.
fn poll_until<T>(timeout: Duration, mut f: impl FnMut() -> Option<T>) -> Option<T> {
    let start = Instant::now();
    loop {
        if let Some(v) = f() {
            return Some(v);
        }
        if start.elapsed() > timeout {
            return None;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
}

/// Read the store as JSON, if it exists yet.
fn read_store(store: &Path) -> Option<serde_json::Value> {
    serde_json::from_str(&std::fs::read_to_string(store).ok()?).ok()
}

#[test]
fn wait_blocks_until_the_tui_releases_the_turn() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    // Seed an agent comment so there's a real session for the TUI to open onto.
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
            "1",
            "--body",
            "look here",
        ],
    );
    assert!(ok, "seed add failed: {stderr}");

    // `agent wait` blocks; a generous --timeout is only a safety net so a bug
    // can't hang the suite forever.
    let child = spawn(repo, data, &["agent", "wait", "--timeout", "30"]);

    // Wait until `wait` has flipped `agent_waiting` on — proof it's blocked.
    poll_until(Duration::from_secs(10), || {
        let v = read_store(&find_store(data)?)?;
        (v["turn"]["agent_waiting"] == serde_json::json!(true)).then_some(())
    })
    .expect("`agent wait` should mark agent_waiting");

    // Launch the *real* TUI on the same repo + store. It reads the turn block and
    // surfaces that the agent is waiting.
    let mut tui = Session::launch_with_env(repo, &[("MUDPUPPY_DATA_DIR", data)]);
    assert!(
        tui.wait_for_screen("agent waiting", Duration::from_secs(10)),
        "TUI never showed the waiting agent; screen was:\n{}",
        tui.screen()
    );

    // The human releases the turn with `r`; that store write is what wakes `wait`.
    tui.feed(b"r");

    let out = child.wait_with_output().expect("wait should exit");
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "wait should exit 0 once the TUI releases; stderr: {stderr}"
    );
    // The TUI released without authoring anything, so there are no annotation
    // changes to report — but the rendezvous still completed.
    assert!(
        stdout.contains("turn released with no changes"),
        "wait should report the (empty) release: {stdout}"
    );

    // The release bumped seq and handed ownership back to the agent.
    let v = read_store(&find_store(data).unwrap()).unwrap();
    assert_eq!(v["turn"]["owner"], serde_json::json!("agent"));
    assert_eq!(v["turn"]["agent_waiting"], serde_json::json!(false));
    assert!(v["turn"]["seq"].as_u64().unwrap() >= 1);
    assert_eq!(
        v["turn"]["approved"],
        serde_json::json!(true),
        "first release approves"
    );

    // The TUI itself quits cleanly.
    tui.feed(b"q");
    assert!(tui
        .wait(Duration::from_secs(10))
        .is_some_and(|s| s.success()));
}

#[test]
fn wait_times_out_cleanly_and_clears_the_flag() {
    let repo = repo_with_changes();
    let data = tempfile::tempdir().unwrap();
    let (repo, data) = (repo.path(), data.path());

    // No release ever arrives; a 1s timeout must return promptly, exit non-zero,
    // and leave `agent_waiting` cleared so the next round isn't confused.
    let (_, stderr, ok) = run(repo, data, &["agent", "wait", "--timeout", "1"]);
    assert!(!ok, "a timeout is a non-zero exit");
    assert!(stderr.contains("timed out"), "stderr: {stderr}");

    let store = find_store(data).expect("wait creates the store");
    let v = read_store(&store).unwrap();
    assert_eq!(
        v["turn"]["agent_waiting"],
        serde_json::json!(false),
        "agent_waiting must be cleared after a timeout"
    );
}
