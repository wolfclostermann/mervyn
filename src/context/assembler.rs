use std::path::PathBuf;
use std::sync::Arc;

pub struct ContextAssembler {
    _db: Arc<redb::Database>,
    _vault_path: PathBuf,
}

impl ContextAssembler {
    pub fn new(db: Arc<redb::Database>, vault_path: PathBuf) -> Self {
        Self {
            _db: db,
            _vault_path: vault_path,
        }
    }
}
