// User authoring: the cursor, the visual selection, and the modal composer
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
    leader(&mut a, "cc");
    assert!(a.composer.is_some(), "c opens the composer");
    type_body(&mut a, "looks good");
    a.handle_key(ctrl('s'));

    assert!(a.composer.is_none(), "Ctrl-S closes the composer");
    assert_eq!(a.annotations.len(), 1);
    let note = &a.annotations[0];
    assert_eq!(note.author, Author::User);
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
    leader(&mut a, "cc");
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
    leader(&mut a, "cf");
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
    leader(&mut a, "cr");
    assert!(a.composer.is_some(), "R opens a reply composer");
    type_body(&mut a, "addressed it");
    a.handle_key(ctrl('s'));

    let reply = a
        .annotations
        .iter()
        .find(|x| x.reply_to.as_deref() == Some("agent001"))
        .expect("a reply threaded under the agent's note");
    assert_eq!(reply.author, Author::User);
    assert_eq!(reply.line, 1, "reply inherits the parent's anchor");
}

#[test]
fn e_edits_the_users_own_annotation() {
    let mut own = note(
        "user0001",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    own.body = "first".to_string();
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "ce");
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
    leader(&mut a, "ce");
    assert!(a.composer.is_none(), "no composer for the agent's note");
    assert!(a.notice.is_some(), "a hint explains why");
}

#[test]
fn capital_d_deletes_after_a_y_confirmation() {
    let own = note(
        "user0001",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cd");
    assert_eq!(a.pending_delete.as_deref(), Some("user0001"));
    a.handle_key(key(KeyCode::Char('y')));
    assert!(a.pending_delete.is_none());
    assert!(a.annotations.is_empty(), "confirmed delete removes it");
}

#[test]
fn delete_confirmation_is_cancelled_by_any_other_key() {
    let own = note(
        "user0001",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cd");
    a.handle_key(key(KeyCode::Char('n')));
    assert!(a.pending_delete.is_none(), "n cancels the prompt");
    assert_eq!(a.annotations.len(), 1, "nothing deleted");
}

#[test]
fn s_cycles_the_status_of_the_cursor_annotation() {
    let own = note(
        "user0001",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cs");
    assert_eq!(a.annotations[0].status, Status::Resolved);
    leader(&mut a, "cs");
    assert_eq!(a.annotations[0].status, Status::Wontfix);
    leader(&mut a, "cs");
    assert_eq!(a.annotations[0].status, Status::Open);
}

#[test]
fn reset_annotations_clears_the_whole_store() {
    // Reset is a clean-slate action: it drops the agent's annotations too, not
    // just the user's, and reports the count it removed.
    let (mut a, _dir) = stored_app_with(vec![
        note(
            "agent001",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            1,
            Severity::Blocker,
        ),
        note(
            "user0002",
            Author::User,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Info,
        ),
    ]);
    a.reset_annotations();
    assert!(a.annotations.is_empty(), "every annotation is gone");
    assert_eq!(a.notice.as_deref(), Some("reset 2 annotations"));
}

#[test]
fn reset_with_an_empty_store_is_a_no_op_with_a_hint() {
    let (mut a, _dir) = stored_app();
    a.reset_annotations();
    assert_eq!(a.notice.as_deref(), Some("no annotations to reset"));
}

#[test]
fn leader_r_confirms_before_clearing() {
    // The `<leader> R` binding opens a guard prompt rather than clearing
    // outright; the annotations survive until the user confirms.
    let (mut a, _dir) = stored_app_with(vec![note(
        "user0001",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    )]);
    leader(&mut a, "R");
    let prompt = a.prompt.as_ref().expect("the reset prompt is open");
    assert!(prompt.message.contains('1'), "the count is in the question");
    assert_eq!(
        a.annotations.len(),
        1,
        "nothing cleared before confirmation"
    );
}

#[test]
fn comment_on_a_non_line_row_is_a_no_op_with_a_hint() {
    let (mut a, _dir) = stored_app();
    a.cursor = 0; // the hunk header row
    leader(&mut a, "cc");
    assert!(a.composer.is_none(), "no composer on a hunk header");
    assert!(a.notice.is_some(), "a hint is surfaced");
    assert!(a.annotations.is_empty());
}

#[test]
fn enter_saves_from_normal_mode() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "looks good"); // typed in the default insert mode
    a.handle_key(key(KeyCode::Esc)); // drop to normal
    assert!(
        a.composer.is_some(),
        "Esc only leaves insert mode, not the composer"
    );
    a.handle_key(key(KeyCode::Enter)); // normal-mode Enter saves

    assert!(a.composer.is_none(), "normal-mode Enter saves and closes");
    assert_eq!(a.annotations.len(), 1);
    assert_eq!(a.annotations[0].body, "looks good");
}

#[test]
fn enter_inserts_a_newline_while_inserting() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "first");
    a.handle_key(key(KeyCode::Enter)); // insert-mode Enter is a newline
    type_body(&mut a, "second");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "first\nsecond");
}

