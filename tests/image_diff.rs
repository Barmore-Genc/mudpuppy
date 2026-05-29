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

use std::path::PathBuf;
use std::time::Duration;

use common::{repo_with_changes, Session};

/// A named still: drive these keystrokes, wait until `settle_marker` is on the
/// screen, then record. Keep this list tiny (TESTING.md: "resist growing this");
/// edge cases belong in the Layer-1 `insta` suite, not in slow pixel renders.
struct Scenario {
    name: &'static str,
    keys: &'static [u8],
    settle_marker: &'static str,
}

const SCENARIOS: &[Scenario] = &[
    // Cold launch: the first paint of a local diff.
    Scenario {
        name: "cold_launch",
        keys: b"",
        settle_marker: "file 1/2",
    },
    // Diff pane focused and scrolled to the bottom of the long file.
    Scenario {
        name: "scrolled_bottom",
        keys: b"lG",
        settle_marker: "BOT",
    },
    // The help overlay drawn over the viewer.
    Scenario {
        name: "help_overlay",
        keys: b"?",
        settle_marker: "toggle this help",
    },
];

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
        let mut session = Session::launch(repo.path());

        // Always wait for the base UI before applying scenario keys.
        assert!(
            session.wait_for_screen("file 1/2", Duration::from_secs(10)),
            "[{}] base UI never appeared",
            scenario.name
        );
        if !scenario.keys.is_empty() {
            session.feed(scenario.keys);
        }
        assert!(
            session.wait_for_screen(scenario.settle_marker, Duration::from_secs(10)),
            "[{}] never settled on {:?}; screen was:\n{}",
            scenario.name,
            scenario.settle_marker,
            session.screen()
        );

        // Record the settled screen as a truecolor SVG of the vt100 grid.
        let svg = session.screen_svg();
        session.kill();

        let path = dir.join(format!("{}.svg", scenario.name));
        std::fs::write(&path, svg).expect("write svg");
        eprintln!("wrote {}", path.display());
    }
}
