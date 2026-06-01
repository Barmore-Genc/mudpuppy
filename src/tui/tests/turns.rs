// The turn protocol (the user's release / first-contact approval, PLAN.md §6)
// and live reload (the data path a notify tick drives, PLAN.md §9).

use super::*;

// ---- turn protocol: the user's release (PLAN.md §6) -------------------

#[test]
fn r_releases_the_turn_back_to_the_agent() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.json");
    let target = Target::Local {
        base: "main".to_string(),
        head_sha: "abc".to_string(),
    };

    // Seed a store where an agent is blocked waiting on the user at seq 3.
    let mut seed = StateFile::new(target.clone());
    seed.turn.agent_waiting = true;
    seed.turn.owner = Author::User;
    seed.turn.seq = 3;
    store::save(&path, &seed).unwrap();

    let mut a = App::new(parse_diff(FIXTURE), target);
    a.attach_store(path.clone(), store::load(&path).unwrap());
    assert!(a.turn.agent_waiting, "attach loads the turn block");

    leader(&mut a, "tr");

    // In memory: ownership handed back, waiting cleared.
    assert_eq!(a.turn.owner, Author::Agent);
    assert!(!a.turn.agent_waiting);
    // On disk: seq bumped past what the waiter recorded, and first-contact
    // approval is now set — this write is what unblocks `agent wait`.
    let saved = store::load(&path).unwrap().unwrap();
    assert_eq!(saved.turn.seq, 4);
    assert_eq!(saved.turn.owner, Author::Agent);
    assert!(!saved.turn.agent_waiting);
    assert!(saved.turn.approved);
}

#[test]
fn r_without_a_store_is_a_harmless_noop() {
    // No store attached (resolution failed / store-less view): `r` must not
    // panic and leaves the default turn untouched.
    let mut a = app();
    leader(&mut a, "tr");
    assert_eq!(a.turn, Turn::default());
}

#[test]
fn status_bar_surfaces_a_waiting_agent() {
    let mut a = app();
    a.turn.agent_waiting = true;
    let term = drive(&mut a, 100, 24, &[]);
    assert!(
        screen(&term).contains("agent waiting"),
        "status bar should advertise the waiting agent:\n{}",
        screen(&term)
    );
}

#[test]
fn awaiting_approval_only_while_unapproved_and_waiting() {
    let mut a = app();
    assert!(!a.awaiting_approval(), "idle session: nothing to approve");
    a.turn.agent_waiting = true;
    assert!(
        a.awaiting_approval(),
        "first contact: waiting, not approved"
    );
    a.turn.approved = true;
    assert!(
        !a.awaiting_approval(),
        "once approved, a still-waiting agent is no longer first contact"
    );
}

#[test]
fn first_contact_shows_approval_banner_and_approve_hint() {
    // Unapproved agent blocked in `agent wait`: the top banner asks for
    // approval and the status hint reads "approve", not "release".
    let mut a = app();
    a.turn.agent_waiting = true;
    let term = drive(&mut a, 100, 24, &[]);
    let text = screen(&term);
    assert!(
        text.contains("wants to collaborate"),
        "approval banner should appear on first contact:\n{text}"
    );
    assert!(
        text.contains("r approve"),
        "hint should offer approval:\n{text}"
    );
    assert!(
        !text.contains("r release"),
        "release is for approved sessions"
    );
}

#[test]
fn approved_waiting_agent_offers_release_without_the_banner() {
    // An established (approved) session with the agent waiting: no approval
    // banner, and the hint goes back to "release".
    let mut a = app();
    a.turn.agent_waiting = true;
    a.turn.approved = true;
    let term = drive(&mut a, 100, 24, &[]);
    let text = screen(&term);
    assert!(
        !text.contains("wants to collaborate"),
        "no approval banner once approved:\n{text}"
    );
    assert!(
        text.contains("r release"),
        "hint should offer release:\n{text}"
    );
}

// ---- live reload: the data path a notify tick drives (PLAN.md §9) -------

#[test]
fn reload_picks_up_another_processs_writes() {
    // The notify watch only decides *when* to reload; `reload` does the work
    // of re-reading the store, so drive it directly to prove a TUI started on
    // an empty store sees an agent's later annotation and turn change.
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("annotations.json");
    let target = Target::Local {
        base: "main".to_string(),
        head_sha: "abc".to_string(),
    };

    let mut a = App::new(parse_diff(FIXTURE), target.clone());
    a.attach_store(path.clone(), store::load(&path).unwrap());
    assert!(a.annotations.is_empty(), "starts with an empty store");

    // Another process (the agent) writes a comment and takes the turn.
    store::update(&path, &target, |s| {
        s.annotations.push(note(
            "agent001",
            Author::Agent,
            "src/alpha.rs",
            Side::Right,
            2,
            Severity::Warning,
        ));
        s.turn.agent_waiting = true;
    })
    .unwrap();

    a.reload();
    assert_eq!(a.annotations.len(), 1, "reload picks up the new annotation");
    assert_eq!(a.annotations[0].id, "agent001");
    assert!(a.turn.agent_waiting, "and the refreshed turn state");
}

#[test]
fn reload_without_a_store_is_a_harmless_noop() {
    // Store-less view (resolution failed): reload must not panic and leaves
    // the empty defaults in place.
    let mut a = app();
    a.reload();
    assert!(a.annotations.is_empty());
    assert_eq!(a.turn, Turn::default());
}
