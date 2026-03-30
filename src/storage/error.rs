use redb::{
    CommitError, Error as RedbError, StorageError as RedbStorageError, TableError, TransactionError,
};
use thiserror::Error;

#[derive(Debug, Error)]
pub enum StorageError {
    #[error("postcard: {0}")]
    Postcard(#[from] postcard::Error),
    #[error("database: {0}")]
    Redb(#[from] RedbError),
}

pub type Result<T> = std::result::Result<T, StorageError>;

impl From<RedbStorageError> for StorageError {
    fn from(err: RedbStorageError) -> Self {
        Self::Redb(err.into())
    }
}

impl From<TransactionError> for StorageError {
    fn from(err: TransactionError) -> Self {
        Self::Redb(err.into())
    }
}

impl From<TableError> for StorageError {
    fn from(err: TableError) -> Self {
        Self::Redb(err.into())
    }
}

impl From<CommitError> for StorageError {
    fn from(err: CommitError) -> Self {
        Self::Redb(err.into())
    }
}