#[test]
fn o_opens_a_line_below_and_resumes_insert() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "first");
    a.handle_key(key(KeyCode::Esc)); // normal
    a.handle_key(key(KeyCode::Char('o'))); // open line below, back to insert
    type_body(&mut a, "second");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "first\nsecond");
}

#[test]
fn ctrl_j_inserts_a_newline() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "first");
    a.handle_key(ctrl('j')); // Ctrl-J inserts a newline in either mode
    type_body(&mut a, "second");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "first\nsecond");
}

#[test]
fn x_deletes_the_char_under_the_cursor() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "abc");
    a.handle_key(key(KeyCode::Esc)); // normal
    a.handle_key(key(KeyCode::Char('0'))); // start of line
    a.handle_key(key(KeyCode::Char('x'))); // delete 'a'
    a.handle_key(key(KeyCode::Enter)); // save
    assert_eq!(a.annotations[0].body, "bc");
}

#[test]
fn dd_deletes_the_current_line() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "first");
    a.handle_key(ctrl('j'));
    type_body(&mut a, "second");
    a.handle_key(key(KeyCode::Esc)); // normal, cursor on the second line
    a.handle_key(key(KeyCode::Char('d')));
    a.handle_key(key(KeyCode::Char('d'))); // dd deletes the current line
    a.handle_key(key(KeyCode::Enter));
    assert_eq!(a.annotations[0].body, "first");
}

#[test]
fn normal_mode_motions_position_the_insert_point() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "ac");
    a.handle_key(key(KeyCode::Esc)); // normal, cursor past the end
    a.handle_key(key(KeyCode::Char('0'))); // column 0
    a.handle_key(key(KeyCode::Char('l'))); // onto 'c'
    a.handle_key(key(KeyCode::Char('i'))); // insert before it
    type_body(&mut a, "b");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "abc");
}

#[test]
fn backspace_at_line_start_joins_with_the_previous_line() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "first");
    a.handle_key(key(KeyCode::Enter)); // newline; cursor at col 0 of line two
    a.handle_key(key(KeyCode::Backspace)); // joins back onto "first"
    type_body(&mut a, "more");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "firstmore");
}

#[test]
fn editing_resumes_with_the_cursor_at_the_end() {
    let mut own = note(
        "user0001",
        Author::User,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Info,
    );
    own.body = "line one\nline two".to_string();
    let (mut a, _dir) = stored_app_with(vec![own]);
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "ce");
    // The prefilled body lands the cursor at the end of the last line.
    type_body(&mut a, "!");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "line one\nline two!");
}

#[test]
fn composer_overlay_renders_with_target_and_body() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    type_body(&mut a, "needs a guard here");
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    assert!(s.contains("Comment"), "composer title missing:\n{s}");
    assert!(s.contains("needs a guard here"), "body missing:\n{s}");
    assert!(s.contains("L1"), "anchor label missing:\n{s}");
    insta::assert_snapshot!(s);
}

