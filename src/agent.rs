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
//! changes (the `notify` coordination bus) until the user releases the turn,
//! then prints what they changed (PLAN.md §6). For this milestone the agent
//! targets **local** changes (the common case); attaching to a live TUI's PR
//! target arrives later.

use std::collections::HashMap;
use std::io::Read;
use std::path::Path;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use jiff::Timestamp;
use notify::{RecursiveMode, Watcher};

use crate::cli::{AddArgs, AgentCommand, CommentCommand, ContextArgs};
use crate::domain::{AnchorScope, Annotation, Author, Severity, Side, StateFile, Status, Tag};
use crate::session::Session;
use crate::{source, store};

/// Route an `agent` subcommand to its handler.
pub fn dispatch(command: AgentCommand) -> Result<()> {
    match command {
        AgentCommand::Diff { file } => diff(file.as_deref()),
        AgentCommand::Comment { command } => comment(command),
        AgentCommand::Wait { timeout, context } => wait(timeout, context),
        AgentCommand::Reset { base, pr } => reset(base, pr),
    }
}

/// Resolve the agent's session: the store path (always the repo's local store, so
/// the agent and the user's `mudpuppy` TUI share one file) plus what's under
/// review. The target is whatever a prior `reset` recorded in that store — a base
/// ref or a PR — falling back to the local changes when nothing's been recorded.
/// Reading it back here is what makes every agent command (diff, anchoring, …)
/// follow the same diff the user sees instead of re-resolving the local default.
fn session() -> Result<Session> {
    let local = source::resolve_target(None, None)?;
    let mut session = Session::resolve(local)?;
    if let Some(state) = store::load(&session.store_path)? {
        session.target = state.target;
    }
    Ok(session)
}

