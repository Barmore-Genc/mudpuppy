//! Implementation of the `mudpuppy agent` subcommands over [`store`] and
//! [`session`].
//!
//! [`store`]: crate::store
//! [`session`]: crate::session
//!
//! Each verb reads or writes the shared annotation store and works whether or
//! not a TUI is running (PLAN.md §6, §7). Writes go through [`store::update`], so
//! they merge-by-id under a lock and never clobber a concurrent writer.
//!
//! `wait` is the exception that reads *and waits*: it blocks on store-directory
//! changes (the `notify` coordination bus) until the human releases the turn,
//! then prints what they changed (PLAN.md §6). For this milestone the agent
//! targets **local** changes (the common case); attaching to a live TUI's PR
//! target arrives later.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use jiff::Timestamp;
use nanoid::nanoid;
use notify::{RecursiveMode, Watcher};

use crate::cli::{AddArgs, AgentCommand, CommentCommand};
use crate::domain::{Annotation, Author, Severity, Side, StateFile, Status, Tag};
use crate::session::Session;
use crate::{source, store};

/// Route an `agent` subcommand to its handler.
pub fn dispatch(command: AgentCommand) -> Result<()> {
    match command {
        AgentCommand::Diff { file } => diff(file.as_deref()),
        AgentCommand::Comment { command } => comment(command),
        AgentCommand::Wait { timeout } => wait(timeout),
        AgentCommand::Reset => reset(),
    }
}

/// Resolve the session (and thus the store path) for the local-changes target.
fn session() -> Result<Session> {
    let target = source::resolve_target(None, None)?;
    Session::resolve(target)
}

/// `agent diff [--file F]` — print the unified diff under review.
fn diff(file: Option<&str>) -> Result<()> {
    let loaded = source::load(None, None)?;
    match file {
        None => print!("{}", loaded.raw),
        Some(f) => match file_section(&loaded.raw, f) {
            Some(section) => print!("{section}"),
            None => bail!("no file matching `{f}` in the diff under review"),
        },
    }
    Ok(())
}

/// Extract a single file's `diff --git …` section from the raw diff, matched by
/// path (exact `display_path`, or a suffix match so `foo.rs` finds `src/foo.rs`).
fn file_section(raw: &str, file: &str) -> Option<String> {
    let mut sections: Vec<String> = Vec::new();
    let mut current = String::new();
    for line in raw.lines() {
        if line.starts_with("diff --git ") && !current.is_empty() {
            sections.push(std::mem::take(&mut current));
        }
        current.push_str(line);
        current.push('\n');
    }
    if !current.is_empty() {
        sections.push(current);
    }
    sections.into_iter().find(|s| section_matches(s, file))
}

/// Whether a single-file diff section is for `file`. Reuses the parser so path
/// extraction (renames, /dev/null, a/ b/ prefixes) stays in one place.
fn section_matches(section: &str, file: &str) -> bool {
    crate::diff::parse_diff(section)
        .first()
        .map(|f| {
            let p = f.display_path();
            p == file || p.ends_with(&format!("/{file}"))
        })
        .unwrap_or(false)
}

/// Route a `comment` subcommand.
fn comment(command: CommentCommand) -> Result<()> {
    match command {
        CommentCommand::Add(args) => add(args),
        CommentCommand::List { open, author, file } => list(open, author, file),
        CommentCommand::Edit {
            id,
            body,
            severity,
            tag,
            status,
        } => edit(id, body, severity, tag, status),
        CommentCommand::Cancel { id } => cancel(id),
        CommentCommand::Resolve { id } => set_status(id, Status::Resolved),
        CommentCommand::Reopen { id } => set_status(id, Status::Open),
        CommentCommand::Wontfix { id } => set_status(id, Status::Wontfix),
    }
}

