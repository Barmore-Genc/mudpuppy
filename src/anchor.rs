//! Change-resilient location anchors for annotations (issue #31).
//!
//! A bare `(file, line)` anchor breaks the moment lines shift or the line is
//! lightly edited: the gutter mark silently lands on the wrong row or vanishes.
//! This module captures a small *signature* of the anchored line plus a little
//! surrounding context at creation time, then **relocates** it in a later
//! version of the file through a cheap-first cascade:
//!
//! 1. **Exact, shift-aware** — if the normalized line text still exists, snap to
//!    the nearest occurrence (disambiguated by context). Handles the common case
//!    (inserted/removed lines above, reorders, moved functions) in a hash lookup.
//! 2. **Fuzzy** — only when the line itself was edited: slide over the file and
//!    score each candidate by token-level edit-distance similarity of the line,
//!    blended with positional context similarity. Accept the best only when it
//!    clears a threshold *and* beats the runner-up by a margin — so repetitive
//!    boilerplate can't lure a confident wrong match.
//!
//! If nothing clears the bar the anchor is [`Outcome::Orphaned`]; the caller
//! re-pins it to the whole file rather than to an unrelated line.
//!
//! The similarity choice (order-aware edit distance, not order-blind set/Jaccard)
//! follows the empirical finding that for source code the edit-distance family
//! carries by far the most signal (Toma, ICSE 2024). Normalization is
//! deliberately language-agnostic — whitespace folding plus a simple
//! identifier/punctuation tokenizer — so there is no per-language parser to
//! maintain; identifier-type abstraction (for heavier rename tolerance) is a
//! possible future refinement.

use serde::{Deserialize, Serialize};

/// Tuning parameters for [`relocate`]. Defaults are the shipped values; kept
/// configurable so the fixture-driven eval (`tests/anchor_eval.rs`) can sweep.
#[derive(Debug, Clone, Copy)]
pub struct Params {
    /// Context lines captured (and compared) on each side of the anchor.
    pub context: usize,
    /// Weight of the anchored line's own similarity in the fuzzy score.
    pub w_line: f64,
    /// Weight of the surrounding context's similarity in the fuzzy score.
    pub w_ctx: f64,
    /// Minimum blended score to accept a fuzzy relocation.
    pub accept: f64,
    /// Minimum lead the best candidate must hold over the runner-up to be
    /// considered unambiguous; guards against boilerplate look-alikes.
    pub margin: f64,
    /// How far the fuzzy (Tier 1) scan ranges around the anchor's original line,
    /// to bound its cost on large files (an edited line rarely migrates far):
    ///
    /// * `> 0` — scan only candidates within ±N lines of the original position.
    /// * `0`   — unbounded; scan the whole file.
    /// * `< 0` — disable the fuzzy tier entirely (exact-match only; otherwise
    ///   orphan).
    ///
    /// Configurable from Lua via `mudpuppy.anchor.set_window(n)`.
    pub fuzzy_window: i64,
}

impl Default for Params {
    fn default() -> Self {
        Params {
            context: 3,
            w_line: 0.6,
            w_ctx: 0.4,
            // Toma (ICSE 2024) finds ~0.8 token-similarity is the "confidently
            // the same, lightly edited" band; below it, prefer orphaning to a
            // wrong match. Tuned against the fixture corpus (tests/anchor_eval).
            accept: 0.80,
            margin: 0.1,
            // An edited line stays near where it was; ±50 keeps the fuzzy scan
            // cheap on huge files without missing realistic moves.
            fuzzy_window: 50,
        }
    }
}

/// A captured signature of an anchored location, persisted on the annotation.
///
/// All text is stored already-normalized (see [`normalize`]) so relocation
/// compares like-for-like and the exact-match tier is a plain string compare.
/// `before` is top-down (`before.last()` is the line immediately above the
/// anchor); `after[0]` is the line immediately below.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AnchorSig {
    /// Normalized text of the anchored line.
    pub line: String,
    /// Up to `Params::context` normalized lines above, in file order.
    #[serde(default)]
    pub before: Vec<String>,
    /// Up to `Params::context` normalized lines below, in file order.
    #[serde(default)]
    pub after: Vec<String>,
}

