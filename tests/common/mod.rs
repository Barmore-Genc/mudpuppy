//! Shared Layer-2 e2e harness: drive the real binary through a PTY against a
//! throwaway fixture git repo. Used by both `e2e.rs` (the coarse PTY smoke
//! suite) and `image_diff.rs` (the SVG emitter feeding the `resvg` pixel
//! oracle). See `../../TESTING.md` and `../../e2e/README.md`.

// Each integration-test binary `mod common;`-includes this file and uses a
// different subset, so some items look unused per-crate.
#![allow(dead_code)]

use std::io::{Read, Write};
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use portable_pty::{native_pty_system, CommandBuilder, ExitStatus, MasterPty, PtySize};

pub const ROWS: u16 = 24;
pub const COLS: u16 = 100;

// Verbatim sequences crossterm emits for the alternate screen (see crossterm's
// `EnterAlternateScreen`/`LeaveAlternateScreen`). Raw mode is a termios change
// and leaves no bytes, so the alt-screen pair is our teardown witness.
pub const ENTER_ALT_SCREEN: &str = "\x1b[?1049h";
pub const LEAVE_ALT_SCREEN: &str = "\x1b[?1049l";

/// A running mudpuppy process attached to a PTY, with a background thread
/// draining the master into a shared buffer.
pub struct Session {
    _master: Box<dyn MasterPty + Send>,
    child: Box<dyn portable_pty::Child + Send + Sync>,
    writer: Box<dyn Write + Send>,
    output: Arc<Mutex<Vec<u8>>>,
    reader: Option<thread::JoinHandle<()>>,
}

impl Session {
    /// Launch the real binary with its cwd inside `repo`.
    pub fn launch(repo: &Path) -> Session {
        Session::launch_with_env(repo, &[])
    }

    /// Launch with extra environment variables on top of the standard terminal +
    /// git-config isolation — e.g. `MUDPUPPY_DATA_DIR` so the TUI shares a store
    /// with a headless `agent` process in the same test.
    pub fn launch_with_env(repo: &Path, extra_env: &[(&str, &Path)]) -> Session {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows: ROWS,
                cols: COLS,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("open pty");

        let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_mudpuppy"));
        cmd.cwd(repo);
        // A real-ish terminal, and config isolation so the host's git config
        // can't change the diff (line endings, default branch, etc.).
        cmd.env("TERM", "xterm-256color");
        cmd.env("GIT_CONFIG_GLOBAL", "/dev/null");
        cmd.env("GIT_CONFIG_SYSTEM", "/dev/null");
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let child = pair.slave.spawn_command(cmd).expect("spawn binary");
        let mut reader = pair.master.try_clone_reader().expect("clone reader");
        let writer = pair.master.take_writer().expect("take writer");
        // Closing our handle to the slave lets the master read hit EOF once the
        // child exits, so the drain thread can finish.
        drop(pair.slave);

        let output = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&output);
        let reader = thread::spawn(move || {
            let mut buf = [0u8; 8192];
            while let Ok(n) = reader.read(&mut buf) {
                if n == 0 {
                    break;
                }
                sink.lock().unwrap().extend_from_slice(&buf[..n]);
            }
        });

