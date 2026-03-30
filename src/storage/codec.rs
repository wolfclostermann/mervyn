use serde::{de::DeserializeOwned, Serialize};

use super::error::StorageError;

pub(crate) fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, StorageError> {
    Ok(postcard::to_allocvec(value)?)
}

pub(crate) fn decode<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, StorageError> {
    Ok(postcard::from_bytes(bytes)?)
}
