//! Performance profile of the two relocation tiers (`mudpuppy::anchor`).
//!
//! Profiles 50 annotations against files of growing size, up to ~20k lines
//! (built by concatenating real source — relocation is line-oriented, so the
//! result need not be syntactically valid). Marked `#[ignore]` so it stays out
//! of the normal suite (timings are environment-sensitive). Run explicitly:
//!
//! ```text
//! cargo test --release --test anchor_bench -- --ignored --nocapture
//! ```

use std::time::Instant;

use mudpuppy::anchor::{relocate, AnchorSig, Params};

const ANCHORS: usize = 50;
const ITERS: usize = 10;

fn read(rel: &str) -> String {
    let path = format!("{}/{}", env!("CARGO_MANIFEST_DIR"), rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {path}: {e}"))
}

/// Build a corpus of at least `min_lines` by concatenating real source files,
/// repeating the pool as needed.
fn corpus(min_lines: usize) -> String {
    let pool = [
        "src/tui/app.rs",
        "src/tui/render.rs",
        "src/lua/mod.rs",
        "src/agent.rs",
        "src/diff.rs",
        "src/lua/api.rs",
    ];
    let mut out = String::new();
    let mut lines = 0;
    let mut i = 0;
    while lines < min_lines {
        let chunk = read(pool[i % pool.len()]);
        lines += chunk.lines().count();
        out.push_str(&chunk);
        out.push('\n');
        i += 1;
    }
    out
}

/// Pick `n` non-blank line numbers (1-based) spread evenly across the file.
fn pick_anchor_lines(content: &str, n: usize) -> Vec<u32> {
    let lines: Vec<&str> = content.lines().collect();
    let total = lines.len();
    let mut out = Vec::with_capacity(n);
    for k in 0..n {
        let mut idx = (k * total) / n;
        while idx < total && lines[idx].trim().is_empty() {
            idx += 1;
        }
        if idx < total {
            out.push(idx as u32 + 1);
        }
    }
    out
}

/// A signature whose anchored line is edited so the exact tier misses and the
/// fuzzy tier runs the full scan.
fn edited_sig(sig: &AnchorSig) -> AnchorSig {
    let mut s = sig.clone();
    s.line.push_str(" /* edited */");
    s
}

fn profile(content: &str, params: &Params) {
    let line_count = content.lines().count();
    let anchor_lines = pick_anchor_lines(content, ANCHORS);
    let sigs: Vec<AnchorSig> = anchor_lines
        .iter()
        .map(|&l| AnchorSig::capture(content, l, params).expect("capture"))
        .collect();
    let edited: Vec<AnchorSig> = sigs.iter().map(edited_sig).collect();

    // Tier 0: exact (line text unchanged).
    let t0 = Instant::now();
    for _ in 0..ITERS {
        for (sig, &line) in sigs.iter().zip(&anchor_lines) {
            std::hint::black_box(relocate(sig, content, line, params));
        }
    }
    let tier0 = t0.elapsed();

    // Tier 1: fuzzy (line edited, full sliding-window scan).
    let t1 = Instant::now();
    for _ in 0..ITERS {
        for (sig, &line) in edited.iter().zip(&anchor_lines) {
            std::hint::black_box(relocate(sig, content, line, params));
        }
    }
    let tier1 = t1.elapsed();

    // How much of the per-call cost is just re-normalizing the whole file?
    // (This is the work a precomputed, shared "prepared file" would do once.)
    let tn = Instant::now();
    for _ in 0..ITERS {
        let normed: Vec<String> = content.lines().map(mudpuppy::anchor::normalize).collect();
        std::hint::black_box(normed);
    }
    let norm_once = tn.elapsed().as_secs_f64() * 1e6 / ITERS as f64;

    let calls = (ANCHORS * ITERS) as f64;
    let t0_us = tier0.as_secs_f64() * 1e6 / calls;
    let t1_us = tier1.as_secs_f64() * 1e6 / calls;
    let t0_batch = tier0.as_secs_f64() * 1e3 / ITERS as f64;
    let t1_batch = tier1.as_secs_f64() * 1e3 / ITERS as f64;

    println!(
        "{:>7} lines | Tier0 {:>8.1} µs/anno ({:>7.1} ms/50) | Tier1 {:>9.1} µs/anno ({:>8.1} ms/50) | {:.0}x | normalize-once {:.0} µs",
        line_count,
        t0_us,
        t0_batch,
        t1_us,
        t1_batch,
        t1_us / t0_us,
        norm_once,
    );
}

#[test]
#[ignore = "performance profile; run with --ignored --nocapture"]
fn profile_relocation_tiers() {
    let params = Params::default();
    println!(
        "\n=== anchor relocation profile ({ANCHORS} annotations, {ITERS} iters, release) ===\n\
         Tier0 = exact (line unchanged); Tier1 = fuzzy full scan (every line edited)\n"
    );
    for min in [1000usize, 5000, 20000] {
        profile(&corpus(min), &params);
    }
    println!();
}
