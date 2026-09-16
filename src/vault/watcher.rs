//! Debounced vault directory watcher → `sync_vault_to_db` (Obsidian edits land in redb without waiting for cron).

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer_opt, Config, DebounceEventResult};
use redb::Database;

use super::sync::{sync_vault_to_db, WriteBackPolicy};
use super::write::VaultAccess;

/// Spawn a background thread that watches `vault_path` and runs sync after debounced changes.
/// Errors during watch setup are logged; the thread exits when the debouncer channel disconnects.
pub fn spawn_vault_watcher(
    db: Arc<Database>,
    vault_path: PathBuf,
    vault: Arc<VaultAccess>,
    policy: WriteBackPolicy,
) {
    std::thread::Builder::new()
        .name("mervyn-vault-notify".into())
        .spawn(move || run_watcher(db, vault_path, vault, policy))
        .expect("spawn vault watcher thread");
}

/// True when every path in the batch still holds exactly the bytes Mervyn last wrote there, i.e.
/// the batch is the echo of our own write-back and contains nothing to import.
///
/// An empty batch is *not* treated as an echo — a notify event with no usable path should still
/// provoke a sync rather than be silently dropped.
fn is_self_write_echo(vault: &VaultAccess, paths: &[PathBuf]) -> bool {
    !paths.is_empty() && paths.iter().all(|p| vault.is_echo_of_self_write(p))
}

fn run_watcher(
    db: Arc<Database>,
    vault_path: PathBuf,
    vault: Arc<VaultAccess>,
    policy: WriteBackPolicy,
) {
    let (sig_tx, sig_rx) = mpsc::channel::<Vec<PathBuf>>();

    let config = Config::default()
        .with_timeout(Duration::from_millis(350))
        .with_batch_mode(false);

    let mut debouncer = match new_debouncer_opt::<_, RecommendedWatcher>(config, move |res: DebounceEventResult| {
        match res {
            Ok(events) if !events.is_empty() => {
                let _ = sig_tx.send(events.into_iter().map(|e| e.path).collect());
            }
            Ok(_) => {}
            Err(e) => tracing::error!(error = %e, "notify debouncer"),
        }
    }) {
        Ok(d) => d,
        Err(e) => {
            tracing::error!(error = %e, "notify new_debouncer_opt");
            return;
        }
    };

    if let Err(e) = debouncer.watcher().watch(&vault_path, RecursiveMode::Recursive) {
        tracing::error!(error = %e, path = %vault_path.display(), "vault watch");
        return;
    }

    loop {
        match sig_rx.recv() {
            Ok(paths) => {
                if is_self_write_echo(vault.as_ref(), &paths) {
                    tracing::trace!(count = paths.len(), "vault watcher: own write, not re-syncing");
                    continue;
                }
                match sync_vault_to_db(db.as_ref(), &vault_path, vault.as_ref(), policy) {
                    Ok(stats) => tracing::info!(?stats, "vault watcher sync"),
                    Err(e) => tracing::warn!(error = %e, "vault watcher sync failed"),
                }
            }
            Err(_) => break,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn echo_of_our_own_write_is_recognised_and_a_later_edit_is_not() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("events.md");
        let vault = VaultAccess::new();
        vault.write_file(&path, "## 2026-04-05 — Gig\n").unwrap();

        let batch = vec![path.clone()];
        assert!(is_self_write_echo(&vault, &batch));

        std::fs::write(&path, "## 2026-04-05 — Gig\nDoors 7pm\n").unwrap();
        assert!(!is_self_write_echo(&vault, &batch));
    }

    #[test]
    fn a_batch_mixing_our_write_with_a_human_edit_still_syncs() {
        let dir = tempdir().unwrap();
        let ours = dir.path().join("events.md");
        let theirs = dir.path().join("reminders.md");
        let vault = VaultAccess::new();
        vault.write_file(&ours, "written by mervyn\n").unwrap();
        std::fs::write(&theirs, "- [ ] typed by hand\n").unwrap();

        assert!(!is_self_write_echo(&vault, &[ours, theirs]));
    }

    #[test]
    fn an_empty_batch_is_not_an_echo() {
        assert!(!is_self_write_echo(&VaultAccess::new(), &[]));
    }
}
