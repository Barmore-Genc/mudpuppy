//! Load / merge-by-id / save for the annotation store, with atomic
//! (temp + rename) and file-locked writes (PLAN.md §4, §6).
//!
//! The store is one JSON file per `(repo, target)` and is the cross-process
//! contract between the TUI and a headless agent. Two rules keep concurrent
//! writers from losing each other's work:
//!
//! 1. **Merge-by-id, never clobber.** A write does not overwrite the whole file
//!    with one process's in-memory view. [`update`] reloads the current state
//!    *inside the lock*, applies this process's delta (via [`StateFile::upsert`]
//!    / [`StateFile::remove`]), and writes the result — so the other process's
//!    untouched records survive.
//! 2. **Atomic + locked.** The new contents are written to a temp file in the
//!    same directory and `rename`d into place (a reader never sees a half-written
//!    file), and the whole load-modify-save runs under an advisory lock on a
//!    sidecar `.lock` file (a separate inode that the atomic rename never
//!    replaces, so the lock stays valid across writes).

use std::fs::{self, File};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use fs4::FileExt;

use crate::domain::{StateFile, Target};

/// Load the store at `path`, returning `None` if it does not exist yet.
///
/// A missing file is the normal "no review started here" case, not an error;
/// callers create a fresh [`StateFile`] from the target when they need one.
pub fn load(path: &Path) -> Result<Option<StateFile>> {
    let bytes = match fs::read(path) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => {
            return Err(e).with_context(|| format!("reading the store at {}", path.display()))
        }
    };
    let state = serde_json::from_slice(&bytes)
        .with_context(|| format!("parsing the store at {} (corrupt JSON?)", path.display()))?;
    Ok(Some(state))
}

/// Atomically write `state` to `path` (temp file in the same dir, then rename).
///
/// Writing into the same directory keeps the rename on one filesystem, so it is
/// a true atomic replace rather than a copy.
pub fn save(path: &Path, state: &StateFile) -> Result<()> {
    let dir = parent_dir(path)?;
    fs::create_dir_all(dir)
        .with_context(|| format!("creating the store directory {}", dir.display()))?;

    let mut tmp = tempfile::NamedTempFile::new_in(dir)
        .with_context(|| format!("creating a temp file in {}", dir.display()))?;
    serde_json::to_writer_pretty(&mut tmp, state).context("serializing the store")?;
    // Flush + fsync so the rename can't expose a truncated file after a crash.
    use std::io::Write as _;
    tmp.flush().context("flushing the store temp file")?;
    tmp.as_file()
        .sync_all()
        .context("syncing the store temp file")?;
    tmp.persist(path)
        .with_context(|| format!("atomically replacing {}", path.display()))?;
    Ok(())
}

/// Run `f` against the store under an exclusive advisory lock: load the current
/// state (or create an empty one from `target`), let `f` mutate it, then save
/// atomically. Returns whatever `f` returns.
///
/// This is the only safe way to mutate the store: because it reloads *inside*
/// the lock, two processes serializing through it compose by id rather than
/// clobbering — the merge-by-id contract (PLAN.md §4). Express the change as a
/// delta (`state.upsert(..)`, `state.remove(..)`); never rebuild the whole list.
pub fn update<F, T>(path: &Path, target: &Target, f: F) -> Result<T>
where
    F: FnOnce(&mut StateFile) -> T,
{
    let dir = parent_dir(path)?;
    fs::create_dir_all(dir)
        .with_context(|| format!("creating the store directory {}", dir.display()))?;

    let lock = LockGuard::acquire(&lock_path(path))?;
    let mut state = load(path)?.unwrap_or_else(|| StateFile::new(target.clone()));
    let out = f(&mut state);
    save(path, &state)?;
    drop(lock);
    Ok(out)
}

/// The sidecar lock-file path for a store: `<store>.lock`. Kept as a distinct
/// file so the store's atomic rename never replaces the inode being locked.
fn lock_path(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".lock");
    PathBuf::from(name)
}

/// The parent directory of the store path, with a clear error if it is somehow
/// a bare filename.
fn parent_dir(path: &Path) -> Result<&Path> {
    path.parent()
        .filter(|p| !p.as_os_str().is_empty())
        .with_context(|| format!("store path {} has no parent directory", path.display()))
}