/// `agent comment add` — create an annotation authored by the agent.
fn add(args: AddArgs) -> Result<()> {
    // Parse the enum-valued flags up front so a typo fails before we touch disk.
    let side: Side = args.side.parse()?;
    let severity: Severity = args.severity.parse()?;
    let tag = args.tag.as_deref().map(str::parse::<Tag>).transpose()?;

    let session = session()?;
    let now = Timestamp::now();
    let annotation = Annotation {
        id: nanoid!(8),
        author: Author::Agent,
        file: args.file,
        line: args.line,
        side,
        severity,
        tag,
        status: Status::Open,
        body: args.body,
        reply_to: args.reply_to,
        created_at: now,
        updated_at: now,
    };

    let id = store::update(
        &session.store_path,
        &session.target,
        |s| -> Result<String> {
            if let Some(parent) = &annotation.reply_to {
                if s.get(parent).is_none() {
                    bail!("reply target `{parent}` not found in the store");
                }
            }
            let id = annotation.id.clone();
            s.upsert(annotation);
            Ok(id)
        },
    )??;

    println!("{id}");
    Ok(())
}

/// `agent comment list [--open] [--author …] [--file F]` — read current state.
fn list(open: bool, author: Option<String>, file: Option<String>) -> Result<()> {
    let author = author.as_deref().map(str::parse::<Author>).transpose()?;
    let session = session()?;
    let Some(state) = store::load(&session.store_path)? else {
        println!("(no annotations yet)");
        return Ok(());
    };

    let mut shown = 0;
    for a in &state.annotations {
        if open && !a.is_open() {
            continue;
        }
        if author.is_some_and(|au| a.author != au) {
            continue;
        }
        if file.as_deref().is_some_and(|f| a.file != f) {
            continue;
        }
        if shown > 0 {
            println!();
        }
        print!("{}", render(a));
        shown += 1;
    }
    if shown == 0 {
        println!("(no matching annotations)");
    }
    Ok(())
}

/// `agent comment edit` — revise one of the agent's own annotations in place.
fn edit(
    id: String,
    body: Option<String>,
    severity: Option<String>,
    tag: Option<String>,
    status: Option<String>,
) -> Result<()> {
    let severity = severity
        .as_deref()
        .map(str::parse::<Severity>)
        .transpose()?;
    let status = status.as_deref().map(str::parse::<Status>).transpose()?;
    let tag = tag.as_deref().map(str::parse::<Tag>).transpose()?;

    let session = session()?;
    store::update(&session.store_path, &session.target, |s| -> Result<()> {
        let a = s
            .get_mut(&id)
            .with_context(|| format!("no annotation with id `{id}`"))?;
        if a.author != Author::Agent {
            bail!("`{id}` is the human's annotation; the agent can only edit its own");
        }
        if let Some(b) = body {
            a.body = b;
        }
        if let Some(sv) = severity {
            a.severity = sv;
        }
        if let Some(t) = tag {
            a.tag = Some(t);
        }
        if let Some(st) = status {
            a.status = st;
        }
        a.updated_at = Timestamp::now();
        Ok(())
    })??;

    println!("edited {id}");
    Ok(())
}

/// `agent comment cancel` — retract one of the agent's own annotations.
///
/// Hard-deletes it when nothing replies to it (turn-internal "changed my mind"
/// noise vanishes); soft-retracts it to `withdrawn` when the human already
/// replied, so the thread stays coherent (PLAN.md §7).
fn cancel(id: String) -> Result<()> {
    let session = session()?;
    let outcome = store::update(
        &session.store_path,
        &session.target,
        |s| -> Result<&'static str> {
            match s.get(&id) {
                None => bail!("no annotation with id `{id}`"),
                Some(a) if a.author != Author::Agent => {
                    bail!("`{id}` is the human's annotation; the agent can only cancel its own")
                }
                Some(_) => {}
            }
            if s.has_replies(&id) {
                let a = s.get_mut(&id).expect("just checked it exists");
                a.status = Status::Withdrawn;
                a.updated_at = Timestamp::now();
                Ok("withdrawn")
            } else {
                s.remove(&id);
                Ok("deleted")
            }
        },
    )??;

    println!("{outcome} {id}");
    Ok(())
}

/// `agent comment resolve|reopen|wontfix` — a status change on any annotation.
fn set_status(id: String, status: Status) -> Result<()> {
    let session = session()?;
    store::update(&session.store_path, &session.target, |s| -> Result<()> {
        let a = s
            .get_mut(&id)
            .with_context(|| format!("no annotation with id `{id}`"))?;
        a.status = status;
        a.updated_at = Timestamp::now();
        Ok(())
    })??;

    println!("{id} -> {}", status_word(status));
    Ok(())
}

