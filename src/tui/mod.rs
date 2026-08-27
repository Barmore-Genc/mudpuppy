//! The ratatui diff viewer (PLAN.md §9), milestone 1: a read-only, keyboard-
//! driven browser over the parsed diff. File tree on the left, diff pane in the
//! center, status bar along the bottom, and a `?` help overlay.
//!
//! Rendering is **virtualized**: only the rows currently in the viewport are
//! turned into styled spans, and a file's hunks are parsed lazily the first
//! time it is opened (and cached), so a 50k-line diff never gets materialized in
//! full.
//!
//! As of milestone 2 the viewer also **reads** the annotation store: it draws
//! severity-coloured gutter markers on annotated lines, lists annotations in a
//! toggleable side panel, and live-reloads when the store changes on disk so an
//! agent's comments appear while the TUI is open. That reload rides the same
//! `notify` coordination bus `agent wait` uses (PLAN.md §9): the event loop
//! watches the store directory and refreshes in place when a write lands.
//!
//! Milestone 3 adds the user's half of the turn protocol (PLAN.md §6): when an
//! agent is blocked in `agent wait`, the store's `turn.agent_waiting` flag is
//! set and the status bar surfaces it; pressing `r` **releases the turn** —
//! bumping `turn.seq`, handing ownership back to the agent, and (on first
//! contact) recording approval. That store write is what wakes the waiting
//! agent. On an agent's **first contact** — before the user has approved — a
//! top banner asks the user to approve, and that first `r` release doubles as
//! approval (the same write `agent wait` gates on).
//!
//! The diff pane is **syntax-highlighted** via [`crate::highlight`] (syntect):
//! each opened file's hunks are coloured in place, under the gutter and
//! annotation overlays. Authoring annotations from inside the TUI is still to
//! come.
//!
//! This module is the event-loop shell: [`launch`] resolves the diff source and
//! store and enters `run_loop`, which multiplexes terminal input, store
//! reloads, and config hot-reloads. The viewer's state and verbs live in
//! `app`, its drawing in `render`.

mod annotate;
mod app;
mod composer;
pub mod debug;
mod interleave;
mod markdown;
mod palette;
mod prompt;
mod render;
#[cfg(test)]
mod tests;

pub(crate) use app::{App, Focus, Row, Sidebar};
use render::{render, target_desc};

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use anyhow::{Context, Result};
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::crossterm::event::{
    DisableMouseCapture, EnableMouseCapture, Event, EventStream, KeyCode, KeyEvent, KeyEventKind,
    KeyModifiers,
};
use ratatui::crossterm::execute;
use ratatui::DefaultTerminal;
use tokio::sync::mpsc::UnboundedSender;
use tokio_stream::StreamExt;

use crate::diff::parse_diff;
use crate::domain::Author;
use crate::lua::keys::KeyChord;
use crate::lua::{self, LuaEngine};
use crate::session::Session;
use crate::{source, store};

/// Environment variable that, when set, disables the launch-time update check.
/// The e2e harness sets it so tests never reach the network; users can set it to
/// opt out without editing their config.
pub const NO_UPDATE_CHECK_ENV: &str = "MUDPUPPY_NO_UPDATE_CHECK";

