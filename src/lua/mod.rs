//! Embedded Luau scripting: the configurable keymap and event hooks.
//!
//! The [`LuaEngine`] owns a sandboxed [`mlua::Lua`] and a few registries: one
//! mapping `(mode, key-sequence)` to a key callback, one mapping an
//! [`EventKind`] to its handlers, and one mapping a `:command` name to its
//! callback. Multi-key bindings, a configurable leader, and a pending count are
//! resolved by the sequence state machine in `dispatch`. Every default binding
//! lives in the embedded `core.luau`; a user
//! config (resolved by [`config_path`]) is loaded on top, last-binding-wins, so a
//! user can rebind or extend without touching Rust. Rust keeps only a hardwired
//! Ctrl-C quit (in `tui::run_loop`) as a safety net so a broken config can never
//! trap the user.
//!
//! The interpreter is a **sandbox**: only the `table`/`string`/`math` libraries
//! are loaded and [`Lua::sandbox`] freezes the globals, so there is no `io`,
//! `os`, `package`, `require`, network, or subprocess. Scripts reach the diff,
//! files, annotations, and what is on screen only through the read-only views in
//! `views`; they mutate the app only through the scoped action verbs in
//! `api`. See `src/lua/AGENTS.md`.

mod api;
pub mod keys;
mod views;

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::rc::Rc;

use anyhow::{anyhow, Result};
use mlua::{Function, Lua, StdLib, Table, Value, Variadic};

use crate::domain::Annotation;
use crate::tui::{App, Focus};
use keys::{Key, KeyChord, KeySeq, Mode};

/// Environment variable that points directly at a config file, overriding the
/// XDG/`$HOME` search.
pub const CONFIG_ENV: &str = "MUDPUPPY_CONFIG";

/// The `(mode, sequence)` → callback registry. A binding is keyed on a *sequence*
/// of chords, so multi-key bindings (`g g`, `<leader> t r`) are first-class; a
/// single-key binding is a length-1 sequence. `Rc<RefCell<…>>` so the `'static`
/// registration closure handed to Lua can mutate it.
type Bindings = Rc<RefCell<HashMap<(Mode, KeySeq), Function>>>;

/// The event → handlers registry, registered via `mudpuppy.on`.
type Events = Rc<RefCell<HashMap<EventKind, Vec<Function>>>>;

/// The command-name → callback registry, registered via `mudpuppy.command` and
/// driven by the `:command` palette.
type Commands = Rc<RefCell<HashMap<String, Function>>>;

/// The file-filter predicates, registered via `mudpuppy.filter_files`. Each runs
/// over every file in the tree; a file is hidden when *any* predicate returns
/// true. `Rc<RefCell<…>>` so the `'static` registration closure can push to it,
/// and cleared on a config reload like the other registries.
type FileFilters = Rc<RefCell<Vec<Function>>>;

/// The configurable leader chord (default `space`), shared so `mudpuppy.leader`
/// can update it and `map` can expand `<leader>` at registration time.
type Leader = Rc<RefCell<KeyChord>>;

/// The callbacks for the currently-open `mudpuppy.prompt`, indexed by option. Set
/// when a prompt opens and cleared when the user chooses or dismisses it; the
/// labels live on `App` (the render side) while these stay here.
type Prompts = Rc<RefCell<Vec<Function>>>;

/// Whether automatic update checks are enabled, shared so `mudpuppy.updates`'s
/// `set_check_enabled`/`disable` can flip it and `check_enabled` can read it.
/// Default `true`; reset to the default on a config reload (like [`Leader`]) so a
/// removed disable line reverts.
type UpdateChecks = Rc<RefCell<bool>>;

/// A relocation scan window (in lines), shared so the `mudpuppy.anchor` setters
/// can change it and the getters can read it. Reset to the default on a config
/// reload (like [`Leader`]).
type AnchorWindow = Rc<RefCell<i64>>;

/// Whether debug logging is on, toggled by `mudpuppy.debug_log = true`, shared so
/// the metatable closure can write it and [`LuaEngine::debug_log`] can read it
/// back. `false` (the default) means logging is off. The config only flips this
/// flag; the binary picks the (fixed, confined) log location itself at startup
/// (see [`crate::cli`]) so a hostile config can't aim writes at an arbitrary path.
type DebugLog = Rc<RefCell<bool>>;

/// The pair of relocation scan windows (see [`crate::anchor::Params`]), bundled
/// so they pass as one argument and reset together on reload.
#[derive(Clone)]
struct AnchorWindows {
    advanced: AnchorWindow,
    fallback: AnchorWindow,
}

impl AnchorWindows {
    /// Both windows at the engine defaults.
    fn defaults() -> AnchorWindows {
        let p = crate::anchor::Params::default();
        AnchorWindows {
            advanced: Rc::new(RefCell::new(p.advanced_match_window)),
            fallback: Rc::new(RefCell::new(p.fallback_match_window)),
        }
    }

    /// Reset both windows to the engine defaults (on config reload).
    fn reset(&self) {
        let p = crate::anchor::Params::default();
        *self.advanced.borrow_mut() = p.advanced_match_window;
        *self.fallback.borrow_mut() = p.fallback_match_window;
    }
}

/// A lifecycle/store event a script can subscribe to with `mudpuppy.on`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EventKind {
    /// Fired once after the first config load.
    Startup,
    /// The selected file changed (and once at startup).
    FileOpen,
    /// The store was reloaded from disk.
    Reload,
    /// A newly-seen annotation appeared (diffed across a reload).
    AnnotationAdded,
    /// The turn's `seq` or `owner` changed.
    TurnChange,
    /// A newer release was found by the launch-time check. Payload:
    /// `{ version, changelog? }`. `core.luau` subscribes to it to prompt the user.
    UpdateCheck,
    /// Installed Claude Code skills are stale (an older version than the binary
    /// ships). Fired at launch when a refresh is worth offering. Payload:
    /// `{ version, message }`. `core.luau` subscribes to it to prompt the user.
    SkillUpdateCheck,
}

impl EventKind {
    fn parse(s: &str) -> Option<EventKind> {
        Some(match s {
            "startup" => EventKind::Startup,
            "file_open" => EventKind::FileOpen,
            "reload" => EventKind::Reload,
            "annotation_added" => EventKind::AnnotationAdded,
            "turn_change" => EventKind::TurnChange,
            "update_check" => EventKind::UpdateCheck,
            "skill_update_check" => EventKind::SkillUpdateCheck,
            _ => return None,
        })
    }
}

/// The scripting engine: a sibling of `App` in the event loop. It holds `&App`
/// (to read) while driving the app's `&mut` verbs through a per-dispatch
/// `RefCell`, so it is deliberately *not* owned by `App`.
pub struct LuaEngine {
    lua: Lua,
    /// The persistent `mudpuppy` table; the scoped action/reader verbs are
    /// installed on it for the duration of each dispatch.
    mudpuppy: Table,
    bindings: Bindings,
    events: Events,
    commands: Commands,
    leader: Leader,
    /// Predicates that hide files from the tree (reset on reload).
    file_filters: FileFilters,
    /// Callbacks for the currently-open prompt, indexed by option.
    prompts: Prompts,
    /// Whether automatic update checks are on (default true, reset on reload).
    update_checks: UpdateChecks,
    /// The configured relocation scan windows (reset on reload).
    anchor_windows: AnchorWindows,
    /// Whether `mudpuppy.debug_log` turned logging on (reset on reload).
    debug_log: DebugLog,
    /// Where the user config lives (if anywhere). `None` disables user config —
    /// used by tests so the default keymap is exercised in isolation.
    config_path: Option<PathBuf>,
    /// The latest script message or config error, surfaced in the status bar
    /// (the alternate screen has no usable stdout).
    status: Rc<RefCell<Option<String>>>,
}

