//! Capture half of Layer 2's **exact-match pixel oracle** (TESTING.md). This
//! test drives the real binary to a settled screen for each scenario and writes
//! a **truecolor SVG** of it (built from the vt100 grid); `e2e/scripts/run.sh`
//! then rasterizes those SVGs with `resvg` to lossless PNGs and pixel-diffs them
//! against committed baselines (zero tolerance). See `e2e/README.md` for the
//! full loop and the willet-cloud design it mirrors.
//!
//! The split is deliberate: capture (deterministic Rust) lives here;
//! rasterization + comparison (needs `resvg`/ImageMagick) lives in the e2e
//! scripts. So this test only runs when asked to emit SVGs — gated on
//! `MUDPUPPY_SVG_DIR`. Normal `cargo test` skips it, and it never shells out to
//! a renderer that isn't there.

mod common;

use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::Duration;

use common::{repo_with_changes, Session};

/// An agent comment to seed into the shared store before the TUI launches, so a
/// scenario can render annotations that actually exist. Anchored to a line of
/// `a_app.rs` (file 1), where the panel and gutter marks will surface it.
struct Seed {
    line: &'static str,
    body: &'static str,
}

/// A named still: optionally seed annotations, drive these keystrokes, wait
/// until `settle_marker` is on the screen, then record. Keep this list tiny
/// (TESTING.md: "resist growing this"); edge cases belong in the Layer-1 `insta`
/// suite, not in slow pixel renders.
struct Scenario {
    name: &'static str,
    /// Agent comments to seed via a shared store; when non-empty the scenario
    /// gets its own `MUDPUPPY_DATA_DIR` so the TUI opens onto them.
    seed: &'static [Seed],
    /// When set, serve a mock release manifest carrying this Markdown changelog
    /// over a loopback HTTP server and point the binary's manifest URL at it, so
    /// `:check-updates` opens the self-update prompt deterministically (no real
    /// network). Mutually exclusive with `seed` in practice.
    update_changelog: Option<&'static str>,
    keys: &'static [u8],
    settle_marker: &'static str,
}

/// The changelog rendered in the self-update prompt scenario. Exercises every
/// Markdown feature the prompt styles (see `src/tui/markdown.rs`): both heading
/// levels, bullet list, `**bold**`/`*italic*`/`` `code` `` inline spans, a
/// blockquote, and a `[text](url)` link.
const UPDATE_CHANGELOG: &str = "\
# mudpuppy v9.9.9

## Highlights

- **Markdown changelogs**: this prompt now styles release notes — \
headings, lists, and inline `code` all render.
- Fixed a crash when opening an *empty* diff.

## Notes

> Run `mudpuppy update` to upgrade, or pick Install below.