/// `agent reset` — clear the current session's annotations for a fresh round.
fn reset() -> Result<()> {
    let session = session()?;
    let cleared = store::update(&session.store_path, &session.target, |s| {
        let n = s.annotations.len();
        s.annotations.clear();
        n
    })?;
    println!("reset: cleared {cleared} annotation(s)");
    Ok(())
}

// --- The turn rendezvous (`agent wait`, PLAN.md §6) -------------------------

/// How the block ended, kept separate from the side effects so the caller can
/// restore state (clear `agent_waiting`) once, regardless of which arm fired.
enum WaitOutcome {
    /// The human bumped `seq` — the turn was released back to the agent.
    Released,
    /// `--timeout` elapsed before any release.
    TimedOut,
    /// Ctrl-C / SIGINT interrupted the wait.
    Interrupted,
}

/// `agent wait [--timeout S]` — block until the human releases the turn, then
/// print everything they changed in the meantime (PLAN.md §6).
///
/// The flow is serverless, entirely over the store directory: record the current
/// `turn.seq`, hand the turn to the human (`owner = human`, `agent_waiting =
/// true`), and snapshot the annotations. Then block on `notify` until the store
/// shows a higher `seq`. While we're blocked only the human writes, so any
/// difference from the snapshot is precisely their work. On any exit path we
/// clear `agent_waiting` so a stale flag never lingers.
fn wait(timeout: Option<u64>) -> Result<()> {
    let session = session()?;
    let (recorded_seq, snapshot) = begin_wait(&session)?;

    // Run the blocking loop, then always clear our waiting flag before reporting
    // — a timeout or Ctrl-C must not leave "agent is waiting" stuck on.
    let outcome = watch_for_release(&session, recorded_seq, timeout);
    clear_waiting(&session);
    let outcome = outcome?;

    match outcome {
        WaitOutcome::Released => {
            let state = store::load(&session.store_path)?
                .context("the store vanished after the turn was released")?;
            print!("{}", render_changes(&snapshot, &state));
            Ok(())
        }
        WaitOutcome::TimedOut => bail!(
            "timed out after {}s waiting for the human to release the turn",
            timeout.unwrap_or(0)
        ),
        WaitOutcome::Interrupted => bail!("interrupted while waiting for the human"),
    }
}

/// Mark the agent as waiting and capture the pre-wait state: returns the `seq`
/// we must see exceeded, plus a snapshot of the annotations keyed by id. Creates
/// the store if it doesn't exist yet, so `wait` works even before the human has
/// opened the TUI.
fn begin_wait(session: &Session) -> Result<(u64, HashMap<String, Annotation>)> {
    store::update(&session.store_path, &session.target, |s| {
        s.turn.agent_waiting = true;
        s.turn.owner = Author::Human;
        let snapshot = s
            .annotations
            .iter()
            .map(|a| (a.id.clone(), a.clone()))
            .collect();
        (s.turn.seq, snapshot)
    })
}

/// Best-effort clear of `agent_waiting` on the way out. Never fails the command:
/// the wait already happened, and a transient store error here shouldn't mask
/// the outcome the caller is about to report.
fn clear_waiting(session: &Session) {
    let _ = store::update(&session.store_path, &session.target, |s| {
        s.turn.agent_waiting = false;
    });
}

