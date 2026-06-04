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

use std::path::{Path, PathBuf};
use std::process::Command;
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
    keys: &'static [u8],
    settle_marker: &'static str,
}

const SCENARIOS: &[Scenario] = &[
    // Cold launch: the first paint of a local diff.
    Scenario {
        name: "cold_launch",
        seed: &[],
        keys: b"",
        settle_marker: "file 1/2",
    },
    // Diff pane focused and scrolled to the bottom of the long file.
    Scenario {
        name: "scrolled_bottom",
        seed: &[],
        keys: b"lG",
        settle_marker: "BOT",
    },
    // The help overlay drawn over the viewer.
    Scenario {
        name: "help_overlay",
        seed: &[],
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
        keys: b"lj cclooks reasonable",
        settle_marker: "-- INSERT --",
    },
    // The `:` command palette with a partial query, showing the fuzzy-matched
    // subset of command names.
    Scenario {
        name: "command_palette",
        seed: &[],
        keys: b":comment",
        settle_marker: "comment-file",
    },
    // The "add any file" picker with a partial query narrowing to one match.
    Scenario {
        name: "file_picker",
        seed: &[],
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
        keys: b" a",
        settle_marker: "Annotations",
    },
];

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
        let mut session = if let Some(data) = &data {
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

        // Record the settled screen as a truecolor SVG of the vt100 grid.
        let svg = session.screen_svg();
        session.kill();

        let path = dir.join(format!("{}.svg", scenario.name));
        std::fs::write(&path, svg).expect("write svg");
        eprintln!("wrote {}", path.display());
    }
}
