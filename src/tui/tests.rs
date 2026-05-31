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

use super::app::{App, Focus};
use super::render::{render, MARK};
use crate::diff::parse_diff;
use crate::domain::{Annotation, Author, Severity, Side, StateFile, Status, Target, Turn};
use crate::store;
use ratatui::backend::TestBackend;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use ratatui::Terminal;

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
        side,
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

// ---- snapshots: key -> state -> render ---------------------------------

#[test]
fn initial_view() {
    // Tree focused, first file (alpha.rs) open in the diff pane.
    let term = drive(&mut app(), 100, 24, &[]);
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn tree_selection_moves_to_added_file() {
    // j x3 walks the tree alpha -> long -> README -> new_module (an add).
    let mut a = app();
    let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('j')); 3]);
    assert_eq!(a.selected, 3);
    assert_eq!(a.current().display_path(), "src/new_module.rs");
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn diff_pane_focused_and_scrolled() {
    // Select the long file, focus the diff (l), scroll down three rows.
    let mut a = app();
    let keys = [
        key(KeyCode::Char('j')),
        key(KeyCode::Char('l')),
        key(KeyCode::Char('j')),
        key(KeyCode::Char('j')),
        key(KeyCode::Char('j')),
    ];
    let term = drive(&mut a, 100, 24, &keys);
    assert_eq!(a.focus, Focus::Diff);
    assert_eq!(a.scroll, 3);
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn status_bar_shows_bottom() {
    // G in the diff pane pins to the last row; status reads BOT.
    let mut a = app();
    let keys = [
        key(KeyCode::Char('j')),
        key(KeyCode::Char('l')),
        key(KeyCode::Char('G')),
    ];
    let term = drive(&mut a, 100, 24, &keys);
    assert_eq!(a.scroll, a.max_scroll());
    assert!(screen(&term).contains("BOT"));
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn help_overlay() {
    let term = drive(&mut app(), 100, 24, &[key(KeyCode::Char('?'))]);
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn binary_file_notice() {
    // j x5: alpha -> long -> README -> new_module -> old.txt -> logo.png.
    let mut a = app();
    let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('j')); 5]);
    assert_eq!(a.current().display_path(), "assets/logo.png");
    assert!(screen(&term).contains("Binary file"));
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn metadata_only_rename_notice() {
    // G jumps to the last file: a 100%-similarity rename with no hunks.
    let mut a = app();
    let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('G'))]);
    assert_eq!(a.current().display_path(), "src/relocated.rs");
    assert!(screen(&term).contains("metadata-only"));
    insta::assert_snapshot!(screen(&term));
}

// ---- behavior: the keymap and viewport math ----------------------------

#[test]
fn vim_jk_scrolls_diff_and_clamps_at_top() {
    let mut a = app();
    // Long file, diff focus.
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
    );
    for _ in 0..4 {
        a.handle_key(key(KeyCode::Char('j')));
    }
    assert_eq!(a.scroll, 4);
    for _ in 0..10 {
        a.handle_key(key(KeyCode::Char('k')));
    }
    assert_eq!(a.scroll, 0, "k clamps at the top");
}

#[test]
fn hunk_navigation_jumps_between_hunks() {
    let mut a = app();
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
    );
    // Two hunks: the second starts well past the viewport, so `}` lands at
    // the clamped bottom; `{` returns to the first hunk at the top.
    a.handle_key(key(KeyCode::Char('}')));
    assert_eq!(a.scroll, a.max_scroll());
    a.handle_key(key(KeyCode::Char('{')));
    assert_eq!(a.scroll, 0);
    // n / N are aliases.
    a.handle_key(key(KeyCode::Char('n')));
    assert_eq!(a.scroll, a.max_scroll());
    a.handle_key(key(KeyCode::Char('N')));
    assert_eq!(a.scroll, 0);
}

#[test]
fn g_and_capital_g_jump_top_and_bottom() {
    let mut a = app();
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
    );
    a.handle_key(key(KeyCode::Char('G')));
    assert_eq!(a.scroll, a.max_scroll());
    a.handle_key(key(KeyCode::Char('g')));
    assert_eq!(a.scroll, 0);
}

