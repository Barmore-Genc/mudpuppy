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
fn annotations_panel_lists_current_file() {
    // `Space a` toggles the panel; it lists alpha.rs's two annotations.
    let mut a = annotated_app(alpha_notes());
    let term = drive(
        &mut a,
        100,
        24,
        &[key(KeyCode::Char(' ')), key(KeyCode::Char('a'))],
    );
    assert!(a.show_panel);
    let text = screen(&term);
    assert!(text.contains("Annotations"));
    assert!(text.contains("L2 agent"));
    assert!(text.contains("L4 user"));
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
