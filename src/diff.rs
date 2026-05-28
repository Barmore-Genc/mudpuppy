//! Hand-rolled unified-diff parser (PLAN.md §8).
//!
//! Turns the raw output of `git diff` / `gh pr diff` into a list of
//! [`FileDiff`]s, each tagged with its [`FileStatus`] and `+`/`-` counts. Hunk
//! bodies are parsed **lazily**: the first pass over a file is a cheap scan that
//! records its paths, status, and line counts, and the per-line
//! `LEFT`/`RIGHT` numbering (the anchoring map) is only materialized when a
//! caller asks for [`FileDiff::hunks`]. That keeps a 50k-line diff from being
//! fully structured up front — we only pay for files the user actually opens.
//!
//! The parser is tuned to exactly the shape git emits; it is not a general
//! patch parser. It deliberately does not handle combined (merge) diffs, since
//! mudpuppy never reviews those.

/// Whether a diff line was unchanged, added, or removed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineKind {
    /// Unchanged line present on both sides.
    Context,
    /// Line added on the new (`RIGHT`) side.
    Addition,
    /// Line removed from the old (`LEFT`) side.
    Deletion,
}

/// A single line within a hunk, carrying its number on each side it exists on.
///
/// A context line has both numbers; an addition has only `new_lineno`; a
/// deletion has only `old_lineno`. This pairing is the line ↔ `(side, number)`
/// mapping annotations anchor against.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffLine {
    /// Whether the line is context, an addition, or a deletion.
    pub kind: LineKind,
    /// Line text, without the leading `+`/`-`/space marker or trailing newline.
    pub content: String,
    /// 1-based line number on the old (`LEFT`) side, if the line exists there.
    pub old_lineno: Option<u32>,
    /// 1-based line number on the new (`RIGHT`) side, if the line exists there.
    pub new_lineno: Option<u32>,
    /// True when git reported "\\ No newline at end of file" for this line.
    pub no_newline: bool,
}

/// One `@@ … @@` hunk: a contiguous changed region with its line ranges.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hunk {
    /// 1-based start line on the old side (0 for an empty old range).
    pub old_start: u32,
    /// Number of old-side lines the hunk spans.
    pub old_count: u32,
    /// 1-based start line on the new side (0 for an empty new range).
    pub new_start: u32,
    /// Number of new-side lines the hunk spans.
    pub new_count: u32,
    /// Trailing context after the second `@@` (often the enclosing function).
    pub section: String,
    /// The hunk's lines, in order.
    pub lines: Vec<DiffLine>,
}

/// How a file changed between the two sides of the diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FileStatus {
    /// File exists only on the new side.
    Added,
    /// File exists only on the old side.
    Deleted,
    /// File changed in place.
    Modified,
    /// File moved; `old_path` and `new_path` differ.
    Renamed,
}

/// A single file's portion of the diff.
///
/// `additions`/`deletions`/`status`/paths are computed in the first cheap pass;
/// the structured hunks are parsed on demand from [`FileDiff::hunks`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileDiff {
    /// Path on the old side (`None` for a freshly added file).
    pub old_path: Option<String>,
    /// Path on the new side (`None` for a deleted file).
    pub new_path: Option<String>,
    /// How the file changed.
    pub status: FileStatus,
    /// True for a binary file (git emits "Binary files … differ", no hunks).
    pub is_binary: bool,
    /// Count of added (`+`) lines across all hunks.
    pub additions: u32,
    /// Count of removed (`-`) lines across all hunks.
    pub deletions: u32,
    /// Raw text of this file's section, from just after the `diff --git` line
    /// through the line before the next file. Hunks are parsed from here.
    body: String,
}

impl FileDiff {
    /// The path to show for this file: the new path, except for a deletion,
    /// where only the old path exists.
    pub fn display_path(&self) -> &str {
        match self.status {
            FileStatus::Deleted => self.old_path.as_deref().unwrap_or("(unknown)"),
            _ => self
                .new_path
                .as_deref()
                .or(self.old_path.as_deref())
                .unwrap_or("(unknown)"),
        }
    }

    /// Parse this file's hunks on demand, building the per-line `LEFT`/`RIGHT`
    /// numbering. Returns an empty vec for binary files and pure mode/rename
    /// changes (which carry no hunks).
    pub fn hunks(&self) -> Vec<Hunk> {
        parse_hunks(&self.body)
    }
}

