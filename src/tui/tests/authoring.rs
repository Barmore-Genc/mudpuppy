// Human authoring: the cursor, the visual selection, and the modal composer
// that turn key presses into store writes (comment / region / whole-file /
// reply / edit / delete / status).

use super::*;

/// An app attached to a fresh (empty) store in a temp dir, focused on the diff
/// pane. The `TempDir` is returned so the store outlives the test.
fn stored_app() -> (App, tempfile::TempDir) {
    stored_app_with(vec![])
}

/// As [`stored_app`], but seeded with `annotations` already on disk.
fn stored_app_with(annotations: Vec<Annotation>) -> (App, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.json");
    let target = Target::Local {
        base: "main".to_string(),
        head_sha: "abc".to_string(),
    };
    let mut seed = StateFile::new(target.clone());
    seed.annotations = annotations;
    store::save(&path, &seed).unwrap();

    let mut a = App::new(parse_diff(FIXTURE), target);
    let state = store::load(&path).unwrap();
    a.attach_store(path, state);
    a.set_focus("diff");
    (a, dir)
}

/// Point the cursor at the row whose new-side line number is `n` (alpha.rs is
/// the file open at launch). Panics if there is no such row.
fn cursor_to_new_line(a: &mut App, n: u32) {
    let idx = a
        .view
        .rows
        .iter()
        .position(|r| matches!(r, Row::Line(l, _) if l.new_lineno == Some(n)))
        .expect("a row with that new-side line number");
    a.cursor = idx;
}

/// Type a body into an open composer, character by character.
fn type_body(a: &mut App, body: &str) {
    for ch in body.chars() {
        a.handle_key(key(KeyCode::Char(ch)));
    }
}

#[test]
fn cursor_follows_viewport_when_it_leaves_the_window() {
    let mut a = app();
    // Long file, diff focus, short viewport so the cursor outruns the window.
    drive(
        &mut a,
        100,
        10,
        &[key(KeyCode::Char('j')), key(KeyCode::Char('l'))],
    );
    let h = a.diff_height.max(1);
    for _ in 0..(h + 5) {
        a.handle_key(key(KeyCode::Char('j')));
    }
    assert!(
        a.cursor >= a.scroll && a.cursor < a.scroll + h,
        "cursor {} stays within [{}, {})",
        a.cursor,
        a.scroll,
        a.scroll + h
    );
    assert!(a.scroll > 0, "viewport scrolled to follow the cursor");
}

#[test]
fn visual_mode_selects_an_inclusive_row_span() {
    let (mut a, _dir) = stored_app();
    // Two consecutive additions in alpha.rs: new lines 2 and 3, both Right side.
    cursor_to_new_line(&mut a, 2);
    a.handle_key(key(KeyCode::Char('v')));
    assert!(a.selection_anchor.is_some(), "v enters visual mode");
    a.handle_key(key(KeyCode::Char('j')));
    let (lo, hi) = a.selection_span().expect("a selection span");
    assert_eq!(hi - lo, 1, "two rows selected");
}

#[test]
fn c_comments_the_cursor_line_through_the_composer() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1); // the `use std::io;` context line
    a.handle_key(key(KeyCode::Char('c')));
    assert!(a.composer.is_some(), "c opens the composer");
    type_body(&mut a, "looks good");
    a.handle_key(ctrl('s'));

    assert!(a.composer.is_none(), "Ctrl-S closes the composer");
    assert_eq!(a.annotations.len(), 1);
    let note = &a.annotations[0];
    assert_eq!(note.author, Author::Human);
    assert_eq!(note.file, "src/alpha.rs");
    assert_eq!(note.line, 1);
    assert_eq!(note.end_line, None);
    assert_eq!(note.side, Side::Right);
    assert_eq!(note.body, "looks good");
}

#[test]
fn c_on_a_selection_writes_a_region() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 2);
    a.handle_key(key(KeyCode::Char('v')));
    a.handle_key(key(KeyCode::Char('j'))); // extend over new lines 2..=3
    a.handle_key(key(KeyCode::Char('c')));
    type_body(&mut a, "this whole block");
    a.handle_key(ctrl('s'));

    assert_eq!(a.annotations.len(), 1);
    let note = &a.annotations[0];
    assert_eq!(note.line, 2);
    assert_eq!(note.end_line, Some(3));
    assert!(a.selection_anchor.is_none(), "saving clears visual mode");
}

#[test]
fn capital_f_comments_the_whole_file() {
    let (mut a, _dir) = stored_app();
    a.handle_key(key(KeyCode::Char('F')));
    type_body(&mut a, "overall this file needs work");
    a.handle_key(ctrl('s'));

    assert_eq!(a.annotations.len(), 1);
    assert_eq!(a.annotations[0].scope, crate::domain::AnchorScope::File);
    assert_eq!(a.annotations[0].file, "src/alpha.rs");
}