/// Launch the interactive review UI.
///
/// `pr` selects a pull-request target (`owner/repo#123` or a URL) when present;
/// otherwise the review targets local changes. `base` overrides the inferred
/// base ref for local reviews.
pub fn launch(pr: Option<String>, base: Option<String>) -> Result<()> {
    let explicit = pr.is_some() || base.is_some();

    // One store per repo, shared with the agent. Resolving it needs a git repo;
    // degrade to a store-less view if we're not in one (e.g. a PR browsed outside
    // a checkout). Load what's there so a plain launch can adopt whatever the
    // agent last recorded as under review.
    let session = source::resolve_target(None, None)
        .ok()
        .and_then(|local| Session::resolve(local).ok());
    let stored = session
        .as_ref()
        .and_then(|s| store::load(&s.store_path).ok().flatten());

    // What to review: a target named on the command line wins; otherwise the one
    // the store last recorded; otherwise the local changes.
    let target = if explicit {
        source::resolve_target(pr.as_deref(), base.as_deref())?
    } else if let Some(state) = &stored {
        state.target.clone()
    } else {
        source::resolve_target(None, None)?
    };

    // An explicitly named target becomes the review's truth: record it so the
    // agent's commands (which read the target back from the store) resolve the
    // same diff. A plain launch leaves the stored target untouched.
    if explicit {
        if let Some(session) = &session {
            let recorded = target.clone();
            let _ = store::update(&session.store_path, &target, move |s| s.target = recorded);
        }
    }

    let raw = source::diff_for_target(&target)?;
    let files = parse_diff(&raw);
    crate::log_debug!(
        "tui launch: head={} files={}",
        target.head_sha(),
        files.len()
    );

    if files.is_empty() {
        // Nothing to render — say so on the normal terminal rather than flashing
        // an empty alternate screen.
        println!("No changes to review ({}).", target_desc(&target));
        return Ok(());
    }

    let mut app = App::new(files, target);
    if let Some(session) = session {
        let state = store::load(&session.store_path)?;
        app.set_repo_root(session.repo_root);
        app.attach_store(session.store_path, state);
    }

    // `ratatui::init` enters the alternate screen, turns on raw mode, and
    // installs a panic hook that restores the terminal; `restore` undoes it.
    // Mouse capture is enabled on top so the TUI receives click/scroll/drag
    // events (issue #29); a best-effort disable on shutdown puts the terminal
    // back in its normal mode. A panic during the loop won't undo mouse
    // capture — ratatui's panic hook only restores raw mode + the alt screen —
    // but the alt-screen exit clears the visible state, so the leak is mostly
    // cosmetic.
    let mut terminal = ratatui::init();
    let _ = execute!(std::io::stdout(), EnableMouseCapture);
    let result = run_loop(&mut terminal, &mut app);
    let _ = execute!(std::io::stdout(), DisableMouseCapture);
    ratatui::restore();
    result
}