/// Parse a full unified diff into its constituent files.
///
/// Splits on `diff --git` boundaries; anything before the first boundary (there
/// should be nothing for `git diff`) is ignored. Each file is scanned cheaply
/// for its paths, status, binary flag, and `+`/`-` counts; hunk bodies are left
/// unparsed until [`FileDiff::hunks`] is called.
pub fn parse_diff(raw: &str) -> Vec<FileDiff> {
    let mut files = Vec::new();
    let mut lines = raw.lines().peekable();

    // Skip anything before the first file header.
    while let Some(line) = lines.peek() {
        if line.starts_with("diff --git ") {
            break;
        }
        lines.next();
    }

    while let Some(header) = lines.next() {
        if !header.starts_with("diff --git ") {
            continue;
        }
        // Collect this file's lines up to (but not including) the next header.
        let mut body = String::new();
        while let Some(line) = lines.peek() {
            if line.starts_with("diff --git ") {
                break;
            }
            body.push_str(lines.next().unwrap());
            body.push('\n');
        }
        files.push(build_file(header, body));
    }

    files
}

/// Build a [`FileDiff`] from its `diff --git` header line and section body,
/// doing the cheap up-front scan (paths, status, binary, counts).
fn build_file(header: &str, body: String) -> FileDiff {
    let mut old_path = None;
    let mut new_path = None;
    let mut status = FileStatus::Modified;
    let mut is_binary = false;
    let mut additions = 0;
    let mut deletions = 0;
    let mut in_hunk = false;

    for line in body.lines() {
        if let Some(rest) = line.strip_prefix("@@") {
            // A second "@@" closes the range; ignore failures and move on.
            let _ = rest;
            in_hunk = true;
        } else if in_hunk {
            // Inside a hunk, classify by the leading marker.
            match line.as_bytes().first() {
                Some(b'+') => additions += 1,
                Some(b'-') => deletions += 1,
                _ => {}
            }
        } else if line.starts_with("new file mode") {
            status = FileStatus::Added;
        } else if line.starts_with("deleted file mode") {
            status = FileStatus::Deleted;
        } else if let Some(p) = line.strip_prefix("rename from ") {
            old_path = Some(p.to_string());
            status = FileStatus::Renamed;
        } else if let Some(p) = line.strip_prefix("rename to ") {
            new_path = Some(p.to_string());
            status = FileStatus::Renamed;
        } else if line.starts_with("Binary files ") {
            is_binary = true;
        } else if let Some(p) = line.strip_prefix("--- ") {
            old_path = path_from_marker(p);
        } else if let Some(p) = line.strip_prefix("+++ ") {
            new_path = path_from_marker(p);
        }
    }

    // `---`/`+++` against /dev/null pin down add/delete even without a mode line.
    if old_path.is_none() && new_path.is_some() && status == FileStatus::Modified {
        status = FileStatus::Added;
    } else if new_path.is_none() && old_path.is_some() && status == FileStatus::Modified {
        status = FileStatus::Deleted;
    }

    // Fall back to the `diff --git a/… b/…` header if we learned nothing else
    // (e.g. a pure mode change carries no `---`/`+++`/rename lines).
    if old_path.is_none() && new_path.is_none() {
        if let Some((a, b)) = paths_from_git_header(header) {
            old_path = Some(a);
            new_path = Some(b);
        }
    }

    FileDiff {
        old_path,
        new_path,
        status,
        is_binary,
        additions,
        deletions,
        body,
    }
}

/// Extract a path from a `--- `/`+++ ` marker line, stripping the `a/`/`b/`
/// prefix git adds. `/dev/null` (a created or deleted side) yields `None`.
fn path_from_marker(marker: &str) -> Option<String> {
    // git may append a tab + timestamp; the path ends at the first tab.
    let raw = marker.split('\t').next().unwrap_or(marker);
    if raw == "/dev/null" {
        return None;
    }
    let stripped = raw
        .strip_prefix("a/")
        .or_else(|| raw.strip_prefix("b/"))
        .unwrap_or(raw);
    Some(stripped.to_string())
}

/// Best-effort path extraction from `diff --git a/PATH b/PATH`. Only reliable
/// when the path has no spaces; used purely as a fallback when no richer header
/// line (`---`/`+++`/rename) was present.
fn paths_from_git_header(header: &str) -> Option<(String, String)> {
    let rest = header.strip_prefix("diff --git ")?;
    let (a, b) = rest.split_once(" b/")?;
    let a = a.strip_prefix("a/").unwrap_or(a);
    Some((a.to_string(), b.to_string()))
}

