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
fn prompt_overlay_shows_message_and_options() {
    // The modal prompt (opened from Lua via `mudpuppy.prompt`) draws the question
    // and a row of numbered option chips on top of the panes.
    let mut a = app();
    a.open_prompt(
        "mudpuppy v9.9.9 is available. Update now?".to_string(),
        vec![
            "Install".to_string(),
            "Ignore for now".to_string(),
            "Skip".to_string(),
        ],
        None,
    );
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    assert!(s.contains("v9.9.9 is available"));
    assert!(s.contains("Install"));
    assert!(s.contains("Skip"));
    insta::assert_snapshot!(s);
}

#[test]
fn skill_refresh_prompt_renders_from_the_event() {
    // The launch-time `skill_update_check` event (fired when a stale skill
    // install exists) runs `core.luau`'s handler, which opens the refresh prompt.
    let mut a = app();
    a.fire_skill_update_check_for_test(2, "We added a new heredoc comment workflow.");
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    assert!(s.contains("heredoc comment workflow"), "screen: {s}");
    assert!(s.contains("Update the installed"), "screen: {s}");
    assert!(s.contains("Skip this version"), "screen: {s}");
}

#[test]
fn prompt_navigation_moves_and_commits_the_selection() {
    let mut a = app();
    a.open_prompt(
        "pick".to_string(),
        vec!["a".to_string(), "b".to_string(), "c".to_string()],
        None,
    );
    // Right/l advances; clamps at the last option.
    a.handle_key(key(KeyCode::Right));
    assert_eq!(a.prompt.as_ref().unwrap().selected, 1);
    a.handle_key(key(KeyCode::Char('l')));
    a.handle_key(key(KeyCode::Char('l')));
    assert_eq!(a.prompt.as_ref().unwrap().selected, 2, "clamped at the end");
    // Left walks back.
    a.handle_key(key(KeyCode::Left));
    assert_eq!(a.prompt.as_ref().unwrap().selected, 1);
    // Enter on the highlighted option closes the prompt (the callback is run by
    // the engine in the real loop; here we only see that it dismisses).
    a.handle_key(key(KeyCode::Enter));
    assert!(a.prompt.is_none(), "Enter commits and closes");
}

#[test]
fn prompt_esc_dismisses_without_choosing() {
    let mut a = app();
    a.open_prompt("q".to_string(), vec!["only".to_string()], None);
    a.handle_key(key(KeyCode::Esc));
    assert!(a.prompt.is_none());
}

#[test]
fn update_prompt_renders_changelog_and_scrolls() {
    // A prompt with a details body (the release changelog) gets the scrollable
    // layout: the top is visible at first, later entries only after scrolling, and
    // the footer advertises the scroll keys.
    let mut a = app();
    let changelog = (1..=40)
        .map(|i| format!("- change number {i}"))
        .collect::<Vec<_>>()
        .join("\n");
    a.open_prompt(
        "mudpuppy v9.9.9 is available. Update now?".to_string(),
        vec![
            "Install".to_string(),
            "Ignore for now".to_string(),
            "Skip".to_string(),
        ],
        Some(changelog),
    );

    let s = screen(&drive(&mut a, 100, 24, &[]));
    assert!(s.contains("change number 1"), "top of changelog shown: {s}");
    assert!(!s.contains("change number 40"), "bottom not shown yet: {s}");
    assert!(s.contains("scroll"), "scroll hint shown: {s}");

    // Down scrolls the body (rather than moving the option selection).
    for _ in 0..25 {
        a.handle_key(key(KeyCode::Down));
    }
    assert_eq!(
        a.prompt.as_ref().unwrap().selected,
        0,
        "down scrolls, not select"
    );
    let s = screen(&drive(&mut a, 100, 24, &[]));
    assert!(
        s.contains("change number 30"),
        "scrolled into later entries: {s}"
    );

    // Left/right still move the option selection while a body is shown.
    a.handle_key(key(KeyCode::Right));
    assert_eq!(a.prompt.as_ref().unwrap().selected, 1);
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
fn structure_rows_start_plain_then_highlight_fills_in() {
    // alpha.rs (Rust) is open at launch. Highlighting now runs off the UI
    // thread, so the freshly-built structure starts with un-coloured rows; the
    // worker (here run synchronously) fills them with syntect truecolor.
    let mut a = app();
    let body_has_rgb = |a: &App| {
        a.base_view.rows.iter().any(|r| match r {
            Row::Line(_, Some(hl)) => hl.iter().any(|(c, _)| matches!(c, Color::Rgb(..))),
            _ => false,
        })
    };
    assert!(!body_has_rgb(&a), "structure should start un-highlighted");
    a.highlight_sync();
    assert!(
        body_has_rgb(&a),
        "the highlight pass should fill in truecolor"
    );
}

#[test]
fn selection_and_status_bar_have_background_fills() {
    let term = drive(&mut app(), 100, 24, &[]);
    assert!(
        any_cell_has_bg(&term, palette::BG_SELECTED_FILE),
        "selected tree row is highlighted"
    );
    assert!(
        any_cell_has_bg(&term, Color::Rgb(30, 33, 40)),
        "status bar has its background fill"
    );
}

#[test]
fn diff_kind_tints_render_and_yield_to_selection() {
    // alpha.rs's first hunk has additions (`+let x = 2;` / `+let y = 3;`) and a
    // deletion (`-let x = 1;`); each kind gets a faint background band.
    let term = drive(&mut app(), 100, 24, &[]);
    assert!(
        any_cell_has_bg(&term, palette::BG_ADDED),
        "added lines carry the addition tint"
    );
    assert!(
        any_cell_has_bg(&term, palette::BG_REMOVED),
        "removed lines carry the deletion tint"
    );
    // `y` is unique to the `+let y = 3;` row, so this pins that exact line's band.
    assert_eq!(
        style_of(&term, "y", 29).bg,
        Some(palette::BG_ADDED),
        "the added line's own band is the addition tint"
    );

    // Selection must override the kind band. Focus the diff, land the cursor on
    // the `+let y = 3;` row (index 4: hunk header, context, deletion, two adds),
    // open a visual selection, then step the cursor up one — so that row stays in
    // the span but is no longer the cursor row, which would otherwise take
    // `BG_CURSOR` on top. Its background should now be the selection tint.
    let keys = [
        key(KeyCode::Char('l')),
        key(KeyCode::Char('j')),
        key(KeyCode::Char('j')),
        key(KeyCode::Char('j')),
        key(KeyCode::Char('j')),
        key(KeyCode::Char('v')),
        key(KeyCode::Char('k')),
    ];
    let mut a = app();
    let term = drive(&mut a, 100, 24, &keys);
    assert_eq!(
        style_of(&term, "y", 29).bg,
        Some(palette::BG_SELECTION),
        "selection background overrides the addition tint"
    );
}