#[test]
fn empty_composer_caret_occupies_a_single_row() {
    // Regression: an empty body rendered its caret as a reverse-video *space*,
    // and a whitespace-only line is drawn as two visual rows by ratatui's `Wrap`
    // — so the caret split off from where typing landed. The empty-line caret is
    // now a block glyph, which stays on one row directly below the mode label.
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    let term = drive(&mut a, 100, 24, &[]);
    let s = screen(&term);
    let rows: Vec<&str> = s.lines().collect();
    let insert = rows
        .iter()
        .position(|r| r.contains("-- INSERT --"))
        .expect("insert-mode label");
    let caret_rows: Vec<usize> = rows
        .iter()
        .enumerate()
        .filter(|(_, r)| r.contains('█'))
        .map(|(i, _)| i)
        .collect();
    assert_eq!(
        caret_rows,
        vec![insert + 1],
        "the caret is a single row directly below the mode label:\n{s}"
    );
}

#[test]
fn file_level_header_and_region_gutter_render() {
    let mut file_note = note(
        "file0001",
        Author::User,
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

// --- inline comment threads & inline compose box ---------------------------

/// Count the `Row::Comment` rows belonging to annotation `id` in the built view.
fn comment_rows_for(a: &App, id: &str) -> usize {
    a.view
        .rows
        .iter()
        .filter(|r| matches!(r, Row::Comment(c) if c.id == id))
        .count()
}

/// Index of the first `Row::Composer` placeholder, if any.
fn composer_row(a: &App) -> Option<usize> {
    a.view
        .rows
        .iter()
        .position(|r| matches!(r, Row::Composer { .. }))
}

#[test]
fn a_thread_renders_inline_under_its_line() {
    // A comment on line 2 splices a `Row::Comment` directly after that diff line.
    let (mut a, _dir) = stored_app_with(vec![note(
        "blk00001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        2,
        Severity::Blocker,
    )]);
    // Render once so the width is measured and the threads re-wrap.
    let _ = drive(&mut a, 100, 24, &[]);

    let line_idx = a
        .view
        .rows
        .iter()
        .position(|r| matches!(r, Row::Line(l, _) if l.new_lineno == Some(2)))
        .expect("the diff line for new-side 2");
    match &a.view.rows[line_idx + 1] {
        Row::Comment(c) => {
            assert!(c.header, "the first comment row carries the header");
            assert_eq!(c.id, "blk00001");
        }
        _ => panic!("expected a comment row directly under the anchored line"),
    }
}

/// Move the cursor onto the (header) comment row belonging to annotation `id`.
fn cursor_to_comment(a: &App) -> usize {
    a.view
        .rows
        .iter()
        .position(|r| matches!(r, Row::Comment(_)))
        .expect("an inline comment row")
}

#[test]
fn adding_a_comment_on_an_inline_comment_row_anchors_to_its_line() {
    // A comment sitting on new-side line 2 renders an inline thread under it.
    let (mut a, _dir) = stored_app_with(vec![note(
        "blk00001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        2,
        Severity::Blocker,
    )]);
    let _ = drive(&mut a, 100, 24, &[]);

    // Sitting on the comment body, not a code line, the new comment still lands
    // on the line that comment anchors to (right side, line 2).
    a.cursor = cursor_to_comment(&a);
    assert_eq!(a.anchor_for_comment(), Some((Side::Right, 2, None)));
    // ...and a reply targets the annotation anchored there.
    assert_eq!(a.annotation_id_at_cursor().as_deref(), Some("blk00001"));
}

#[test]
fn a_long_body_wraps_to_several_comment_rows() {
    let mut long = note(
        "blk00001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        2,
        Severity::Blocker,
    );
    // A body far wider than the diff pane must wrap to more than one row.
    long.body = "word ".repeat(80);
    let (mut a, _dir) = stored_app_with(vec![long]);
    let _ = drive(&mut a, 100, 24, &[]);
    assert!(
        comment_rows_for(&a, "blk00001") > 1,
        "a long body wraps to several single-line rows"
    );
}

#[test]
fn the_compose_box_appears_inline_under_the_cursor_line() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    let idx = composer_row(&a).expect("a composer placeholder was spliced in");
    // It sits just after the anchored diff line (new-side 1).
    let line_idx = a
        .view
        .rows
        .iter()
        .position(|r| matches!(r, Row::Line(l, _) if l.new_lineno == Some(1)))
        .unwrap();
    assert_eq!(idx, line_idx + 1, "the box follows the cursor line");
    assert_eq!(
        a.cursor, idx,
        "the cursor moves onto the composer to reveal it"
    );
}

#[test]
fn cancelling_the_inline_composer_removes_its_rows() {
    let (mut a, _dir) = stored_app();
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cc");
    assert!(composer_row(&a).is_some());
    // Esc → normal, Esc → cancel.
    a.handle_key(key(KeyCode::Esc));
    a.handle_key(key(KeyCode::Esc));
    assert!(a.composer.is_none(), "the composer closed");
    assert!(
        composer_row(&a).is_none(),
        "its placeholder rows are gone after the rebuild"
    );
}

#[test]
fn the_reply_box_appears_below_the_thread() {
    let (mut a, _dir) = stored_app_with(vec![note(
        "agent001",
        Author::Agent,
        "src/alpha.rs",
        Side::Right,
        1,
        Severity::Warning,
    )]);
    cursor_to_new_line(&mut a, 1);
    leader(&mut a, "cr");
    let composer = composer_row(&a).expect("a reply composer placeholder");
    let last_comment = a
        .view
        .rows
        .iter()
        .rposition(|r| matches!(r, Row::Comment(c) if c.id == "agent001"))
        .expect("the parent comment's rows");
    assert!(
        composer > last_comment,
        "the reply box sits below the thread it replies to"
    );
}

/// The reserved-row span of the (single) open composer placeholder, if any.
fn composer_box_span(a: &App) -> Option<(usize, usize)> {
    a.view.rows.iter().enumerate().find_map(|(i, r)| match r {
        Row::Composer { rows } => Some((i, i + *rows as usize - 1)),
        _ => None,
    })
}

#[test]
fn opening_a_composer_at_the_file_end_scrolls_the_whole_box_into_view() {
    let (mut a, _dir) = stored_app();
    // A short viewport, so a box splayed off the last row would overflow it.
    a.diff_height = 10;
    let last_line = a
        .view
        .rows
        .iter()
        .rposition(|r| matches!(r, Row::Line(l, _) if l.new_lineno.is_some()))
        .expect("a new-side line to comment on");
    a.cursor = last_line;
    leader(&mut a, "cc");

    let (top, bottom) = composer_box_span(&a).expect("a composer placeholder");
    assert!(top >= a.scroll, "the box top is on-screen");
    assert!(
        bottom < a.scroll + a.diff_height,
        "the whole box (rows {top}..={bottom}) fits in the viewport [{}, {})",
        a.scroll,
        a.scroll + a.diff_height
    );
}

#[test]
fn the_composer_box_stays_in_view_as_its_body_grows() {
    let (mut a, _dir) = stored_app();
    a.diff_height = 14;
    let last_line = a
        .view
        .rows
        .iter()
        .rposition(|r| matches!(r, Row::Line(l, _) if l.new_lineno.is_some()))
        .expect("a new-side line to comment on");
    a.cursor = last_line;
    leader(&mut a, "cc");

    // Grow the body one line at a time; the box must never spill past the bottom.
    for _ in 0..4 {
        a.handle_key(key(KeyCode::Enter));
        let (top, bottom) = composer_box_span(&a).expect("the composer placeholder");
        assert!(
            top >= a.scroll && bottom < a.scroll + a.diff_height,
            "the growing box (rows {top}..={bottom}) stays within [{}, {})",
            a.scroll,
            a.scroll + a.diff_height
        );
    }
}

#[test]
fn interleave_keeps_hunk_starts_pointing_at_hunk_rows() {
    // Two annotations on the first hunk shift every later row down; `hunk_starts`
    // must be recomputed so `}`/`{` still land on real hunk headers.
    let (mut a, _dir) = stored_app_with(alpha_notes_for_interleave());
    let _ = drive(&mut a, 100, 24, &[]);

    assert!(!a.view.hunk_starts.is_empty(), "alpha.rs has hunks");
    for &s in &a.view.hunk_starts {
        assert!(
            matches!(a.view.rows[s], Row::Hunk(_)),
            "every recomputed hunk_start indexes a Row::Hunk"
        );
    }
    // The second hunk's start is pushed past the spliced comment rows: there is
    // at least one comment row before it.
    let comments = a
        .view
        .rows
        .iter()
        .filter(|r| matches!(r, Row::Comment(_)))
        .count();
    assert!(comments >= 2, "both threads spliced their comment rows");
}

// --- vim-like editing in the composer -------------------------------------

/// Send each char of `s` as a normal-mode key press.
fn keys(a: &mut App, s: &str) {
    for ch in s.chars() {
        a.handle_key(key(KeyCode::Char(ch)));
    }
}

/// Open the composer on new-side line 1, type `body`, and drop to normal mode.
fn open_normal(a: &mut App, body: &str) {
    cursor_to_new_line(a, 1);
    leader(a, "cc");
    type_body(a, body);
    a.handle_key(key(KeyCode::Esc));
}

/// As [`open_normal`], but seed a multi-line body (joined with real newlines).
fn open_multiline_normal(a: &mut App, lines: &[&str]) {
    cursor_to_new_line(a, 1);
    leader(a, "cc");
    for (i, line) in lines.iter().enumerate() {
        if i > 0 {
            a.handle_key(ctrl('j'));
        }
        type_body(a, line);
    }
    a.handle_key(key(KeyCode::Esc));
}

/// The composer's cursor as `(row, col)`.
fn cursor(a: &App) -> (usize, usize) {
    let c = a.composer.as_ref().unwrap();
    (c.row, c.col)
}

/// The body that would be saved right now.
fn body(a: &App) -> String {
    a.composer.as_ref().unwrap().body()
}

#[test]
fn word_motions_w_b_e_land_on_word_boundaries() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "foo bar baz");
    keys(&mut a, "gg"); // row 0, col 0
    keys(&mut a, "w");
    assert_eq!(cursor(&a), (0, 4), "w jumps to the next word start");
    keys(&mut a, "e");
    assert_eq!(cursor(&a), (0, 6), "e lands on the word end");
    keys(&mut a, "b");
    assert_eq!(cursor(&a), (0, 4), "b returns to the word start");
}

