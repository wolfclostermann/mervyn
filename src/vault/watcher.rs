//! Debounced vault directory watcher → `sync_vault_to_db` (Obsidian edits land in redb without waiting for cron).

use std::path::PathBuf;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use notify::{RecommendedWatcher, RecursiveMode};
use notify_debouncer_mini::{new_debouncer_opt, Config, DebounceEventResult};
use redb::Database;

use super::sync::sync_vault_to_db;

/// Spawn a background thread that watches `vault_path` and runs sync after debounced changes.
/// Errors during watch setup are logged; the thread exits when the debouncer channel disconnects.
pub fn spawn_vault_watcher(db: Arc<Database>, vault_path: PathBuf) {
    std::thread::Builder::new()
        .name("mervyn-vault-notify".into())
        .spawn(move || run_watcher(db, vault_path))
        .expect("spawn vault watcher thread");
}

fn run_watcher(db: Arc<Database>, vault_path: PathBuf) {
    let (sig_tx, sig_rx) = mpsc::channel::<()>();

    let config = Config::default()
        .with_timeout(Duration::from_millis(350))
        .with_batch_mode(false);

    let mut debouncer = match new_debouncer_opt::<_, RecommendedWatcher>(config, move |res: DebounceEventResult| {
        match res {
            Ok(events) if !events.is_empty() => {
                let _ = sig_tx.send(());
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
            Ok(()) => match sync_vault_to_db(db.as_ref(), &vault_path) {
                Ok(stats) => tracing::info!(?stats, "vault watcher sync"),
                Err(e) => tracing::warn!(error = %e, "vault watcher sync failed"),
            },
            Err(_) => break,
        }
    }
}