/// Block until the store shows a turn past `recorded_seq`, `--timeout` elapses,
/// or Ctrl-C arrives. Uses `notify` to watch the store directory and a small
/// Tokio runtime to multiplex the three wake sources in one `select!`.
fn watch_for_release(
    session: &Session,
    recorded_seq: u64,
    timeout: Option<u64>,
) -> Result<WaitOutcome> {
    let store_path = session.store_path.clone();
    let dir = store_path
        .parent()
        .context("the store path has no parent directory to watch")?
        .to_path_buf();

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the wait runtime")?;

    runtime.block_on(async move {
        // notify fires this from its own thread on every event in the directory;
        // we don't care which event, only that *something* changed, so we forward
        // a bare tick and re-read the store to decide.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        })
        .context("creating the filesystem watcher")?;
        watcher
            .watch(&dir, RecursiveMode::NonRecursive)
            .with_context(|| format!("watching the store directory {}", dir.display()))?;

        // Close the race between snapshotting `seq` and the watch attaching: if
        // the human released in that window, the event is already lost, so check
        // once up front.
        if turn_released(&store_path, recorded_seq)? {
            return Ok(WaitOutcome::Released);
        }

        // A timeout future that simply never resolves when none was requested.
        let deadline = async {
            match timeout {
                Some(secs) => tokio::time::sleep(Duration::from_secs(secs)).await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::pin!(deadline);

        loop {
            tokio::select! {
                _ = tokio::signal::ctrl_c() => return Ok(WaitOutcome::Interrupted),
                _ = &mut deadline => return Ok(WaitOutcome::TimedOut),
                tick = rx.recv() => {
                    if turn_released(&store_path, recorded_seq)? {
                        return Ok(WaitOutcome::Released);
                    }
                    // `None` means the watcher was dropped without a release in
                    // hand; nothing else will ever wake us, so don't spin.
                    if tick.is_none() {
                        bail!("the filesystem watcher stopped before the turn was released");
                    }
                }
            }
        }
    })
}

/// Whether the store now records a turn past `recorded_seq` — i.e. the human
/// released. A missing store (shouldn't happen after `begin_wait`) reads as "not
/// yet".
fn turn_released(store_path: &Path, recorded_seq: u64) -> Result<bool> {
    Ok(store::load(store_path)?.is_some_and(|s| s.turn.seq > recorded_seq))
}

/// Render what the human changed during their turn, by diffing the released
/// `state` against the pre-wait `snapshot`. Each entry is tagged `[+] new`,
/// `[~] changed`, or `[-] removed` and followed by the annotation as
/// [`render`] would show it in `comment list`, so the agent reads one familiar
/// format throughout.
fn render_changes(snapshot: &HashMap<String, Annotation>, state: &StateFile) -> String {
    let mut out = String::new();
    let mut count = 0;

    // New and changed, in the store's own order so output is stable.
    for a in &state.annotations {
        match snapshot.get(&a.id) {
            None => {
                out.push_str("[+] new\n");
                out.push_str(&render(a));
                count += 1;
            }
            Some(prev) if prev != a => {
                out.push_str("[~] changed\n");
                out.push_str(&render(a));
                count += 1;
            }
            Some(_) => {}
        }
    }

    // Removed: present before, gone now. Sort by id for deterministic output
    // (the snapshot is an unordered map).
    let mut removed: Vec<&Annotation> = snapshot
        .iter()
        .filter(|(id, _)| state.get(id).is_none())
        .map(|(_, a)| a)
        .collect();
    removed.sort_by(|a, b| a.id.cmp(&b.id));
    for a in removed {
        out.push_str("[-] removed\n");
        out.push_str(&render(a));
        count += 1;
    }

    if count == 0 {
        return "(turn released with no changes)\n".to_string();
    }
    out
}

/// Render an annotation as an agent-readable block: a header line with id,
/// author, severity, tag, status, and anchor, then the indented body.
fn render(a: &Annotation) -> String {
    let tag = a
        .tag
        .map(|t| format!(" {}", tag_symbol(t)))
        .unwrap_or_default();
    let side = match a.side {
        Side::Right => "right",
        Side::Left => "left",
    };
    let mut out = format!(
        "[{}] {} {}{} {}  {}:{} ({})\n",
        a.id,
        author_word(a.author),
        severity_word(a.severity),
        tag,
        status_word(a.status),
        a.file,
        a.line,
        side,
    );
    if let Some(parent) = &a.reply_to {
        out.push_str(&format!("  reply to {parent}\n"));
    }
    for line in a.body.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    out
}

fn author_word(a: Author) -> &'static str {
    match a {
        Author::Agent => "agent",
        Author::Human => "human",
    }
}

fn severity_word(s: Severity) -> &'static str {
    match s {
        Severity::Info => "info",
        Severity::Suggestion => "suggestion",
        Severity::Warning => "warning",
        Severity::Blocker => "blocker",
    }
}