impl AnchorSig {
    /// Capture a signature for the 1-based `line` of `content`.
    ///
    /// Returns `None` if `line` is out of range, so a caller that can't resolve
    /// file content simply stores no signature (the annotation falls back to its
    /// bare line number).
    pub fn capture(content: &str, line: u32, params: &Params) -> Option<AnchorSig> {
        let lines: Vec<&str> = content.lines().collect();
        let idx = (line as usize).checked_sub(1)?;
        let anchor = lines.get(idx)?;

        let before_start = idx.saturating_sub(params.context);
        let before = lines[before_start..idx]
            .iter()
            .map(|l| normalize(l))
            .collect();

        let after_end = (idx + 1 + params.context).min(lines.len());
        let after = lines[idx + 1..after_end]
            .iter()
            .map(|l| normalize(l))
            .collect();

        Some(AnchorSig {
            line: normalize(anchor),
            before,
            after,
        })
    }
}

/// Where a signature ended up in a (possibly edited) file.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Relocation {
    /// 1-based line the anchor now sits on.
    pub line: u32,
    /// Blended similarity score (`1.0` for an exact-tier match).
    pub score: f64,
    /// Whether this came from the exact tier (line text unchanged) vs. fuzzy.
    pub exact: bool,
}

/// The result of trying to relocate a signature in current file content.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum Outcome {
    /// Confidently placed at a line.
    Located(Relocation),
    /// No confident placement — caller should orphan the annotation
    /// (re-pin it to the whole file).
    Orphaned,
}

/// A file prepared once for relocation: every line normalized and tokenized
/// up front, so relocating any number of annotations against it reuses that
/// work instead of re-normalizing/re-tokenizing the whole file per annotation.
///
/// The benchmark (`tests/anchor_bench.rs`) showed the naive per-call shape spent
/// almost all of its time re-normalizing the file; for N annotations that is N×
/// redundant. Build this once (see [`PreparedCache`] to also skip rebuilding
/// when the file is unchanged) and call [`PreparedFile::relocate`] per annotation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedFile {
    /// Normalized text of each line, in file order.
    norm: Vec<String>,
    /// Tokenized form of each normalized line; `tokens[i]` matches `norm[i]`.
    tokens: Vec<Vec<String>>,
}

impl PreparedFile {
    /// Normalize and tokenize every line of `content`.
    pub fn new(content: &str) -> Self {
        let norm: Vec<String> = content.lines().map(normalize).collect();
        let tokens = norm
            .iter()
            .map(|l| tokenize(l).into_iter().map(str::to_string).collect())
            .collect();
        PreparedFile { norm, tokens }
    }

    /// Number of lines.
    pub fn len(&self) -> usize {
        self.norm.len()
    }

    /// Whether the file is empty.
    pub fn is_empty(&self) -> bool {
        self.norm.is_empty()
    }