#[test]
fn ctrl_d_and_u_half_page() {
    let mut a = app();
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
    );
    let half = a.diff_height / 2;
    a.handle_key(ctrl('d'));
    assert_eq!(a.scroll, half.min(a.max_scroll()));
    a.handle_key(ctrl('u'));
    assert_eq!(a.scroll, 0);
}

#[test]
fn tab_toggles_focus() {
    let mut a = app();
    assert_eq!(a.focus, Focus::Tree);
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.focus, Focus::Diff);
    a.handle_key(key(KeyCode::Tab));
    assert_eq!(a.focus, Focus::Tree);
}

#[test]
fn capital_j_k_switch_files_from_the_diff_pane() {
    let mut a = app();
    a.handle_key(key(KeyCode::Char('l'))); // focus diff
    assert_eq!(a.selected, 0);
    a.handle_key(key(KeyCode::Char('J')));
    assert_eq!(a.selected, 1);
    assert_eq!(a.focus, Focus::Diff, "stays in the diff pane");
    a.handle_key(key(KeyCode::Char('K')));
    assert_eq!(a.selected, 0);
}

#[test]
fn quit_keys_signal_exit() {
    assert!(app().handle_key(key(KeyCode::Char('q'))), "q quits");
    assert!(app().handle_key(ctrl('c')), "Ctrl-c quits");
    assert!(
        !app().handle_key(key(KeyCode::Char('j'))),
        "ordinary keys do not"
    );
}

#[test]
fn help_overlay_swallows_navigation() {
    let mut a = app();
    a.handle_key(key(KeyCode::Char('l'))); // focus diff
    a.handle_key(key(KeyCode::Char('?'))); // open help
    assert!(a.show_help);
    a.handle_key(key(KeyCode::Char('j'))); // swallowed
    assert_eq!(a.scroll, 0, "help eats navigation keys");
    a.handle_key(key(KeyCode::Esc)); // Esc closes it
    assert!(!a.show_help);
}

// ---- styles: the buffer carries colour + modifiers ---------------------

#[test]
fn diff_rows_are_styled_by_kind() {
    // alpha.rs is open: a cyan-bold hunk header, green additions, red
    // deletions. Restrict to the diff pane (x >= 29) so the tree's own
    // +N/-N counts don't satisfy the marker lookups.
    let term = drive(&mut app(), 100, 24, &[]);

    let hunk = style_of(&term, "@", 29);
    assert_eq!(hunk.fg, Some(Color::Cyan));
    assert!(hunk.add_modifier.contains(Modifier::BOLD));

    // The +/- *markers* sit in a fixed gutter column (~41), well past the
    // +N,M / -N,M ranges inside the cyan hunk header, so min_x = 40 picks
    // the addition/deletion lines rather than the header digits.
    assert_eq!(style_of(&term, "+", 40).fg, Some(Color::Green));
    assert_eq!(style_of(&term, "-", 40).fg, Some(Color::Red));
}

#[test]
fn diff_content_is_syntax_highlighted() {
    // alpha.rs (Rust) is open at launch. The diff body otherwise only ever
    // uses *named* colours (cyan header, green/red markers, dark-gray line
    // numbers), so a truecolor `Rgb` foreground anywhere in the body can
    // only have come from syntect — proof the highlighter is wired through
    // rendering, not just unit-tested in isolation.
    let term = drive(&mut app(), 100, 24, &[]);
    let buf = term.backend().buffer();
    // Body rows only: 0 is the pane title, the last row is the status bar.
    let highlighted = (1..23).any(|y| {
        (29..buf.area.width).any(|x| {
            buf.cell((x, y))
                .is_some_and(|c| matches!(c.style().fg, Some(Color::Rgb(..))))
        })
    });
    assert!(highlighted, "expected syntect truecolor in the diff body");
}

#[test]
fn selection_and_status_bar_have_background_fills() {
    let term = drive(&mut app(), 100, 24, &[]);
    assert!(
        any_cell_has_bg(&term, Color::Rgb(40, 44, 52)),
        "selected tree row is highlighted"
    );
    assert!(
        any_cell_has_bg(&term, Color::Rgb(30, 33, 40)),
        "status bar has its background fill"
    );
}

// ---- annotations: gutter marks, panel, and the lookup map --------------

