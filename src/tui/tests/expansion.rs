// Context expansion (revealing hidden gap lines, PR #2) and the synthetic
// files the tree grows for out-of-diff annotations.

use super::*;

// ---- context expansion (PR #2) -----------------------------------------

/// A diff whose single hunk sits in the middle of the file, so there is a
/// gap of hidden context both above and below it.
const MIDDLE_DIFF: &str = "\
diff --git a/src/mid.rs b/src/mid.rs
index 1234567..89abcde 100644
--- a/src/mid.rs
+++ b/src/mid.rs
@@ -5,2 +5,3 @@ fn mid() {
 context at five
+inserted line
 context at six
";

/// An 8-line stub Head blob for `MIDDLE_DIFF` (the hunk touches new lines
/// 5..=7, leaving 1..4 above and 8 below hidden).
fn middle_blob() -> Vec<String> {
    (1..=8).map(|n| format!("line {n}")).collect()
}

fn middle_file() -> FileDiff {
    parse_diff(MIDDLE_DIFF).into_iter().next().unwrap()
}

#[test]
fn build_without_blob_still_shows_expanders() {
    let view = FileView::build(&middle_file(), None, &ViewPlan::default());
    let expanders = view
        .rows
        .iter()
        .filter(|r| matches!(r, Row::Expander { .. }))
        .count();
    // The before-first gap always shows. The trailing gap needs the blob's
    // total line count to bound it, so without a blob only one renders.
    assert_eq!(expanders, 1);
    // With no blob, no context lines are revealed.
    assert!(!view
        .rows
        .iter()
        .any(|r| matches!(r, Row::Line(l, _) if l.new_lineno == Some(1))));
}

#[test]
fn expand_down_reveals_top_of_gap_with_correct_numbers() {
    let blob = middle_blob();
    // Reveal the first two lines from the top of the before-first gap.
    let plan = ViewPlan {
        expanded: vec![GapExpansion {
            from_top: 2,
            from_bottom: 0,
        }],
    };
    let view = FileView::build(&middle_file(), Some(&blob), &plan);
    let revealed: Vec<(Option<u32>, Option<u32>)> = view
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Line(l, _) if l.kind == LineKind::Context && l.new_lineno < Some(5) => {
                Some((l.old_lineno, l.new_lineno))
            }
            _ => None,
        })
        .collect();
    // The before-first gap has delta 0, so old == new for revealed lines.
    assert_eq!(revealed, vec![(Some(1), Some(1)), (Some(2), Some(2))]);
    // An expander still covers the rest of that gap (lines 3..4 hidden).
    assert!(view
        .rows
        .iter()
        .any(|r| matches!(r, Row::Expander { new, .. } if new.start == 3)));
}

#[test]
fn expand_all_reveals_whole_gap_and_drops_expander() {
    let blob = middle_blob();
    // Saturate the before-first gap from the top.
    let plan = ViewPlan {
        expanded: vec![GapExpansion {
            from_top: usize::MAX,
            from_bottom: 0,
        }],
    };
    let view = FileView::build(&middle_file(), Some(&blob), &plan);
    // No expander remains for the fully-revealed first gap (the trailing gap
    // keeps its own).
    let first_gap_expanders = view
        .rows
        .iter()
        .filter(|r| matches!(r, Row::Expander { new, .. } if new.start < 5))
        .count();
    assert_eq!(first_gap_expanders, 0);
    // All four hidden lines (1..=4) are now context rows.
    for n in 1..=4 {
        assert!(view
            .rows
            .iter()
            .any(|r| matches!(r, Row::Line(l, _) if l.new_lineno == Some(n))));
    }
}

#[test]
fn trailing_gap_appears_when_blob_extends_past_last_hunk() {
    let blob = middle_blob();
    let view = FileView::build(&middle_file(), Some(&blob), &ViewPlan::default());
    // The hunk ends at new line 7; line 8 trails it as an after-last gap,
    // so a downward-only expander is present.
    assert!(view.rows.iter().any(|r| matches!(
        r,
        Row::Expander {
            new,
            can_up,
            can_down,
            ..
        } if new.start == 8 && !*can_up && *can_down
    )));
}

#[test]
fn set_repo_root_drops_misses_cached_before_the_root_was_known() {
    // `App::new` builds the opening view before a repo root exists, so its blob
    // lookups are forced misses and get cached as `None`. If those poisoned
    // entries survived, later expansion would keep reading the miss and reveal
    // nothing (the PR-target "expand does nothing" bug). `set_repo_root` must
    // clear them so the next lookup re-fetches for real.
    let mut a = App::new(
        parse_diff(MIDDLE_DIFF),
        Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        },
    );
    let key = ("src/mid.rs".to_string(), BlobSide::Head);
    assert_eq!(
        a.blob_cache.get(&key),
        Some(&None),
        "opening build should have cached a forced miss"
    );
    a.set_repo_root(std::path::PathBuf::from("/nonexistent"));
    assert!(
        !a.blob_cache.contains_key(&key),
        "set_repo_root must drop the pre-root miss so expansion can re-fetch"
    );
}