/// The draw → await-event → handle loop. Returns when the user quits.
///
/// Runs on a small current-thread Tokio runtime — the same async foundation
/// `agent wait` uses (agent.rs) — so the two halves of the `notify` coordination
/// bus share one model. A `tokio::select!` multiplexes the two wake sources with
/// no busy poll: crossterm's async [`EventStream`] for terminal input, and a
/// channel the store-directory watcher ticks on every write. A store tick reloads
/// in place — the live-reload half of the bus that wakes the agent (PLAN.md
/// §6, §9).
fn run_loop(terminal: &mut DefaultTerminal, app: &mut App) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .context("starting the TUI runtime")?;

    // The scripting engine is a sibling of `App` (not owned by it), so it can read
    // `&App` while driving the app's `&mut` verbs through a per-dispatch borrow.
    // A broken `core.luau` is a hard error here; a broken *user* config is not — it
    // is surfaced in the status bar and the last good bindings stay in effect.
    let engine = LuaEngine::new(lua::config_path()).context("starting the scripting engine")?;

    // The store was attached (and relocated) before the engine existed, so it used
    // the default scan windows. Push the now-loaded config's windows in and
    // re-relocate from the store's original captures.
    let (adv, fb) = engine.anchor_windows();
    app.set_anchor_windows(adv, fb);
    app.reload();
    // Apply the config's `filter_files` predicates to the launch file list before
    // the first frame, so hidden files never flash on screen.
    engine.apply_file_filters(app);

    runtime.block_on(async move {
        // Store-change ticks: the watcher fires these from notify's own thread;
        // the loop only cares that *something* changed and re-reads to decide.
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<()>();

        // Watch the store directory if one was resolved. The watcher must outlive
        // the loop, so it's bound here; dropping it would stop delivery. Live
        // reload is best-effort: if the watch can't be set up we still browse the
        // diff, just without picking up the agent's writes (mirrors the store-less
        // degrade in `launch`).
        let _watcher = app
            .store_path
            .as_deref()
            .and_then(Path::parent)
            .and_then(|dir| watch_store_dir(dir, tx).ok());

        // Config hot-reload ticks on the same pattern: watch the config file's
        // directory (and, in dev builds, the on-disk `core.luau`) and re-exec the
        // keymap when an edit lands, so a rebind takes effect without a restart.
        let (cfg_tx, mut cfg_rx) = tokio::sync::mpsc::unbounded_channel::<()>();
        let _config_watcher = watch_config(cfg_tx);

        let mut events = EventStream::new();

        // One-shot, launch-time update check. Reviews are short-lived and opened
        // afresh, so a single check per launch is plenty — no timer (and never
        // more than once a session). The blocking HTTP fetch runs on a
        // `spawn_blocking` thread so it never stalls the loop; its result (a newer
        // version tag, if any) arrives on `update_rx`, and the loop turns that into
        // an `update_check` event so `core.luau` can prompt. Skipped entirely when
        // `MUDPUPPY_NO_UPDATE_CHECK` is set (the e2e harness sets it) or the user
        // disabled checks in their config.
        let (update_tx, mut update_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::update::ReleaseInfo>();
        if std::env::var_os(NO_UPDATE_CHECK_ENV).is_none() && engine.update_checks_enabled() {
            let tx = update_tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(Some(release)) = crate::update::check() {
                    let _ = tx.send(release);
                }
            });
        }

        // Background syntax highlighting. The structure (plain rows) renders
        // immediately; each file switch / context expansion queues a job that a
        // `spawn_blocking` worker runs off the UI thread (so a 20k-line file
        // never blocks input on `parse_line`). Results arrive on `hl_rx` and the
        // next draw shows the colours. `cancel` carries the latest wanted
        // generation: a newer structure overwrites it, and the in-flight job
        // bails when it sees the mismatch.
        let (hl_tx, mut hl_rx) =
            tokio::sync::mpsc::unbounded_channel::<crate::highlight::HighlightResult>();
        let cancel = Arc::new(AtomicU64::new(0));

        // Initial events: `startup` once, then `file_open` for the file the
        // viewer opens on.
        engine.fire_startup(app)?;
        engine.fire_file_open(app)?;

        // Launch-time skill-staleness check. Unlike the release check this is local
        // file IO (read a few `SKILL.md` stamps), so it runs synchronously here —
        // no thread, no network. Fires only when a stale install exists and the
        // user hasn't skipped this version, so `core.luau` can offer a refresh.
        if crate::install::should_prompt_skill_refresh() {
            let before = Snapshot::of(app);
            engine.fire_skill_update_check(
                app,
                crate::install::SKILL_VERSION,
                crate::install::SKILL_UPDATE_MESSAGE,
            )?;
            if app.should_quit {
                return Ok(());
            }
            let after = Snapshot::of(app);
            fire_changes(&engine, app, &before, &after)?;
        }

        loop {
            app.status_msg = engine.status_message();
            // Hand any queued highlight job to a blocking worker. The structure
            // is already on screen; the colour fills land on `hl_rx`. Storing the
            // job's generation into `cancel` supersedes any older in-flight job.
            if let Some(req) = app.take_highlight_request() {
                cancel.store(req.generation, Ordering::Relaxed);
                let tx = hl_tx.clone();
                let cancel = Arc::clone(&cancel);
                tokio::task::spawn_blocking(move || {
                    let _ = tx.send(crate::highlight::run_request(&req, &cancel));
                });
            }
            terminal.draw(|frame| render(frame, app))?;
            tokio::select! {
                // Terminal input. `EventStream::next` is cancel-safe, so the
                // dropped future on a store tick loses nothing.
                maybe_event = events.next() => match maybe_event {
                    // Only act on key *presses*; on Windows crossterm also emits
                    // release and repeat events that would double every keystroke.
                    Some(Ok(Event::Key(key))) if key.kind == KeyEventKind::Press => {
                        // Hardwired Ctrl-C safety net: checked before the engine and
                        // independent of any Lua state, so a broken config can never
                        // trap the user.
                        if is_ctrl_c(&key) {
                            return Ok(());
                        }
                        // Everything else — the modal overlays included — goes
                        // through the keymap. `active_mode` picks the overlay's
                        // mode while it is open, so every key an overlay responds
                        // to is rebindable; a key no binding claims falls back to
                        // the overlay's Rust handler (typing, the composer's vim
                        // engine, cancelling a delete confirmation).
                        //
                        // A binding can create or remove annotations (composer
                        // save, delete) and can stage a palette command or a
                        // prompt choice, whose callbacks must run outside the
                        // borrow the binding held — hence the drain below.
                        if let Some(chord) = KeyChord::from_event(&key) {
                            let before = Snapshot::of(app);
                            // Clear the prior transient hint on each fresh key so
                            // a "can't comment here" notice doesn't linger.
                            app.notice = None;
                            if !engine.dispatch(app, chord)? {
                                app.overlay_fallback_key(key);
                            }
                            app.refresh_composer_view();
                            if let Some(name) = app.take_pending_command() {
                                engine.run_command(app, &name)?;
                            }
                            if let Some(index) = app.take_pending_prompt() {
                                engine.run_prompt(app, index)?;
                            }
                            if app.should_quit {
                                return Ok(());
                            }
                            let after = Snapshot::of(app);
                            fire_changes(&engine, app, &before, &after)?;
                        }
                    }
                    // Mouse input — scroll, click-to-focus/select, drag for
                    // visual mode, double-click in the diff to comment (issue
                    // #29). Routed through `App::handle_mouse_event`, which
                    // touches the same fields a key dispatch would, so we wrap
                    // it in the same `Snapshot`/`fire_changes` dance to keep
                    // `file_open`/`turn_change` events firing.
                    Some(Ok(Event::Mouse(me))) => {
                        let before = Snapshot::of(app);
                        app.notice = None;
                        let changed = app.handle_mouse_event(me);
                        // A click on a `:command` palette row routes the
                        // chosen command back through the engine, mirroring
                        // what `handle_palette_key` returns on Enter.
                        if let Some(name) = app.take_pending_command() {
                            engine.run_command(app, &name)?;
                            if app.should_quit {
                                return Ok(());
                            }
                        }
                        if changed {
                            let after = Snapshot::of(app);
                            fire_changes(&engine, app, &before, &after)?;
                        }
                    }
                    // Non-press key events and resize/other events just redraw.
                    Some(Ok(_)) => {}
                    Some(Err(e)) => return Err(e).context("reading terminal input"),
                    // The input stream ended (terminal closed): nothing to wait on.
                    None => return Ok(()),
                },
                // A store write landed (the agent's comments, or our own release).
                // `None` means the watcher was dropped/never attached; keep going
                // on input alone rather than spinning, since `recv` then stays
                // pending.
                Some(()) = rx.recv() => {
                    let before = Snapshot::of(app);
                    app.reload();
                    // A reload re-derives the file list (synthetic files may
                    // reappear), so re-hide anything the filters reject.
                    engine.apply_file_filters(app);
                    engine.fire_reload(app)?;
                    let after = Snapshot::of(app);
                    fire_changes(&engine, app, &before, &after)?;
                }
                // A config (or dev `core.luau`) edit landed: re-exec the keymap.
                // Errors are non-fatal and surface in the status bar.
                Some(()) = cfg_rx.recv() => {
                    let _ = engine.reload_config();
                    // The config may have changed the scan windows; pick them up
                    // and re-relocate from the store's original captures.
                    let (adv, fb) = engine.anchor_windows();
                    app.set_anchor_windows(adv, fb);
                    app.reload();
                    // Pick up a newly-added (or changed) `filter_files` predicate.
                    // Note: a removed filter can't restore an already-hidden file
                    // without a restart, since the diff file list isn't retained.
                    engine.apply_file_filters(app);
                }
                // A background highlight job finished. Apply its colour fills
                // (a stale result, superseded by a file switch or expansion, is
                // dropped by the generation check) and let the next draw show them.
                Some(result) = hl_rx.recv() => {
                    app.apply_highlights(result);
                }
                // The launch-time update check found a newer release. Fire
                // `update_check` so `core.luau` can prompt; its callbacks can
                // release the turn or quit, so snapshot around it.
                Some(release) = update_rx.recv() => {
                    let before = Snapshot::of(app);
                    engine.fire_update_check(app, &release.version, release.changelog.as_deref())?;
                    if app.should_quit {
                        return Ok(());
                    }
                    let after = Snapshot::of(app);
                    fire_changes(&engine, app, &before, &after)?;
                }
            }
        }
    })
}