/// Two annotations on alpha.rs (the file open at launch): a blocker on the
/// `let x = 2;` addition (RIGHT line 2) and an info on a context line.
fn alpha_notes() -> Vec<Annotation> {
    vec![
        note(
            "blk00001",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Blocker,
        ),
        note(
            "inf00002",
            Author::Human,
            "src/alpha.rs",
            Side::Right,
            4,
            Severity::Info,
        ),
    ]
}

#[test]
fn gutter_marks_render_on_annotated_lines() {
    let term = drive(&mut annotated_app(alpha_notes()), 100, 24, &[]);
    // A severity-coloured mark appears in the diff body (rows 1..22, between
    // the pane title and the status bar). The blocker is red; the info, blue.
    assert!(has_fg_symbol(&term, MARK, Color::Red, (1, 22)));
    assert!(has_fg_symbol(&term, MARK, Color::Blue, (1, 22)));
    insta::assert_snapshot!(screen(&term));
}

#[test]
fn unannotated_file_has_no_gutter_column() {
    // README.md (file index 2) carries no annotations, so its diff body has
    // no reserved mark column — even while other files do.
    let mut a = annotated_app(alpha_notes());
    let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('j')); 2]);
    assert_eq!(a.current().display_path(), "README.md");
    assert!(a.line_marks().is_empty());
    // No mark in the diff body; the status bar's total-count glyph (bottom
    // row) is expected and excluded by the row range.
    assert!(!has_fg_symbol(&term, MARK, Color::Red, (1, 22)));
    assert!(!has_fg_symbol(&term, MARK, Color::Blue, (1, 22)));
}

#[test]
fn annotations_panel_lists_current_file() {
    // `a` toggles the panel; it lists alpha.rs's two annotations.
    let mut a = annotated_app(alpha_notes());
    let term = drive(&mut a, 100, 24, &[key(KeyCode::Char('a'))]);
    assert!(a.show_panel);
    let text = screen(&term);
    assert!(text.contains("Annotations"));
    assert!(text.contains("L2 agent"));
    assert!(text.contains("L4 human"));
    insta::assert_snapshot!(text);
}

#[test]
fn status_bar_shows_annotation_count() {
    let term = drive(&mut annotated_app(alpha_notes()), 100, 24, &[]);
    // Two annotations, both open.
    assert!(screen(&term).contains("2 (2 open)"));
}

#[test]
fn line_marks_keep_the_most_severe_per_line() {
    // A warning and a blocker collide on the same (side, line): blocker wins.
    let a = annotated_app(vec![
        note(
            "w0000001",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Warning,
        ),
        note(
            "b0000002",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Blocker,
        ),
    ]);
    let marks = a.line_marks();
    assert_eq!(marks.get(&(Side::Right, 2)), Some(&Severity::Blocker));
}

#[test]
fn current_file_annotations_filter_by_path() {
    let a = annotated_app(vec![
        note(
            "a0000001",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Info,
        ),
        note(
            "o0000002",
            Author::Agent,
            "README.md",
            Side::Right,
            1,
            Severity::Info,
        ),
    ]);
    // alpha.rs is selected at launch.
    let here = a.current_file_annotations();
    assert_eq!(here.len(), 1);
    assert_eq!(here[0].id, "a0000001");
}

// ---- turn protocol: the human's release (PLAN.md §6) -------------------

#[test]
fn r_releases_the_turn_back_to_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.json");
    let target = Target::Local {
        base: "main".to_string(),
        head_sha: "abc".to_string(),
    };

    // Seed a store where an agent is blocked waiting on the human at seq 3.
    let mut seed = StateFile::new(target.clone());
    seed.turn.agent_waiting = true;
    seed.turn.owner = Author::Human;
    seed.turn.seq = 3;
    store::save(&path, &seed).unwrap();

    let mut a = App::new(parse_diff(FIXTURE), target);
    a.attach_store(path.clone(), store::load(&path).unwrap());
    assert!(a.turn.agent_waiting, "attach loads the turn block");

    a.handle_key(key(KeyCode::Char('r')));

    // In memory: ownership handed back, waiting cleared.
    assert_eq!(a.turn.owner, Author::Agent);
    assert!(!a.turn.agent_waiting);
    // On disk: seq bumped past what the waiter recorded, and first-contact
    // approval is now set — this write is what unblocks `agent wait`.
    let saved = store::load(&path).unwrap().unwrap();
    assert_eq!(saved.turn.seq, 4);
    assert_eq!(saved.turn.owner, Author::Agent);
    assert!(!saved.turn.agent_waiting);
    assert!(saved.turn.approved);
}

