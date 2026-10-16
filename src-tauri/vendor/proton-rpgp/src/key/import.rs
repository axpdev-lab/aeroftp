use std::io::{BufRead, Read};

use pgp::composed::{Deserializable, SignedPublicKey, SignedSecretKey};

use crate::KeyOperationError;

pub(crate) trait ImportSingleKeyExt: Deserializable {
    /// Parse a single armored key, rejecting inputs with more than one key.
    fn from_armor_single_enforce<R: Read>(input: R) -> Result<Self, KeyOperationError> {
        let (keys, _headers) = Self::from_armor_many(input).map_err(KeyOperationError::Decode)?;
        exactly_one_key(keys)
    }

    /// Parse a single unarmored key, rejecting inputs with more than one key.
    fn from_bytes_single_enforce<R: BufRead>(bytes: R) -> Result<Self, KeyOperationError> {
        let keys = Self::from_bytes_many(bytes).map_err(KeyOperationError::Decode)?;
        exactly_one_key(keys)
    }
}

impl ImportSingleKeyExt for SignedSecretKey {}
impl ImportSingleKeyExt for SignedPublicKey {}

fn exactly_one_key<T>(
    mut keys: impl Iterator<Item = pgp::errors::Result<T>>,
) -> Result<T, KeyOperationError> {
    let key = keys
        .next()
        .ok_or(KeyOperationError::DecodeNotFound)?
        .map_err(KeyOperationError::Decode)?;
    if keys.next().is_some() {
        return Err(KeyOperationError::DecodeMultipleKeys);
    }
    Ok(key)
}
