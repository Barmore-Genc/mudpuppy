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
mod prompt;
mod render;
#[cfg(test)]
mod tests;

pub(crate) use app::{App, Focus, Row, Sidebar};
use render::{render, target_desc};

use std::collections::HashSet;
use std::path::{Path, PathBuf};

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
    let loaded = source::load(pr.as_deref(), base.as_deref())?;
    let files = parse_diff(&loaded.raw);

    if files.is_empty() {
        // Nothing to render — say so on the normal terminal rather than flashing
        // an empty alternate screen.
        println!("No changes to review ({}).", target_desc(&loaded.target));
        return Ok(());
    }

    // Resolve where this review's annotations live and load any that exist.
    // Resolution failure shouldn't block browsing the diff, so degrade to an
    // empty, store-less view rather than aborting.
    let mut app = App::new(files, loaded.target.clone());
    if let Ok(session) = Session::resolve(loaded.target) {
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
            tokio::sync::mpsc::unbounded_channel::<crate::update::Update>();
        if std::env::var_os(NO_UPDATE_CHECK_ENV).is_none() && engine.update_checks_enabled() {
            let tx = update_tx.clone();
            tokio::task::spawn_blocking(move || {
                if let Ok(Some(update)) = crate::update::check() {
                    let _ = tx.send(update);
                }
            });
        }

        // Initial events: `startup` once, then `file_open` for the file the
        // viewer opens on.
        engine.fire_startup(app)?;
        engine.fire_file_open(app)?;

        loop {
            app.status_msg = engine.status_message();
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
                        // The composer and a pending delete-confirm capture keys
                        // before Lua, the same precedence as the picker overlay
                        // and the hardwired Ctrl-C above. A capture can still
                        // create/remove annotations (composer save, delete), so
                        // diff snapshots around it and fire the change events.
                        // The help overlay captures keys before Lua so it can
                        // be scrolled / dismissed without touching the keymap.
                        // Same precedence as the composer and modal overlays.
                        if app.handle_help_key(key) {
                            continue;
                        }
                        if app.composer.is_some() || app.pending_delete.is_some() {
                            let before = Snapshot::of(app);
                            app.notice = None;
                            let _ = app.handle_composer_key(key)
                                || app.handle_pending_delete_key(key);
                            let after = Snapshot::of(app);
                            fire_changes(&engine, app, &before, &after)?;
                            continue;
                        }
                        // A modal prompt (opened by `mudpuppy.prompt`) captures
                        // keys ahead of the keymap, like the picker and palette.
                        // Choosing an option runs its Lua callback through the
                        // engine's scoped machinery, which can release the turn,
                        // open another prompt, or quit — so snapshot around it.
                        if app.prompt.is_some() {
                            let before = Snapshot::of(app);
                            app.notice = None;
                            if let Some(index) = app.handle_prompt_key(key) {
                                engine.run_prompt(app, index)?;
                                if app.should_quit {
                                    return Ok(());
                                }
                            }
                            let after = Snapshot::of(app);
                            fire_changes(&engine, app, &before, &after)?;
                            continue;
                        }
                        if app.handle_picker_key(key) {
                            continue;
                        }
                        // The command palette captures keys before the engine,
                        // like the picker. Enter chooses a command, which runs
                        // through the same scoped machinery as a key binding, so
                        // diff snapshots around it and fire any change events.
                        if app.palette.is_some() {
                            let before = Snapshot::of(app);
                            app.notice = None;
                            if let Some(name) = app.handle_palette_key(key) {
                                engine.run_command(app, &name)?;
                                if app.should_quit {
                                    return Ok(());
                                }
                            }
                            let after = Snapshot::of(app);
                            fire_changes(&engine, app, &before, &after)?;
                            continue;
                        }
                        if let Some(chord) = KeyChord::from_event(&key) {
                            let before = Snapshot::of(app);
                            // Clear the prior transient hint on each fresh key so
                            // a "can't comment here" notice doesn't linger.
                            app.notice = None;
                            engine.dispatch(app, chord)?;
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
                    engine.fire_reload(app)?;
                    let after = Snapshot::of(app);
                    fire_changes(&engine, app, &before, &after)?;
                }
                // A config (or dev `core.luau`) edit landed: re-exec the keymap.
                // Errors are non-fatal and surface in the status bar.
                Some(()) = cfg_rx.recv() => {
                    let _ = engine.reload_config();
                }
                // The launch-time update check found a newer release. Fire
                // `update_check` so `core.luau` can prompt; its callbacks can
                // release the turn or quit, so snapshot around it.
                Some(update) = update_rx.recv() => {
                    let before = Snapshot::of(app);
                    engine.fire_update_check(app, &update)?;
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