/// Whether a key press is the hardwired Ctrl-C quit.
fn is_ctrl_c(key: &KeyEvent) -> bool {
    key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL)
}

/// A snapshot of the state the event hooks key off, taken before and after a
/// mutating step so [`fire_changes`] can diff it.
struct Snapshot {
    selected: usize,
    ann_ids: HashSet<String>,
    turn_seq: u64,
    turn_owner: Author,
}

impl Snapshot {
    fn of(app: &App) -> Snapshot {
        Snapshot {
            selected: app.selected,
            ann_ids: app.annotations.iter().map(|a| a.id.clone()).collect(),
            turn_seq: app.turn.seq,
            turn_owner: app.turn.owner,
        }
    }
}

/// Fire the events implied by the difference between two snapshots: a changed
/// selection fires `file_open`, each newly-seen annotation fires
/// `annotation_added`, and a changed turn `seq`/`owner` fires `turn_change`.
/// Each event runs in its own scope inside the engine, so a handler can call
/// action verbs without colliding with a held borrow.
fn fire_changes(
    engine: &LuaEngine,
    app: &mut App,
    before: &Snapshot,
    after: &Snapshot,
) -> Result<()> {
    if before.selected != after.selected {
        engine.fire_file_open(app)?;
    }
    let new_ids: Vec<String> = after.ann_ids.difference(&before.ann_ids).cloned().collect();
    for id in new_ids {
        if let Some(annotation) = app.annotations.iter().find(|a| a.id == id).cloned() {
            engine.fire_annotation_added(app, &annotation)?;
        }
    }
    if before.turn_seq != after.turn_seq || before.turn_owner != after.turn_owner {
        engine.fire_turn_change(app)?;
    }
    Ok(())
}

