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
    // `]h` / `[h` are aliases (the bracket family).
    a.handle_key(key(KeyCode::Char(']')));
    a.handle_key(key(KeyCode::Char('h')));
    assert_eq!(a.cursor, second);
    a.handle_key(key(KeyCode::Char('[')));
    a.handle_key(key(KeyCode::Char('h')));
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
    // `g g` (a two-key sequence) jumps to the top.
    a.handle_key(key(KeyCode::Char('g')));
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
fn count_prefix_scales_a_motion() {
    let mut a = app();
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
    );
    // `5j` moves the cursor five rows in one shot, then clears the count.
    a.handle_key(key(KeyCode::Char('5')));
    assert_eq!(a.pending_count, Some(5), "the digit accumulates a count");
    a.handle_key(key(KeyCode::Char('j')));
    assert_eq!(a.cursor, 5);
    assert_eq!(a.pending_count, None, "the count clears once consumed");
    // A bare motion after that moves by one again.
    a.handle_key(key(KeyCode::Char('j')));
    assert_eq!(a.cursor, 6);
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
fn h_l_pan_the_code_horizontally_and_clamp() {
    // A line far wider than the pane, so horizontal scroll has somewhere to go.
    let long_line = "a".repeat(200);
    let diff = format!(
        "diff --git a/wide.rs b/wide.rs\n\
         index 1111111..2222222 100644\n\
         --- a/wide.rs\n\
         +++ b/wide.rs\n\
         @@ -1 +1 @@\n\
         -short\n\
         +{long_line}\n"
    );
    let mut a = App::new(
        parse_diff(&diff),
        Target::Local {
            base: "main".to_string(),
            head_sha: "deadbeefcafe".to_string(),
        },
    );
    // Render at a known width (sets `diff_width`) and open the file into the diff.
    drive(&mut a, 100, 24, &[key(KeyCode::Char('l'))]);
    assert_eq!(a.focus, Focus::Diff);
    assert_eq!(a.h_scroll, 0, "starts unscrolled");
    let max = a.max_h_scroll();
    assert!(max > 0, "the 200-col line overflows the 100-col pane");

    a.handle_key(key(KeyCode::Char('l')));
    assert!(a.h_scroll > 0, "l pans the code right");

    for _ in 0..50 {
        a.handle_key(key(KeyCode::Char('l')));
    }
    assert_eq!(a.h_scroll, max, "l clamps at the widest line");

    // Pan left until we reach the edge. The press that lands exactly on 0 still
    // scrolls (it had scroll left to give), so focus stays in the diff.
    for _ in 0..50 {
        a.handle_key(key(KeyCode::Char('h')));
        if a.h_scroll == 0 {
            break;
        }
    }
    assert_eq!(a.h_scroll, 0, "h clamps at the left edge");
    assert_eq!(
        a.focus,
        Focus::Diff,
        "reaching the edge keeps focus in the diff"
    );
}

#[test]
fn h_at_the_left_edge_focuses_the_sidebar() {
    // Wide line so the diff can scroll horizontally at all.
    let long_line = "a".repeat(200);
    let diff = format!(
        "diff --git a/wide.rs b/wide.rs\n\
         index 1111111..2222222 100644\n\
         --- a/wide.rs\n\
         +++ b/wide.rs\n\
         @@ -1 +1 @@\n\
         -short\n\
         +{long_line}\n"
    );
    let mut a = App::new(
        parse_diff(&diff),
        Target::Local {
            base: "main".to_string(),
            head_sha: "deadbeefcafe".to_string(),
        },
    );
    drive(&mut a, 100, 24, &[key(KeyCode::Char('l'))]); // open the file into the diff
    assert_eq!(a.focus, Focus::Diff);

    // While there is scroll to give, `h` pans the code and keeps diff focus.
    // One `l` pans right by a step; one `h` pans exactly back to the edge.
    a.handle_key(key(KeyCode::Char('l')));
    assert!(a.h_scroll > 0);
    a.handle_key(key(KeyCode::Char('h')));
    assert_eq!(a.h_scroll, 0, "panned back to the left edge");
    assert_eq!(a.focus, Focus::Diff, "still in the diff at the edge");

    // Now at the edge, the next `h` steps focus out to the sidebar — the mirror
    // of `l`/Enter in the sidebar stepping into the diff.
    a.handle_key(key(KeyCode::Char('h')));
    assert_eq!(
        a.focus,
        Focus::Tree,
        "h at the left edge focuses the sidebar"
    );
    assert_eq!(a.h_scroll, 0, "and the diff stays at the left edge");

    // Left arrow at the edge behaves the same as `h`.
    a.handle_key(key(KeyCode::Char('l'))); // back into the diff
    assert_eq!(a.focus, Focus::Diff);
    a.handle_key(key(KeyCode::Left));
    assert_eq!(
        a.focus,
        Focus::Tree,
        "left arrow at the edge focuses the sidebar too"
    );
}

