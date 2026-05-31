//! The "add any file" fuzzy-find picker: state plus a small subsequence
//! matcher. The TUI owns rendering and key handling; this module is the pure,
//! testable core (filtering and ranking).

/// Overlay state for the file picker. `filtered` holds indices into `all`,
/// best-ranked first; `selected` indexes into `filtered`.
pub struct Picker {
    pub query: String,
    pub all: Vec<String>,
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl Picker {
    pub fn new(all: Vec<String>) -> Picker {
        let filtered = (0..all.len()).collect();
        Picker {
            query: String::new(),
            all,
            filtered,
            selected: 0,
        }
    }

    /// Recompute `filtered` from the current query and clamp `selected`.
    pub fn refilter(&mut self) {
        if self.query.is_empty() {
            self.filtered = (0..self.all.len()).collect();
        } else {
            let mut scored: Vec<(i64, usize)> = self
                .all
                .iter()
                .enumerate()
                .filter_map(|(i, cand)| fuzzy_match(&self.query, cand).map(|m| (m.score, i)))
                .collect();
            // Higher score first; ties broken by shorter path, then original order.
            scored.sort_by(|a, b| {
                b.0.cmp(&a.0)
                    .then_with(|| self.all[a.1].len().cmp(&self.all[b.1].len()))
                    .then_with(|| a.1.cmp(&b.1))
            });
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }
        self.clamp_selected();
    }

    fn clamp_selected(&mut self) {
        if self.filtered.is_empty() {
            self.selected = 0;
        } else if self.selected >= self.filtered.len() {
            self.selected = self.filtered.len() - 1;
        }
    }

    pub fn move_down(&mut self) {
        if !self.filtered.is_empty() && self.selected + 1 < self.filtered.len() {
            self.selected += 1;
        }
    }

    pub fn move_up(&mut self) {
        self.selected = self.selected.saturating_sub(1);
    }

    /// The path under the cursor, if any.
    pub fn current_path(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .map(|&i| self.all[i].as_str())
    }
}

/// A successful subsequence match: which byte positions in the candidate were
/// matched, plus a rank score (higher is better).
pub struct Match {
    pub positions: Vec<usize>,
    pub score: i64,
}

/// Case-insensitive subsequence match: every char of `query` must appear in
/// `candidate` in order. Rewards contiguous runs and earlier matches, and
/// lightly penalizes longer candidates so a tight match beats a sprawling one.
pub fn fuzzy_match(query: &str, candidate: &str) -> Option<Match> {
    if query.is_empty() {
        return Some(Match {
            positions: Vec::new(),
            score: 0,
        });
    }

    let cand: Vec<(usize, char)> = candidate
        .char_indices()
        .map(|(i, c)| (i, c.to_ascii_lowercase()))
        .collect();
    let mut positions = Vec::new();
    let mut score: i64 = 0;
    let mut ci = 0;
    let mut prev_match: Option<usize> = None;

    for qc in query.chars() {
        let qc = qc.to_ascii_lowercase();
        let mut found = false;
        while ci < cand.len() {
            let (byte, c) = cand[ci];
            if c == qc {
                positions.push(byte);
                // Reward contiguous runs; reward matching right after a path
                // separator (start of a path component).
                if prev_match == Some(ci.wrapping_sub(1)) {
                    score += 8;
                }
                if ci == 0 || matches!(cand[ci - 1].1, '/' | '_' | '-' | '.') {
                    score += 5;
                }
                // Earlier matches score slightly higher.
                score += (100 - (ci as i64).min(100)) / 10;
                prev_match = Some(ci);
                ci += 1;
                found = true;
                break;
            }
            ci += 1;
        }
        if !found {
            return None;
        }
    }

    // Slight penalty for length so a closer/shorter path wins on ties.
    score -= candidate.len() as i64 / 8;
    Some(Match { positions, score })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_subsequence_in_order() {
        assert!(fuzzy_match("fb", "foo/bar").is_some());
        assert!(fuzzy_match("foo", "foo/bar").is_some());
        // Out-of-order chars do not match.
        assert!(fuzzy_match("bf", "foo/bar").is_none());
        // Missing chars do not match.
        assert!(fuzzy_match("xyz", "foo/bar").is_none());
    }

    #[test]
    fn case_insensitive() {
        assert!(fuzzy_match("FB", "foo/bar").is_some());
        assert!(fuzzy_match("fb", "FOO/BAR").is_some());
    }

    #[test]
    fn closer_match_ranks_higher() {
        // "fb" should rank a path where f and b are component starts above one
        // where the chars are buried.
        let close = fuzzy_match("fb", "foo/bar").unwrap();
        let far = fuzzy_match("fb", "afoo_xbqz").unwrap();
        assert!(close.score > far.score, "{} !> {}", close.score, far.score);
    }

    #[test]
    fn refilter_ranks_best_first() {
        let mut p = Picker::new(vec![
            "src/zebra/foobar.rs".to_string(),
            "fb.rs".to_string(),
            "unrelated.txt".to_string(),
        ]);
        p.query = "fb".to_string();
        p.refilter();
        // Only the two matching entries survive, best first.
        assert_eq!(p.filtered.len(), 2);
        assert_eq!(p.all[p.filtered[0]], "fb.rs");
    }

    #[test]
    fn empty_query_keeps_original_order() {
        let p = Picker::new(vec!["b.rs".to_string(), "a.rs".to_string()]);
        assert_eq!(p.filtered, vec![0, 1]);
        assert_eq!(p.current_path(), Some("b.rs"));
    }

    #[test]
    fn match_positions_recorded() {
        let m = fuzzy_match("fb", "foo/bar").unwrap();
        // f at 0, b at 4.
        assert_eq!(m.positions, vec![0, 4]);
    }
}
