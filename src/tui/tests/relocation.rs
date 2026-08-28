//! On-reload relocation: the in-memory annotation line follows edits to the
//! file made since capture, driven off the persisted signature. Blob content is
//! injected straight into `App::blob_cache` so these stay pure (no repo/subprocess).

use super::*;
use crate::anchor::{AnchorSig, Params};

/// Seed the Head blob for `path` so relocation reads `content` without a repo.
fn set_head_blob(a: &mut App, path: &str, content: &str) {
    let lines: Vec<String> = content.lines().map(String::from).collect();
    a.blob_cache
        .insert((path.to_string(), BlobSide::Head), Some(lines));
}

fn signed_note(id: &str, file: &str, line: u32, sig: AnchorSig) -> Annotation {
    let mut n = note(id, Author::User, file, Side::Right, line, Severity::Info);
    n.signature = Some(sig);
    n
}

#[test]
fn relocates_line_shifted_down_since_capture() {
    let path = "src/alpha.rs";
    let original = "fn x() {\n    let target = compute();\n    done();\n}";
    let sig = AnchorSig::capture(original, 2, &Params::default()).unwrap();

    let mut a = app();
    // The file now has two lines inserted above the anchor.
    set_head_blob(
        &mut a,
        path,
        "// new\n// lines\nfn x() {\n    let target = compute();\n    done();\n}",
    );
    a.annotations = vec![signed_note("a1", path, 2, sig)];
    a.relocate_annotations();

    assert_eq!(a.annotations[0].line, 4);
    assert_eq!(a.annotations[0].scope, AnchorScope::Line);
    assert!(a.orphaned_anchors.is_empty());
}

#[test]
fn orphans_when_anchor_is_gone() {
    let path = "src/alpha.rs";
    let original = "fn x() {\n    let target = compute();\n    done();\n}";
    let sig = AnchorSig::capture(original, 2, &Params::default()).unwrap();

    let mut a = app();
    // Nothing resembling the anchor remains.
    set_head_blob(&mut a, path, "wholly\nunrelated\nreplacement\ntext");
    a.annotations = vec![signed_note("a1", path, 2, sig)];
    a.relocate_annotations();

    assert_eq!(a.annotations[0].scope, AnchorScope::File);
    assert!(a.orphaned_anchors.contains("a1"));
}

#[test]
fn region_end_slides_with_its_start() {
    let path = "src/alpha.rs";
    let original = "fn x() {\n    a();\n    b();\n    c();\n}";
    // Anchor the region start on line 2 (`a();`).
    let sig = AnchorSig::capture(original, 2, &Params::default()).unwrap();

    let mut a = app();
    // Three lines pushed in above; the whole block shifts down by 3.
    set_head_blob(
        &mut a,
        path,
        "// 1\n// 2\n// 3\nfn x() {\n    a();\n    b();\n    c();\n}",
    );
    let mut ann = signed_note("a1", path, 2, sig);
    ann.end_line = Some(4); // region was lines 2..=4
    a.annotations = vec![ann];
    a.relocate_annotations();

    assert_eq!(a.annotations[0].line, 5);
    // End shifted by the same +3 delta, preserving the region length.
    assert_eq!(a.annotations[0].end_line, Some(7));
}

#[test]
fn signatureless_annotation_keeps_its_bare_line() {
    let path = "src/alpha.rs";
    let mut a = app();
    set_head_blob(&mut a, path, "anything\nat\nall");
    // No signature (old store / capture miss): line is trusted as-is.
    a.annotations = vec![note(
        "a1",
        Author::User,
        path,
        Side::Right,
        2,
        Severity::Info,
    )];
    a.relocate_annotations();

    assert_eq!(a.annotations[0].line, 2);
    assert_eq!(a.annotations[0].scope, AnchorScope::Line);
}

#[test]
fn a_reply_adopts_its_parents_anchor() {
    // The agent's replies carry an anchor of their own (the CLI has to be given
    // one), which can point somewhere else entirely — here at a line that is
    // gone. The reply must still land where its parent landed.
    let path = "src/alpha.rs";
    let original = "fn x() {\n    let target = compute();\n    done();\n}";
    let parent_sig = AnchorSig::capture(original, 2, &Params::default()).unwrap();
    let stale_sig = AnchorSig::capture("gone();\nvanished();\n", 1, &Params::default()).unwrap();

    let mut a = app();
    // Two lines inserted above the parent's anchor; the reply's own anchor is
    // nowhere in the file.
    set_head_blob(
        &mut a,
        path,
        "// new\n// lines\nfn x() {\n    let target = compute();\n    done();\n}",
    );
    let mut reply = signed_note("r1", path, 99, stale_sig);
    reply.author = Author::Agent;
    reply.reply_to = Some("a1".to_string());
    a.annotations = vec![signed_note("a1", path, 2, parent_sig), reply];
    a.relocate_annotations();

    assert_eq!(a.annotations[1].line, a.annotations[0].line);
    assert_eq!(a.annotations[1].scope, AnchorScope::Line);
    assert!(
        !a.orphaned_anchors.contains("r1"),
        "a reply is never orphaned on its own"
    );

    // ...and so it renders inline, right under the comment it answers.
    a.rebuild_view();
    let thread: Vec<&str> = a
        .view
        .rows
        .iter()
        .filter_map(|r| match r {
            Row::Comment(c) if c.header => Some(c.id.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(thread, vec!["a1", "r1"]);
}

#[test]
fn a_reply_follows_its_parent_to_the_file_level() {
    // The agent rewrote the line the user commented on, then replied. The
    // parent's anchor is gone, so the whole thread — reply included — moves to
    // the file level instead of the reply drifting off on its own anchor.
    let path = "src/alpha.rs";
    let original = "fn x() {\n    let target = compute();\n    done();\n}";
    let parent_sig = AnchorSig::capture(original, 2, &Params::default()).unwrap();
    let replacement = "fn x() {\n    let fixed = recompute();\n    done();\n}";
    let reply_sig = AnchorSig::capture(replacement, 2, &Params::default()).unwrap();

    let mut a = app();
    set_head_blob(&mut a, path, "wholly\nunrelated\nreplacement\ntext");
    let mut reply = signed_note("r1", path, 2, reply_sig);
    reply.author = Author::Agent;
    reply.reply_to = Some("a1".to_string());
    a.annotations = vec![signed_note("a1", path, 2, parent_sig), reply];
    a.relocate_annotations();

    assert_eq!(a.annotations[1].scope, AnchorScope::File);
    assert!(a.orphaned_anchors.contains("r1"));
}

#[test]
fn capture_signature_round_trips_through_relocation() {
    let path = "src/alpha.rs";
    let content = "fn x() {\n    let value = lookup(key);\n    return value;\n}";
    let mut a = app();
    set_head_blob(&mut a, path, content);

    // Capture via the App helper, exactly as authoring does.
    let sig = a
        .capture_signature(path, 2, Side::Right)
        .expect("content is cached, line in range");
    a.annotations = vec![signed_note("a1", path, 2, sig)];
    a.relocate_annotations();

    // Unchanged content: the anchor stays put.
    assert_eq!(a.annotations[0].line, 2);
}