/// Parse the hunks out of a file section body, assigning each line its
/// `LEFT`/`RIGHT` number.
fn parse_hunks(body: &str) -> Vec<Hunk> {
    let mut hunks: Vec<Hunk> = Vec::new();
    let mut old_no = 0u32;
    let mut new_no = 0u32;

    for line in body.lines() {
        if line.starts_with("@@") {
            if let Some(hunk) = parse_hunk_header(line) {
                old_no = hunk.old_start;
                new_no = hunk.new_start;
                hunks.push(hunk);
            }
            continue;
        }

        let Some(hunk) = hunks.last_mut() else {
            // Lines before the first @@ (file headers) aren't hunk content.
            continue;
        };

        match line.as_bytes().first() {
            Some(b'+') => {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Addition,
                    content: line[1..].to_string(),
                    old_lineno: None,
                    new_lineno: Some(new_no),
                    no_newline: false,
                });
                new_no += 1;
            }
            Some(b'-') => {
                hunk.lines.push(DiffLine {
                    kind: LineKind::Deletion,
                    content: line[1..].to_string(),
                    old_lineno: Some(old_no),
                    new_lineno: None,
                    no_newline: false,
                });
                old_no += 1;
            }
            Some(b'\\') => {
                // "\ No newline at end of file" annotates the preceding line.
                if let Some(last) = hunk.lines.last_mut() {
                    last.no_newline = true;
                }
            }
            // A leading space is context; a fully empty line is context too
            // (git emits a bare "" for a blank context line in some tools).
            _ => {
                let content = line.strip_prefix(' ').unwrap_or(line).to_string();
                hunk.lines.push(DiffLine {
                    kind: LineKind::Context,
                    content,
                    old_lineno: Some(old_no),
                    new_lineno: Some(new_no),
                    no_newline: false,
                });
                old_no += 1;
                new_no += 1;
            }
        }
    }

    hunks
}

/// Parse a `@@ -old,oldcount +new,newcount @@ section` header. Counts default
/// to 1 when omitted (git's convention for single-line ranges).
fn parse_hunk_header(line: &str) -> Option<Hunk> {
    let rest = line.strip_prefix("@@ ")?;
    let (ranges, section) = match rest.split_once(" @@") {
        Some((r, s)) => (r, s.strip_prefix(' ').unwrap_or(s).to_string()),
        None => return None,
    };
    let (old_part, new_part) = ranges.split_once(' ')?;
    let (old_start, old_count) = parse_range(old_part.strip_prefix('-')?)?;
    let (new_start, new_count) = parse_range(new_part.strip_prefix('+')?)?;
    Some(Hunk {
        old_start,
        old_count,
        new_start,
        new_count,
        section,
        lines: Vec::new(),
    })
}

