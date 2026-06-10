//! Fixture-driven evaluation of the location-anchor relocation engine
//! (`mudpuppy::anchor`). For each fixture in `tests/fixtures/anchor/*.json` we
//! capture a signature on the original file's anchored line, then relocate it in
//! every mutated variant and classify the result.
//!
//! This is a *measurement* harness, not a strict gate: the corpus deliberately
//! includes near-miss and boilerplate-trap cases that tell us whether the
//! approach is worth shipping. Run `cargo test --test anchor_eval -- --nocapture`
//! to see the full per-case report. A conservative aggregate floor guards
//! against gross regressions without pinning the known-hard cases.

use std::fs;
use std::path::PathBuf;

use mudpuppy::anchor::{AnchorSig, Outcome, Params, PreparedFile};
use serde::Deserialize;

#[derive(Debug, Deserialize)]
struct Fixture {
    language: String,
    original: String,
    anchor_line: u32,
    anchor_text: String,
    cases: Vec<Case>,
}

#[derive(Debug, Deserialize)]
struct Case {
    name: String,
    category: String, // "survive" | "orphan"
    modified: String,
    expect_line: Option<u32>,
    expect_text: Option<String>,
    #[allow(dead_code)]
    note: String,
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Verdict {
    /// Located exactly where expected (text matches).
    Correct,
    /// Located, but on the wrong line.
    WrongLine,
    /// Expected a location but orphaned it.
    FalseOrphan,
    /// Expected an orphan and got one.
    CorrectOrphan,
    /// Expected an orphan but confidently matched — the dangerous failure.
    FalseMatch,
}

fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/anchor")
}

fn load_fixtures() -> Vec<Fixture> {
    let dir = fixtures_dir();
    let mut out = Vec::new();
    for entry in fs::read_dir(&dir).expect("fixtures dir") {
        let path = entry.unwrap().path();
        if path.extension().and_then(|e| e.to_str()) != Some("json") {
            continue;
        }
        let text = fs::read_to_string(&path).expect("read fixture");
        let fx: Fixture =
            serde_json::from_str(&text).unwrap_or_else(|e| panic!("parse {path:?}: {e}"));
        out.push(fx);
    }
    out.sort_by(|a, b| a.language.cmp(&b.language));
    out
}

/// Line `n` (1-based) of `content`, if present.
fn line_at(content: &str, n: u32) -> Option<String> {
    content
        .lines()
        .nth(n.checked_sub(1)? as usize)
        .map(str::to_string)
}

fn evaluate(case: &Case, outcome: Outcome) -> Verdict {
    let expects_location = case.category == "survive";
    match (expects_location, outcome) {
        (true, Outcome::Located(r)) => {
            // Validate by text so a fixture off-by-one in line numbering can't
            // mask a real relocation. `expect_text` is authoritative.
            let got = line_at(&case.modified, r.line);
            if got.as_deref() == case.expect_text.as_deref() {
                Verdict::Correct
            } else {
                Verdict::WrongLine
            }
        }
        (true, Outcome::Orphaned) => Verdict::FalseOrphan,
        (false, Outcome::Located(_)) => Verdict::FalseMatch,
        (false, Outcome::Orphaned) => Verdict::CorrectOrphan,
    }
}

#[test]
fn anchor_relocation_eval() {
    let params = Params::default();
    let fixtures = load_fixtures();
    assert!(
        !fixtures.is_empty(),
        "no fixtures found in {:?}",
        fixtures_dir()
    );

    let mut total = 0;
    let mut correct = 0;
    let mut wrong_line = 0;
    let mut false_orphan = 0;
    let mut correct_orphan = 0;
    let mut false_match = 0;

    println!("\n=== anchor relocation eval ({params:?}) ===");
    for fx in &fixtures {
        let sig = AnchorSig::capture(&fx.original, fx.anchor_line, &params)
            .unwrap_or_else(|| panic!("{}: could not capture anchor", fx.language));
        // The fixtures pin the anchor by text; make sure we captured that line.
        assert_eq!(
            sig.line,
            mudpuppy::anchor::normalize(&fx.anchor_text),
            "{}: anchor_line/anchor_text mismatch",
            fx.language
        );

        println!(
            "\n[{}] anchored on L{}: {}",
            fx.language, fx.anchor_line, fx.anchor_text
        );
        for case in &fx.cases {
            let outcome = PreparedFile::new(&case.modified).relocate(&sig, fx.anchor_line, &params);
            let verdict = evaluate(case, outcome);
            total += 1;
            match verdict {
                Verdict::Correct => correct += 1,
                Verdict::WrongLine => wrong_line += 1,
                Verdict::FalseOrphan => false_orphan += 1,
                Verdict::CorrectOrphan => correct_orphan += 1,
                Verdict::FalseMatch => false_match += 1,
            }
            let mark = match verdict {
                Verdict::Correct | Verdict::CorrectOrphan => "ok  ",
                Verdict::WrongLine | Verdict::FalseOrphan => "MISS",
                Verdict::FalseMatch => "BAD ",
            };
            let detail = match outcome {
                Outcome::Located(r) => {
                    format!("-> L{} (score {:.2}, exact={})", r.line, r.score, r.exact)
                }
                Outcome::Orphaned => "-> orphaned".to_string(),
            };
            let want = match case.expect_line {
                Some(l) => format!("want L{l}"),
                None => "want orphan".to_string(),
            };
            println!(
                "  {mark} {:<22} {:<10} {detail:<34} [{:?}]",
                case.name, want, verdict
            );
        }
    }

    let located = correct + wrong_line + false_orphan; // "survive" cases
    let orphan_cases = correct_orphan + false_match; // "orphan" cases
    println!("\n--- totals ({total} cases) ---");
    println!("  survive cases:  {located}");
    println!("    correct:      {correct}");
    println!("    wrong line:   {wrong_line}");
    println!("    false orphan: {false_orphan}");
    println!("  orphan cases:   {orphan_cases}");
    println!("    correct:      {correct_orphan}");
    println!("    FALSE MATCH:  {false_match}   (anchored to unrelated code)");
    let rate = 100.0 * (correct + correct_orphan) as f64 / total as f64;
    println!("  overall correct: {correct}+{correct_orphan} / {total} = {rate:.0}%\n");

    // Conservative floors, not tight gates. False matches are the genuinely
    // harmful outcome (a comment silently re-anchored to wrong code); allow a
    // small number for the boilerplate-trap stress cases, but not a flood.
    assert!(
        false_match <= 2,
        "too many false matches ({false_match}): anchors latching onto unrelated code"
    );
    assert!(
        rate >= 70.0,
        "overall correctness {rate:.0}% below floor; relocation approach regressed"
    );
}