    /// Relocate `sig` within this file, given the line it was originally on
    /// (`original_line`, 1-based) for nearest-occurrence tie-breaking.
    pub fn relocate(&self, sig: &AnchorSig, original_line: u32, params: &Params) -> Outcome {
        if self.norm.is_empty() {
            return Outcome::Orphaned;
        }

        // Tokenize the signature once; reused across every candidate below.
        let before_tokens: Vec<Vec<&str>> = sig.before.iter().map(|l| tokenize(l)).collect();
        let after_tokens: Vec<Vec<&str>> = sig.after.iter().map(|l| tokenize(l)).collect();
        let ctx = |i: usize| self.context_score(&before_tokens, &after_tokens, i);

        // Tier 0: exact, shift-aware. The normalized line text still present?
        let exact: Vec<usize> = self
            .norm
            .iter()
            .enumerate()
            .filter(|(_, l)| **l == sig.line && !sig.line.is_empty())
            .map(|(i, _)| i)
            .collect();
        if !exact.is_empty() {
            // Multiple identical lines (repetitive code): pick the one whose
            // context matches best, breaking ties toward the original position.
            let orig0 = original_line.saturating_sub(1) as i64;
            let best = exact
                .iter()
                .copied()
                .max_by(|&a, &b| {
                    ctx(a)
                        .partial_cmp(&ctx(b))
                        .unwrap_or(std::cmp::Ordering::Equal)
                        .then_with(|| {
                            // Closer to the original line wins the tie.
                            let da = (a as i64 - orig0).abs();
                            let db = (b as i64 - orig0).abs();
                            db.cmp(&da)
                        })
                })
                .expect("exact is non-empty");
            return Outcome::Located(Relocation {
                line: best as u32 + 1,
                score: 1.0,
                exact: true,
            });
        }

        // Tier 1: fuzzy. The line was edited (or removed). A negative window
        // disables the fuzzy tier outright; otherwise bound the scan to a window
        // around the original position (an edited line rarely moves far), with 0
        // meaning the whole file.
        if params.fuzzy_window < 0 {
            return Outcome::Orphaned;
        }
        let len = self.tokens.len();
        let (lo, hi) = if params.fuzzy_window == 0 {
            (0, len)
        } else {
            let w = params.fuzzy_window as usize;
            let center = (original_line.saturating_sub(1) as usize).min(len - 1);
            (center.saturating_sub(w), (center + w + 1).min(len))
        };

        let anchor_tokens = tokenize(&sig.line);
        let has_context = !sig.before.is_empty() || !sig.after.is_empty();
        let mut scored: Vec<(usize, f64)> = (lo..hi)
            .map(|i| {
                let line_score = lev_ratio(&anchor_tokens, &self.tokens[i]);
                let total = if has_context {
                    params.w_line * line_score + params.w_ctx * ctx(i)
                } else {
                    line_score
                };
                (i, total)
            })
            .collect();
        scored.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));

        if scored.is_empty() {
            return Outcome::Orphaned;
        }
        let (best_i, best) = scored[0];
        let runner_up = scored.get(1).map(|&(_, s)| s).unwrap_or(0.0);
        if best >= params.accept && (best - runner_up) >= params.margin {
            Outcome::Located(Relocation {
                line: best_i as u32 + 1,
                score: best,
                exact: false,
            })
        } else {
            Outcome::Orphaned
        }
    }

    /// Similarity of the signature's (pre-tokenized) context to the lines
    /// positionally surrounding candidate index `i`, as the mean per-line
    /// edit-distance ratio. Context lines that fall outside the file score 0, so
    /// a candidate near a boundary that loses expected context is penalized.
    /// Returns 0 when the signature has no context.
    fn context_score(&self, before: &[Vec<&str>], after: &[Vec<&str>], i: usize) -> f64 {
        let mut sum = 0.0;
        let mut count = 0usize;

        // `before` is file-ordered; its last element is the line just above `i`.
        for (k, b) in before.iter().enumerate() {
            let offset = before.len() - k;
            let ratio = match i.checked_sub(offset) {
                Some(j) => lev_ratio(b, &self.tokens[j]),
                None => 0.0,
            };
            sum += ratio;
            count += 1;
        }
        for (k, a) in after.iter().enumerate() {
            let j = i + 1 + k;
            let ratio = if j < self.tokens.len() {
                lev_ratio(a, &self.tokens[j])
            } else {
                0.0
            };
            sum += ratio;
            count += 1;
        }

        if count == 0 {
            0.0
        } else {
            sum / count as f64
        }
    }
}

/// A content-addressed cache of [`PreparedFile`]s, keyed by an opaque file key
/// (e.g. its path). [`prepare`](PreparedCache::prepare) rebuilds a file's
/// prepared form only when its contents change — so relocation runs the
/// (linear) prepare pass once per *edit*, not once per render. Pair with the
/// caller's existing reload trigger; the cache itself holds no file watcher.
#[derive(Debug, Default)]
pub struct PreparedCache {
    entries: std::collections::HashMap<String, CacheEntry>,
}

#[derive(Debug)]
struct CacheEntry {
    hash: u64,
    prepared: PreparedFile,
}

impl PreparedCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return a prepared view of `content` stored under `key`, rebuilding only
    /// if the content changed since the last call for that key.
    pub fn prepare(&mut self, key: &str, content: &str) -> &PreparedFile {
        let hash = hash_content(content);
        let stale = self
            .entries
            .get(key)
            .map(|e| e.hash != hash)
            .unwrap_or(true);
        if stale {
            self.entries.insert(
                key.to_string(),
                CacheEntry {
                    hash,
                    prepared: PreparedFile::new(content),
                },
            );
        }
        &self.entries.get(key).expect("just inserted").prepared
    }

    /// Drop any cached entries whose key is not in `live` (e.g. files no longer
    /// under review), so the cache doesn't grow without bound across a session.
    pub fn retain_keys<F: Fn(&str) -> bool>(&mut self, live: F) {
        self.entries.retain(|k, _| live(k));
    }
}

/// Hash file content for cache invalidation. In-memory only (not persisted), so
/// the standard hasher is fine.
fn hash_content(content: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    content.hash(&mut h);
    h.finish()
}