#[test]
fn capital_r_replies_to_the_annotation_on_the_cursor_line() {
    let parent = note(
        "agent001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Warning,
    );
    let (mut a, _dir) = stored_app_with(vec![parent]);
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('R')));
    assert!(a.composer.is_some(), "R opens a reply composer");
    type_body(&mut a, "addressed it");
    a.handle_key(ctrl('s'));

    let reply = a
        .annotations
        .iter()
        .find(|x| x.reply_to.as_deref() == Some("agent001"))
        .expect("a reply threaded under the agent's note");
    assert_eq!(reply.author, Author::Human);
    assert_eq!(reply.line, 1, "reply inherits the parent's anchor");
}

#[test]
fn e_edits_the_humans_own_annotation() {
    let mut own = note(
        "human001",
        Author::Human,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    own.body = "first".to_string();
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('e')));
    assert!(a.composer.is_some(), "e opens the composer prefilled");
    // Append to the prefilled body and save.
    type_body(&mut a, " + more");
    a.handle_key(ctrl('s'));

    assert_eq!(a.annotations.len(), 1, "edit replaces in place");
    assert_eq!(a.annotations[0].body, "first + more");
}

#[test]
fn e_refuses_to_edit_the_agents_annotation() {
    let theirs = note(
        "agent001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Warning,
    );
    let (mut a, _dir) = stored_app_with(vec![theirs]);
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('e')));
    assert!(a.composer.is_none(), "no composer for the agent's note");
    assert!(a.notice.is_some(), "a hint explains why");
}

#[test]
fn capital_d_deletes_after_a_y_confirmation() {
    let own = note(
        "human001",
        Author::Human,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('D')));
    assert_eq!(a.pending_delete.as_deref(), Some("human001"));
    a.handle_key(key(KeyCode::Char('y')));
    assert!(a.pending_delete.is_none());
    assert!(a.annotations.is_empty(), "confirmed delete removes it");
}

#[test]
fn delete_confirmation_is_cancelled_by_any_other_key() {
    let own = note(
        "human001",
        Author::Human,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('D')));
    a.handle_key(key(KeyCode::Char('n')));
    assert!(a.pending_delete.is_none(), "n cancels the prompt");
    assert_eq!(a.annotations.len(), 1, "nothing deleted");
}

#[test]
fn s_cycles_the_status_of_the_cursor_annotation() {
    let own = note(
        "human001",
        Author::Human,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('s')));
    assert_eq!(a.annotations[0].status, Status::Resolved);
    a.handle_key(key(KeyCode::Char('s')));
    assert_eq!(a.annotations[0].status, Status::Wontfix);
    a.handle_key(key(KeyCode::Char('s')));
    assert_eq!(a.annotations[0].status, Status::Open);
}

#[test]
fn comment_on_a_non_line_row_is_a_no_op_with_a_hint() {
    let (mut a, _dir) = stored_app();
    a.cursor = 0; // the hunk header row
    a.handle_key(key(KeyCode::Char('c')));
    assert!(a.composer.is_none(), "no composer on a hunk header");
    assert!(a.notice.is_some(), "a hint is surfaced");
    assert!(a.annotations.is_empty());
}

#[test]
fn composer_overlay_renders_with_target_and_body() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    a.handle_key(key(KeyCode::Char('c')));
    type_body(&mut a, "needs a guard here");
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    assert!(s.contains("Comment"), "composer title missing:\n{s}");
    assert!(s.contains("needs a guard here"), "body missing:\n{s}");
    assert!(s.contains("L1"), "anchor label missing:\n{s}");
    insta::assert_snapshot!(s);
}

#[test]
fn file_level_header_and_region_gutter_render() {
    let mut file_note = note(
        "file0001",
        Author::Human,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Warning,
    );
    file_note.scope = AnchorScope::File;
    let mut region = note(
        "rgn00002",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        2,
        Severity::Blocker,
    );
    region.end_line = Some(3);
    let mut a = annotated_app(vec![file_note, region]);
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    // The whole-file note rides a header row; the region marks both lines 2 & 3.
    assert!(
        s.contains("file-level: 1"),
        "file-level header missing:\n{s}"
    );
    assert_eq!(
        a.line_marks().get(&(Side::Right, 2)),
        Some(&Severity::Blocker)
    );
    assert_eq!(
        a.line_marks().get(&(Side::Right, 3)),
        Some(&Severity::Blocker)
    );
    insta::assert_snapshot!(s);
}

#[test]
fn esc_clears_the_visual_selection() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 2);
    a.handle_key(key(KeyCode::Char('v')));
    assert!(a.selection_anchor.is_some());
    a.handle_key(key(KeyCode::Esc));
    assert!(a.selection_anchor.is_none(), "Esc leaves visual mode");
}
