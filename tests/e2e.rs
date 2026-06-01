//! Layer 2 of TESTING.md: end-to-end smoke tests over the **real compiled
//! binary**, driven through a pseudo-terminal against a **real fixture git
//! repo**. This is the thin layer that proves the seams layer 1 mocks away
//! actually work once shipped:
//!
//! - the binary starts and parses its args (`main.rs` → `cli` → `tui::launch`);
//! - the `git` subprocess runs and its diff is parsed and painted;
//! - real tty bytes are decoded by crossterm (we type into the PTY master);
//! - raw mode + the alternate screen are entered **and torn back down** — the
//!   classic TUI shipping bug (TESTING.md Tier 1 #3).
//!
//! Following "assert coarsely, eyeball richly": we interpret the escape stream
//! with `vt100` into the settled character grid and assert on what a user would
//! see, plus the verbatim alt-screen enter/leave sequences and the process exit
//! code. The exact-match *pixel* oracle (real `resvg` renders) is a separate
//! layer — see `image_diff.rs` and `e2e/README.md`.
//!
//! These are hermetic: each test builds its own throwaway repo, points the
//! binary at it, and never touches the mudpuppy repo itself. `git` is the only
//! external requirement (a hard runtime dependency anyway), and PTYs exist on
//! both macOS dev and the Linux CI runners.

mod common;

use std::time::Duration;

use common::{repo_clean, repo_with_changes, write, Session, ENTER_ALT_SCREEN, LEAVE_ALT_SCREEN};

/// Tier 1 #1 — cold launch on a local diff: binary starts, the `git` subprocess
/// runs, the diff is parsed, and the first paint shows it in a real emulator.
#[test]
fn cold_launch_renders_local_diff() {
    let repo = repo_with_changes();
    let mut session = Session::launch(repo.path());

    // Settle on the status bar, the last thing painted, so we don't snapshot a
    // half-drawn first frame (the file name shows in the tree title before the
    // diff body and status bar land).
    assert!(
        session.wait_for_screen("file 1/2", Duration::from_secs(10)),
        "first paint never settled; screen was:\n{}",
        session.screen()
    );
    let screen = session.screen();
    assert!(
        screen.contains("a_app.rs"),
        "modified file missing:\n{screen}"
    );
    assert!(
        screen.contains("b_notes.txt"),
        "added file missing:\n{screen}"
    );
    // A real diff body line from the parsed git output (a deletion near the top
    // of the open file's first hunk).
    assert!(screen.contains("line 05"), "diff body missing:\n{screen}");

    session.feed(b"q");
    let status = session.wait(Duration::from_secs(10)).expect("clean exit");
    assert!(status.success(), "expected exit 0, got {status:?}");
}

/// Tier 1 #2 — keyboard navigation: typed keys are decoded end-to-end and the
/// screen re-renders. We focus the long file's diff and scroll to the bottom,
/// then switch files — both effects are read off the settled grid, not the raw
/// bytes, so ratatui's incremental cell redraws don't fool us.
#[test]
fn keyboard_navigation_updates_the_screen() {
    let repo = repo_with_changes();
    let mut session = Session::launch(repo.path());
    assert!(session.wait_for_screen("file 1/2", Duration::from_secs(10)));

    // Focus the (long) first file's diff and jump to the bottom: proves the
    // vim keys are decoded and the viewport actually scrolls.
    session.feed(b"l"); // tree -> diff focus
    session.feed(b"G"); // bottom of the diff
    assert!(
        session.wait_for_screen("BOT", Duration::from_secs(5)),
        "G did not scroll to the bottom; screen was:\n{}",
        session.screen()
    );

    // J switches to the next file without leaving the diff pane.
    session.feed(b"J");
    assert!(
        session.wait_for_screen("file 2/2", Duration::from_secs(5)),
        "J did not move to the next file; screen was:\n{}",
        session.screen()
    );

    session.feed(b"q");
    assert!(session
        .wait(Duration::from_secs(10))
        .is_some_and(|s| s.success()));
}

/// Tier 1 #3 — the most-overlooked TUI check: a clean quit restores the
/// terminal. We assert the binary both entered and *left* the alternate screen
/// and exited 0, so a user is never dumped back into a wrecked terminal.
#[test]
fn clean_exit_restores_the_terminal() {
    let repo = repo_with_changes();
    let mut session = Session::launch(repo.path());
    assert!(session.wait_for_screen("a_app.rs", Duration::from_secs(10)));

    session.feed(b"q");
    let status = session.wait(Duration::from_secs(10)).expect("clean exit");
    assert!(status.success(), "expected exit 0, got {status:?}");

    let raw = String::from_utf8_lossy(&session.raw()).into_owned();
    assert!(
        raw.contains(ENTER_ALT_SCREEN),
        "never entered the alt screen"
    );
    assert!(
        raw.contains(LEAVE_ALT_SCREEN),
        "alt screen was not restored on exit — terminal would be left dirty"
    );
    // Teardown must come last.
    assert!(
        raw.rfind(LEAVE_ALT_SCREEN) > raw.rfind(ENTER_ALT_SCREEN),
        "leave sequence did not follow enter"
    );
}