See the [full release notes](https://example.com/releases/v9.9.9).";

const SCENARIOS: &[Scenario] = &[
    // Cold launch: the first paint of a local diff.
    Scenario {
        name: "cold_launch",
        seed: &[],
        update_changelog: None,
        keys: b"",
        settle_marker: "file 1/2",
    },
    // Diff pane focused and scrolled to the bottom of the long file.
    Scenario {
        name: "scrolled_bottom",
        seed: &[],
        update_changelog: None,
        keys: b"lG",
        settle_marker: "BOT",
    },
    // The help overlay drawn over the viewer.
    Scenario {
        name: "help_overlay",
        seed: &[],
        update_changelog: None,
        keys: b"?",
        settle_marker: "mudpuppy — default keymap",
    },
    // The comment composer open on a diff line with a body typed — exercises
    // the modal that lets a user author an annotation. `l` focuses the diff,
    // `j` moves the cursor off the hunk header onto a content line, `Space c c`
    // opens the composer, and the remaining chars become the body.
    Scenario {
        name: "annotation_composer",
        seed: &[],
        update_changelog: None,
        keys: b"lj cclooks reasonable",
        settle_marker: "-- INSERT --",
    },
    // The `:` command palette with a partial query, showing the fuzzy-matched
    // subset of command names.
    Scenario {
        name: "command_palette",
        seed: &[],
        update_changelog: None,
        keys: b":comment",
        settle_marker: "comment-file",
    },
    // The "add any file" picker with a partial query narrowing to one match.
    Scenario {
        name: "file_picker",
        seed: &[],
        update_changelog: None,
        keys: b" fnot",
        settle_marker: "Add file",
    },
    // The annotations tab open over a store that has comments, listing them all
    // grouped by file, with their gutter marks still showing in the diff.
    Scenario {
        name: "annotations_tab",
        seed: &[
            Seed {
                line: "3",
                body: "this loop reruns the query on every keypress",
            },
            Seed {
                line: "12",
                body: "extract this into a helper",
            },
        ],
        update_changelog: None,
        keys: b" a",
        settle_marker: "Annotations",
    },
    // The self-update prompt overlay showing a Markdown changelog: a mock
    // manifest server announces a far-future version, `:check-updates` fetches it
    // and opens the prompt, whose details body styles the changelog.
    Scenario {
        name: "update_prompt_changelog",
        seed: &[],
        update_changelog: Some(UPDATE_CHANGELOG),
        keys: b":check-updates\r",
        settle_marker: "Update now?",
    },
];

/// Serve a canned `dist-manifest.json` — announcing `v9.9.9` (far newer than the
/// running build, so the update check reports it) with `changelog` as the release
/// notes — over a loopback HTTP server. Returns the URL for
/// `MUDPUPPY_UPDATE_MANIFEST_URL`. The listener runs on a detached thread for the
/// rest of the (short-lived) test process; `:check-updates` makes one request.
fn start_mock_manifest_server(changelog: &str) -> String {
    let manifest = serde_json::json!({
        "announcement_tag": "v9.9.9",
        "announcement_changelog": changelog,
    })
    .to_string();
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock manifest server");
    let addr = listener.local_addr().expect("mock server addr");
    thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            // Drain the request so the client's write half completes before we
            // reply; the body is irrelevant (we always serve the same manifest).
            let mut buf = [0u8; 1024];
            let _ = stream.read(&mut buf);
            let response = format!(
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\
                 Content-Length: {}\r\nConnection: close\r\n\r\n{}",
                manifest.len(),
                manifest,
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}/dist-manifest.json")
}

/// Seed one agent comment into the store under `data` by shelling out to the
/// real `agent comment add` surface — the same path the AI agent uses.
fn seed_comment(repo: &Path, data: &Path, seed: &Seed) {
    let status = Command::new(env!("CARGO_BIN_EXE_mudpuppy"))
        .args([
            "agent", "comment", "add", "--file", "a_app.rs", "--line", seed.line, "--body",
            seed.body,
        ])
        .current_dir(repo)
        .env("MUDPUPPY_DATA_DIR", data)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .status()
        .expect("run agent comment add");
    assert!(
        status.success(),
        "seeding comment on line {} failed",
        seed.line
    );
}

#[test]
fn emit_scenario_svgs() {
    let Some(dir) = std::env::var_os("MUDPUPPY_SVG_DIR") else {
        eprintln!(
            "MUDPUPPY_SVG_DIR unset — skipping SVG emission (run via ./scripts/test-snapshots.sh)"
        );
        return;
    };
    let dir = PathBuf::from(dir);
    std::fs::create_dir_all(&dir).expect("create svg dir");

    for scenario in SCENARIOS {
        let repo = repo_with_changes();

        // Scenarios with seeds share a store with a headless `agent` that writes
        // the comments first; the temp dir must outlive the session.
        let data = (!scenario.seed.is_empty()).then(|| tempfile::tempdir().unwrap());
        let mut session = if let Some(changelog) = scenario.update_changelog {
            let url = start_mock_manifest_server(changelog);
            Session::launch_with_str_env(repo.path(), &[("MUDPUPPY_UPDATE_MANIFEST_URL", &url)])
        } else if let Some(data) = &data {
            for seed in scenario.seed {
                seed_comment(repo.path(), data.path(), seed);
            }
            Session::launch_with_env(repo.path(), &[("MUDPUPPY_DATA_DIR", data.path())])
        } else {
            Session::launch(repo.path())
        };

        // Always wait for the base UI before applying scenario keys. A capture
        // is still worth recording even if it never settles — failing to settle
        // shouldn't lose the still (the pixel diff will catch a wrong screen).
        if !session.wait_for_screen("file 1/2", Duration::from_secs(10)) {
            eprintln!(
                "[{}] base UI never appeared; capturing anyway",
                scenario.name
            );
        }
        if !scenario.keys.is_empty() {
            session.feed(scenario.keys);
        }
        if !session.wait_for_screen(scenario.settle_marker, Duration::from_secs(10)) {
            eprintln!(
                "[{}] never settled on {:?}; capturing anyway. screen was:\n{}",
                scenario.name,
                scenario.settle_marker,
                session.screen()
            );
        }
        // The marker can land a paint or two before keys typed after it (e.g.
        // the composer body) have finished rendering, which truncates the
        // capture. Syntax highlighting is also asynchronous now — the worker
        // fills colour a frame or two after the plain structure renders — so we
        // settle on the *SVG* (which encodes RGB), not just the character grid,
        // to wait for the colours to land before recording.
        if !session.wait_for_stable_svg(3, Duration::from_secs(10)) {
            eprintln!(
                "[{}] screen never quiesced after {:?}; capturing anyway. screen was:\n{}",
                scenario.name,
                scenario.settle_marker,
                session.screen()
            );
        }

        // Record the settled screen as a truecolor SVG of the vt100 grid.
        let svg = session.screen_svg();
        session.kill();

        let path = dir.join(format!("{}.svg", scenario.name));
        std::fs::write(&path, svg).expect("write svg");
        eprintln!("wrote {}", path.display());
    }
}