#[test]
fn count_prefixed_motion_repeats() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "alpha beta gamma delta");
    keys(&mut a, "gg");
    keys(&mut a, "3w"); // alpha -> delta
    assert_eq!(cursor(&a), (0, 17), "3w skips three words");
}

#[test]
fn dollar_and_caret_and_zero_move_within_the_line() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "  indented");
    keys(&mut a, "gg");
    keys(&mut a, "$");
    assert_eq!(
        cursor(&a).1,
        "  indented".chars().count(),
        "$ to end of line"
    );
    keys(&mut a, "^");
    assert_eq!(cursor(&a).1, 2, "^ to first non-blank");
    keys(&mut a, "0");
    assert_eq!(cursor(&a).1, 0, "0 to column zero");
}

#[test]
fn gg_and_capital_g_jump_between_first_and_last_line() {
    let (mut a, _dir) = stored_app();
    open_multiline_normal(&mut a, &["one", "two", "three"]);
    keys(&mut a, "gg");
    assert_eq!(cursor(&a).0, 0, "gg to the first line");
    keys(&mut a, "G");
    assert_eq!(cursor(&a).0, 2, "G to the last line");
    keys(&mut a, "2G");
    assert_eq!(cursor(&a).0, 1, "2G to line two");
}

#[test]
fn dw_deletes_a_word() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "foo bar baz");
    keys(&mut a, "gg");
    keys(&mut a, "dw");
    assert_eq!(
        body(&a),
        "bar baz",
        "dw removes the word and its trailing space"
    );
}

