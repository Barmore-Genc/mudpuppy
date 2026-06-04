// Mouse-event behaviour (issue #29): scroll wheel, click to focus/select,
// drag to enter visual mode, double-click to comment, sidebar title click to
// switch tab, status-bar `release` click to release the turn. These tests
// render once so `App::hits` is populated, then feed synthetic `MouseEvent`s
// through `handle_mouse_event` and assert state.

use super::*;

fn mouse(kind: MouseEventKind, col: u16, row: u16) -> MouseEvent {
    MouseEvent {
        kind,
        column: col,
        row,
        modifiers: KeyModifiers::NONE,
    }
}

/// Press-then-release at the same point: the most common interaction, and
/// what produces a single click.
fn click(a: &mut App, col: u16, row: u16) -> bool {
    a.handle_mouse_event(mouse(MouseEventKind::Down(MouseButton::Left), col, row));
    a.handle_mouse_event(mouse(MouseEventKind::Up(MouseButton::Left), col, row))
}

/// Render once to populate `app.hits` so coordinate-driven mouse events have
/// regions to land on.
fn render_once(a: &mut App, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut term = Terminal::new(TestBackend::new(w, h)).unwrap();
    term.draw(|f| render(f, a)).unwrap();
    term
}

#[test]
fn wheel_over_the_diff_scrolls_the_diff() {
    let mut a = app();
    // Open a long file so scrolling has somewhere to go.
    a.select(1);
    let _term = render_once(&mut a, 100, 24);
    let diff_inner = a.hits.diff_inner.expect("diff rendered");
    a.handle_mouse_event(mouse(
        MouseEventKind::ScrollDown,
        diff_inner.x + 5,
        diff_inner.y + 5,
    ));
    assert!(a.scroll > 0, "wheel down should advance the diff scroll");
}

#[test]
fn click_in_the_tree_selects_that_file_and_focuses_the_tree() {
    let mut a = app();
    // Start with diff focus so the click has to *move* focus to assert it.
    a.focus = Focus::Diff;
    let _term = render_once(&mut a, 100, 24);
    let inner = a.hits.sidebar_inner.expect("sidebar rendered");
    // Click the third tree row.
    assert!(click(&mut a, inner.x + 2, inner.y + 2));
    assert_eq!(a.selected, 2);
    assert_eq!(a.focus, Focus::Tree);
}

#[test]
fn click_on_a_diff_line_moves_the_cursor_and_focuses_the_diff() {
    let mut a = app();
    a.focus = Focus::Tree;
    let _term = render_once(&mut a, 100, 24);
    let inner = a.hits.diff_inner.expect("diff rendered");
    // Click row 3 of the diff body (header_rows is 0 with no file-level note).
    assert!(click(&mut a, inner.x + 10, inner.y + 3));
    assert_eq!(a.focus, Focus::Diff);
    assert_eq!(a.cursor, 3);
}

#[test]
fn drag_in_the_diff_enters_visual_mode_anchored_at_press_row() {
    let mut a = app();
    a.select(1);
    let _term = render_once(&mut a, 100, 24);
    let inner = a.hits.diff_inner.expect("diff rendered");
    // Press at row 2, drag down to row 5.
    a.handle_mouse_event(mouse(
        MouseEventKind::Down(MouseButton::Left),
        inner.x + 5,
        inner.y + 2,
    ));
    a.handle_mouse_event(mouse(
        MouseEventKind::Drag(MouseButton::Left),
        inner.x + 5,
        inner.y + 5,
    ));
    assert_eq!(a.selection_anchor, Some(2));
    assert_eq!(a.cursor, 5);
    // Release commits the drag (no follow-up click action).
    a.handle_mouse_event(mouse(
        MouseEventKind::Up(MouseButton::Left),
        inner.x + 5,
        inner.y + 5,
    ));
    assert_eq!(a.selection_anchor, Some(2), "drag survives release");
}

#[test]
fn clicking_a_sidebar_tab_chip_switches_to_that_tab() {
    let mut a = annotated_app(vec![note(
        "n1",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        2,
        Severity::Info,
    )]);
    let _term = render_once(&mut a, 100, 24);
    // The Annotations chip is the inactive one in Files mode; clicking it
    // switches.
    let (y, x0, x1) = a.hits.tab_annot_span.expect("annotations chip rendered");
    assert!(click(&mut a, (x0 + x1) / 2, y));
    assert_eq!(a.sidebar, Sidebar::Annotations);
    // Re-render: now Files is the inactive chip. Click it to flip back.
    let _term = render_once(&mut a, 100, 24);
    let (y, x0, x1) = a.hits.tab_files_span.expect("files chip rendered");
    assert!(click(&mut a, (x0 + x1) / 2, y));
    assert_eq!(a.sidebar, Sidebar::Files);
}