/// `agent diff [--file F]` — print the unified diff under review. Resolves the
/// target from the session store so it shows whatever `reset` pointed the review
/// at (a base ref or a PR), matching what the user sees.
fn diff(file: Option<&str>) -> Result<()> {
    let session = session()?;
    let raw = source::diff_for_target(&session.target)?;
    match file {
        None => print!("{raw}"),
        Some(f) => match file_section(&raw, f) {
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
        CommentCommand::List {
            open,
            author,
            file,
            context,
        } => list(open, author, file, context),
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

/// Resolve the comment body from the mutually-exclusive `--body`/`--body-file`
/// sources. A `-` for either reads stdin to end — the heredoc form Claude Code
/// can run without approval-gating ANSI-C (`$'…\n…'`) quoting. Exactly one
/// source is required; zero or both is an error. A single trailing newline is
/// trimmed so a heredoc's terminating newline doesn't bloat the stored body.
fn resolve_body(body: Option<&str>, body_file: Option<&str>) -> Result<String> {
    let raw = match (body, body_file) {
        (Some(_), Some(_)) => {
            bail!("pass exactly one of --body or --body-file, not both")
        }
        (None, None) => bail!("a comment body is required: pass --body or --body-file"),
        (Some("-"), None) | (None, Some("-")) => read_stdin()?,
        (Some(inline), None) => inline.to_string(),
        (None, Some(path)) => std::fs::read_to_string(path)
            .with_context(|| format!("reading the comment body from {path}"))?,
    };
    Ok(trim_trailing_newline(raw))
}

/// Read stdin to end as UTF-8 for a `--body -`/`--body-file -` body.
fn read_stdin() -> Result<String> {
    let mut buf = String::new();
    std::io::stdin()
        .read_to_string(&mut buf)
        .context("reading the comment body from stdin")?;
    Ok(buf)
}

/// Drop a single trailing `\n` (and the `\r` of a `\r\n`), so a heredoc or
/// editor file doesn't leave a dangling blank line on the stored body.
fn trim_trailing_newline(mut s: String) -> String {
    if s.ends_with('\n') {
        s.pop();
        if s.ends_with('\r') {
            s.pop();
        }
    }
    s
}

/// Where an annotation attaches — the fields a reply inherits from its parent.
struct Anchor {
    file: String,
    line: u32,
    end_line: Option<u32>,
    side: Side,
    scope: AnchorScope,
    signature: Option<crate::anchor::AnchorSig>,
}

/// `agent comment add` — create an annotation authored by the agent.
fn add(args: AddArgs) -> Result<()> {
    // Parse the enum-valued flags up front so a typo fails before we touch disk.
    let side: Side = args.side.parse()?;
    let severity: Severity = args.severity.parse()?;
    let tag = args.tag.as_deref().map(str::parse::<Tag>).transpose()?;
    let body = resolve_body(args.body.as_deref(), args.body_file.as_deref())?;

    let session = session()?;
    let now = Timestamp::now();
    let scope = if args.whole_file {
        AnchorScope::File
    } else {
        AnchorScope::Line
    };
    // A reply takes its anchor from the parent inside the store lock, so nothing
    // to capture up front for one.
    let anchor = match args.reply_to {
        Some(_) => None,
        None => {
            let file = args
                .file
                .clone()
                .context("--file is required (unless --reply-to is given)")?;
            let line = args
                .line
                .context("--line is required (unless --reply-to is given)")?;
            // Capture a relocation signature for line-scoped notes so the viewer
            // can follow the line if the file is edited before review.
            // File-scoped notes have no line to anchor.
            let signature = (scope == AnchorScope::Line)
                .then(|| {
                    crate::blob::capture_signature(
                        &session.target,
                        &session.repo_root,
                        &file,
                        line,
                        side,
                    )
                })
                .flatten();
            Some(Anchor {
                file,
                line,
                end_line: args.end_line,
                side,
                scope,
                signature,
            })
        }
    };

    let id = store::update(
        &session.store_path,
        &session.target,
        |s| -> Result<String> {
            // A reply is part of its parent's thread, so it inherits the parent's
            // anchor wholesale rather than carrying one of its own, the same rule
            // the viewer applies when the user replies.
            let anchor = match &args.reply_to {
                Some(parent) => {
                    let p = s.get(parent).with_context(|| {
                        format!("reply target `{parent}` not found in the store")
                    })?;
                    Anchor {
                        file: p.file.clone(),
                        line: p.line,
                        end_line: p.end_line,
                        side: p.side,
                        scope: p.scope,
                        signature: p.signature.clone(),
                    }
                }
                None => anchor.expect("a non-reply captured its anchor above"),
            };
            let annotation = Annotation {
                id: Annotation::new_id(),
                author: Author::Agent,
                file: anchor.file,
                line: anchor.line,
                end_line: anchor.end_line,
                side: anchor.side,
                scope: anchor.scope,
                signature: anchor.signature,
                severity,
                tag,
                status: Status::Open,
                body,
                reply_to: args.reply_to,
                created_at: now,
                updated_at: now,
            };
            let id = annotation.id.clone();
            s.upsert(annotation);
            Ok(id)
        },
    )??;

    println!("{id}");
    Ok(())
}

/// `agent comment list [--open] [--author …] [--file F]` — read current state.
fn list(
    open: bool,
    author: Option<String>,
    file: Option<String>,
    context: ContextArgs,
) -> Result<()> {
    let author = author.as_deref().map(str::parse::<Author>).transpose()?;
    let width = CodeWidth::from_args(&context);
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
        let code = annotated_code(&session, a, width);
        print!("{}", render(a, code.as_deref()));
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
            bail!("`{id}` is the user's annotation; the agent can only edit its own");
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
/// noise vanishes); soft-retracts it to `withdrawn` when the user already
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
                    bail!("`{id}` is the user's annotation; the agent can only cancel its own")
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

/// `agent reset [--base REF | --pr REF]` — clear the session's annotations and
/// start a fresh round, optionally recording what's under review.
///
/// This is how the agent tells mudpuppy what it's reviewing: with `--base` the
/// review is the local changes against `REF`; with `--pr` it's that pull request
/// (diff from `gh pr diff`). The chosen target is written into the session store,
/// which `session` reads back, so the agent's later commands and an open TUI both
/// resolve the same diff. The store path is unchanged (always the repo's local
/// store) — only the recorded target changes — so the TUI keeps watching the same
/// file and reloads onto the new diff. Without either flag, the current target is
/// kept and only the round is cleared. `clear` also rotates the store's
/// `log_seed`, so debug-log hashes from the previous round don't correlate.
fn reset(base: Option<String>, pr: Option<String>) -> Result<()> {
    // The store path stays the local one (shared with the TUI and the agent's
    // other commands); only the recorded target changes. So resolve the session
    // for the path and seed, then resolve the new target — if any — separately.
    let session = session()?;
    let new_target = match (&base, &pr) {
        (Some(b), _) => {
            crate::log_debug!("agent reset: switching base to {}", crate::logging::hash(b));
            Some(source::resolve_target(None, Some(b))?)
        }
        (_, Some(p)) => {
            crate::log_debug!("agent reset: switching to PR {}", crate::logging::hash(p));
            Some(source::resolve_target(Some(p), None)?)
        }
        (None, None) => None,
    };
    let cleared = store::update(&session.store_path, &session.target, |s| {
        let n = s.clear();
        if let Some(target) = &new_target {
            s.target = target.clone();
        }
        n
    })?;
    match (base, pr) {
        (Some(_), _) => {
            println!("reset: cleared {cleared} annotation(s) and switched the review base")
        }
        (_, Some(_)) => {
            println!("reset: cleared {cleared} annotation(s) and switched the review to the PR")
        }
        (None, None) => println!("reset: cleared {cleared} annotation(s)"),
    }
    Ok(())
}

// --- The turn rendezvous (`agent wait`, PLAN.md §6) -------------------------

/// How the block ended, kept separate from the side effects so the caller can
/// restore state (clear `agent_waiting`) once, regardless of which arm fired.
enum WaitOutcome {
    /// The user bumped `seq` — the turn was released back to the agent.
    Released,
    /// `--timeout` elapsed before any release.
    TimedOut,
    /// Ctrl-C / SIGINT interrupted the wait.
    Interrupted,
}

/// `agent wait [--timeout S]` — block until the user releases the turn, then
/// print everything they changed in the meantime (PLAN.md §6).
///
/// The flow is serverless, entirely over the store directory: record the current
/// `turn.seq`, hand the turn to the user (`owner = user`, `agent_waiting =
/// true`), and snapshot the annotations. Then block on `notify` until the store
/// shows a higher `seq` *and* the user has approved (first contact gates here
/// until they opt in; once approved a session stays approved). While we're
/// blocked only the user writes, so any difference from the snapshot is
/// precisely their work. On any exit path we clear `agent_waiting` so a stale
/// flag never lingers.
fn wait(timeout: Option<u64>, context: ContextArgs) -> Result<()> {
    let width = CodeWidth::from_args(&context);
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
            print!(
                "{}",
                render_changes(&snapshot, &state, |a| annotated_code(&session, a, width))
            );
            Ok(())
        }
        WaitOutcome::TimedOut => bail!(
            "timed out after {}s waiting for the user to release the turn",
            timeout.unwrap_or(0)
        ),
        WaitOutcome::Interrupted => bail!("interrupted while waiting for the user"),
    }
}

/// Mark the agent as waiting and capture the pre-wait state: returns the `seq`
/// we must see exceeded, plus a snapshot of the annotations keyed by id. Creates
/// the store if it doesn't exist yet, so `wait` works even before the user has
/// opened the TUI.
fn begin_wait(session: &Session) -> Result<(u64, HashMap<String, Annotation>)> {
    store::update(&session.store_path, &session.target, |s| {
        s.turn.agent_waiting = true;
        s.turn.owner = Author::User;
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
        // the user released in that window, the event is already lost, so check
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

/// Whether the store now records a turn the agent may take: a `seq` past
/// `recorded_seq` (the user released) *and* `approved` set. The approval gate
/// holds the agent at first contact until the user has opted in — the user's
/// first turn-release sets both at once, so an established session (already
/// approved) is unaffected. A missing store (shouldn't happen after
/// `begin_wait`) reads as "not yet".
fn turn_released(store_path: &Path, recorded_seq: u64) -> Result<bool> {
    Ok(store::load(store_path)?.is_some_and(|s| s.turn.approved && s.turn.seq > recorded_seq))
}

/// Render what the user changed during their turn, by diffing the released
/// `state` against the pre-wait `snapshot`. Each entry is tagged `[+] new`,
/// `[~] changed`, or `[-] removed` and followed by the annotation as
/// [`render`] would show it in `comment list`, so the agent reads one familiar
/// format throughout.
fn render_changes(
    snapshot: &HashMap<String, Annotation>,
    state: &StateFile,
    code: impl Fn(&Annotation) -> Option<String>,
) -> String {
    let mut out = String::new();
    let mut count = 0;

    // New and changed, in the store's own order so output is stable.
    for a in &state.annotations {
        match snapshot.get(&a.id) {
            None => {
                out.push_str("[+] new\n");
                out.push_str(&render(a, code(a).as_deref()));
                count += 1;
            }
            Some(prev) if prev != a => {
                out.push_str("[~] changed\n");
                out.push_str(&render(a, code(a).as_deref()));
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
        out.push_str(&render(a, code(a).as_deref()));
        count += 1;
    }

    if count == 0 {
        return "(turn released with no changes)\n".to_string();
    }
    out
}

/// Default lines of surrounding code shown on each side of an annotation when no
/// width flag is given. Matches the anchor signature's context width so the
/// excerpt lines up with what relocation already reasons about.
const DEFAULT_CODE_WIDTH: i64 = 3;

/// How many lines of code to show around an annotation, per side. Resolved from
/// the CLI's [`ContextArgs`]: `--context` sets both sides, `-A`/`-B` override one
/// side each, and a negative request clamps to zero (which suppresses that side).
#[derive(Clone, Copy)]
struct CodeWidth {
    before: usize,
    after: usize,
}

impl CodeWidth {
    fn from_args(args: &ContextArgs) -> Self {
        let base = args.context.unwrap_or(DEFAULT_CODE_WIDTH);
        let clamp = |n: i64| n.max(0) as usize;
        CodeWidth {
            before: clamp(args.before.unwrap_or(base)),
            after: clamp(args.after.unwrap_or(base)),
        }
    }

    /// Nothing to show on either side — suppress the excerpt entirely.
    fn is_off(&self) -> bool {
        self.before == 0 && self.after == 0
    }
}

/// Build the `annotated code` excerpt for `a`: the current source around the
/// annotated line(s), so the agent can locate the code the user is pointing at.
/// Returns `None` when the excerpt is off, the note is whole-file, or the file
/// content / stored line can't be resolved at the target's revision.
fn annotated_code(session: &Session, a: &Annotation, width: CodeWidth) -> Option<String> {
    if width.is_off() || a.scope == AnchorScope::File {
        return None;
    }
    let lines = crate::blob::contents(
        &session.target,
        &session.repo_root,
        &a.file,
        crate::blob::blob_side(a.side),
    )
    .ok()
    .flatten()?;
    excerpt(&lines, a, width)
}

/// Slice the `annotated code` block out of a file's `lines` for `a`, without line
/// numbers: `width.before` lines above the anchor, the annotated line (through
/// `end_line` for a region), then `width.after` lines below. The source is kept
/// verbatim so the agent can match it against the working tree. Returns `None`
/// when the stored line falls outside the file (e.g. it shrank since capture).
fn excerpt(lines: &[String], a: &Annotation, width: CodeWidth) -> Option<String> {
    let total = lines.len();
    if a.line == 0 || a.line as usize > total {
        return None;
    }
    let start = a.line as usize; // 1-based
    let end = a
        .end_line
        .map(|e| e as usize)
        .filter(|&e| e >= start)
        .unwrap_or(start)
        .min(total);

    let from = start.saturating_sub(width.before).max(1);
    let to = (end + width.after).min(total);

    let mut out = String::from("  annotated code:\n");
    for line in &lines[from - 1..to] {
        out.push_str("    ");
        out.push_str(line);
        out.push('\n');
    }
    Some(out)
}

/// Render an annotation as an agent-readable block: a header line with id,
/// author, severity, tag, status, and anchor, then the indented body, then the
/// optional `annotated code` excerpt (already formatted by [`annotated_code`]).
fn render(a: &Annotation, code: Option<&str>) -> String {
    let tag = a
        .tag
        .map(|t| format!(" {}", tag_symbol(t)))
        .unwrap_or_default();
    let mut out = format!(
        "[{}] {} {}{} {}  {}\n",
        a.id,
        author_word(a.author),
        severity_word(a.severity),
        tag,
        status_word(a.status),
        anchor_desc(a),
    );
    if let Some(parent) = &a.reply_to {
        out.push_str(&format!("  reply to {parent}\n"));
    }
    for line in a.body.lines() {
        out.push_str("  ");
        out.push_str(line);
        out.push('\n');
    }
    if let Some(code) = code {
        out.push_str(code);
    }
    out
}

/// The anchor portion of a rendered annotation: `file (whole file)` for a
/// file-scoped note, `file:42–50 (side)` for a region, `file:42 (side)` for a
/// single line.
fn anchor_desc(a: &Annotation) -> String {
    if a.scope == AnchorScope::File {
        return format!("{} (whole file)", a.file);
    }
    let side = match a.side {
        Side::Right => "right",
        Side::Left => "left",
    };
    match a.end_line {
        Some(end) if end != a.line => format!("{}:{}–{} ({side})", a.file, a.line, end),
        _ => format!("{}:{} ({side})", a.file, a.line),
    }
}

fn author_word(a: Author) -> &'static str {
    match a {
        Author::Agent => "agent",
        Author::User => "user",
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

    #[test]
    fn resolve_body_inline_and_file_with_trailing_newline_trim() {
        // Inline body is taken verbatim; a single trailing newline is trimmed.
        assert_eq!(resolve_body(Some("hi"), None).unwrap(), "hi");

        // A file body round-trips its embedded newline but loses one trailing one.
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("body.md");
        std::fs::write(&path, "line one\nline two\n").unwrap();
        assert_eq!(
            resolve_body(None, Some(path.to_str().unwrap())).unwrap(),
            "line one\nline two"
        );
    }

    #[test]
    fn resolve_body_requires_exactly_one_source() {
        assert!(resolve_body(None, None).is_err(), "zero sources");
        assert!(
            resolve_body(Some("x"), Some("y")).is_err(),
            "conflicting sources"
        );
    }

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
            end_line: None,
            side: Side::Right,
            scope: AnchorScope::Line,
            signature: None,
            severity: Severity::Warning,
            tag: Some(Tag::Concern),
            status: Status::Open,
            body: "line one\nline two".to_string(),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        };
        let text = render(&a, None);
        assert!(text.contains("[abc12345] agent warning ! open  src/lib.rs:42 (right)"));
        assert!(text.contains("\n  line one\n  line two\n"));
    }

    #[test]
    fn render_shows_region_and_whole_file_anchors() {
        let base = ann("rgn00001", Author::User, "spans lines");
        let mut region = base.clone();
        region.end_line = Some(50);
        assert!(
            render(&region, None).contains("src/lib.rs:1–50 (right)"),
            "{}",
            render(&region, None)
        );

        let mut whole = base;
        whole.scope = AnchorScope::File;
        assert!(
            render(&whole, None).contains("src/lib.rs (whole file)"),
            "{}",
            render(&whole, None)
        );
    }

    #[test]
    fn code_width_defaults_on_and_clamps_negatives() {
        let args = |after, before, context| ContextArgs {
            after,
            before,
            context,
        };

        // No flags → the default symmetric width, excerpt on.
        let w = CodeWidth::from_args(&args(None, None, None));
        assert_eq!(
            (w.before, w.after),
            (DEFAULT_CODE_WIDTH as usize, DEFAULT_CODE_WIDTH as usize)
        );
        assert!(!w.is_off());

        // `--context 0` (or any negative width) suppresses the excerpt.
        assert!(CodeWidth::from_args(&args(None, None, Some(0))).is_off());
        assert!(CodeWidth::from_args(&args(None, None, Some(-5))).is_off());

        // `-A`/`-B` override one side each, over `--context`'s both-sides value.
        let w = CodeWidth::from_args(&args(Some(1), Some(7), Some(4)));
        assert_eq!((w.before, w.after), (7, 1));

        // An unset side falls back to `--context`, else the default.
        let w = CodeWidth::from_args(&args(Some(2), None, None));
        assert_eq!((w.before, w.after), (DEFAULT_CODE_WIDTH as usize, 2));
    }

    #[test]
    fn excerpt_slices_without_line_numbers_and_clamps_at_edges() {
        let lines: Vec<String> = (1..=6).map(|n| format!("line {n}")).collect();
        let mut a = ann("excerpt0", Author::User, "look here");
        a.line = 3;

        // Two before, the anchor, two after — verbatim source, no line numbers.
        let text = excerpt(
            &lines,
            &a,
            CodeWidth {
                before: 2,
                after: 2,
            },
        )
        .unwrap();
        assert_eq!(
            text,
            "  annotated code:\n    line 1\n    line 2\n    line 3\n    line 4\n    line 5\n"
        );

        // Near the top, the before-context clamps at the first line.
        a.line = 1;
        let text = excerpt(
            &lines,
            &a,
            CodeWidth {
                before: 3,
                after: 1,
            },
        )
        .unwrap();
        assert_eq!(text, "  annotated code:\n    line 1\n    line 2\n");

        // A stored line past the end of the file yields no excerpt.
        a.line = 99;
        assert!(excerpt(
            &lines,
            &a,
            CodeWidth {
                before: 2,
                after: 2
            }
        )
        .is_none());
    }

    #[test]
    fn excerpt_covers_a_region_through_end_line() {
        let lines: Vec<String> = (1..=8).map(|n| format!("line {n}")).collect();
        let mut a = ann("region00", Author::User, "this block");
        a.line = 3;
        a.end_line = Some(5);
        // One before (L2), the region (L3–L5), one after (L6).
        let text = excerpt(
            &lines,
            &a,
            CodeWidth {
                before: 1,
                after: 1,
            },
        )
        .unwrap();
        assert_eq!(
            text,
            "  annotated code:\n    line 2\n    line 3\n    line 4\n    line 5\n    line 6\n"
        );
    }

    fn ann(id: &str, author: Author, body: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            author,
            file: "src/lib.rs".to_string(),
            line: 1,
            end_line: None,
            side: Side::Right,
            scope: AnchorScope::Line,
            signature: None,
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
        // Snapshot taken before the user's turn: one agent comment.
        let original = ann("aaaaaaaa", Author::Agent, "agent body");
        let snapshot: HashMap<String, Annotation> = [(original.id.clone(), original.clone())]
            .into_iter()
            .collect();

        // After release: the agent comment was edited, the user added a reply,
        // and a (snapshot-only) comment was removed.
        let mut edited = original.clone();
        edited.status = Status::Resolved;
        edited.body = "agent body (resolved)".to_string();
        let reply = ann("bbbbbbbb", Author::User, "user reply");

        let mut snapshot = snapshot;
        let removed = ann("cccccccc", Author::Agent, "gone");
        snapshot.insert(removed.id.clone(), removed);

        let state = state_with(vec![edited, reply]);
        let text = render_changes(&snapshot, &state, |_| None);

        assert!(text.contains("[~] changed\n[aaaaaaaa]"), "edited: {text}");
        assert!(text.contains("agent body (resolved)"));
        assert!(text.contains("[+] new\n[bbbbbbbb]"), "added: {text}");
        assert!(text.contains("user reply"));
        assert!(text.contains("[-] removed\n[cccccccc]"), "removed: {text}");
    }

    #[test]
    fn render_changes_reports_a_quiet_turn() {
        let a = ann("aaaaaaaa", Author::Agent, "unchanged");
        let snapshot: HashMap<String, Annotation> =
            [(a.id.clone(), a.clone())].into_iter().collect();
        let state = state_with(vec![a]);
        assert_eq!(
            render_changes(&snapshot, &state, |_| None),
            "(turn released with no changes)\n"
        );
    }

    #[test]
    fn turn_released_gates_first_contact_on_approval() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotations.json");
        let mut s = state_with(vec![]);

        // First contact: the user bumped `seq` but has not approved yet — the
        // agent must stay blocked.
        s.turn.seq = 1;
        s.turn.approved = false;
        store::save(&path, &s).unwrap();
        assert!(
            !turn_released(&path, 0).unwrap(),
            "an unapproved release does not unblock the agent"
        );

        // Approval flips it (the user's first release sets both at once).
        s.turn.approved = true;
        store::save(&path, &s).unwrap();
        assert!(turn_released(&path, 0).unwrap(), "approved + advanced = go");

        // Approved but no advance past the recorded seq: still waiting.
        assert!(
            !turn_released(&path, 1).unwrap(),
            "approval alone, without a fresh release, is not a turn"
        );
    }
}
