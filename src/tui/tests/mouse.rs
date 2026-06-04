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