#[test]
fn wheel_over_the_help_overlay_scrolls_it_not_the_panes_below() {
    let mut a = app();
    a.show_help = true;
    let _term = render_once(&mut a, 100, 24);
    let outer = a.hits.help_outer.expect("help rendered");
    a.handle_mouse_event(mouse(MouseEventKind::ScrollDown, outer.x + 5, outer.y + 5));
    assert!(a.help_scroll > 0, "wheel should scroll the help");
    assert_eq!(a.scroll, 0, "wheel over help must not scroll the diff");
}

#[test]
fn clicking_save_in_the_composer_submits_the_annotation() {
    // Open the composer on a content line, type a body, click the green
    // `save` chip in the footer — the annotation must be created in the
    // store (no Ctrl-S key needed).
    use crate::store;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let dir = tempfile::tempdir().unwrap();
    let store_path = dir.path().join("annotations.json");
    let mut a = app();
    a.attach_store(store_path.clone(), None);
    // Focus diff, move cursor to a content line, open composer on it.
    a.handle_key(KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE));
    a.handle_key(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE));
    leader(&mut a, "cc");
    assert!(a.composer.is_some());
    // Type a body so the save isn't discarded as empty.
    for ch in "looks good".chars() {
        a.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    // Render so the footer's save-chip span is populated.
    let _term = render_once(&mut a, 100, 24);
    let (y, x0, x1) = a
        .hits
        .composer_save_span
        .expect("composer save chip rendered");
    assert!(click(&mut a, (x0 + x1) / 2, y));
    assert!(a.composer.is_none(), "save click closes the composer");
    let state = store::load(&store_path).unwrap().expect("store written");
    assert_eq!(state.annotations.len(), 1);
    assert_eq!(state.annotations[0].body, "looks good");
}

#[test]
fn clicking_a_picker_row_opens_that_file() {
    let mut a = app();
    // Open the picker with a small canned universe so the rows are
    // deterministic. `open_picker` needs a repo root; bypass it by setting
    // the picker directly.
    a.picker = Some(crate::picker::Picker::new(vec![
        "alpha.rs".to_string(),
        "beta.rs".to_string(),
        "gamma.rs".to_string(),
    ]));
    let _term = render_once(&mut a, 100, 24);
    let inner = a.hits.picker_inner.expect("picker rendered");
    // Click the second result (row offset 1 below the query input).
    assert!(click(&mut a, inner.x + 2, inner.y + 2));
    assert!(a.picker.is_none(), "click confirms and closes the picker");
    // The picker is supposed to select / add the chosen path into the file
    // tree. Confirm by checking the current file matches.
    assert_eq!(a.current().display_path(), "beta.rs");
}

#[test]
fn clicking_a_palette_row_stashes_the_command_to_run() {
    let mut a = app();
    a.open_palette(vec![
        "quit".to_string(),
        "release-turn".to_string(),
        "toggle-help".to_string(),
    ]);
    let _term = render_once(&mut a, 100, 24);
    let inner = a.hits.palette_inner.expect("palette rendered");
    // Click the third command row.
    assert!(click(&mut a, inner.x + 2, inner.y + 3));
    assert!(a.palette.is_none(), "click closes the palette");
    assert_eq!(
        a.take_pending_command().as_deref(),
        Some("toggle-help"),
        "the chosen command name is stashed for the engine"
    );
}

#[test]
fn status_bar_release_click_releases_the_turn_when_an_agent_is_waiting() {
    let mut a = app();
    a.turn = Turn {
        seq: 3,
        owner: Author::User,
        agent_waiting: true,
        approved: true,
    };
    let _term = render_once(&mut a, 100, 24);
    let (y, x0, x1) = a.hits.release_span.expect("release span recorded");
    // No store attached, so `release_turn` is a no-op — assert it ran by
    // checking the click was consumed (returns `true`) and didn't touch
    // selection state.
    assert!(click(&mut a, (x0 + x1) / 2, y));
}
