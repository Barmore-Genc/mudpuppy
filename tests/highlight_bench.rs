//! Backend A/B + size-scaling profile of the syntax highlighter
//! (`mudpuppy::highlight`).
//!
//! Times [`Highlighter::for_path`] + [`Highlighter::hunk`] over a large corpus
//! of real Rust source at growing line counts. Backend-agnostic: it exercises
//! only the public `highlight` API, so the same test measures whichever regex
//! engine the build selected. Marked `#[ignore]` so it stays out of the normal
//! suite (timings are environment-sensitive). The engine is the per-target
//! default (onig on most hosts, fancy on arm64 Linux/Windows); force the other
//! to A/B them on one host:
//!
//! ```text
//! # per-target default for this host:
//! cargo test --release --test highlight_bench -- --ignored --nocapture
//! # force oniguruma (C):
//! cargo test --release --features onig \
//!     --test highlight_bench -- --ignored --nocapture
//! # force pure-Rust fancy-regex (on a host whose default is onig, swap the
//! # engine in Cargo.toml's [target.…] table — features are additive and onig
//! # wins when both are compiled):
//! ```

use std::time::Instant;

use mudpuppy::highlight::Highlighter;

const ITERS: usize = 5;

fn read(rel: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Build a corpus of at least `min_lines` by concatenating real `.rs` source,
/// repeating the pool as needed. Reused from `anchor_bench.rs`'s `corpus` idea.
fn corpus(min_lines: usize) -> Vec<String> {
    let pool = [
        "src/tui/app.rs",
        "src/tui/render.rs",
        "src/lua/mod.rs",
        "src/agent.rs",
        "src/diff.rs",
        "src/highlight.rs",
    ];
    let mut out: Vec<String> = Vec::new();
    let mut i = 0;
    while out.len() < min_lines {
        for line in read(pool[i % pool.len()]).lines() {
            out.push(line.to_string());
        }
        i += 1;
    }
    out.truncate(min_lines);
    out
}

/// Mean ms to resolve the Rust syntax and highlight every line of `lines`, and
/// the derived µs/line.
fn profile(lines: &[String]) {
    let texts: Vec<&str> = lines.iter().map(String::as_str).collect();
    let t = Instant::now();
    for _ in 0..ITERS {
        let hl = Highlighter::for_path("corpus.rs", None).expect("rust syntax");
        std::hint::black_box(hl.hunk(&texts));
    }
    let total_ms = t.elapsed().as_secs_f64() * 1e3 / ITERS as f64;
    let us_per_line = total_ms * 1e3 / lines.len() as f64;
    println!(
        "{:>7} lines | {:>9.1} ms | {:>6.2} µs/line",
        lines.len(),
        total_ms,
        us_per_line,
    );
}

#[test]
#[ignore = "performance profile; run with --ignored --nocapture"]
fn profile_highlight_backends() {
    // Mirror the per-target engine split in Cargo.toml, plus the explicit
    // `--features onig` force (the only override that wins, since onig takes
    // precedence when both engines are compiled).
    let backend = if cfg!(feature = "onig") {
        "onig (forced)"
    } else if cfg!(feature = "fancy") {
        "fancy (forced)"
    } else if cfg!(all(
        target_arch = "aarch64",
        any(target_os = "linux", target_os = "windows")
    )) {
        "fancy (per-target default)"
    } else {
        "onig (per-target default)"
    };
    println!(
        "\n=== highlight profile (backend: {backend}, {ITERS} iters, release) ===\n\
         Highlighter::for_path + hunk over real Rust source, one reset per call.\n"
    );
    for n in [1_000usize, 5_000, 20_000, 50_000] {
        profile(&corpus(n));
    }
    println!();
}