        Session {
            _master: pair.master,
            child,
            writer,
            output,
            reader: Some(reader),
        }
    }

    /// Everything emitted so far, raw (escape sequences included).
    pub fn raw(&self) -> Vec<u8> {
        self.output.lock().unwrap().clone()
    }

    /// The settled screen as a user would see it: the escape stream replayed
    /// through a fresh vt100 grid, rows joined by newlines.
    pub fn screen(&self) -> String {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(&self.raw());
        parser.screen().contents()
    }

    /// The settled screen as a **truecolor SVG** built from the vt100 grid —
    /// one `<rect>` run per background color, one `<text>` run per styled glyph
    /// span. The pixel oracle rasterizes this with `resvg` to a lossless 24-bit
    /// PNG. We render SVG→PNG rather than going through `agg`'s GIF because GIF
    /// is 8-bit (≤256 colors): the TUI's anti-aliased text blows past 256
    /// shades, so GIF re-quantizes the *whole* image on any change and diffs
    /// stop localizing. (`agg`'s own default renderer is `resvg` too, so glyph
    /// quality is unchanged — we just skip the lossy palette step.)
    pub fn screen_svg(&self) -> String {
        let mut parser = vt100::Parser::new(ROWS, COLS, 0);
        parser.process(&self.raw());
        render_svg(parser.screen())
    }

    /// Poll the screen until `needle` appears or `timeout` elapses.
    pub fn wait_for_screen(&self, needle: &str, timeout: Duration) -> bool {
        let start = Instant::now();
        loop {
            if self.screen().contains(needle) {
                return true;
            }
            if start.elapsed() >= timeout {
                return false;
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    /// Type raw bytes into the PTY (delivered to the app as real tty input).
    pub fn feed(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("write to pty");
        self.writer.flush().ok();
    }

    /// Block until the child exits, killing it if it overruns `timeout`.
    pub fn wait(&mut self, timeout: Duration) -> Option<ExitStatus> {
        let start = Instant::now();
        loop {
            if let Ok(Some(status)) = self.child.try_wait() {
                if let Some(h) = self.reader.take() {
                    let _ = h.join();
                }
                return Some(status);
            }
            if start.elapsed() >= timeout {
                let _ = self.child.kill();
                return None;
            }
            thread::sleep(Duration::from_millis(40));
        }
    }

    /// Kill the child now (used after capturing a still — we don't want the
    /// quit-time teardown sequences in the recording).
    pub fn kill(&mut self) {
        let _ = self.child.kill();
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        // Never leave a stray process behind if an assertion panicked mid-test.
        let _ = self.child.kill();
    }
}

// ---- truecolor SVG rendering of the settled screen ------------------------

// Geometry. DejaVu Sans Mono's advance is ≈0.6·font-size, so CW ≈ that keeps
// each text run's `textLength` scaling near 1 (no glyph distortion) while still
// snapping every cell to an exact grid column.
const FS: f64 = 15.0; // font size (px)
const CW: f64 = 9.0; // cell width (px)
const CH: f64 = 18.0; // cell height (px)
const BASELINE: f64 = 13.5; // text baseline within a cell row
const THEME_BG: (u8, u8, u8) = (24, 26, 31); // default background
const THEME_FG: (u8, u8, u8) = (208, 208, 208); // default foreground

/// Foreground + background of a cell as concrete RGB, applying the `inverse`
/// swap and the theme defaults.
fn resolve(cell: &vt100::Cell) -> ((u8, u8, u8), (u8, u8, u8)) {
    let conv = |c: vt100::Color, default: (u8, u8, u8)| match c {
        vt100::Color::Default => default,
        vt100::Color::Idx(i) => xterm256(i),
        vt100::Color::Rgb(r, g, b) => (r, g, b),
    };
    let mut fg = conv(cell.fgcolor(), THEME_FG);
    let mut bg = conv(cell.bgcolor(), THEME_BG);
    if cell.inverse() {
        std::mem::swap(&mut fg, &mut bg);
    }
    (fg, bg)
}

fn cell_bg(screen: &vt100::Screen, row: u16, col: u16) -> (u8, u8, u8) {
    screen.cell(row, col).map_or(THEME_BG, |c| resolve(c).1)
}

/// The run-merge key for text: same fg + bold/italic/underline renders as one
/// `<text>` element.
fn text_style(screen: &vt100::Screen, row: u16, col: u16) -> ((u8, u8, u8), bool, bool, bool) {
    match screen.cell(row, col) {
        Some(c) => (resolve(c).0, c.bold(), c.italic(), c.underline()),
        None => (THEME_FG, false, false, false),
    }
}

fn render_svg(screen: &vt100::Screen) -> String {
    let w = COLS as f64 * CW;
    let h = ROWS as f64 * CH;
    let mut out = String::new();
    out.push_str(&format!(
        r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}">"#
    ));
    out.push_str(&format!(
        r#"<rect width="{w}" height="{h}" fill="{}" shape-rendering="crispEdges"/>"#,
        hex(THEME_BG)
    ));

    // Background runs: merge horizontally-adjacent cells sharing a non-default bg.
    for row in 0..ROWS {
        let mut col = 0u16;
        while col < COLS {
            let bg = cell_bg(screen, row, col);
            if bg == THEME_BG {
                col += 1;
                continue;
            }
            let start = col;
            while col < COLS && cell_bg(screen, row, col) == bg {
                col += 1;
            }
            out.push_str(&format!(
                r#"<rect x="{}" y="{}" width="{}" height="{CH}" fill="{}" shape-rendering="crispEdges"/>"#,
                start as f64 * CW,
                row as f64 * CH,
                (col - start) as f64 * CW,
                hex(bg)
            ));
        }
    }

    // Text runs: merge adjacent drawable glyphs sharing fg + bold/italic/underline.
    // textLength pins each run to an exact column span so the grid never drifts.
    out.push_str(&format!(
        r#"<g font-family="DejaVu Sans Mono" font-size="{FS}px">"#
    ));
    for row in 0..ROWS {
        let mut col = 0u16;
        while col < COLS {
            let drawable = screen
                .cell(row, col)
                .map(|c| !c.contents().is_empty() && c.contents() != " ")
                .unwrap_or(false);
            if !drawable {
                col += 1;
                continue;
            }
            let style = text_style(screen, row, col);
            let start = col;
            let mut run = String::new();
            while col < COLS {
                let Some(c) = screen.cell(row, col) else {
                    break;
                };
                let t = c.contents();
                if t.is_empty() || t == " " || text_style(screen, row, col) != style {
                    break;
                }
                run.push_str(&t);
                col += 1;
            }
            let (fg, bold, italic, underline) = style;
            let mut attrs = String::new();
            if bold {
                attrs.push_str(r#" font-weight="bold""#);
            }
            if italic {
                attrs.push_str(r#" font-style="italic""#);
            }
            if underline {
                attrs.push_str(r#" text-decoration="underline""#);
            }
            out.push_str(&format!(
                r#"<text x="{}" y="{}" fill="{}" textLength="{}" lengthAdjust="spacingAndGlyphs"{attrs} xml:space="preserve">{}</text>"#,
                start as f64 * CW,
                row as f64 * CH + BASELINE,
                hex(fg),
                (col - start) as f64 * CW,
                xml_escape(&run),
            ));
        }
    }

    out.push_str("</g></svg>");
    out
}

fn hex((r, g, b): (u8, u8, u8)) -> String {
    format!("#{r:02x}{g:02x}{b:02x}")
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

/// The standard xterm 256-color palette: 0–15 system, 16–231 a 6×6×6 cube,
/// 232–255 grayscale. ratatui's named colors arrive as low indices here.
fn xterm256(idx: u8) -> (u8, u8, u8) {
    const SYSTEM: [(u8, u8, u8); 16] = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    match idx {
        0..=15 => SYSTEM[idx as usize],
        16..=231 => {
            let i = idx - 16;
            let steps = [0u8, 95, 135, 175, 215, 255];
            (
                steps[(i / 36) as usize],
                steps[((i / 6) % 6) as usize],
                steps[(i % 6) as usize],
            )
        }
        232..=255 => {
            let v = 8 + 10 * (idx - 232);
            (v, v, v)
        }
    }
}

// ---- fixture repos ---------------------------------------------------------

pub fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .args(args)
        .current_dir(repo)
        // Same isolation as the binary sees, so fixture creation is byte-stable.
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_SYSTEM", "/dev/null")
        .env("GIT_AUTHOR_NAME", "mudpuppy e2e")
        .env("GIT_AUTHOR_EMAIL", "e2e@mudpuppy.test")
        .env("GIT_COMMITTER_NAME", "mudpuppy e2e")
        .env("GIT_COMMITTER_EMAIL", "e2e@mudpuppy.test")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

pub fn write(repo: &Path, rel: &str, contents: &str) {
    let path = repo.join(rel);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    std::fs::write(path, contents).unwrap();
}

/// A repo whose working tree has changes against its only commit: one heavily
/// modified file and one added file. `git diff` lists files alphabetically, so
/// `a_app.rs` is file 1 and `b_notes.txt` is file 2 — deterministic regardless
/// of host. `a_app.rs` rewrites every line, producing a diff far taller than
/// the 24-row viewport so the scroll test has somewhere to go.
pub fn repo_with_changes() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();

    git(repo, &["-c", "init.defaultBranch=main", "init", "-q"]);

    // Base commit.
    let base: String = (1..=40).map(|n| format!("line {n:02}\n")).collect();
    write(repo, "a_app.rs", &base);
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);

    // Working-tree changes: rewrite every line of a_app.rs (a big, scrollable
    // diff) and add a new file. Stage so the add shows up in `git diff HEAD`.
    let edited: String = (1..=40).map(|n| format!("line {n:02} edited\n")).collect();
    write(repo, "a_app.rs", &edited);
    write(repo, "b_notes.txt", "a brand new file\nsecond line\n");
    git(repo, &["add", "-A"]);

    dir
}

/// A clean repo: one commit, no working-tree changes, so there is nothing to
/// review.
pub fn repo_clean() -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    git(repo, &["-c", "init.defaultBranch=main", "init", "-q"]);
    write(repo, "README.md", "# nothing to see here\n");
    git(repo, &["add", "-A"]);
    git(repo, &["commit", "-q", "-m", "base"]);
    dir
}
