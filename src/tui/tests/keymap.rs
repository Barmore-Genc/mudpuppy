// The keymap and viewport math: cursor moves, hunk hops, paging, focus
// toggles, and the quit/help keys — asserted against `App` state, no snapshots.

use super::*;

#[test]
fn vim_jk_moves_cursor_and_clamps_at_top() {
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
    assert_eq!(a.cursor, 4);
    for _ in 0..10 {
        a.handle_key(key(KeyCode::Char('k')));
    }
    assert_eq!(a.cursor, 0, "k clamps the cursor at the top");
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
    // Two hunks: `}` moves the cursor to the second hunk header, `{` back to
    // the first (at row 0).
    let second = a.view.hunk_starts[1];
    a.handle_key(key(KeyCode::Char('}')));
    assert_eq!(a.cursor, second);
    a.handle_key(key(KeyCode::Char('{')));
    assert_eq!(a.cursor, 0);
    // n / N are aliases.
    a.handle_key(key(KeyCode::Char('n')));
    assert_eq!(a.cursor, second);
    a.handle_key(key(KeyCode::Char('N')));
    assert_eq!(a.cursor, 0);
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