/// Watch the user config file's directory (and, in debug builds, the on-disk
/// `core.luau`) for edits, ticking `tx` on any change. Best-effort: returns an
/// empty `Vec` of watchers if nothing can be watched, in which case hot-reload is
/// simply unavailable and the loaded bindings stay in effect.
fn watch_config(tx: UnboundedSender<()>) -> Vec<RecommendedWatcher> {
    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(parent) = lua::config_path().as_deref().and_then(Path::parent) {
        dirs.push(parent.to_path_buf());
    }
    #[cfg(debug_assertions)]
    {
        let core = Path::new(env!("CARGO_MANIFEST_DIR")).join("src/lua");
        dirs.push(core);
    }

    let mut watchers = Vec::new();
    for dir in dirs {
        let tx = tx.clone();
        let watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
            if res.is_ok() {
                let _ = tx.send(());
            }
        });
        if let Ok(mut w) = watcher {
            if w.watch(&dir, RecursiveMode::NonRecursive).is_ok() {
                watchers.push(w);
            }
        }
    }
    watchers
}

/// Start watching `dir` for changes, forwarding a `()` tick on every filesystem
/// event. The returned watcher must be kept alive for as long as ticks are
/// wanted. Mirrors `agent wait`'s watch (non-recursive on the store directory,
/// since atomic writes land as a temp file + rename within it).
///
/// The store directory may not exist yet when the user opens the TUI before any
/// annotation is written, so it's created first — both so the watch can attach
/// and so it's already in place to catch the agent's very first write.
fn watch_store_dir(dir: &Path, tx: UnboundedSender<()>) -> Result<RecommendedWatcher> {
    std::fs::create_dir_all(dir)
        .with_context(|| format!("creating the store directory {} to watch", dir.display()))?;
    let mut watcher = notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        // We don't care which event fired, only that something changed; the loop
        // re-reads the store to decide. Drop the tick if the receiver is gone.
        if res.is_ok() {
            let _ = tx.send(());
        }
    })?;
    watcher.watch(dir, RecursiveMode::NonRecursive)?;
    Ok(watcher)
}
