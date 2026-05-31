// What the panes draw: whole-screen `insta` snapshots of the initial views and
// overlays, plus the buffer-style assertions that check colour and modifiers
// the character grid can't capture.

use super::*;

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
fn diff_pane_focused_and_cursor_moved() {
    // Select the long file, focus the diff (l), move the cursor down three rows.
    // The viewport is tall enough that the cursor stays on screen without
    // scrolling, so `scroll` holds at the top.
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
    assert_eq!(a.cursor, 3);
    assert_eq!(a.scroll, 0, "cursor still in view, no scroll yet");
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
fn picker_overlay_lists_filtered_files() {
    // Set the overlay state directly: a real open shells out to `git
    // ls-files`, so a fixed file list keeps the snapshot deterministic.
    let mut a = app();
    let mut picker = crate::picker::Picker::new(vec![
        "src/alpha.rs".to_string(),
        "src/beta.rs".to_string(),
        "docs/guide.md".to_string(),
        "src/long_file.rs".to_string(),
    ]);
    picker.query = "src".to_string();
    picker.refilter();
    a.picker = Some(picker);
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    assert!(s.contains("Add file"), "picker overlay missing:\n{s}");
    assert!(s.contains("alpha.rs"), "filtered result missing:\n{s}");
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