/// Config hot-reload over the real binary: the keymap is re-read live when the
/// config file changes, with no restart. We confirm the default `Space a`
/// (toggle the annotations panel) works, then rewrite the config to `unmap` that
/// leader sequence and bind the panel to `p` instead, and confirm — in the same
/// running process — that `Space a` goes dead and `p` takes over. The
/// `print(...)` marker in the new config is our signal (on the status bar) that
/// the reload has actually landed.
#[test]
fn config_hot_reload_rebinds_keys_live() {
    let repo = repo_with_changes();
    // Isolate both the store and the config so the test never touches host state;
    // the config file's directory is what the binary watches for hot-reload.
    let data_dir = tempfile::tempdir().unwrap();
    let config_dir = tempfile::tempdir().unwrap();
    let config = config_dir.path().join("mudpuppy.luau");

    let mut session = Session::launch_with_env(
        repo.path(),
        &[
            ("MUDPUPPY_CONFIG", config.as_path()),
            ("MUDPUPPY_DATA_DIR", data_dir.path()),
        ],
    );
    assert!(session.wait_for_screen("file 1/2", Duration::from_secs(10)));

    // Default binding: `Space a` opens the annotations panel.
    session.feed(b" a");
    assert!(
        session.wait_for_screen("Annotations", Duration::from_secs(5)),
        "default `Space a` did not open the panel; screen:\n{}",
        session.screen()
    );
    // Close it again so the post-reload checks start from a known (closed) state.
    session.feed(b" a");
    assert!(
        session.wait_until_absent("Annotations", Duration::from_secs(5)),
        "default `Space a` did not close the panel; screen:\n{}",
        session.screen()
    );

    // Rewrite the config: drop the default `Space a`, bind the panel to `p`, and
    // print a marker we can wait on to know the hot-reload has landed.
    std::fs::write(
        &config,
        "mudpuppy.unmap(\"global\", \"<leader> a\")\n\
         mudpuppy.map(\"global\", \"p\", function() mudpuppy.toggle_panel() end)\n\
         print(\"HOTRELOADED\")\n",
    )
    .unwrap();
    assert!(
        session.wait_for_screen("HOTRELOADED", Duration::from_secs(10)),
        "config never hot-reloaded; screen:\n{}",
        session.screen()
    );

    // The old sequence is now dead: `Space a` must do nothing.
    session.feed(b" a");
    assert!(
        session.absent_after("Annotations", Duration::from_millis(600)),
        "`Space a` still toggled the panel after being unmapped; screen:\n{}",
        session.screen()
    );

    // The new key works: `p` opens the panel.
    session.feed(b"p");
    assert!(
        session.wait_for_screen("Annotations", Duration::from_secs(5)),
        "rebound `p` did not open the panel; screen:\n{}",
        session.screen()
    );

    session.feed(b"q");
    assert!(session
        .wait(Duration::from_secs(10))
        .is_some_and(|s| s.success()));
}

/// Tier 2 #7 (edge state) — nothing to review: the binary says so on the normal
/// screen and exits cleanly, without ever flipping into the alternate screen.
#[test]
fn no_changes_prints_notice_without_entering_tui() {
    let repo = repo_clean();
    let mut session = Session::launch(repo.path());

    let status = session
        .wait(Duration::from_secs(10))
        .expect("exit promptly");
    assert!(status.success(), "expected exit 0, got {status:?}");

    let raw = String::from_utf8_lossy(&session.raw()).into_owned();
    assert!(
        raw.contains("No changes to review"),
        "missing the no-changes notice; output was:\n{raw}"
    );
    assert!(
        !raw.contains(ENTER_ALT_SCREEN),
        "should not have entered the alternate screen with nothing to show"
    );
}

/// Tier 2 — the "add any file" picker: Ctrl-P reaches a file git never put in the
/// diff (an untracked file), and selecting it shows the file's real content.
#[test]
fn picker_pulls_in_an_untracked_file() {
    let repo = repo_with_changes();
    // Untracked (never `git add`ed), so it is absent from the diff and the tree
    // and can only be reached through the picker.
    write(
        repo.path(),
        "scratch_pad.txt",
        "PICKED_MARKER_LINE\nsecond line\n",
    );

    let mut session = Session::launch(repo.path());
    assert!(session.wait_for_screen("file 1/2", Duration::from_secs(10)));
    assert!(
        !session.screen().contains("scratch_pad.txt"),
        "untracked file should not be in the initial tree; screen:\n{}",
        session.screen()
    );

    // `Space f` opens the picker; filter to the untracked file, then select it.
    session.feed(b" f");
    session.feed(b"scratch");
    assert!(
        session.wait_for_screen("scratch_pad.txt", Duration::from_secs(10)),
        "picker did not list the untracked file; screen:\n{}",
        session.screen()
    );
    session.feed(b"\r");

    assert!(
        session.wait_for_screen("PICKED_MARKER_LINE", Duration::from_secs(10)),
        "picked file's content never showed; screen:\n{}",
        session.screen()
    );

    session.feed(b"q");
    let status = session.wait(Duration::from_secs(10)).expect("clean exit");
    assert!(status.success(), "expected exit 0, got {status:?}");
}