/// Collapse all runs of whitespace to single spaces and trim the ends. This is
/// what gives free tolerance to reindentation and internal spacing changes.
pub fn normalize(line: &str) -> String {
    line.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Split a normalized line into tokens: maximal `[A-Za-z0-9_]` runs (identifiers,
/// keywords, numbers) and individual non-whitespace punctuation characters. The
/// unit of comparison for the edit-distance similarity.
fn tokenize(line: &str) -> Vec<&str> {
    let mut tokens = Vec::new();
    let bytes = line.as_bytes();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if c.is_ascii_whitespace() {
            i += 1;
        } else if is_word_byte(c) {
            let start = i;
            while i < bytes.len() && is_word_byte(bytes[i]) {
                i += 1;
            }
            tokens.push(&line[start..i]);
        } else {
            // A single punctuation byte. Lines are ASCII-normalized source; a
            // multibyte char is rare here and handled as its leading byte slice
            // via char boundaries.
            let ch_len = utf8_len(c);
            let end = (i + ch_len).min(line.len());
            tokens.push(&line[i..end]);
            i = end;
        }
    }
    tokens
}

fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_'
}

/// Byte length of a UTF-8 sequence from its leading byte.
fn utf8_len(lead: u8) -> usize {
    match lead {
        0x00..=0x7F => 1,
        0xC0..=0xDF => 2,
        0xE0..=0xEF => 3,
        _ => 4,
    }
}

/// Edit-distance similarity ratio of two token sequences, in `[0, 1]`:
/// `(|a| + |b| - levenshtein) / (|a| + |b|)`. Two empty sequences are identical.
/// Generic over the token storage so signature tokens (`&str`, borrowed from the
/// signature) compare directly against prepared-file tokens (owned `String`).
fn lev_ratio<A: AsRef<str>, B: AsRef<str>>(a: &[A], b: &[B]) -> f64 {
    let total = a.len() + b.len();
    if total == 0 {
        return 1.0;
    }
    let dist = levenshtein(a, b);
    (total - dist) as f64 / total as f64
}

