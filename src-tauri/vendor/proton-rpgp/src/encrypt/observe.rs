use std::fmt::Debug;

use crate::{preferences::EncryptionMechanism, PrivateComponentKeyPublicView, PublicComponentKey};

/// A trait for collecting statistics about the encryption process.
pub trait EncryptionObserver: Debug {
    /// Provide information about the encryption keys used.
    fn observe_encryption_keys(&self, keys: &[PublicComponentKey<'_>]);

    /// Provide information about the signing keys used.
    fn observe_signing_keys(&self, key_views: &[PrivateComponentKeyPublicView<'_>]);

    /// Provide information about the encryption mechanism used.
    fn observe_encryption_mechanism(&self, mechanism: &EncryptionMechanism);
}