#[test]
fn opening_a_different_file_resets_horizontal_scroll() {
    // Two files, both wide enough to scroll; switching between them must land at
    // the left edge even though the new file could itself hold a scroll.
    let wide = "b".repeat(200);
    let diff = format!(
        "diff --git a/one.rs b/one.rs\n\
         index 1111111..2222222 100644\n\
         --- a/one.rs\n\
         +++ b/one.rs\n\
         @@ -1 +1 @@\n\
         -short\n\
         +{wide}\n\
         diff --git a/two.rs b/two.rs\n\
         index 3333333..4444444 100644\n\
         --- a/two.rs\n\
         +++ b/two.rs\n\
         @@ -1 +1 @@\n\
         -short\n\
         +{wide}\n"
    );
    let mut a = App::new(
        parse_diff(&diff),
        Target::Local {
            base: "main".to_string(),
            head_sha: "deadbeefcafe".to_string(),
        },
    );
    drive(&mut a, 100, 24, &[key(KeyCode::Char('l'))]); // open one.rs into the diff
    a.handle_key(key(KeyCode::Char('l'))); // pan right
    assert!(a.h_scroll > 0);
    a.handle_key(key(KeyCode::Char('J'))); // switch to two.rs
    assert_eq!(a.selected, 1);
    assert_eq!(a.h_scroll, 0, "a fresh file opens unscrolled");
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
    a.handle_key(key(KeyCode::Char('j'))); // swallowed (scrolls help, not diff)
    assert_eq!(a.scroll, 0, "help eats navigation keys");
    a.handle_key(key(KeyCode::Esc)); // Esc closes it
    assert!(!a.show_help);
}

#[test]
fn help_overlay_scrolls_with_j_k_and_g() {
    // The overlay is taller than the body of a short terminal, so `j`
    // advances and `g` jumps back. `?` reopens at the top (toggle resets
    // scroll).
    let mut a = app();
    let mut term = Terminal::new(TestBackend::new(100, 24)).unwrap();
    term.draw(|f| render(f, &mut a)).unwrap();
    a.handle_key(key(KeyCode::Char('?')));
    term.draw(|f| render(f, &mut a)).unwrap();
    assert_eq!(a.help_scroll, 0);
    a.handle_key(key(KeyCode::Char('j')));
    term.draw(|f| render(f, &mut a)).unwrap();
    assert_eq!(a.help_scroll, 1);
    a.handle_key(key(KeyCode::Char('G')));
    term.draw(|f| render(f, &mut a)).unwrap();
    assert_eq!(a.help_scroll, a.max_help_scroll());
    a.handle_key(key(KeyCode::Char('g')));
    assert_eq!(a.help_scroll, 0);
    // Close + reopen lands back at the top.
    a.handle_key(key(KeyCode::Char('G')));
    a.handle_key(key(KeyCode::Char('?')));
    a.handle_key(key(KeyCode::Char('?')));
    assert!(a.show_help);
    assert_eq!(a.help_scroll, 0);
}
