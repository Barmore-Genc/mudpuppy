// Annotations as the viewer surfaces them: gutter marks, the panel, the
// status-bar count, and the per-line/per-file lookup maps.

use super::*;

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
            Author::User,
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
fn annotations_tab_lists_every_file() {
    // `Space a` swaps the file tree for the annotations tab, which lists every
    // annotation in the store — including the one on README.md, a file other
    // than the one open in the diff.
    let mut notes = alpha_notes();
    notes.push(note(
        "rdm00003",
        Author::User,
        "README.md",
        Side::Right,
        1,
        Severity::Warning,
    ));
    let mut a = annotated_app(notes);
    let term = drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char(' ')), key(KeyCode::Char('a'))],
    );
    assert_eq!(a.sidebar, Sidebar::Annotations);
    let text = screen(&term);
    assert!(text.contains("Annotations 3"));
    // Grouped under bold file headers, both files' annotations are present.
    assert!(text.contains("src/alpha.rs"));
    assert!(text.contains("README.md"));
    assert!(text.contains("L2 agent"));
    assert!(text.contains("L4 user"));
    insta::assert_snapshot!(text);
}

#[test]
fn annotations_tab_jumps_to_the_selected_annotation() {
    // With the tab open, navigating to README.md's annotation and confirming
    // selects that file in the diff and focuses the diff pane.
    let mut notes = alpha_notes();
    notes.push(note(
        "rdm00003",
        Author::User,
        "README.md",
        Side::Right,
        1,
        Severity::Warning,
    ));
    let mut a = annotated_app(notes);
    // Open the tab, jump to the first item (README.md sorts before src/alpha.rs),
    // and confirm — even though alpha.rs is the file open at launch.
    drive(
        &mut a,
        100,
        24,
        &[
            key(KeyCode::Char(' ')),
            key(KeyCode::Char('a')),
            key(KeyCode::Char('g')),
            key(KeyCode::Char('g')),
            key(KeyCode::Enter),
        ],
    );
    assert_eq!(a.current().display_path(), "README.md");
    assert_eq!(a.focus, Focus::Diff);
}

#[test]
fn navigating_the_tab_previews_without_changing_focus() {
    // Moving through the list switches the diff to each annotation's file and
    // line live — no Enter needed — while focus stays on the list so j/k keep
    // walking it.
    let mut notes = alpha_notes();
    notes.push(note(
        "rdm00003",
        Author::User,
        "README.md",
        Side::Right,
        1,
        Severity::Warning,
    ));
    let mut a = annotated_app(notes);
    // Opening the tab previews the first row (README.md sorts first), even
    // though alpha.rs was the file open at launch.
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char(' ')), key(KeyCode::Char('a'))],
    );
    assert_eq!(a.current().display_path(), "README.md");
    assert_eq!(a.focus, Focus::Tree, "focus stays on the list");
    // `j` moves to src/alpha.rs's first note: the diff follows, focus does not.
    a.handle_key(key(KeyCode::Char('j')));
    assert_eq!(a.current().display_path(), "src/alpha.rs");
    assert_eq!(a.focus, Focus::Tree);
    // The cursor sits on the annotated line (RIGHT line 2 of alpha.rs).
    assert_eq!(a.annotation_id_at_cursor().as_deref(), Some("blk00001"));
}

#[test]
fn long_annotation_list_scrolls_to_reach_the_last_item() {
    // Regression: the list scroll math counted logical lines while the pane
    // wrapped, so once bodies wrapped the bottom annotations became unreachable.
    // With wrap-aware scrolling, jumping to the last note (G) keeps it on screen.
    let mut notes = Vec::new();
    for i in 1..=15u32 {
        let mut n = note(
            &format!("note{i:04}"),
            Author::User,
            "src/long.rs",
            Side::Right,
            i,
            Severity::Info,
        );
        n.body =
            "a deliberately long body that wraps across several rows in the narrow sidebar pane"
                .to_string();
        notes.push(n);
    }
    let mut a = annotated_app(notes);
    let term = drive(
        &mut a,
        100,
        24,
        &[
            key(KeyCode::Char(' ')),
            key(KeyCode::Char('a')),
            key(KeyCode::Char('G')),
        ],
    );
    // The 15th note (RIGHT line 15) is selected and visible, not scrolled past.
    assert_eq!(a.annotation_selected, 14);
    assert!(
        screen(&term).contains("L15 user"),
        "the last annotation should be on screen after jumping to it"
    );
}

#[test]
fn selecting_an_annotation_centers_it_in_the_diff() {
    // Regression: previewing a note jumped the diff so the anchored line landed
    // flush at the bottom edge; it should sit near the vertical middle instead.
    let mut a = annotated_app(vec![note(
        "mid00001",
        Author::User,
        "src/long.rs",
        Side::Right,
        15,
        Severity::Info,
    )]);
    drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char(' ')), key(KeyCode::Char('a'))],
    );
    let height = a.diff_height;
    let offset = a.cursor - a.scroll; // cursor's row within the viewport
    assert!(
        offset != height - 1,
        "the anchored line should not be pinned to the bottom row"
    );
    let mid = height / 2;
    assert!(
        offset.abs_diff(mid) <= 1,
        "expected the anchored line near the middle (row {mid}), got row {offset}"
    );
}

#[test]
fn status_bar_shows_annotation_count() {
    let term = drive(&mut annotated_app(alpha_notes()), 100, 24, &[]);
    // Two annotations, both open.
    assert!(screen(&term).contains("2 (2 open)"));
}

#[test]
fn long_comment_body_wraps_without_clipping_at_the_pane_edge() {
    // A wrapped inline comment must stay inside the diff pane: the wrap budget
    // has to account for the full rendered prefix (the gutter pad, the "▏ "
    // thread bar, and the body indent), not just the bar. When it under-counts,
    // each wrapped line runs a few columns past the inner edge and the tail is
    // clipped, silently dropping words from the comment.
    let mut n = note(
        "long0001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        2,
        Severity::Info,
    );
    // Distinct tokens (none collide with the prefix label words) so a missing
    // one unambiguously means the renderer clipped it off the right edge.
    let tokens: Vec<String> = (0..60).map(|i| format!("W{i:02}")).collect();
    n.body = tokens.join(" ");

    let term = drive(&mut annotated_app(vec![n]), 100, 24, &[]);
    let shown = screen(&term);
    let missing: Vec<&String> = tokens.iter().filter(|t| !shown.contains(*t)).collect();
    assert!(
        missing.is_empty(),
        "comment body words clipped at the pane edge: {missing:?}\n{shown}"
    );
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