#[test]
fn de_deletes_to_the_word_end_inclusive() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "foo bar");
    keys(&mut a, "gg");
    keys(&mut a, "de");
    assert_eq!(body(&a), " bar", "de deletes through the word's last char");
}

#[test]
fn cw_changes_to_the_word_end_like_ce() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "foo bar");
    keys(&mut a, "gg");
    keys(&mut a, "cw"); // acts like ce: trailing space is kept
    type_body(&mut a, "qux");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "qux bar");
}

#[test]
fn dt_deletes_up_to_a_found_char() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "hello world");
    keys(&mut a, "gg");
    keys(&mut a, "dt "); // delete till the space
    assert_eq!(body(&a), " world");
}

#[test]
fn df_deletes_through_a_found_char() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "hello world");
    keys(&mut a, "gg");
    keys(&mut a, "dfo"); // delete through the first 'o'
    assert_eq!(body(&a), " world");
}

#[test]
fn semicolon_repeats_the_last_find() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "a.b.c.d");
    keys(&mut a, "gg");
    keys(&mut a, "f."); // first dot, col 1
    assert_eq!(cursor(&a).1, 1);
    keys(&mut a, ";"); // next dot, col 3
    assert_eq!(cursor(&a).1, 3);
}

#[test]
fn count_prefixed_x_deletes_several_chars() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "abcdef");
    keys(&mut a, "gg");
    keys(&mut a, "3x");
    assert_eq!(body(&a), "def");
}