fn status_word(s: Status) -> &'static str {
    match s {
        Status::Open => "open",
        Status::Resolved => "resolved",
        Status::Wontfix => "wontfix",
        Status::Withdrawn => "withdrawn",
    }
}

fn tag_symbol(t: Tag) -> &'static str {
    match t {
        Tag::Question => "?",
        Tag::Concern => "!",
        Tag::Direction => ">",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RAW: &str = "\
diff --git a/src/alpha.rs b/src/alpha.rs
index 1..2 100644
--- a/src/alpha.rs
+++ b/src/alpha.rs
@@ -1 +1 @@
-old
+new
diff --git a/README.md b/README.md
index 3..4 100644
--- a/README.md
+++ b/README.md
@@ -1 +1 @@
-# Old
+# New
";

    #[test]
    fn file_section_matches_exact_and_suffix() {
        let exact = file_section(RAW, "src/alpha.rs").unwrap();
        assert!(exact.starts_with("diff --git a/src/alpha.rs"));
        assert!(!exact.contains("README"), "only the one file's section");

        // A bare filename matches by path suffix.
        let suffix = file_section(RAW, "alpha.rs").unwrap();
        assert!(suffix.starts_with("diff --git a/src/alpha.rs"));

        assert!(file_section(RAW, "nope.rs").is_none());
    }

    #[test]
    fn render_includes_header_anchor_and_indented_body() {
        let a = Annotation {
            id: "abc12345".to_string(),
            author: Author::Agent,
            file: "src/lib.rs".to_string(),
            line: 42,
            side: Side::Right,
            severity: Severity::Warning,
            tag: Some(Tag::Concern),
            status: Status::Open,
            body: "line one\nline two".to_string(),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        };
        let text = render(&a);
        assert!(text.contains("[abc12345] agent warning ! open  src/lib.rs:42 (right)"));
        assert!(text.contains("\n  line one\n  line two\n"));
    }

    fn ann(id: &str, author: Author, body: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            author,
            file: "src/lib.rs".to_string(),
            line: 1,
            side: Side::Right,
            severity: Severity::Suggestion,
            tag: None,
            status: Status::Open,
            body: body.to_string(),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        }
    }

    fn state_with(annotations: Vec<Annotation>) -> StateFile {
        let mut s = StateFile::new(crate::domain::Target::Local {
            base: "main".to_string(),
            head_sha: "abc".to_string(),
        });
        s.annotations = annotations;
        s
    }

    #[test]
    fn render_changes_flags_new_changed_and_removed() {
        // Snapshot taken before the human's turn: one agent comment.
        let original = ann("aaaaaaaa", Author::Agent, "agent body");
        let snapshot: HashMap<String, Annotation> = [(original.id.clone(), original.clone())]
            .into_iter()
            .collect();

        // After release: the agent comment was edited, the human added a reply,
        // and a (snapshot-only) comment was removed.
        let mut edited = original.clone();
        edited.status = Status::Resolved;
        edited.body = "agent body (resolved)".to_string();
        let reply = ann("bbbbbbbb", Author::Human, "human reply");

        let mut snapshot = snapshot;
        let removed = ann("cccccccc", Author::Agent, "gone");
        snapshot.insert(removed.id.clone(), removed);

        let state = state_with(vec![edited, reply]);
        let text = render_changes(&snapshot, &state);

        assert!(text.contains("[~] changed\n[aaaaaaaa]"), "edited: {text}");
        assert!(text.contains("agent body (resolved)"));
        assert!(text.contains("[+] new\n[bbbbbbbb]"), "added: {text}");
        assert!(text.contains("human reply"));
        assert!(text.contains("[-] removed\n[cccccccc]"), "removed: {text}");
    }

    #[test]
    fn render_changes_reports_a_quiet_turn() {
        let a = ann("aaaaaaaa", Author::Agent, "unchanged");
        let snapshot: HashMap<String, Annotation> =
            [(a.id.clone(), a.clone())].into_iter().collect();
        let state = state_with(vec![a]);
        assert_eq!(
            render_changes(&snapshot, &state),
            "(turn released with no changes)\n"
        );
    }
}