impl LuaEngine {
    /// Build the engine, install the sandbox and the `mudpuppy` table, and load
    /// `core.luau` plus the user config at `config_path` (if any). A failure in
    /// `core.luau` is a hard error (an embed bug); a user-config failure is
    /// surfaced in the status bar and otherwise ignored.
    pub fn new(config_path: Option<PathBuf>) -> Result<LuaEngine> {
        // Minimal safe libraries only: no io/os/package, so a script has no file,
        // process, or environment access. `sandbox(true)` (below) freezes globals.
        let lua = Lua::new_with(
            StdLib::TABLE | StdLib::STRING | StdLib::MATH,
            mlua::LuaOptions::default(),
        )
        .map_err(|e| anyhow!("initializing the Lua sandbox: {e}"))?;

        let bindings: Bindings = Rc::new(RefCell::new(HashMap::new()));
        let events: Events = Rc::new(RefCell::new(HashMap::new()));
        let commands: Commands = Rc::new(RefCell::new(HashMap::new()));
        let file_filters: FileFilters = Rc::new(RefCell::new(Vec::new()));
        // Default leader is `space`; a config can change it with `mudpuppy.leader`.
        let leader: Leader = Rc::new(RefCell::new(KeyChord::plain(Key::Char(' '))));
        let prompts: Prompts = Rc::new(RefCell::new(Vec::new()));
        // Automatic update checks are on unless the user config disables them.
        let update_checks: UpdateChecks = Rc::new(RefCell::new(true));
        // The relocation windows default to the engine defaults; a config can
        // override them with `mudpuppy.anchor.set_{advanced,fallback}_match_window`.
        let anchor_windows = AnchorWindows::defaults();
        // Debug logging is off unless the user config turns it on.
        let debug_log: DebugLog = Rc::new(RefCell::new(false));
        let status: Rc<RefCell<Option<String>>> = Rc::new(RefCell::new(None));

        // Redirect `print` to the status buffer — the TUI owns the screen, so a
        // bare stdout write would corrupt the alternate screen.
        let status_for_print = status.clone();
        let print = lua
            .create_function(move |_, args: Variadic<String>| {
                *status_for_print.borrow_mut() = Some(args.join("\t"));
                Ok(())
            })
            .map_err(|e| anyhow!("installing print: {e}"))?;
        lua.globals()
            .set("print", print)
            .map_err(|e| anyhow!("binding print: {e}"))?;

        let mudpuppy = api::build_table(
            &lua,
            bindings.clone(),
            events.clone(),
            commands.clone(),
            leader.clone(),
            file_filters.clone(),
            update_checks.clone(),
            anchor_windows.clone(),
            debug_log.clone(),
        )
        .map_err(|e| anyhow!("building the mudpuppy table: {e}"))?;
        lua.globals()
            .set("mudpuppy", mudpuppy.clone())
            .map_err(|e| anyhow!("binding the mudpuppy table: {e}"))?;

        // Freeze globals against a script reassigning them, then re-open just the
        // `mudpuppy` table so we can install the per-dispatch scoped verbs on it.
        lua.sandbox(true)
            .map_err(|e| anyhow!("enabling the sandbox: {e}"))?;
        mudpuppy.set_readonly(false);
        // `sandbox(true)` also enables Luau's `safeenv`, which caches global
        // lookups (treating `mudpuppy.quit` as a load-time constant). That would
        // pin a binding to a *destructed* scoped verb from an earlier dispatch, so
        // turn it back off: each call then does a live lookup and finds the verb
        // currently installed. Read-only globals (the actual hardening) stay on.
        lua.globals().set_safeenv(false);

        let engine = LuaEngine {
            lua,
            mudpuppy,
            bindings,
            events,
            commands,
            leader,
            file_filters,
            prompts,
            update_checks,
            anchor_windows,
            debug_log,
            config_path,
            status,
        };
        engine.load_scripts(true)?;
        Ok(engine)
    }

    /// Load `core.luau` (hard error on failure) then the user config (soft error,
    /// surfaced in the status bar). `core.luau` failing means the embedded default
    /// keymap is broken — a build bug, not a user one.
    fn load_scripts(&self, core_hard: bool) -> Result<()> {
        let core = self.lua.load(&*core_source()).set_name("core.luau").exec();
        if let Err(e) = core {
            if core_hard {
                return Err(anyhow!("loading the embedded core.luau keymap: {e}"));
            }
            self.set_status(format!("core.luau reload error: {e}"));
        }

        if let Some(path) = &self.config_path {
            match std::fs::read_to_string(path) {
                Ok(src) => {
                    if let Err(e) = self
                        .lua
                        .load(&src)
                        .set_name(path.to_string_lossy().as_ref())
                        .exec()
                    {
                        // Keep the last good bindings; just report the failure.
                        self.set_status(format!("config error: {e}"));
                    }
                }
                // No user config is the common case, not an error.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                Err(e) => self.set_status(format!("config unreadable: {e}")),
            }
        }
        Ok(())
    }

    /// Re-run the keymap scripts after a config (or dev `core.luau`) edit: drop the
    /// old registries, freeing the stored callbacks, and re-exec from scratch so
    /// removed bindings actually disappear. A reload-time `core.luau` error is
    /// soft (we keep running on whatever loaded) rather than killing a live TUI.
    pub fn reload_config(&self) -> Result<()> {
        self.bindings.borrow_mut().clear();
        self.events.borrow_mut().clear();
        self.commands.borrow_mut().clear();
        self.file_filters.borrow_mut().clear();
        // The leader is config-set too, so reset it before re-exec so a removed
        // `mudpuppy.leader` call reverts to the default.
        *self.leader.borrow_mut() = KeyChord::plain(Key::Char(' '));
        // Same for the update-check flag: default on, then let the config disable
        // it again if its skip line is still present.
        *self.update_checks.borrow_mut() = true;
        // And the relocation windows: back to the engine defaults, then let the
        // config set them again.
        self.anchor_windows.reset();
        // Turn debug logging back off so a removed `mudpuppy.debug_log` reverts.
        // (The process's already-open log file is unaffected; this only governs a
        // fresh read of the setting.)
        *self.debug_log.borrow_mut() = false;
        self.prompts.borrow_mut().clear();
        *self.status.borrow_mut() = None;
        self.load_scripts(false)
    }

    /// Whether the config turned debug logging on via `mudpuppy.debug_log = true`.
    /// The binary reads this once at startup; when on it opens its role-split log
    /// file at a fixed location it chooses (the config never names the path).
    pub fn debug_log(&self) -> bool {
        *self.debug_log.borrow()
    }

    /// The currently-configured relocation scan windows as `(advanced, fallback)`,
    /// so the loop can push them into `App` for its on-reload relocation.
    pub fn anchor_windows(&self) -> (i64, i64) {
        (
            *self.anchor_windows.advanced.borrow(),
            *self.anchor_windows.fallback.borrow(),
        )
    }

    /// Run the registered `filter_files` predicates over `app`'s canonical file
    /// list and record every file at least one predicate rejects as hidden. Each
    /// predicate receives a `{ path, status, additions, deletions, binary }` table
    /// (the same shape as `mudpuppy.files()`) and returns true to hide that file.
    /// A faulty predicate is surfaced in the status bar and skipped, never
    /// propagated — a bad filter must not crash the viewer. With no filter
    /// registered the hidden set is cleared (so removing a filter from the config
    /// restores its files). The projection (and never emptying the list) is
    /// [`App::set_hidden_files`].
    pub(crate) fn apply_file_filters(&self, app: &mut App) {
        let filters = self.file_filters.borrow();
        // Run over the full list, not just the visible files, so a file already
        // hidden stays evaluated rather than silently reappearing.
        let mut hidden: HashSet<String> = HashSet::new();
        for file in app.all_files() {
            let table = match views::file_summary(&self.lua, file) {
                Ok(t) => t,
                Err(e) => {
                    self.set_status(format!("file filter error: {e}"));
                    continue;
                }
            };
            for filter in filters.iter() {
                match filter.call::<Value>(table.clone()) {
                    // Lua truthiness: anything but nil/false hides the file.
                    Ok(v) if !matches!(v, Value::Nil | Value::Boolean(false)) => {
                        hidden.insert(file.display_path().to_string());
                        break;
                    }
                    Ok(_) => {}
                    Err(e) => self.set_status(format!("file filter error: {e}")),
                }
            }
        }
        drop(filters);
        app.set_hidden_files(hidden);
    }

    /// The latest script/config message to show in the status bar, if any.
    pub fn status_message(&self) -> Option<String> {
        self.status.borrow().clone()
    }

    fn set_status(&self, msg: String) {
        *self.status.borrow_mut() = Some(msg);
    }