#[test]
fn r_replaces_the_char_under_the_cursor() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "cat");
    keys(&mut a, "gg");
    keys(&mut a, "rb");
    assert_eq!(body(&a), "bat");
}

#[test]
fn tilde_toggles_case_and_advances() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "abc");
    keys(&mut a, "gg");
    keys(&mut a, "~");
    assert_eq!(body(&a), "Abc");
    assert_eq!(cursor(&a).1, 1, "~ moves past the toggled char");
}

#[test]
fn capital_j_joins_lines_with_a_space() {
    let (mut a, _dir) = stored_app();
    open_multiline_normal(&mut a, &["foo", "bar"]);
    keys(&mut a, "gg");
    keys(&mut a, "J");
    assert_eq!(body(&a), "foo bar");
}

#[test]
fn yy_and_p_duplicate_a_line() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "abc");
    keys(&mut a, "gg");
    keys(&mut a, "yyp");
    assert_eq!(body(&a), "abc\nabc");
}

#[test]
fn cc_changes_the_whole_line() {
    let (mut a, _dir) = stored_app();
    open_multiline_normal(&mut a, &["foo", "bar"]);
    keys(&mut a, "gg");
    keys(&mut a, "cc");
    type_body(&mut a, "new");
    a.handle_key(ctrl('s'));
    assert_eq!(a.annotations[0].body, "new\nbar");
}

#[test]
fn count_prefixed_dd_deletes_several_lines() {
    let (mut a, _dir) = stored_app();
    open_multiline_normal(&mut a, &["one", "two", "three"]);
    keys(&mut a, "gg");
    keys(&mut a, "2dd");
    assert_eq!(body(&a), "three");
}

#[test]
fn u_undoes_and_ctrl_r_redoes() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "keep me");
    keys(&mut a, "gg");
    keys(&mut a, "dd"); // line gone
    assert_eq!(body(&a), "");
    keys(&mut a, "u");
    assert_eq!(body(&a), "keep me", "u restores the deleted line");
    a.handle_key(ctrl('r'));
    assert_eq!(body(&a), "", "Ctrl-R redoes the delete");
}

#[test]
fn capital_d_deletes_to_end_of_line() {
    let (mut a, _dir) = stored_app();
    open_normal(&mut a, "keep this");
    keys(&mut a, "gg");
    keys(&mut a, "ft"); // land on the 't' of "this"
    keys(&mut a, "D");
    assert_eq!(body(&a), "keep ");
}

/// Two single-line notes on alpha.rs's first hunk, for the interleave test.
fn alpha_notes_for_interleave() -> Vec<Annotation> {
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
