// Layer 1 of TESTING.md: the dense correctness layer. We drive the *decoupled*
// `update(state, event)` transition (`App::handle_key`) with real crossterm
// `KeyEvent`s, render `App` into a ratatui `TestBackend`, and snapshot the
// resulting cell grid with `insta` — plus a few explicit style assertions,
// because the buffer carries fg/bg/modifiers and "did it render *green*" is part
// of correctness. No real terminal, no PTY, no async: fully deterministic.
//
// Everything layer 1 deliberately *cannot* see (real binary, raw-mode
// setup/teardown, tty byte decoding, the `git`/`gh` subprocesses) is layer 2's
// job and lives in `tests/e2e.rs`.
//
// The tests are grouped by topic into submodules; the shared fixtures and
// rendering helpers used across them live here and reach the submodules through
// `use super::*`.

// Re-exported (rather than plain `use`) so the submodules can pull the whole
// toolbox in with `use super::*` regardless of which names each one touches.
pub(super) use super::app::{App, FileView, Focus, GapExpansion, Row, ViewPlan};
pub(super) use super::render::{render, MARK};
pub(super) use crate::blob::BlobSide;
pub(super) use crate::diff::{parse_diff, DiffLine, FileDiff, FileStatus, LineKind};
pub(super) use crate::domain::{
    AnchorScope, Annotation, Author, Severity, Side, StateFile, Status, Target, Turn,
};
pub(super) use crate::store;
pub(super) use ratatui::backend::TestBackend;
pub(super) use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
pub(super) use ratatui::style::{Color, Modifier, Style};
pub(super) use ratatui::Terminal;

mod annotations;
mod authoring;
mod expansion;
mod keymap;
mod rendering;
mod turns;

/// A deterministic multi-file diff exercising every status the tree renders
/// (modified, added, deleted, renamed, binary) plus a metadata-only rename
/// and a file long enough to scroll/page through. Byte-stable: no clock, no
/// ids — the viewer neither reads nor writes the store yet.
const FIXTURE: &str = r#"diff --git a/src/alpha.rs b/src/alpha.rs
index 1111111..2222222 100644
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1,4 +1,5 @@ fn alpha() {
 use std::io;
-let x = 1;
+let x = 2;
+let y = 3;
 println!("{x}");
 done
@@ -20,2 +21,2 @@ fn beta() {
 let beta = compute();
-old beta
+new beta
diff --git a/src/long.rs b/src/long.rs
index 3333333..4444444 100644
--- a/src/long.rs
+++ b/src/long.rs
@@ -1,19 +1,20 @@ impl Long {
 line 01
 line 02
 line 03
 line 04
 line 05
 line 06
 line 07
 line 08
-line 09 old
+line 09 new
 line 10
 line 11
 line 12
 line 13
 line 14
 line 15
 line 16
 line 17
 line 18
+line 19 added
 line 20
@@ -40,2 +41,3 @@ fn far() {
 far context
-far old
+far new
+far extra
diff --git a/README.md b/README.md
index 5555555..6666666 100644
--- a/README.md
+++ b/README.md
@@ -1,2 +1,2 @@
-# Old Title
+# New Title
 intro line
diff --git a/src/new_module.rs b/src/new_module.rs
new file mode 100644
index 0000000..7777777
--- /dev/null
+++ b/src/new_module.rs
@@ -0,0 +1,3 @@
+pub fn hello() {}
+pub fn world() {}
+// brand new
diff --git a/old.txt b/old.txt
deleted file mode 100644
index 8888888..0000000
--- a/old.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-gone one
-gone two
diff --git a/assets/logo.png b/assets/logo.png
new file mode 100644
index 0000000..9999999
Binary files /dev/null and b/assets/logo.png differ
diff --git a/src/moved.rs b/src/relocated.rs
similarity index 100%
rename from src/moved.rs
rename to src/relocated.rs
"#;

fn app() -> App {
    App::new(
        parse_diff(FIXTURE),
        Target::Local {
            base: "main".to_string(),
            head_sha: "deadbeefcafe".to_string(),
        },
    )
}

/// Build an annotation with byte-stable fields (fixed clock/id) for snapshots.
fn note(id: &str, author: Author, file: &str, side: Side, line: u32, sev: Severity) -> Annotation {
    Annotation {
        id: id.to_string(),
        author,
        file: file.to_string(),
        line,
        end_line: None,
        side,
        scope: AnchorScope::Line,
        severity: sev,
        tag: None,
        status: Status::Open,
        body: format!("body for {id}"),
        reply_to: None,
        created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
    }
}

/// An app pre-loaded with annotations (no store path; reload is not exercised).
fn annotated_app(annotations: Vec<Annotation>) -> App {
    let mut a = app();
    a.annotations = annotations;
    a
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

fn ctrl(c: char) -> KeyEvent {
    KeyEvent::new(KeyCode::Char(c), KeyModifiers::CONTROL)
}

/// Render `app` into a fresh `TestBackend`, mirroring the real loop: draw
/// once (which records the viewport heights `handle_key` relies on for
/// paging/clamping), then for each key apply the transition and redraw.
fn drive(app: &mut App, w: u16, h: u16, keys: &[KeyEvent]) -> Terminal<TestBackend> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| render(f, app)).unwrap();
    for &k in keys {
        app.handle_key(k);
        term.draw(|f| render(f, app)).unwrap();
    }
    term
}

fn screen(term: &Terminal<TestBackend>) -> String {
    term.backend().to_string()
}

/// Style of the first cell (row-major) whose symbol equals `sym` and whose
/// column is `>= min_x` (use `min_x >= 29` to stay inside the diff pane,
/// past the 28-column tree and its border).
fn style_of(term: &Terminal<TestBackend>, sym: &str, min_x: u16) -> Style {
    let buf = term.backend().buffer();
    for y in 0..buf.area.height {
        for x in min_x..buf.area.width {
            if let Some(c) = buf.cell((x, y)) {
                if c.symbol() == sym {
                    return c.style();
                }
            }
        }
    }
    panic!("no cell with symbol {sym:?} at column >= {min_x}");
}

fn any_cell_has_bg(term: &Terminal<TestBackend>, bg: Color) -> bool {
    let buf = term.backend().buffer();
    (0..buf.area.height).any(|y| {
        (0..buf.area.width).any(|x| buf.cell((x, y)).map(|c| c.style().bg) == Some(Some(bg)))
    })
}

/// Whether any cell in `rows` (inclusive y range) renders `sym` with `fg`.
/// Used to find a gutter mark in the diff *body* while ignoring the same
/// glyph in the pane title (y == 0) and the status bar (bottom row).
fn has_fg_symbol(term: &Terminal<TestBackend>, sym: &str, fg: Color, rows: (u16, u16)) -> bool {
    let buf = term.backend().buffer();
    (rows.0..=rows.1).any(|y| {
        (0..buf.area.width).any(|x| {
            buf.cell((x, y))
                .is_some_and(|c| c.symbol() == sym && c.style().fg == Some(fg))
        })
    })
}