    /// Dispatch a key press through the sequence state machine.
    ///
    /// The pending count and partial sequence live on [`App`] (so `render` can
    /// surface them). A digit with no sequence in flight folds into the count;
    /// otherwise the chord extends `pending_seq`, which is resolved against the
    /// active mode (with the `Global` fallback, except in the exclusive `Help`
    /// mode):
    ///
    /// * a **strict prefix** of a longer binding waits for the next key;
    /// * an **exact match** runs (with the count still set, so count-aware verbs
    ///   read it) and then clears the count + sequence;
    /// * a **miss** discards the dead sequence and the count.
    ///
    /// A faulty binding (a Lua error) is surfaced in the status bar, never
    /// propagated — a broken key must not crash the viewer.
    pub(crate) fn dispatch(&self, app: &mut App, chord: KeyChord) -> Result<()> {
        let mode = active_mode(app);

        // 1. Count prefix: only while no sequence is in flight. A leading `0`
        //    (no count yet) is a normal key, matching vim.
        if app.pending_seq.is_empty() {
            if let Some(d) = chord.count_digit() {
                if d != 0 || app.pending_count.is_some() {
                    let cur = app.pending_count.unwrap_or(0);
                    // Cap so a long digit run can't overflow; far past any view.
                    let next = cur.saturating_mul(10).saturating_add(d).min(1_000_000);
                    app.pending_count = Some(next);
                    return Ok(());
                }
            }
        }

        // 2. Extend the pending sequence and resolve it.
        app.pending_seq.push(chord);
        match self.resolve(mode, &app.pending_seq) {
            Resolution::Prefix => Ok(()), // wait for the next key
            Resolution::Miss => {
                app.pending_seq.clear();
                app.pending_count = None;
                Ok(())
            }
            Resolution::Exact(callback) => {
                app.pending_seq.clear();
                let result = self.run_in_scope(app, &callback);
                app.pending_count = None;
                if let Err(e) = result {
                    self.set_status(format!("key error: {e}"));
                }
                Ok(())
            }
        }
    }

    /// Resolve a pending sequence against `mode` (then `Global`, except in
    /// `Help`). A longer binding sharing this prefix always wins over an exact
    /// match here — the prefix-wait rule — so a binding may not be both a prefix
    /// of another and a usable terminal (documented as the one keymap-authoring
    /// constraint; the default keymap obeys it).
    fn resolve(&self, mode: Mode, seq: &[KeyChord]) -> Resolution {
        let bindings = self.bindings.borrow();
        let modes: &[Mode] = if mode == Mode::Help {
            &[Mode::Help]
        } else {
            &[mode, Mode::Global][..]
        };

        if bindings
            .keys()
            .any(|(m, k)| modes.contains(m) && k.len() > seq.len() && k.starts_with(seq))
        {
            return Resolution::Prefix;
        }
        for m in modes {
            if let Some(f) = bindings.get(&(*m, seq.to_vec())) {
                return Resolution::Exact(f.clone());
            }
        }
        Resolution::Miss
    }

    /// Run a registered command by name (the `:command` palette's Enter action),
    /// in the same scoped machinery as a key binding. An unknown name is a no-op;
    /// a Lua error is surfaced, not propagated.
    pub(crate) fn run_command(&self, app: &mut App, name: &str) -> Result<()> {
        let callback = self.commands.borrow().get(name).cloned();
        let Some(callback) = callback else {
            return Ok(());
        };
        if let Err(e) = self.run_in_scope(app, &callback) {
            self.set_status(format!("command error: {e}"));
        }
        Ok(())
    }

    /// Run one callback with the scoped action/reader verbs installed, borrowing
    /// `app` through a `RefCell` for the duration of the call.
    fn run_in_scope(&self, app: &mut App, callback: &Function) -> mlua::Result<()> {
        let cell = RefCell::new(app);
        let table = &self.mudpuppy;
        self.lua.scope(|scope| {
            api::install_scoped(
                &self.lua,
                scope,
                table,
                &cell,
                &self.commands,
                &self.prompts,
            )?;
            callback.call::<()>(())
        })
    }

    /// Run the callback for the option the user chose in an open prompt (the
    /// index [`App::handle_prompt_key`] returned), then clear the stored
    /// callbacks. Runs in the same scoped machinery as a command, so the option
    /// can drive the app (release the turn, quit, open another prompt, trigger an
    /// update). An out-of-range index or a faulty callback is surfaced, not
    /// propagated.
    pub(crate) fn run_prompt(&self, app: &mut App, index: usize) -> Result<()> {
        let callback = self.prompts.borrow().get(index).cloned();
        // The choice is consumed regardless; a fresh prompt reseeds the registry.
        self.prompts.borrow_mut().clear();
        if let Some(callback) = callback {
            if let Err(e) = self.run_in_scope(app, &callback) {
                self.set_status(format!("prompt error: {e}"));
            }
        }
        Ok(())
    }

    /// Fire `startup` (no payload).
    pub(crate) fn fire_startup(&self, app: &mut App) -> Result<()> {
        self.fire(app, EventKind::Startup, |lua, _| lua.create_table())
    }

    /// Fire `reload` (no payload).
    pub(crate) fn fire_reload(&self, app: &mut App) -> Result<()> {
        self.fire(app, EventKind::Reload, |lua, _| lua.create_table())
    }

    /// Fire `file_open{file=…}` for the currently selected file.
    pub(crate) fn fire_file_open(&self, app: &mut App) -> Result<()> {
        self.fire(app, EventKind::FileOpen, |lua, app| {
            let t = lua.create_table()?;
            t.set("file", views::current_file(lua, app)?)?;
            Ok(t)
        })
    }

    /// Fire `annotation_added{annotation=…}` for one newly-seen annotation.
    pub(crate) fn fire_annotation_added(
        &self,
        app: &mut App,
        annotation: &Annotation,
    ) -> Result<()> {
        self.fire(app, EventKind::AnnotationAdded, |lua, _| {
            let t = lua.create_table()?;
            t.set("annotation", views::annotation_table(lua, annotation)?)?;
            Ok(t)
        })
    }

    /// Fire `turn_change{turn=…}` with the current turn block.
    pub(crate) fn fire_turn_change(&self, app: &mut App) -> Result<()> {
        self.fire(app, EventKind::TurnChange, |lua, app| {
            let t = lua.create_table()?;
            t.set("turn", views::turn_table(lua, &app.turn)?)?;
            Ok(t)
        })
    }

    /// Fire `update_check{version, changelog?}` after the launch-time check found a
    /// newer release. `core.luau` handles the prompt; the event loop did the fetch.
    pub(crate) fn fire_update_check(
        &self,
        app: &mut App,
        version: &str,
        changelog: Option<&str>,
    ) -> Result<()> {
        let version = version.to_string();
        let changelog = changelog.map(str::to_string);
        self.fire(app, EventKind::UpdateCheck, move |lua, _| {
            let t = lua.create_table()?;
            t.set("version", version)?;
            if let Some(changelog) = changelog {
                t.set("changelog", changelog)?;
            }
            Ok(t)
        })
    }

    /// Fire `skill_update_check{version, message}` when the launch-time check
    /// found stale installed skills. `core.luau` handles the refresh prompt.
    pub(crate) fn fire_skill_update_check(
        &self,
        app: &mut App,
        version: u32,
        message: &str,
    ) -> Result<()> {
        let message = message.to_string();
        self.fire(app, EventKind::SkillUpdateCheck, move |lua, _| {
            let t = lua.create_table()?;
            t.set("version", version)?;
            t.set("message", message)?;
            Ok(t)
        })
    }

    /// Whether automatic update checks are enabled (the shared flag a config can
    /// turn off). The event loop reads this before launching the check.
    pub(crate) fn update_checks_enabled(&self) -> bool {
        *self.update_checks.borrow()
    }

