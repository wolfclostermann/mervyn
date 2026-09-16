//! Write-side plumbing for the vault: the reconcile lock, atomic file replacement,
//! self-write suppression and the one-shot pre-write backup.
//!
//! Nothing here decides *what* to write — that is the reconciler's job. This module exists so
//! that when writing starts, three things are already true: only one task touches the vault at a
//! time, a half-written file can never be observed, and Mervyn's own writes do not wake the
//! watcher into a re-sync loop.
//!
//! Phase 0 of `docs/two-way-vault-sync.md` lands this plumbing ahead of the reconciler that will
//! call it, so the write path can be reviewed and tested on its own. Everything here is exercised
//! by this module's tests; the production call sites arrive with phase 1.
#![allow(dead_code)]

use std::collections::HashMap;
use std::fs;
use std::hash::Hasher;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard};

use fnv::FnvHasher;

/// Distinguishes concurrent temp files; process-local, only ever used in a file name.
static TEMP_SEQ: AtomicU64 = AtomicU64::new(0);

fn content_hash(bytes: &[u8]) -> u64 {
    let mut h = FnvHasher::default();
    h.write(bytes);
    h.finish()
}

/// Serialises vault access and remembers what Mervyn last wrote to each file.
///
/// Shared through [`crate::state::AppState`]; every caller of the reconciler holds [`lock`] for
/// the duration of a parse → merge → write cycle. Until write-back exists the lock is only
/// preventing overlapping *reads*, which is harmless but keeps the call sites honest.
///
/// [`lock`]: VaultAccess::lock
#[derive(Debug, Default)]
pub struct VaultAccess {
    reconcile: Mutex<()>,
    /// Path → hash of the bytes Mervyn last wrote there.
    last_written: Mutex<HashMap<PathBuf, u64>>,
    backed_up: Mutex<bool>,
}

impl VaultAccess {
    pub fn new() -> Self {
        Self::default()
    }

    /// Exclusive access for one reconcile cycle. A poisoned lock is recovered rather than
    /// propagated: a panic in a previous cycle must not wedge every later sync.
    pub fn lock(&self) -> MutexGuard<'_, ()> {
        self.reconcile
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Replace `path`'s contents atomically, then remember the hash so the watcher can recognise
    /// the resulting notify event as Mervyn's own.
    ///
    /// Writes *through* a symlink rather than replacing it — `worklog.md` is a symlink into the
    /// worklog clone, and a rename over it would turn it into a regular file and silently detach
    /// the vault from git. A **dangling** symlink is an error instead: on the VPS that is the
    /// documented healthy state of `worklog.md` (it points at an in-container path), and creating
    /// the target from the host would be exactly the wrong repair.
    pub fn write_file(&self, path: &Path, contents: &str) -> io::Result<()> {
        let target = resolve_write_target(path)?;
        let dir = target.parent().ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!("{} has no parent directory", target.display()),
            )
        })?;

        let seq = TEMP_SEQ.fetch_add(1, Ordering::Relaxed);
        let tmp = dir.join(format!(".mervyn-{}-{seq}.tmp", std::process::id()));

        let write_result = (|| -> io::Result<()> {
            let mut f = fs::File::create(&tmp)?;
            f.write_all(contents.as_bytes())?;
            f.sync_all()?;
            drop(f);
            fs::rename(&tmp, &target)
        })();

        if write_result.is_err() {
            let _ = fs::remove_file(&tmp);
            return write_result;
        }

        self.record_write(path, contents.as_bytes());
        if target != path {
            self.record_write(&target, contents.as_bytes());
        }
        Ok(())
    }

    fn record_write(&self, path: &Path, bytes: &[u8]) {
        let mut map = self
            .last_written
            .lock()
            .unwrap_or_else(|p| p.into_inner());
        map.insert(path.to_path_buf(), content_hash(bytes));
    }

    /// Whether `path` currently holds exactly the bytes Mervyn last wrote to it — i.e. this
    /// notify event is the echo of our own write and there is nothing new to import.
    ///
    /// Hash comparison rather than a timestamp window: if the user edits the file a millisecond
    /// after Mervyn writes it, the hash differs and the edit is picked up.
    pub fn is_echo_of_self_write(&self, path: &Path) -> bool {
        let expected = {
            let map = self
                .last_written
                .lock()
                .unwrap_or_else(|p| p.into_inner());
            match map.get(path) {
                Some(h) => *h,
                None => return false,
            }
        };
        match fs::read(path) {
            Ok(bytes) => content_hash(&bytes) == expected,
            Err(_) => false,
        }
    }

    /// Copy `files` (relative to `vault_path`) into a timestamped sibling directory, once per
    /// process, before the first write-back of a run. Returns the directory if one was made.
    ///
    /// Symlinks are skipped: their targets belong to something else (the worklog clone), and
    /// copying through one would put a stale duplicate somewhere confusing.
    pub fn backup_once(
        &self,
        vault_path: &Path,
        files: &[&str],
        now: chrono::DateTime<chrono::Utc>,
    ) -> io::Result<Option<PathBuf>> {
        let mut done = self.backed_up.lock().unwrap_or_else(|p| p.into_inner());
        if *done {
            return Ok(None);
        }

        let stamp = now.format("%Y%m%d-%H%M%S");
        let name = format!(
            "{}.backup.vault-writeback-{stamp}",
            vault_path
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_else(|| "vault".to_string())
        );
        let dest = match vault_path.parent() {
            Some(parent) => parent.join(name),
            None => return Ok(None),
        };

        let mut copied = 0usize;
        for file in files {
            let src = vault_path.join(file);
            if src.symlink_metadata().map(|m| m.is_symlink()).unwrap_or(false) {
                continue;
            }
            if !src.is_file() {
                continue;
            }
            if copied == 0 {
                fs::create_dir_all(&dest)?;
            }
            fs::copy(&src, dest.join(file))?;
            copied += 1;
        }

        *done = true;
        Ok((copied > 0).then_some(dest))
    }
}