#[test]
fn expand_down_mutates_the_view_through_app() {
    let mut a = App::new(
        parse_diff(MIDDLE_DIFF),
        Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        },
    );
    a.diff_height = 20;
    // Inject a stub blob so expansion has content without shelling to git.
    a.blob_cache.insert(
        ("src/mid.rs".to_string(), BlobSide::Head),
        Some(middle_blob()),
    );
    a.rebuild_view();
    let collapsed = a.view.rows.len();
    a.expand_down();
    assert!(a.view.rows.len() > collapsed, "expand_down adds rows");
    a.expand_all();
    assert!(a.view.rows.len() >= collapsed);
}

// ---- synthetic files for out-of-diff annotations -----------------------

/// A store-less app whose annotations are merged into the tree, exercising
/// the synthetic-file path directly (`app()` itself never runs the merge).
fn synth_app(annotations: Vec<Annotation>) -> App {
    let mut a = app();
    a.annotations = annotations;
    a.merge_synthetic_files();
    a
}

/// An out-of-diff annotation for synthetic-file tests.
fn out_of_diff_note(id: &str, file: &str, line: u32) -> Annotation {
    note(id, Author::User, file, Side::Right, line, Severity::Info)
}

#[test]
fn merge_appends_synthetic_for_out_of_diff_annotation() {
    let app = synth_app(vec![out_of_diff_note("a1", "docs/notes.md", 1)]);
    let synthetic: Vec<&FileDiff> = app.files.iter().filter(|f| f.synthetic).collect();
    assert_eq!(synthetic.len(), 1);
    assert_eq!(synthetic[0].display_path(), "docs/notes.md");
    assert_eq!(synthetic[0].status, FileStatus::Unchanged);
}

#[test]
fn merge_is_idempotent_for_repeated_calls_and_duplicate_paths() {
    let mut app = synth_app(vec![
        out_of_diff_note("a1", "docs/notes.md", 1),
        out_of_diff_note("a2", "docs/notes.md", 2),
    ]);
    app.merge_synthetic_files();
    let count = app
        .files
        .iter()
        .filter(|f| f.display_path() == "docs/notes.md")
        .count();
    assert_eq!(count, 1);
}

#[test]
fn merge_drops_stale_synthetic_but_keeps_real_files_in_order() {
    let mut app = synth_app(vec![out_of_diff_note("a1", "docs/notes.md", 1)]);
    let before: Vec<String> = app
        .files
        .iter()
        .map(|f| f.display_path().to_string())
        .collect();
    assert!(before.iter().any(|p| p == "docs/notes.md"));
    let real_files: Vec<String> = before
        .iter()
        .filter(|p| p.as_str() != "docs/notes.md")
        .cloned()
        .collect();

    // The annotation vanishes: its synthetic entry should be dropped, while
    // every real diff file stays put in the same order.
    app.annotations.clear();
    app.merge_synthetic_files();
    let after: Vec<String> = app
        .files
        .iter()
        .map(|f| f.display_path().to_string())
        .collect();
    assert_eq!(after, real_files);
    assert!(app.files.iter().all(|f| !f.synthetic));
}

#[test]
fn build_synthetic_renders_all_context_lines() {
    let blob = vec!["alpha".to_string(), "beta".to_string()];
    let file = FileDiff::synthetic("docs/notes.md");
    let view = FileView::build(&file, Some(&blob), &ViewPlan::default());
    let lines: Vec<&DiffLine> = view
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Line(l, _) => Some(l),
            _ => None,
        })
        .collect();
    assert_eq!(lines.len(), 2);
    assert!(lines.iter().all(|l| l.kind == LineKind::Context));
    assert_eq!(lines[0].old_lineno, Some(1));
    assert_eq!(lines[0].new_lineno, Some(1));
    assert_eq!(lines[1].old_lineno, Some(2));
    assert_eq!(lines[1].new_lineno, Some(2));
}

#[test]
fn build_synthetic_without_blob_shows_an_unavailable_notice() {
    let file = FileDiff::synthetic("docs/notes.md");
    let view = FileView::build(&file, None, &ViewPlan::default());
    assert!(matches!(view.rows.as_slice(), [Row::Notice(_)]));
}

#[test]
fn synthetic_file_renders_in_tree_and_diff() {
    let mut app = synth_app(vec![out_of_diff_note("a1", "docs/notes.md", 2)]);
    // Select the synthetic file (appended last) and seed its Head blob so the
    // diff pane shows all-context content.
    let idx = app
        .files
        .iter()
        .position(|f| f.display_path() == "docs/notes.md")
        .unwrap();
    app.blob_cache.insert(
        ("docs/notes.md".to_string(), BlobSide::Head),
        Some(vec![
            "# Notes".to_string(),
            "first line".to_string(),
            "second line".to_string(),
        ]),
    );
    app.select(idx);
    assert_eq!(app.current().display_path(), "docs/notes.md");
    let term = drive(&mut app, 80, 24, &[]);
    let text = screen(&term);
    // The synthetic file is shown as all-context (no +/- markers): its blob
    // lines appear verbatim in the diff pane.
    assert!(
        text.contains("# Notes"),
        "synthetic content missing:\n{text}"
    );
    assert!(text.contains("first line"));
    insta::assert_snapshot!(text);
}