    /// Run every handler for `kind` in a fresh scope (so handlers can call action
    /// verbs without a re-entrant borrow). `build` produces the payload table; it
    /// runs while a shared `&App` borrow is held, which is released before the
    /// handlers run.
    fn fire(
        &self,
        app: &mut App,
        kind: EventKind,
        build: impl FnOnce(&Lua, &App) -> mlua::Result<Table>,
    ) -> Result<()> {
        let handlers = self.events.borrow().get(&kind).cloned().unwrap_or_default();
        if handlers.is_empty() {
            return Ok(());
        }

        let cell = RefCell::new(app);
        let table = &self.mudpuppy;
        let result = self.lua.scope(|scope| {
            api::install_scoped(
                &self.lua,
                scope,
                table,
                &cell,
                &self.commands,
                &self.prompts,
            )?;
            // Build the payload, then drop the read borrow before calling the
            // handlers (which may take a write borrow via an action verb).
            let payload = {
                let app = cell.borrow();
                build(&self.lua, &app)?
            };
            for handler in &handlers {
                handler.call::<()>(payload.clone())?;
            }
            Ok(())
        });
        // A faulty event handler is surfaced, not propagated — same rationale as
        // dispatch.
        if let Err(e) = result {
            self.set_status(format!("event error: {e}"));
        }
        Ok(())
    }
}

/// The outcome of resolving a pending key sequence against the registry.
enum Resolution {
    /// A strict prefix of at least one longer binding — wait for the next key.
    Prefix,
    /// An exact match — run this callback.
    Exact(Function),
    /// No binding matches — discard the sequence.
    Miss,
}

/// The active keymap mode: `Help` while the overlay is open, otherwise the
/// focused pane.
fn active_mode(app: &App) -> Mode {
    if app.show_help {
        Mode::Help
    } else {
        match app.focus {
            Focus::Tree => Mode::Tree,
            Focus::Diff => Mode::Diff,
        }
    }
}

/// The embedded default keymap source. In debug builds this prefers the on-disk
/// `src/lua/core.luau` (so edits hot-reload in dev); release builds use the
/// `include_str!`-embedded copy.
fn core_source() -> Cow<'static, str> {
    #[cfg(debug_assertions)]
    {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/src/lua/core.luau");
        if let Ok(src) = std::fs::read_to_string(path) {
            return Cow::Owned(src);
        }
    }
    Cow::Borrowed(include_str!("core.luau"))
}

/// Resolve the user config path with a hand-rolled XDG search (no `directories`
/// dependency for config): `$MUDPUPPY_CONFIG` → `$XDG_CONFIG_HOME/mudpuppy/` →
/// `%APPDATA%\mudpuppy\` on Windows → `$HOME/.config/mudpuppy/`. The macOS path
/// is the same `~/.config` as Linux — a CLI's config does not belong in
/// *Application Support*. Returns `None` only when no home can be found.
pub fn config_path() -> Option<PathBuf> {
    if let Some(p) = std::env::var_os(CONFIG_ENV).filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(p));
    }
    if let Some(xdg) = std::env::var_os("XDG_CONFIG_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("mudpuppy").join("mudpuppy.luau"));
    }
    #[cfg(windows)]
    if let Some(appdata) = std::env::var_os("APPDATA").filter(|v| !v.is_empty()) {
        return Some(
            PathBuf::from(appdata)
                .join("mudpuppy")
                .join("mudpuppy.luau"),
        );
    }
    if let Some(home) = std::env::var_os("HOME").filter(|v| !v.is_empty()) {
        return Some(
            PathBuf::from(home)
                .join(".config")
                .join("mudpuppy")
                .join("mudpuppy.luau"),
        );
    }
    None
}

/// The full configuration & scripting reference printed by `mudpuppy help
/// config`. Built at runtime so it can name the config path that *this* machine
/// would actually use.
pub fn config_help() -> String {
    let current = match config_path() {
        Some(p) => format!(
            "Right now mudpuppy would load your config from:\n  {}\n",
            p.display()
        ),
        None => "Right now mudpuppy can't resolve a config path (no home directory found).\n"
            .to_string(),
    };
    // The literal format string holds only the three placeholders; the braces in
    // the Lua examples inside HELP_HEAD/HELP_TAIL are plain data, not re-parsed.
    format!("{HELP_HEAD}{current}{HELP_TAIL}")
}

const HELP_HEAD: &str = r#"mudpuppy configuration & scripting
==================================

mudpuppy is configured with a Luau script. Luau is a fast, sandboxed dialect of
Lua 5.1 (the same language Roblox uses); if you know Lua, you already know it.

On startup mudpuppy loads its built-in keymap, then loads your config on top, so
your file only needs to express the changes you want: rebind or disable keys,
add your own commands, and react to events.

Config file
-----------
mudpuppy looks for your config at, in order:
  1. $MUDPUPPY_CONFIG                       (an explicit file path)
  2. $XDG_CONFIG_HOME/mudpuppy/mudpuppy.luau
  3. ~/.config/mudpuppy/mudpuppy.luau        (%APPDATA%\mudpuppy\mudpuppy.luau on Windows)

"#;

const HELP_TAIL: &str = r#"
The file is optional — without it you get the default keymap. Edits are picked
up live: save the file and the keymap reloads without a restart. A config error
is non-fatal; it is shown in the status bar and your last good keymap stays in
effect. (Ctrl-C always quits, even if your config is broken.)

Sandbox
-------
The interpreter is sandboxed for safety: there is no filesystem, network,
process, or environment access — `io`, `os`, `package`, and `require` are not
available. Scripts can read the diff, the files, the annotations, and what is on
screen, and drive the UI through the actions below; nothing else. `print(...)`
writes to the status bar (there is no console behind the TUI).

The mudpuppy table
------------------
Everything lives on the global `mudpuppy` table.

Registration (call at load time, i.e. at the top level of your config):
  mudpuppy.map(mode, keys, fn)   bind a key sequence in `mode` to a function
  mudpuppy.unmap(mode, keys)     remove a binding
  mudpuppy.on(event, fn)         run `fn` when `event` fires
  mudpuppy.command(name, fn)     register a `:name` command for the palette
  mudpuppy.filter_files(fn)      hide files from the tree for which `fn` returns
                                 true (see Filtering files)
  mudpuppy.leader(key)           set the leader chord (default "space"); call it
                                 before any map that uses <leader>

Actions (call these from inside a binding or hook):
  mudpuppy.quit()
  mudpuppy.toggle_focus()
  mudpuppy.set_focus("tree" | "diff")
  mudpuppy.toggle_help()
  mudpuppy.toggle_annotations()  flip the sidebar to the all-annotations tab
  mudpuppy.toggle_file_filters() show all files / re-apply the file filters
  mudpuppy.release_turn()        hand the turn back to the agent
  mudpuppy.open_picker()         the fuzzy "add any file" picker
  mudpuppy.open_palette()        the `:command` palette
  mudpuppy.select_file(i)        open file i (1-based; clamped to the file list)
  mudpuppy.move_selection(delta) move the tree selection by delta (count-aware)
  mudpuppy.sidebar_move(delta)   move the focused sidebar tab's selection
  mudpuppy.sidebar_first()       jump the sidebar tab to its first item
  mudpuppy.sidebar_last(i)       jump to item i (1-based), or the last if i < 1
  mudpuppy.sidebar_confirm()     open the file / jump to the annotation
  mudpuppy.set_scroll(n)         scroll the diff to absolute row n (clamped)
  mudpuppy.scroll(delta)         scroll the diff by delta rows (count-aware)
  mudpuppy.next_hunk()           (count-aware)
  mudpuppy.prev_hunk()           (count-aware)
  mudpuppy.move_cursor(delta)    move the diff line cursor (count-aware)
  mudpuppy.set_cursor(n)         move the cursor to absolute row n (clamped)
  mudpuppy.cursor_to_top()
  mudpuppy.cursor_to_bottom()
  mudpuppy.expand_down() / expand_up() / expand_all()   reveal hidden context
  mudpuppy.toggle_visual()       start/stop a whole-line region selection
  mudpuppy.clear_selection()     leave visual mode (and cancel a delete prompt)
  mudpuppy.add_comment()         comment the cursor line (or the selection)
  mudpuppy.comment_file()        comment the whole file
  mudpuppy.reply()               reply to the annotation on the cursor line
  mudpuppy.edit_comment()        edit your annotation on the cursor line
  mudpuppy.delete_comment()      delete your annotation (confirm with y)
  mudpuppy.cycle_status()        open → resolved → wontfix for the cursor line
  mudpuppy.reset_annotations()   delete every annotation (clean-slate reset)
  mudpuppy.prompt(msg, options[, details])  open a modal question (see Prompts)