/// Levenshtein distance between two token sequences (classic two-row DP).
fn levenshtein<A: AsRef<str>, B: AsRef<str>>(a: &[A], b: &[B]) -> usize {
    if a.is_empty() {
        return b.len();
    }
    if b.is_empty() {
        return a.len();
    }
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0usize; b.len() + 1];
    for (i, ta) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, tb) in b.iter().enumerate() {
            let cost = if ta.as_ref() == tb.as_ref() { 0 } else { 1 };
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_folds_whitespace() {
        assert_eq!(normalize("    foo  =   bar( 1 )  "), "foo = bar( 1 )");
        assert_eq!(normalize("\tx\t=\t1"), "x = 1");
    }

    #[test]
    fn tokenize_splits_words_and_punct() {
        assert_eq!(
            tokenize("self.set(\"max_fps\", 144);"),
            vec!["self", ".", "set", "(", "\"", "max_fps", "\"", ",", "144", ")", ";"]
        );
    }

    #[test]
    fn lev_ratio_bounds() {
        let a = tokenize("foo = bar(1)");
        assert_eq!(lev_ratio(&a, &a), 1.0);
        // One substitution over two tokens total.
        assert_eq!(lev_ratio(&tokenize("a"), &tokenize("b")), 0.5);
        // A disjoint sequence vs. empty bottoms out at 0.
        assert_eq!(lev_ratio(&tokenize("a b c"), &tokenize("")), 0.0);
        assert_eq!(lev_ratio(&tokenize(""), &tokenize("")), 1.0);
    }

    #[test]
    fn capture_gathers_context() {
        let content = "a\nb\nc\nd\ne";
        let sig = AnchorSig::capture(content, 3, &Params::default()).unwrap();
        assert_eq!(sig.line, "c");
        assert_eq!(sig.before, vec!["a", "b"]);
        assert_eq!(sig.after, vec!["d", "e"]);
    }

    #[test]
    fn capture_clamps_at_file_edges() {
        let content = "a\nb\nc";
        let sig = AnchorSig::capture(content, 1, &Params::default()).unwrap();
        assert!(sig.before.is_empty());
        assert_eq!(sig.after, vec!["b", "c"]);
        assert!(AnchorSig::capture(content, 99, &Params::default()).is_none());
    }

    /// Convenience: relocate `sig` in raw `content` (builds a one-off
    /// `PreparedFile`), as callers tend to think of it.
    fn relocate_in(sig: &AnchorSig, content: &str, original_line: u32) -> Outcome {
        PreparedFile::new(content).relocate(sig, original_line, &Params::default())
    }

    #[test]
    fn relocates_identical_line_shifted_down() {
        let orig = "fn a() {}\nlet target = compute(x);\nfn b() {}";
        let sig = AnchorSig::capture(orig, 2, &Params::default()).unwrap();
        let edited = "use std;\nfn a() {}\nlet helper = 1;\nlet target = compute(x);\nfn b() {}";
        match relocate_in(&sig, edited, 2) {
            Outcome::Located(r) => {
                assert_eq!(r.line, 4);
                assert!(r.exact);
            }
            other => panic!("expected located, got {other:?}"),
        }
    }

    #[test]
    fn relocates_edited_line_via_fuzzy() {
        let orig = "fn a() {}\nlet target = compute(x);\nfn b() {}";
        let sig = AnchorSig::capture(orig, 2, &Params::default()).unwrap();
        // Rename `target` -> `result`: one token differs, context intact.
        let edited = "fn a() {}\nlet result = compute(x);\nfn b() {}";
        match relocate_in(&sig, edited, 2) {
            Outcome::Located(r) => {
                assert_eq!(r.line, 2);
                assert!(!r.exact);
            }
            other => panic!("expected located, got {other:?}"),
        }
    }

    #[test]
    fn orphans_when_line_deleted_and_no_close_match() {
        let orig = "fn a() {}\nlet target = compute(x);\nreturn done();";
        let sig = AnchorSig::capture(orig, 2, &Params::default()).unwrap();
        let edited = "fn a() {}\nreturn done();";
        assert_eq!(relocate_in(&sig, edited, 2), Outcome::Orphaned);
    }

    #[test]
    fn fuzzy_window_bounds_the_scan() {
        // Two near-identical edited candidates: one near the original line, one
        // far away. A tight window must only consider the near one.
        let line = "let total = price * qty;";
        let mut content = vec!["// header"];
        content.push(line); // L2 (original anchor)
        content.extend(std::iter::repeat_n("filler();", 200));
        content.push("let total = price * qtys;"); // far edited copy (~L203)
        let content = content.join("\n");

        let mut sig = AnchorSig::capture(&content, 2, &Params::default()).unwrap();
        // Edit the anchor so the exact tier misses and Tier 1 runs.
        sig.line = normalize("let total = price * quantity;");

        // Tight window around L2: the far copy at ~L203 is out of range, so the
        // near (now-deleted exact) line region is all that's scanned.
        let near = Params {
            fuzzy_window: 5,
            ..Params::default()
        };
        match PreparedFile::new(&content).relocate(&sig, 2, &near) {
            Outcome::Located(r) => assert!(r.line <= 8, "expected a near match, got L{}", r.line),
            // Orphaning is also acceptable here (no good near match); the point is
            // it must NOT jump to the far copy.
            Outcome::Orphaned => {}
        }

        // Negative window disables the fuzzy tier: an edited line always orphans.
        let disabled = Params {
            fuzzy_window: -1,
            ..Params::default()
        };
        assert_eq!(
            PreparedFile::new(&content).relocate(&sig, 2, &disabled),
            Outcome::Orphaned
        );
    }

    #[test]
    fn prepared_file_reused_matches_oneoff() {
        // A shared PreparedFile must produce the same answer as building one
        // per call — the precompute is a pure optimization.
        let content = "fn a() {}\nlet x = f(1);\nlet y = g(2);\nfn b() {}";
        let prepared = PreparedFile::new(content);
        for line in 1..=prepared.len() as u32 {
            let sig = AnchorSig::capture(content, line, &Params::default()).unwrap();
            assert_eq!(
                prepared.relocate(&sig, line, &Params::default()),
                relocate_in(&sig, content, line),
            );
        }
    }

    #[test]
    fn cache_rebuilds_only_on_change() {
        let mut cache = PreparedCache::new();
        let v1 = "let a = 1;\nlet b = 2;";
        let p1 = cache.prepare("f.rs", v1).clone();
        // Same content → same prepared form (and, in practice, no rebuild).
        assert_eq!(cache.prepare("f.rs", v1), &p1);
        // Changed content → prepared form reflects the new lines.
        let v2 = "let a = 1;\nlet b = 2;\nlet c = 3;";
        let p2 = cache.prepare("f.rs", v2);
        assert_eq!(p2.len(), 3);
        assert_ne!(p2, &p1);
        // Eviction of keys no longer in use.
        cache.retain_keys(|k| k != "f.rs");
        assert_eq!(cache.prepare("f.rs", v1).len(), 2);
    }
}
