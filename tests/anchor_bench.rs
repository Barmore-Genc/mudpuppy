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

use mudpuppy::anchor::{AnchorSig, Params, PreparedFile};

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

/// Mean µs to relocate each of `sigs` against `prepared` under `params`.
fn time_relocate(
    prepared: &PreparedFile,
    sigs: &[AnchorSig],
    anchor_lines: &[u32],
    params: &Params,
) -> f64 {
    let t = Instant::now();
    for _ in 0..ITERS {
        for (sig, &line) in sigs.iter().zip(anchor_lines) {
            std::hint::black_box(prepared.relocate(sig, line, params));
        }
    }
    t.elapsed().as_secs_f64() * 1e6 / (ANCHORS * ITERS) as f64
}

fn profile(content: &str) {
    let line_count = content.lines().count();
    let anchor_lines = pick_anchor_lines(content, ANCHORS);
    let base = Params::default();
    let sigs: Vec<AnchorSig> = anchor_lines
        .iter()
        .map(|&l| AnchorSig::capture(content, l, &base).expect("capture"))
        .collect();
    let edited: Vec<AnchorSig> = sigs.iter().map(edited_sig).collect();

    // Prepare once and reuse — with the cache this happens once per file edit.
    let tp = Instant::now();
    for _ in 0..ITERS {
        std::hint::black_box(PreparedFile::new(content));
    }
    let prepare = tp.elapsed().as_secs_f64() * 1e6 / ITERS as f64;
    let prepared = PreparedFile::new(content);

    // Advanced scan in isolation (fallback off): an *edited* line drives the
    // fuzzy ±advanced_match_window scan.
    let advanced_only = Params {
        fallback_match_window: -1,
        ..base
    };
    let adv = time_relocate(&prepared, &edited, &anchor_lines, &advanced_only);

    // Fallback scan in isolation (advanced off): an *unchanged* line drives the
    // exact ±fallback_match_window scan.
    let fallback_only = Params {
        advanced_match_window: -1,
        ..base
    };
    let fb = time_relocate(&prepared, &sigs, &anchor_lines, &fallback_only);

    // The exact fallback with no window cap, for comparison.
    let fallback_unbounded = Params {
        advanced_match_window: -1,
        fallback_match_window: 0,
        ..base
    };
    let fb_unb = time_relocate(&prepared, &sigs, &anchor_lines, &fallback_unbounded);

    println!(
        "{:>7} lines | prepare {:>7.1} µs | advanced(±{}) {:>7.1} µs/anno | fallback(±{}) {:>6.1} µs/anno | fallback(∞) {:>7.1} µs/anno",
        line_count,
        prepare,
        base.advanced_match_window,
        adv,
        base.fallback_match_window,
        fb,
        fb_unb,
    );
}

#[test]
#[ignore = "performance profile; run with --ignored --nocapture"]
fn profile_relocation_tiers() {
    println!(
        "\n=== anchor relocation profile ({ANCHORS} annotations, {ITERS} iters, release) ===\n\
         PreparedFile built once per file (cached per edit).\n\
         advanced = fuzzy scan of an edited line; fallback = exact scan of an unchanged line.\n"
    );
    for min in [1000usize, 5000, 20000] {
        profile(&corpus(min));
    }
    println!();
}