The count-aware verbs multiply/repeat by the pending count (see Counts below);
the absolute verbs (select_file, set_scroll, set_cursor, cursor_to_*) ignore it.

Readers (return tables describing the current state):
  mudpuppy.state()        { focus, selected, scroll, h_scroll, cursor, count, show_help,
                            sidebar, selection = { lo, hi } | nil,
                            turn = { owner, seq, agent_waiting, approved },
                            viewport = { height, total, top } }
  mudpuppy.files()        array of { path, status, additions, deletions, binary }
  mudpuppy.current_file() the open file, plus { hunks = { { ..., lines } } }
  mudpuppy.annotations()  array of { id, author, file, line, end_line, side,
                            scope, severity, tag, status, body, reply_to,
                            created_at, updated_at }
  mudpuppy.screen()       the diff rows currently visible on screen

`state()` is a live view, not a snapshot: each field is read on access. Its
`count` field is the only writable one — `mudpuppy.state().count = 5` sets the
pending count and `= nil` clears it; assigning any other field is an error.
(Because there are no real keys, `pairs(mudpuppy.state())` does not enumerate.)

`selected` and `select_file(i)` are 1-based.

Modes
-----
  global   active in every pane, and the fallback when the focused pane has no
           binding for the key
  tree     the file tree (left pane)
  diff     the diff view (center pane)
  help     the help overlay; exclusive — it does NOT fall back to global, so the
           overlay swallows keys it doesn't bind

A key is looked up in the active mode first, then in `global` (except in `help`).
So a mode-specific binding wins over a global one.

Key names & sequences
---------------------
A key is a single character (case-sensitive, so "G" is shift-g) or a named key,
optionally prefixed with modifiers:
  modifiers:  ctrl-  (or c-),  alt-  (or m-)      e.g. "ctrl-d", "c-d"
  named keys: tab, backtab, enter, esc, up, down, left, right, home, end,
              pageup, pagedown, space, backspace, delete
  examples:   "q"   "G"   "?"   "ctrl-d"   "tab"   "pageup"

A binding is keyed on a *sequence* of keys separated by spaces, so multi-key
chords are first-class. The token "<leader>" expands to the configured leader
(default "space") at map time, so set the leader first if you change it.
  "g g"            press g, then g
  "<leader> t r"   leader, then t, then r
  "q"              a single key is just a length-1 sequence

Authoring constraint: a sequence that is a prefix of a longer binding cannot
also be its own terminal binding — the prefix always wins and waits for the next
key. So don't bind both "g" and "g g"; the "g" binding would be unreachable.

Counts
------
A number typed before a motion is an ambient count, applied in one shot by the
count-aware verbs: "5j" moves five rows, "100G" jumps to row 100. A leading "0"
(with no count yet) is a normal key, matching vim. The count is readable and
writable as `mudpuppy.state().count`, so a custom binding can act on it.

The :command palette
--------------------
Press ":" to open a fuzzy command palette over every name registered with
`mudpuppy.command(name, fn)`. Type to filter, Tab to autocomplete to the top
match, Enter to run, Esc to cancel. The built-in `check-updates` command checks
GitHub for a newer release on demand.

Prompts
-------
`mudpuppy.prompt(message, options[, details])` opens a modal question. `options`
is an ordered array; each option is either a `{ "Label", function() ... end }`
pair or a `{ label = "Label", action = function() ... end }` table. The labels
render as numbered chips; ←/→ (or h/l) move the highlight, 1-9 pick directly,
Enter confirms the highlighted option and runs its function, Esc dismisses
without running anything. The optional `details` string renders as a scrollable
body between the question and the chips (↑/↓, j/k, or PageUp/PageDown scroll it);
the auto-update flow passes the release changelog here. It is a general
primitive — the auto-update flow is one user.

Updates
-------
Once per launch mudpuppy checks for a newer release (by reading the published
release manifest over HTTPS — no `gh` needed) and, when one exists, prompts you
to install it, ignore it for now, or skip (stop checking — this writes a line to
your config). The same primitives are scriptable:
  mudpuppy.updates.check()              -> { version = "vX.Y.Z", changelog = "…" }
                                           if a newer release exists (changelog
                                           absent if the manifest has none), else
                                           nil (a blocking fetch; the launch check
                                           runs off-thread)
  mudpuppy.updates.update(version)      install `version`; it must be a strict
                                           "vMAJOR.MINOR.PATCH" tag (validated
                                           before anything is run)
  mudpuppy.updates.check_enabled()      -> whether automatic checks are on
  mudpuppy.updates.set_check_enabled(b) turn automatic checks on/off (memory only)
  mudpuppy.updates.disable()            stop checking and persist that to config
Set MUDPUPPY_NO_UPDATE_CHECK in the environment to disable the launch check
without touching your config.

Skills
------
Separately from the binary self-update, mudpuppy can refresh the Claude Code
skill files it wrote with `mudpuppy install claude` when they go stale (an older
version than this binary ships). At launch, if a stale install is found and you
haven't skipped that version, it offers to update them. The primitives are
scriptable:
  mudpuppy.skills.check()    -> { version = N, message = "…" } if a stale install
                                should be refreshed, else nil
  mudpuppy.skills.refresh()  rewrite the installed SKILL.md files to the current
                                version
  mudpuppy.skills.skip(n)    record that you skipped version n, so the prompt
                                stays quiet until a newer one ships

Filtering files
---------------
`mudpuppy.filter_files(fn)` hides files from the tree (and the diff). `fn` is
called once per file with a `{ path, status, additions, deletions, binary }`
table (the same shape `mudpuppy.files()` returns) and returns true to hide that
file. Call it more than once to layer rules — a file is hidden when any
predicate returns true. Hide generated lockfiles, say:
  mudpuppy.filter_files(function(f)
    return f.path:match("%.lock$") ~= nil or f.path == "package-lock.json"
  end)
Files you pull in by hand with the picker are never hidden, and the list is
never emptied — if every file matches, nothing is hidden so there's always
something to review. Filters are evaluated at launch and re-applied when the
store reloads or the config is saved; the full file list is retained, so
removing a filter (or editing it) restores the affected files live.

Hidden files aren't dropped, just folded away: the built-in `:unhide-files`
command (and `mudpuppy.toggle_file_filters()`) toggles every filter off to reveal
them, then on again to re-hide. Rebind it if you like:
  mudpuppy.map("global", "<leader> u", function() mudpuppy.toggle_file_filters() end)

Anchors
-------
Annotations remember the code they were attached to and re-locate it when the
file changes. A primary "advanced" scan does fuzzy (edit-distance) matching in a
tight window around the original line; if it finds nothing confident, a wider
"fallback" scan looks for the exact line text to catch a line that moved further
(e.g. a relocated function). Each window: n>0 = scan ±n lines around the
original position; 0 = scan the whole file; negative = disable that scan.
  mudpuppy.anchor.advanced_match_window()       -> the advanced (fuzzy) window
  mudpuppy.anchor.set_advanced_match_window(n)  set it (default 50)
  mudpuppy.anchor.fallback_match_window()       -> the fallback (exact) window
  mudpuppy.anchor.set_fallback_match_window(n)  set it (default 1000)

Debug logging
-------------
`mudpuppy.debug_log = true` turns on a debug log. mudpuppy picks the location (a
`logs/` dir under its own data directory); the config can't name a path, so a
config can't aim log writes at an arbitrary place on disk. The TUI and the
headless `agent` get separate files so they don't interleave:
  <data-dir>/logs/mudpuppy-tui.log   and   <data-dir>/logs/mudpuppy-agent.log
The log is for diagnosing mudpuppy itself (diff/base resolution, store reloads).
It never records review content: file paths and branch names are written only as
salted, non-reversible hashes; commit SHAs and counts are written plain. The salt
lives in the store and rotates on `agent reset`. (`MUDPUPPY_LOG=<file>` in the
environment still works as an explicit single-file override, and it does take a
path since the environment is outside the config sandbox.)
  mudpuppy.debug_log = true