/// Parse a `start` or `start,count` range; count defaults to 1.
fn parse_range(part: &str) -> Option<(u32, u32)> {
    match part.split_once(',') {
        Some((start, count)) => Some((start.parse().ok()?, count.parse().ok()?)),
        None => Some((part.parse().ok()?, 1)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_a_simple_modification() {
        let raw = "\
diff --git a/src/lib.rs b/src/lib.rs
index 1234567..89abcde 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,4 +1,5 @@ fn main() {
 context one
-removed line
+added line
+another added
 context two
";
        let files = parse_diff(raw);
        assert_eq!(files.len(), 1);
        let f = &files[0];
        assert_eq!(f.status, FileStatus::Modified);
        assert_eq!(f.display_path(), "src/lib.rs");
        assert_eq!(f.additions, 2);
        assert_eq!(f.deletions, 1);
        assert!(!f.is_binary);

        let hunks = f.hunks();
        assert_eq!(hunks.len(), 1);
        let h = &hunks[0];
        assert_eq!((h.old_start, h.old_count), (1, 4));
        assert_eq!((h.new_start, h.new_count), (1, 5));
        assert_eq!(h.section, "fn main() {");
        assert_eq!(h.lines.len(), 5);

        // Context line carries both numbers.
        assert_eq!(h.lines[0].kind, LineKind::Context);
        assert_eq!(h.lines[0].old_lineno, Some(1));
        assert_eq!(h.lines[0].new_lineno, Some(1));

        // Deletion: only an old number.
        assert_eq!(h.lines[1].kind, LineKind::Deletion);
        assert_eq!(h.lines[1].old_lineno, Some(2));
        assert_eq!(h.lines[1].new_lineno, None);
        assert_eq!(h.lines[1].content, "removed line");

        // Additions: only new numbers, consecutive.
        assert_eq!(h.lines[2].kind, LineKind::Addition);
        assert_eq!(h.lines[2].new_lineno, Some(2));
        assert_eq!(h.lines[3].new_lineno, Some(3));

        // Trailing context resumes numbering on both sides.
        assert_eq!(h.lines[4].kind, LineKind::Context);
        assert_eq!(h.lines[4].old_lineno, Some(3));
        assert_eq!(h.lines[4].new_lineno, Some(4));
    }

    #[test]
    fn parses_added_file() {
        let raw = "\
diff --git a/new.txt b/new.txt
new file mode 100644
index 0000000..3b18e51
--- /dev/null
+++ b/new.txt
@@ -0,0 +1,2 @@
+hello
+world
";
        let f = &parse_diff(raw)[0];
        assert_eq!(f.status, FileStatus::Added);
        assert_eq!(f.old_path, None);
        assert_eq!(f.new_path.as_deref(), Some("new.txt"));
        assert_eq!(f.display_path(), "new.txt");
        assert_eq!((f.additions, f.deletions), (2, 0));
        assert_eq!(f.hunks()[0].new_start, 1);
    }

    #[test]
    fn parses_deleted_file() {
        let raw = "\
diff --git a/gone.txt b/gone.txt
deleted file mode 100644
index 3b18e51..0000000
--- a/gone.txt
+++ /dev/null
@@ -1,2 +0,0 @@
-hello
-world
";
        let f = &parse_diff(raw)[0];
        assert_eq!(f.status, FileStatus::Deleted);
        assert_eq!(f.new_path, None);
        assert_eq!(f.display_path(), "gone.txt");
        assert_eq!((f.additions, f.deletions), (0, 2));
    }

    #[test]
    fn parses_rename() {
        let raw = "\
diff --git a/old/name.rs b/new/name.rs
similarity index 92%
rename from old/name.rs
rename to new/name.rs
index 1234567..89abcde 100644
--- a/old/name.rs
+++ b/new/name.rs
@@ -1,1 +1,1 @@
-fn old() {}
+fn renamed() {}
";
        let f = &parse_diff(raw)[0];
        assert_eq!(f.status, FileStatus::Renamed);
        assert_eq!(f.old_path.as_deref(), Some("old/name.rs"));
        assert_eq!(f.new_path.as_deref(), Some("new/name.rs"));
        assert_eq!(f.display_path(), "new/name.rs");
    }

    #[test]
    fn parses_binary_file() {
        let raw = "\
diff --git a/logo.png b/logo.png
new file mode 100644
index 0000000..abcdef0
Binary files /dev/null and b/logo.png differ
";
        let f = &parse_diff(raw)[0];
        assert!(f.is_binary);
        assert_eq!(f.status, FileStatus::Added);
        assert!(f.hunks().is_empty());
    }

    #[test]
    fn handles_no_newline_at_eof() {
        let raw = "\
diff --git a/f.txt b/f.txt
index 1234567..89abcde 100644
--- a/f.txt
+++ b/f.txt
@@ -1 +1 @@
-old
\\ No newline at end of file
+new
\\ No newline at end of file
";
        let h = &parse_diff(raw)[0].hunks()[0];
        assert_eq!(h.lines.len(), 2);
        assert!(h.lines[0].no_newline);
        assert!(h.lines[1].no_newline);
        assert_eq!(h.lines[0].content, "old");
        assert_eq!(h.lines[1].content, "new");
    }

    #[test]
    fn parses_multiple_files_and_hunks() {
        let raw = "\
diff --git a/a.txt b/a.txt
index 111..222 100644
--- a/a.txt
+++ b/a.txt
@@ -1,2 +1,2 @@
 keep
-a-old
+a-new
diff --git a/b.txt b/b.txt
index 333..444 100644
--- a/b.txt
+++ b/b.txt
@@ -1,1 +1,2 @@
 keep
+b-added
@@ -10,1 +11,1 @@ section
-x
+y
";
        let files = parse_diff(raw);
        assert_eq!(files.len(), 2);
        assert_eq!(files[0].display_path(), "a.txt");
        assert_eq!(files[1].display_path(), "b.txt");
        assert_eq!(files[1].hunks().len(), 2);
        assert_eq!(files[1].hunks()[1].section, "section");
        assert_eq!(files[1].hunks()[1].old_start, 10);
    }

    #[test]
    fn single_line_range_defaults_count_to_one() {
        let h = parse_hunk_header("@@ -5 +6 @@").unwrap();
        assert_eq!((h.old_start, h.old_count), (5, 1));
        assert_eq!((h.new_start, h.new_count), (6, 1));
        assert_eq!(h.section, "");
    }

    #[test]
    fn empty_diff_yields_no_files() {
        assert!(parse_diff("").is_empty());
        assert!(parse_diff("\n\n").is_empty());
    }
}