/// The path a write should actually land on: the symlink's target if `path` is a link.
fn resolve_write_target(path: &Path) -> io::Result<PathBuf> {
    let Ok(meta) = path.symlink_metadata() else {
        // No such entry yet — a plain create.
        return Ok(path.to_path_buf());
    };
    if !meta.is_symlink() {
        return Ok(path.to_path_buf());
    }
    fs::canonicalize(path).map_err(|e| {
        io::Error::new(
            e.kind(),
            format!(
                "{} is a symlink whose target does not resolve ({e}); refusing to replace the \
                 link with a regular file",
                path.display()
            ),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use tempfile::tempdir;

    #[test]
    fn write_file_replaces_contents_and_leaves_no_temp_files() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.md");
        let v = VaultAccess::new();

        v.write_file(&path, "first\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "first\n");
        v.write_file(&path, "second\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "second\n");

        let leftovers: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".tmp"))
            .collect();
        assert!(leftovers.is_empty(), "temp files left behind: {leftovers:?}");
    }

    #[test]
    fn echo_detection_is_true_after_our_write_and_false_after_an_edit() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("reminders.md");
        let v = VaultAccess::new();

        v.write_file(&path, "- [ ] a\n").unwrap();
        assert!(v.is_echo_of_self_write(&path));

        fs::write(&path, "- [ ] a\n- [ ] b\n").unwrap();
        assert!(!v.is_echo_of_self_write(&path), "a human edit is not an echo");
    }

    #[test]
    fn echo_detection_is_false_for_a_file_we_never_wrote() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("notes.md");
        fs::write(&path, "hello\n").unwrap();
        assert!(!VaultAccess::new().is_echo_of_self_write(&path));
    }

    #[test]
    fn write_follows_a_symlink_instead_of_replacing_it() {
        let dir = tempdir().unwrap();
        let real = dir.path().join("real.md");
        fs::write(&real, "old\n").unwrap();
        let link = dir.path().join("worklog.md");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        VaultAccess::new().write_file(&link, "new\n").unwrap();

        assert!(link.symlink_metadata().unwrap().is_symlink(), "link survived");
        assert_eq!(fs::read_to_string(&real).unwrap(), "new\n");
    }

    #[test]
    fn write_refuses_to_clobber_a_dangling_symlink() {
        let dir = tempdir().unwrap();
        let link = dir.path().join("worklog.md");
        std::os::unix::fs::symlink("/worklog/worklog.md", &link).unwrap();

        let err = VaultAccess::new().write_file(&link, "x\n").unwrap_err();
        assert!(err.to_string().contains("refusing to replace the link"));
        assert!(link.symlink_metadata().unwrap().is_symlink());
    }

    #[test]
    fn backup_runs_once_and_skips_symlinks() {
        let dir = tempdir().unwrap();
        let vault = dir.path().join("vault");
        fs::create_dir_all(&vault).unwrap();
        fs::write(vault.join("events.md"), "e\n").unwrap();
        std::os::unix::fs::symlink("/worklog/worklog.md", vault.join("worklog.md")).unwrap();

        let v = VaultAccess::new();
        let files = ["events.md", "worklog.md", "absent.md"];
        let dest = v.backup_once(&vault, &files, Utc::now()).unwrap().unwrap();

        assert_eq!(fs::read_to_string(dest.join("events.md")).unwrap(), "e\n");
        assert!(!dest.join("worklog.md").exists(), "symlink not followed");
        assert!(v.backup_once(&vault, &files, Utc::now()).unwrap().is_none());
    }
}