Events
------
  startup           once, after the config first loads
  file_open         when the open file changes (and once at startup)
                    payload: { file = current_file() }
  reload            after the annotation store is reloaded from disk
  annotation_added  for each newly-seen annotation
                    payload: { annotation = { ... } }
  turn_change       when the turn's owner or seq changes
                    payload: { turn = { ... } }
  update_check      once per launch, when the check found a newer release
                    payload: { version = "vX.Y.Z", changelog = "…"? }
                    (the default handler prompts, showing the changelog)
  skill_update_check once per launch, when the installed Claude Code skills are
                    stale and not skipped
                    payload: { version = N, message = "…" }  (default: prompts)

Examples
--------
Rebind a key — make `x` quit:
  mudpuppy.map("global", "x", function() mudpuppy.quit() end)

Disable a key — stop `q` from quitting:
  mudpuppy.unmap("global", "q")

Use ctrl-n / ctrl-p to move between files in the tree:
  mudpuppy.map("tree", "ctrl-n", function()
    mudpuppy.select_file(mudpuppy.state().selected + 1)
  end)
  mudpuppy.map("tree", "ctrl-p", function()
    mudpuppy.select_file(mudpuppy.state().selected - 1)
  end)

Jump to the last file with `G` from the diff pane too:
  mudpuppy.map("diff", "G", function()
    mudpuppy.select_file(#mudpuppy.files())
  end)

React to events — notice when the agent is waiting on you:
  mudpuppy.on("turn_change", function(ev)
    if ev.turn.agent_waiting then
      print("the agent is waiting for your review")
    end
  end)

  mudpuppy.on("file_open", function(ev)
    print("viewing " .. ev.file.path)
  end)

Ask a question with a modal prompt:
  mudpuppy.command("quit?", function()
    mudpuppy.prompt("Quit mudpuppy?", {
      { "Yes", function() mudpuppy.quit() end },
      { "No",  function() end },
    })
  end)

Every built-in binding is written against this same API, so the default keymap
is a working example of everything above.
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::diff::parse_diff;
    use crate::domain::{AnchorScope, Annotation, Author, Severity, Side, Status, Target};
    use crate::tui::App;
    use keys::KeyChord;

    /// A tiny two-file diff so file-selection (and thus `file_open`) has somewhere
    /// to move and `current_file` has hunks to expose.
    const DIFF: &str = "\
diff --git a/a.rs b/a.rs
index 111..222 100644
--- a/a.rs
+++ b/a.rs
@@ -1,2 +1,2 @@
 keep
-old
+new
diff --git a/b.rs b/b.rs
index 333..444 100644
--- a/b.rs
+++ b/b.rs
@@ -1,1 +1,2 @@
 keep
+added
";

    fn app() -> App {
        App::new(
            parse_diff(DIFF),
            Target::Local {
                base: "main".to_string(),
                head_sha: "abc".to_string(),
            },
        )
    }

    /// Write a user config to a throwaway dir and return it (kept alive) plus its
    /// path.
    fn config(src: &str) -> (tempfile::TempDir, PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mudpuppy.luau");
        std::fs::write(&path, src).unwrap();
        (dir, path)
    }

    fn note(id: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            author: Author::Agent,
            file: "a.rs".to_string(),
            line: 2,
            end_line: None,
            side: Side::Right,
            scope: AnchorScope::Line,
            signature: None,
            severity: Severity::Warning,
            tag: None,
            status: Status::Open,
            body: "b".to_string(),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn sandbox_hides_filesystem_and_process_globals() {
        let engine = LuaEngine::new(None).unwrap();
        for name in ["io", "os", "package", "require", "dofile", "loadfile"] {
            let v: mlua::Value = engine.lua.globals().get(name).unwrap();
            assert!(v.is_nil(), "`{name}` must not exist in the sandbox");
        }
        // With `io` absent a script cannot open a file: indexing nil is an error.
        assert!(
            engine
                .lua
                .load("return io.open('/etc/passwd')")
                .exec()
                .is_err(),
            "a script must not be able to open a file"
        );
    }

    #[test]
    fn core_lua_reproduces_quit_and_help_bindings() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();

        // `q` quits from the tree (via the Global fallback).
        engine
            .dispatch(&mut a, KeyChord::parse("q").unwrap())
            .unwrap();
        assert!(a.should_quit);

        // `?` toggles help; while help is open it is exclusive (swallows `j`).
        let mut a = app();
        engine
            .dispatch(&mut a, KeyChord::parse("?").unwrap())
            .unwrap();
        assert!(a.show_help);
        engine
            .dispatch(&mut a, KeyChord::parse("j").unwrap())
            .unwrap();
        assert_eq!(a.selected, 0, "help swallows navigation");
        engine
            .dispatch(&mut a, KeyChord::parse("esc").unwrap())
            .unwrap();
        assert!(!a.show_help);
    }

    #[test]
    fn count_prefix_accumulates_then_scales_and_clears() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        a.set_focus("diff");
        // `5` then `0` builds a count of 50 without running anything.
        engine
            .dispatch(&mut a, KeyChord::parse("5").unwrap())
            .unwrap();
        engine
            .dispatch(&mut a, KeyChord::parse("0").unwrap())
            .unwrap();
        assert_eq!(a.pending_count, Some(50));
        assert_eq!(a.cursor, 0, "a count alone moves nothing");
        // The next motion applies it in one shot (clamped) and clears the count.
        engine
            .dispatch(&mut a, KeyChord::parse("j").unwrap())
            .unwrap();
        assert_eq!(a.cursor, a.view.rows.len() - 1, "50j ran past the end");
        assert_eq!(a.pending_count, None);
    }

    #[test]
    fn leading_zero_is_a_normal_key_not_a_count() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        a.set_focus("diff");
        // `0` with no count pending is just a key (unbound here) — not a count.
        engine
            .dispatch(&mut a, KeyChord::parse("0").unwrap())
            .unwrap();
        assert_eq!(a.pending_count, None);
        assert!(a.pending_seq.is_empty(), "the unbound 0 missed and cleared");
    }

    #[test]
    fn sequence_waits_on_a_prefix_then_runs_the_exact_match() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        a.set_focus("diff");
        a.cursor = 1;
        // `g` is a strict prefix of `g g`: it waits, running nothing.
        engine
            .dispatch(&mut a, KeyChord::parse("g").unwrap())
            .unwrap();
        assert_eq!(a.pending_seq.len(), 1, "g waits for more keys");
        assert_eq!(a.cursor, 1, "nothing ran on the prefix");
        // The second `g` completes `g g` → jump to top.
        engine
            .dispatch(&mut a, KeyChord::parse("g").unwrap())
            .unwrap();
        assert_eq!(a.cursor, 0);
        assert!(a.pending_seq.is_empty());
    }

    #[test]
    fn a_dead_sequence_is_discarded_on_a_miss() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        a.set_focus("diff");
        a.cursor = 1;
        engine
            .dispatch(&mut a, KeyChord::parse("g").unwrap())
            .unwrap();
        // `g x` matches nothing: the sequence is dropped, cursor untouched.
        engine
            .dispatch(&mut a, KeyChord::parse("x").unwrap())
            .unwrap();
        assert!(a.pending_seq.is_empty());
        assert_eq!(a.cursor, 1);
    }

    #[test]
    fn leader_sequence_runs_a_bound_verb() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        // `<leader> a` (Space a) flips the sidebar to the annotations tab.
        engine
            .dispatch(&mut a, KeyChord::parse("space").unwrap())
            .unwrap();
        assert_eq!(a.pending_seq.len(), 1, "the leader waits");
        engine
            .dispatch(&mut a, KeyChord::parse("a").unwrap())
            .unwrap();
        assert_eq!(
            a.sidebar,
            crate::tui::Sidebar::Annotations,
            "Space a opened the annotations tab"
        );
        assert!(a.pending_seq.is_empty());
    }

    #[test]
    fn state_count_is_readable_and_writable_from_lua() {
        // Reading: a binding sees the pending count. Writing: a binding sets it,
        // and the very next count-aware verb in the same call applies it.
        let (_dir, path) = config(
            "mudpuppy.map(\"global\", \"z\", function() print(\"c=\" .. tostring(mudpuppy.state().count)) end)\n\
             mudpuppy.map(\"diff\", \"w\", function() mudpuppy.state().count = 4; mudpuppy.move_cursor(1) end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        a.set_focus("diff");
        // Read: `3z` prints the pending count.
        engine
            .dispatch(&mut a, KeyChord::parse("3").unwrap())
            .unwrap();
        engine
            .dispatch(&mut a, KeyChord::parse("z").unwrap())
            .unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("c=3"));
        // Write: `w` sets count=4 then moves once → lands four rows down (clamped).
        let mut a = app();
        a.set_focus("diff");
        engine
            .dispatch(&mut a, KeyChord::parse("w").unwrap())
            .unwrap();
        assert_eq!(
            a.cursor,
            a.view.rows.len() - 1,
            "the written count scaled the move"
        );
    }

    #[test]
    fn the_palette_opens_seeded_with_registered_commands() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        // `:` opens the palette, seeded with the default command names.
        engine
            .dispatch(&mut a, KeyChord::parse(":").unwrap())
            .unwrap();
        let palette = a.palette.as_ref().expect("the palette is open");
        assert!(palette.all.iter().any(|n| n == "release-turn"));
        assert!(palette.all.iter().any(|n| n == "annotations"));
    }

    #[test]
    fn run_command_invokes_a_registered_command() {
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "help").unwrap();
        assert!(a.show_help, "the help command toggled the overlay");
        // An unknown command name is a silent no-op.
        engine.run_command(&mut a, "no-such-command").unwrap();
    }

    #[test]
    fn user_config_overrides_a_default_binding() {
        // Rebind `q` (normally quit) to open the annotations tab instead.
        let (_dir, path) =
            config(r#"mudpuppy.map("global", "q", function() mudpuppy.toggle_annotations() end)"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine
            .dispatch(&mut a, KeyChord::parse("q").unwrap())
            .unwrap();
        assert!(!a.should_quit, "the override replaced quit");
        assert_eq!(
            a.sidebar,
            crate::tui::Sidebar::Annotations,
            "`q` now opens the annotations tab"
        );
    }

    #[test]
    fn unmap_removes_a_default_and_falls_back_to_global() {
        // `j` in the diff scrolls; `unmap` it and it should do nothing (the diff
        // map has no fallback for it, and Global has no `j`).
        let (_dir, path) = config(r#"mudpuppy.unmap("diff", "j")"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        a.set_focus("diff");
        engine
            .dispatch(&mut a, KeyChord::parse("j").unwrap())
            .unwrap();
        assert_eq!(a.scroll, 0, "unmapped `j` no longer scrolls");

        // Removing the diff `g` (jump to top) lets the key fall back to whatever
        // Global binds — nothing here — so it is simply inert, not an error.
        let (_dir, path) = config(r#"mudpuppy.unmap("global", "q")"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine
            .dispatch(&mut a, KeyChord::parse("q").unwrap())
            .unwrap();
        assert!(!a.should_quit, "unmapped `q` no longer quits");
    }

    #[test]
    fn a_broken_user_config_is_non_fatal() {
        let (_dir, path) = config("this is not valid lua %%%");
        // Construction succeeds despite the bad config…
        let engine = LuaEngine::new(Some(path)).unwrap();
        assert!(
            engine
                .status_message()
                .is_some_and(|m| m.contains("config")),
            "the error is surfaced for the status bar"
        );
        // …and the core defaults still work.
        let mut a = app();
        engine
            .dispatch(&mut a, KeyChord::parse("q").unwrap())
            .unwrap();
        assert!(a.should_quit);
    }

    #[test]
    fn filter_files_hides_matching_files() {
        // A predicate that hides `b.rs` removes it from the tree, keeping `a.rs`.
        let (_dir, path) =
            config(r#"mudpuppy.filter_files(function(f) return f.path == "b.rs" end)"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        assert_eq!(a.files.len(), 2);
        engine.apply_file_filters(&mut a);
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.current().display_path(), "a.rs");
    }

    #[test]
    fn filter_files_layers_predicates_and_keeps_non_matches() {
        // Two predicates compose: a file is hidden when either returns true. A
        // predicate returning false (or nil) leaves the file visible.
        let (_dir, path) = config(
            "mudpuppy.filter_files(function(f) return f.path == \"a.rs\" end)\n\
             mudpuppy.filter_files(function(f) return false end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.apply_file_filters(&mut a);
        assert_eq!(a.files.len(), 1);
        assert_eq!(a.current().display_path(), "b.rs");
    }

    #[test]
    fn unhide_restores_filtered_files_then_re_hides() {
        // The filtered file is retained, so `toggle_file_filters` (the
        // `unhide-files` command) reveals it again and toggling back re-hides it.
        let (_dir, path) =
            config(r#"mudpuppy.filter_files(function(f) return f.path == "b.rs" end)"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.apply_file_filters(&mut a);
        assert_eq!(a.files.len(), 1);
        a.toggle_file_filters();
        assert_eq!(a.files.len(), 2, "unhide shows the filtered file again");
        assert!(a.files.iter().any(|f| f.display_path() == "b.rs"));
        a.toggle_file_filters();
        assert_eq!(a.files.len(), 1, "toggling back re-hides it");
    }

    #[test]
    fn removing_the_filter_restores_files() {
        // A config with no filter clears the hidden set, restoring the file — the
        // hot-reload path (clear registries, re-exec, re-apply).
        let (_dir, path) =
            config(r#"mudpuppy.filter_files(function(f) return f.path == "b.rs" end)"#);
        let engine = LuaEngine::new(Some(path.clone())).unwrap();
        let mut a = app();
        engine.apply_file_filters(&mut a);
        assert_eq!(a.files.len(), 1);

        std::fs::write(&path, "").unwrap();
        engine.reload_config().unwrap();
        engine.apply_file_filters(&mut a);
        assert_eq!(a.files.len(), 2, "a removed filter restores the file");
        assert!(a.files.iter().any(|f| f.display_path() == "b.rs"));
    }

    #[test]
    fn filter_files_never_empties_the_list() {
        // A filter that matches every file is ignored — the viewer must always
        // have something to show.
        let (_dir, path) = config(r#"mudpuppy.filter_files(function(f) return true end)"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.apply_file_filters(&mut a);
        assert_eq!(a.files.len(), 2, "hiding everything is a no-op");
    }

    #[test]
    fn file_open_handler_receives_the_selected_file() {
        let (_dir, path) =
            config(r#"mudpuppy.on("file_open", function(ev) print("open:" .. ev.file.path) end)"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        a.select(1); // open b.rs
        engine.fire_file_open(&mut a).unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("open:b.rs"));
    }

    #[test]
    fn annotation_added_handler_receives_the_annotation() {
        let (_dir, path) = config(
            r#"mudpuppy.on("annotation_added", function(ev) print(ev.annotation.id .. "/" .. ev.annotation.severity) end)"#,
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine
            .fire_annotation_added(&mut a, &note("xyz12345"))
            .unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("xyz12345/warning"));
    }

    #[test]
    fn turn_change_handler_sees_the_turn_block() {
        let (_dir, path) =
            config(r#"mudpuppy.on("turn_change", function(ev) print(ev.turn.owner) end)"#);
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.fire_turn_change(&mut a).unwrap();
        // A fresh turn belongs to the agent.
        assert_eq!(engine.status_message().as_deref(), Some("agent"));
    }

    #[test]
    fn prompt_opens_an_overlay_and_runs_the_chosen_options_callback() {
        // A command opens a two-option prompt; running each option fires its own
        // callback through the engine's scoped machinery.
        let (_dir, path) = config(
            "mudpuppy.command(\"ask\", function()\n\
               mudpuppy.prompt(\"pick one\", {\n\
                 { \"first\", function() print(\"chose-first\") end },\n\
                 { \"second\", function() mudpuppy.quit() end },\n\
               })\n\
             end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();

        engine.run_command(&mut a, "ask").unwrap();
        let prompt = a.prompt.as_ref().expect("the prompt is open");
        assert_eq!(prompt.message, "pick one");
        assert_eq!(
            prompt.options,
            vec!["first".to_string(), "second".to_string()]
        );

        // Option 0 runs its callback (a print, surfaced in the status bar).
        engine.run_prompt(&mut a, 0).unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("chose-first"));
        assert!(!a.should_quit);

        // Re-open and pick option 1, which quits.
        engine.run_command(&mut a, "ask").unwrap();
        engine.run_prompt(&mut a, 1).unwrap();
        assert!(a.should_quit);
    }

    #[test]
    fn prompt_accepts_the_keyed_option_form() {
        let (_dir, path) = config(
            "mudpuppy.command(\"ask\", function()\n\
               mudpuppy.prompt(\"q\", { { label = \"only\", action = function() end } })\n\
             end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "ask").unwrap();
        assert_eq!(
            a.prompt.as_ref().map(|p| p.options.clone()),
            Some(vec!["only".to_string()])
        );
    }

    #[test]
    fn prompt_with_no_options_is_an_error_and_opens_nothing() {
        let (_dir, path) =
            config("mudpuppy.command(\"ask\", function() mudpuppy.prompt(\"empty\", {}) end)");
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "ask").unwrap();
        assert!(a.prompt.is_none(), "an empty prompt must not open");
        assert!(engine
            .status_message()
            .is_some_and(|m| m.contains("at least one option")));
    }

    #[test]
    fn run_prompt_with_an_out_of_range_index_is_a_noop() {
        let (_dir, path) = config(
            "mudpuppy.command(\"ask\", function()\n\
               mudpuppy.prompt(\"q\", { { \"only\", function() mudpuppy.quit() end } })\n\
             end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "ask").unwrap();
        engine.run_prompt(&mut a, 9).unwrap();
        assert!(!a.should_quit, "no callback for a bad index");
    }

    #[test]
    fn update_checks_flag_defaults_on_and_a_config_line_disables_it() {
        // Default: checks enabled.
        let (_dir, path) = config(
            "mudpuppy.command(\"flag\", function() print(tostring(mudpuppy.updates.check_enabled())) end)",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "flag").unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("true"));

        // The line the skip action persists turns it off on load.
        let (_dir, path) = config(
            "mudpuppy.updates.set_check_enabled(false)\n\
             mudpuppy.command(\"flag\", function() print(tostring(mudpuppy.updates.check_enabled())) end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "flag").unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("false"));
    }

    #[test]
    fn debug_log_defaults_off_and_a_config_line_turns_it_on() {
        // No config line: debug logging stays off.
        let engine = LuaEngine::new(None).unwrap();
        assert!(!engine.debug_log());

        // `= true` flips the flag the binary reads at startup.
        let (_dir, path) = config("mudpuppy.debug_log = true");
        let engine = LuaEngine::new(Some(path)).unwrap();
        assert!(engine.debug_log());
    }

    #[test]
    fn debug_log_rejects_a_non_boolean() {
        // A path (the old API) is now a hard config error, not a silent string.
        let (_dir, path) = config("mudpuppy.debug_log = \"/tmp/logs\"");
        let engine = LuaEngine::new(Some(path)).unwrap();
        assert!(!engine.debug_log());
        assert!(engine
            .status_message()
            .is_some_and(|m| m.contains("debug_log")));
    }

    #[test]
    fn anchor_windows_default_and_are_config_settable() {
        // Defaults mirror the engine defaults.
        let defaults = crate::anchor::Params::default();
        let (_dir, path) = config(
            "mudpuppy.command(\"w\", function()\n\
               print(mudpuppy.anchor.advanced_match_window() .. \",\" .. mudpuppy.anchor.fallback_match_window())\n\
             end)",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "w").unwrap();
        assert_eq!(
            engine.status_message().as_deref(),
            Some(
                format!(
                    "{},{}",
                    defaults.advanced_match_window, defaults.fallback_match_window
                )
                .as_str()
            )
        );

        // A config can set each independently (including the disable sentinel).
        let (_dir, path) = config(
            "mudpuppy.anchor.set_advanced_match_window(-1)\n\
             mudpuppy.anchor.set_fallback_match_window(1000)\n\
             mudpuppy.command(\"w\", function()\n\
               print(mudpuppy.anchor.advanced_match_window() .. \",\" .. mudpuppy.anchor.fallback_match_window())\n\
             end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "w").unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("-1,1000"));
    }

    #[test]
    fn updates_update_rejects_a_malicious_version_without_spawning() {
        // `update` validates before it would ever spawn `cargo`; a funky version
        // raises, which pcall catches as a `false` first return.
        let (_dir, path) = config(
            "mudpuppy.command(\"bad\", function()\n\
               local ok = pcall(function() mudpuppy.updates.update(\"v1.2.3; rm -rf /\") end)\n\
               print(tostring(ok))\n\
             end)\n",
        );
        let engine = LuaEngine::new(Some(path)).unwrap();
        let mut a = app();
        engine.run_command(&mut a, "bad").unwrap();
        assert_eq!(engine.status_message().as_deref(), Some("false"));
    }

    /// Serializes the tests that mutate process-global env vars (`HOME`,
    /// `XDG_CONFIG_HOME`, `MUDPUPPY_CONFIG`), which would otherwise race under the
    /// parallel test runner.
    static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    #[test]
    fn skill_skip_suppresses_the_next_check() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            [CONFIG_ENV, "XDG_CONFIG_HOME", "HOME"]
                .iter()
                .map(|k| (*k, std::env::var_os(k)))
                .collect();

        // A temp HOME holding a stale (version-0, unstamped) user-level skill
        // install, so `should_prompt_skill_refresh` sees something to refresh.
        let home = tempfile::tempdir().unwrap();
        let skills = home
            .path()
            .join(".claude")
            .join("skills")
            .join("mudpuppy-pr-review");
        std::fs::create_dir_all(&skills).unwrap();
        std::fs::write(skills.join("SKILL.md"), "---\nname: x\n---\nbody\n").unwrap();

        // The config (and thus the skip file beside it) lives under the same temp
        // tree, isolated from the host.
        let cfg = home.path().join("config").join("mudpuppy.luau");
        std::env::remove_var("XDG_CONFIG_HOME");
        std::env::set_var("HOME", home.path());
        std::env::set_var(CONFIG_ENV, &cfg);

        // Before skipping, `check()` reports the stale install; `skip(version)`
        // then suppresses it.
        let engine = LuaEngine::new(None).unwrap();
        let mut a = app();
        engine
            .lua
            .load(
                "mudpuppy.command(\"chk\", function()\n\
                   local s = mudpuppy.skills.check()\n\
                   print(s and tostring(s.version) or \"nil\")\n\
                 end)\n\
                 mudpuppy.command(\"skip\", function() mudpuppy.skills.skip(mudpuppy.skills.check().version) end)\n",
            )
            .exec()
            .unwrap();

        engine.run_command(&mut a, "chk").unwrap();
        assert_eq!(
            engine.status_message().as_deref(),
            Some(crate::install::SKILL_VERSION.to_string().as_str()),
            "a stale install is reported before skipping"
        );

        engine.run_command(&mut a, "skip").unwrap();
        engine.run_command(&mut a, "chk").unwrap();
        assert_eq!(
            engine.status_message().as_deref(),
            Some("nil"),
            "after skip, check is suppressed"
        );

        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }

    #[test]
    fn config_path_honors_the_search_order() {
        let _guard = ENV_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        // These env vars are process-global; the env lock above serializes the
        // tests that touch them so mutating them in sequence is safe.
        let saved: Vec<(&str, Option<std::ffi::OsString>)> =
            [CONFIG_ENV, "XDG_CONFIG_HOME", "HOME"]
                .iter()
                .map(|k| (*k, std::env::var_os(k)))
                .collect();

        std::env::remove_var(CONFIG_ENV);
        std::env::remove_var("XDG_CONFIG_HOME");

        #[cfg(not(windows))]
        {
            std::env::set_var("HOME", "/home/u");
            assert_eq!(
                config_path(),
                Some(PathBuf::from("/home/u/.config/mudpuppy/mudpuppy.luau"))
            );
        }

        std::env::set_var("XDG_CONFIG_HOME", "/xdg");
        assert_eq!(
            config_path(),
            Some(PathBuf::from("/xdg/mudpuppy/mudpuppy.luau"))
        );

        std::env::set_var(CONFIG_ENV, "/explicit/my.lua");
        assert_eq!(config_path(), Some(PathBuf::from("/explicit/my.lua")));

        for (k, v) in saved {
            match v {
                Some(val) => std::env::set_var(k, val),
                None => std::env::remove_var(k),
            }
        }
    }
}
