//! The `:command` palette: state plus fuzzy filtering over registered command
//! names. Mirrors [`crate::picker::Picker`] (and reuses its
//! [`fuzzy_match`](crate::picker::fuzzy_match) matcher) — the TUI owns rendering
//! and key handling; this module is the pure, testable core.

use crate::picker::fuzzy_match;

/// Overlay state for the command palette. `filtered` holds indices into `all`,
/// best-ranked first; `selected` indexes into `filtered`.
pub struct CommandPalette {
    pub query: String,
    pub all: Vec<String>,
    pub filtered: Vec<usize>,
    pub selected: usize,
}

impl CommandPalette {
    /// Build a palette over the registered command names (already sorted by the
    /// caller for a stable initial order).
    pub fn new(all: Vec<String>) -> CommandPalette {
        let filtered = (0..all.len()).collect();
        CommandPalette {
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
            // Higher score first; ties broken by shorter name, then original order.
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

    /// The command name under the cursor, if any.
    pub fn current_name(&self) -> Option<&str> {
        self.filtered
            .get(self.selected)
            .map(|&i| self.all[i].as_str())
    }

    /// The top-ranked name (used by Tab autocomplete), if any.
    pub fn top_name(&self) -> Option<&str> {
        self.filtered.first().map(|&i| self.all[i].as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_query_keeps_all_in_order() {
        let p = CommandPalette::new(vec!["quit".to_string(), "help".to_string()]);
        assert_eq!(p.filtered, vec![0, 1]);
        assert_eq!(p.current_name(), Some("quit"));
    }

    #[test]
    fn filters_and_ranks_by_fuzzy_match() {
        let mut p = CommandPalette::new(vec![
            "release-turn".to_string(),
            "comment-line".to_string(),
            "comment-file".to_string(),
        ]);
        p.query = "cf".to_string();
        p.refilter();
        // Only the comment commands match "c…f…"; comment-file ranks first.
        assert_eq!(p.current_name(), Some("comment-file"));
        assert!(!p.filtered.iter().any(|&i| p.all[i] == "release-turn"));
    }

    #[test]
    fn top_name_drives_tab_autocomplete() {
        let mut p = CommandPalette::new(vec!["goto-top".to_string(), "goto-bottom".to_string()]);
        p.query = "gob".to_string();
        p.refilter();
        assert_eq!(p.top_name(), Some("goto-bottom"));
    }
}