#[test]
fn r_without_a_store_is_a_harmless_noop() {
    // No store attached (resolution failed / store-less view): `r` must not
    // panic and leaves the default turn untouched.
    let mut a = app();
    a.handle_key(key(KeyCode::Char('r')));
    assert_eq!(a.turn, Turn::default());
}

#[test]
fn status_bar_surfaces_a_waiting_agent() {
    let mut a = app();
    a.turn.agent_waiting = true;
    let term = drive(&mut a, 100, 24, &[]);
    assert!(
        screen(&term).contains("agent waiting"),
        "status bar should advertise the waiting agent:\n{}",
        screen(&term)
    );
}

#[test]
fn awaiting_approval_only_while_unapproved_and_waiting() {
    let mut a = app();
    assert!(!a.awaiting_approval(), "idle session: nothing to approve");
    a.turn.agent_waiting = true;
    assert!(
        a.awaiting_approval(),
        "first contact: waiting, not approved"
    );
    a.turn.approved = true;
    assert!(
        !a.awaiting_approval(),
        "once approved, a still-waiting agent is no longer first contact"
    );
}

#[test]
fn first_contact_shows_approval_banner_and_approve_hint() {
    // Unapproved agent blocked in `agent wait`: the top banner asks for
    // approval and the status hint reads "approve", not "release".
    let mut a = app();
    a.turn.agent_waiting = true;
    let term = drive(&mut a, 100, 24, &[]);
    let text = screen(&term);
    assert!(
        text.contains("wants to collaborate"),
        "approval banner should appear on first contact:\n{text}"
    );
    assert!(
        text.contains("r approve"),
        "hint should offer approval:\n{text}"
    );
    assert!(
        !text.contains("r release"),
        "release is for approved sessions"
    );
}

#[test]
fn approved_waiting_agent_offers_release_without_the_banner() {
    // An established (approved) session with the agent waiting: no approval
    // banner, and the hint goes back to "release".
    let mut a = app();
    a.turn.agent_waiting = true;
    a.turn.approved = true;
    let term = drive(&mut a, 100, 24, &[]);
    let text = screen(&term);
    assert!(
        !text.contains("wants to collaborate"),
        "no approval banner once approved:\n{text}"
    );
    assert!(
        text.contains("r release"),
        "hint should offer release:\n{text}"
    );
}

// ---- live reload: the data path a notify tick drives (PLAN.md §9) -------

#[test]
fn reload_picks_up_another_processs_writes() {
    // The notify watch only decides *when* to reload; `reload` does the work
    // of re-reading the store, so drive it directly to prove a TUI started on
    // an empty store sees an agent's later annotation and turn change.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.json");
    let target = Target::Local {
        base: "main".to_string(),
        head_sha: "abc".to_string(),
    };

    let mut a = App::new(parse_diff(FIXTURE), target.clone());
    a.attach_store(path.clone(), store::load(&path).unwrap());
    assert!(a.annotations.is_empty(), "starts with an empty store");

    // Another process (the agent) writes a comment and takes the turn.
    store::update(&path, &target, |s| {
        s.annotations.push(note(
            "agent001",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Warning,
        ));
        s.turn.agent_waiting = true;
    })
    .unwrap();

    a.reload();
    assert_eq!(a.annotations.len(), 1, "reload picks up the new annotation");
    assert_eq!(a.annotations[0].id, "agent001");
    assert!(a.turn.agent_waiting, "and the refreshed turn state");
}

#[test]
fn reload_without_a_store_is_a_harmless_noop() {
    // Store-less view (resolution failed): reload must not panic and leaves
    // the empty defaults in place.
    let mut a = app();
    a.reload();
    assert!(a.annotations.is_empty());
    assert_eq!(a.turn, Turn::default());
}