/// An acquired exclusive advisory lock on the sidecar file, released on drop.
struct LockGuard {
    file: File,
}

impl LockGuard {
    fn acquire(lock_path: &Path) -> Result<LockGuard> {
        if let Some(dir) = lock_path.parent() {
            fs::create_dir_all(dir)
                .with_context(|| format!("creating the lock directory {}", dir.display()))?;
        }
        let file = File::create(lock_path)
            .with_context(|| format!("opening the lock file {}", lock_path.display()))?;
        // Blocks until any other writer releases the lock; this is what makes a
        // live TUI and a headless agent serialize instead of racing.
        FileExt::lock(&file).with_context(|| format!("locking {}", lock_path.display()))?;
        Ok(LockGuard { file })
    }
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        // Best-effort: the OS also drops the lock when the handle closes.
        let _ = FileExt::unlock(&self.file);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{AnchorScope, Annotation, Author, Severity, Side, Status};

    fn target() -> Target {
        Target::Local {
            base: "main".to_string(),
            head_sha: "abc123".to_string(),
        }
    }

    fn ann(id: &str, body: &str) -> Annotation {
        Annotation {
            id: id.to_string(),
            author: Author::Agent,
            file: "src/lib.rs".to_string(),
            line: 10,
            end_line: None,
            side: Side::Right,
            scope: AnchorScope::Line,
            signature: None,
            severity: Severity::Suggestion,
            tag: None,
            status: Status::Open,
            body: body.to_string(),
            reply_to: None,
            created_at: "2026-05-28T12:00:00Z".parse().unwrap(),
            updated_at: "2026-05-28T12:00:00Z".parse().unwrap(),
        }
    }

    #[test]
    fn load_missing_store_is_none() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("nested/annotations.json");
        assert!(load(&path).unwrap().is_none());
    }

    #[test]
    fn update_creates_then_round_trips() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("a/b/annotations.json");

        update(&path, &target(), |s| s.upsert(ann("id1", "first"))).unwrap();
        assert!(path.exists(), "update creates the store and its parents");

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.target, target());
        assert_eq!(loaded.annotations.len(), 1);
        assert_eq!(loaded.get("id1").unwrap().body, "first");
    }

    #[test]
    fn update_merges_by_id_over_existing_state() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotations.json");

        // Simulate two independent writers: each reloads inside the lock and
        // applies only its own delta, so both records survive.
        update(&path, &target(), |s| s.upsert(ann("a", "from-writer-1"))).unwrap();
        update(&path, &target(), |s| s.upsert(ann("b", "from-writer-2"))).unwrap();

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(
            loaded.annotations.len(),
            2,
            "neither write clobbered the other"
        );

        // An update to an existing id replaces in place rather than duplicating.
        update(&path, &target(), |s| s.upsert(ann("a", "edited"))).unwrap();
        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.annotations.len(), 2);
        assert_eq!(loaded.get("a").unwrap().body, "edited");
    }

    #[test]
    fn round_trips_region_and_whole_file_fields() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotations.json");

        let mut region = ann("rgn", "spans a region");
        region.end_line = Some(20);
        let mut whole = ann("whl", "about the file");
        whole.scope = AnchorScope::File;
        update(&path, &target(), |s| {
            s.upsert(region);
            s.upsert(whole);
        })
        .unwrap();

        let loaded = load(&path).unwrap().unwrap();
        assert_eq!(loaded.get("rgn").unwrap().end_line, Some(20));
        assert_eq!(loaded.get("whl").unwrap().scope, AnchorScope::File);
    }

    #[test]
    fn save_is_pretty_and_reparses() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("annotations.json");
        let mut state = StateFile::new(target());
        state.upsert(ann("x", "body"));
        save(&path, &state).unwrap();

        let text = fs::read_to_string(&path).unwrap();
        assert!(text.contains('\n'), "pretty-printed across lines");
        assert_eq!(load(&path).unwrap().unwrap(), state);
    }

    #[test]
    fn lock_path_is_sidecar() {
        assert_eq!(
            lock_path(Path::new("/d/annotations.json")),
            PathBuf::from("/d/annotations.json.lock")
        );
    }
}
