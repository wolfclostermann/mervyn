//! Debounced vault directory watcher → `sync_vault_to_db` (Obsidian edits land in redb without waiting for cron).

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use notify::{EventKind, RecursiveMode, Watcher};
use redb::Database;

use super::sync::sync_vault_to_db;

/// Spawn a background thread that watches `vault_path` and runs sync after debounced `.md` changes.
/// Errors during watch setup are logged; the thread exits on unrecoverable notify errors.
pub fn spawn_vault_watcher(db: Arc<Database>, vault_path: PathBuf) {
    std::thread::Builder::new()
        .name("mervyn-vault-notify".into())
        .spawn(move || run_watcher(db, vault_path))
        .expect("spawn vault watcher thread");
}

fn run_watcher(db: Arc<Database>, vault_path: PathBuf) {
    let (sig_tx, sig_rx) = mpsc::channel::<()>();

    let mut watcher = match notify::recommended_watcher(move |res: notify::Result<notify::Event>| {
        if let Ok(ev) = res {
            if matches!(
                ev.kind,
                EventKind::Modify(_) | EventKind::Create(_) | EventKind::Remove(_)
            ) {
                let _ = sig_tx.send(());
            }
        }
    }) {
        Ok(w) => w,
        Err(e) => {
            tracing::error!(error = %e, "notify recommended_watcher");
            return;
        }
    };

    if let Err(e) = watcher.watch(&vault_path, RecursiveMode::Recursive) {
        tracing::error!(error = %e, path = %vault_path.display(), "vault watch");
        return;
    }

    loop {
        match sig_rx.recv() {
            Ok(()) => {
                std::thread::sleep(Duration::from_millis(350));
                while sig_rx.recv_timeout(Duration::ZERO).is_ok() {}

                match sync_vault_to_db(db.as_ref(), &vault_path) {
                    Ok(stats) => tracing::info!(?stats, "vault watcher sync"),
                    Err(e) => tracing::warn!(error = %e, "vault watcher sync failed"),
                }
            }
            Err(_) => break,
        }
    }
}
