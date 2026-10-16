use pgp::{
    crypto::public_key::PublicKeyAlgorithm,
    types::{Fingerprint, Imprint, KeyDetails, KeyId},
};
use sha2::Sha256;

use crate::{
    check_key_not_expired, AsPublicKeyRef, CertificationSelectionExt, CheckUnixTime,
    FingerprintSha256, GenericKeyIdentifier, Profile, PublicComponentKey, PublicKeySelectionExt,
    SignatureUsage,
};

/// A trait for types that can provide information about an `OpenPGP` key.
pub trait AccessKeyInfo {
    /// Returns the `OpenPGP` key version.
    fn version(&self) -> u8;

    /// Returns the key id of the `OpenPGP` primary key.
    fn key_id(&self) -> KeyId;

    /// Returns the fingerprint of the `OpenPGP` primary key.
    fn fingerprint(&self) -> Fingerprint;

    /// Returns the key ids of the `OpenPGP` primary key and its subkeys.
    fn key_ids(&self) -> Vec<KeyId>;

    /// Returns the fingerprints of the `OpenPGP` primary key and its subkeys.
    fn fingerprints(&self) -> Vec<Fingerprint>;

    /// Returns sha256 fingerprints of the key and its subkeys.
    ///
    /// If creation fails, the fingerprint is set to the zero value.
    fn fingerprints_sha256(&self) -> Vec<FingerprintSha256>;

    /// Checks if the key can encrypt at the given unixtime.
    ///
    /// Returns an error if no valid encryption key can be found.
    fn check_can_encrypt(&self, profile: &Profile, date: CheckUnixTime) -> crate::Result<()>;

    /// Checks if any of the keys can be used for verification at the given date.
    ///
    /// Returns an error if no valid verification keys can be found.
    fn check_can_verify(&self, profile: &Profile, date: CheckUnixTime) -> crate::Result<()>;

    /// Checks if the primary key is expired at the given date.
    ///
    /// Also returns `true` if no valid primary self-certification can be found.
    fn is_expired(&self, profile: &Profile, date: CheckUnixTime) -> bool;

    /// Checks if the primary key is revoked at the given date.
    ///
    /// Note that third-party revocation signatures are not supported.
    /// Note also that Identity and Subkey revocation should be checked separately.
    fn is_revoked(&self, profile: &Profile, date: CheckUnixTime) -> bool;
}

// Implement the `KeyInfo` trait for types that can access a `PublicKey` reference.
impl<T: AsPublicKeyRef> AccessKeyInfo for T {
    /// Returns the `OpenPGP` key version.
    fn version(&self) -> u8 {
        self.as_public_key().as_signed_public_key().version().into()
    }

    /// Returns the key id of the `OpenPGP` primary key.
    fn key_id(&self) -> KeyId {
        self.as_public_key().as_signed_public_key().legacy_key_id()
    }

    /// Returns the fingerprint of the `OpenPGP` primary key.
    fn fingerprint(&self) -> Fingerprint {
        self.as_public_key().as_signed_public_key().fingerprint()
    }

    /// Returns the key ids of the `OpenPGP` primary key and its subkeys.
    fn key_ids(&self) -> Vec<KeyId> {
        let pub_key_ref = &self.as_public_key().as_signed_public_key();
        let mut output = Vec::with_capacity(pub_key_ref.public_subkeys.len() + 1);
        output.push(pub_key_ref.legacy_key_id());
        for subkey in &pub_key_ref.public_subkeys {
            output.push(subkey.legacy_key_id());
        }
        output
    }

    /// Returns the fingerprints of the `OpenPGP` primary key and its subkeys.
    fn fingerprints(&self) -> Vec<Fingerprint> {
        let pub_key_ref = &self.as_public_key().as_signed_public_key();
        let mut output = Vec::with_capacity(pub_key_ref.public_subkeys.len() + 1);
        output.push(pub_key_ref.fingerprint());
        for subkey in &pub_key_ref.public_subkeys {
            output.push(subkey.fingerprint());
        }
        output
    }

    /// Returns sha256 fingerprints of the key and its subkeys.
    ///
    /// If creation fails, the fingerprint is set to the zero value.
    fn fingerprints_sha256(&self) -> Vec<FingerprintSha256> {
        let mut fingerprints =
            Vec::with_capacity(self.as_public_key().inner.public_subkeys.len() + 1);
        let primary_fp = self
            .as_public_key()
            .inner
            .imprint::<Sha256>()
            .unwrap_or_default();
        fingerprints.push(FingerprintSha256(primary_fp.into()));
        for subkey in &self.as_public_key().inner.public_subkeys {
            let subkey_fp = subkey.imprint::<Sha256>().unwrap_or_default();
            fingerprints.push(FingerprintSha256(subkey_fp.into()));
        }
        fingerprints
    }

    /// Checks if the key can encrypt at the given unixtime.
    ///
    /// Returns an error if no valid encryption key can be found.
    fn check_can_encrypt(&self, profile: &Profile, date: CheckUnixTime) -> crate::Result<()> {
        self.as_public_key()
            .as_signed_public_key()
            .encryption_key(date, profile)
            .map(|_| ())
            .map_err(Into::into)
    }

    /// Checks if any of the keys can be used for verification at the given date.
    ///
    /// Returns an error if no valid verification keys can be found.
    fn check_can_verify(&self, profile: &Profile, date: CheckUnixTime) -> crate::Result<()> {
        self.as_public_key()
            .as_signed_public_key()
            .verification_keys(date, Vec::default(), SignatureUsage::Sign, profile)
            .map(|_| ())
            .map_err(Into::into)
    }

    /// Checks if the primary key is expired at the given date.
    ///
    /// Also retruns `true` if no valid primary self-certification can be found.
    fn is_expired(&self, profile: &Profile, date: CheckUnixTime) -> bool {
        let pub_key_ref = self.as_public_key().as_signed_public_key();
        let Ok(self_signature) = pub_key_ref.primary_self_signature(date, profile) else {
            return true;
        };

        check_key_not_expired(pub_key_ref, self_signature, date).is_err()
    }

    /// Checks if the primary key is revoked at the given date.
    ///
    /// Note that third-party revocation signatures are not supported.
    /// Note also that Identity and Subkey revocation should be checked separately.
    fn is_revoked(&self, profile: &Profile, date: CheckUnixTime) -> bool {
        let pub_key_ref = self.as_public_key().as_signed_public_key();
        pub_key_ref.revoked(pub_key_ref.primary_key(), None, date, profile)
    }
}

/// Information about an `OpenPGP` key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyInfo {
    /// The key id of the `OpenPGP` key.
    pub key_id: KeyId,

    /// The fingerprint of the `OpenPGP` key.
    pub fingerprint: Fingerprint,

    /// The public key algorithm of the `OpenPGP` key.
    pub algorithm: PublicKeyAlgorithm,
}

impl<'a> From<PublicComponentKey<'a>> for KeyInfo {
    fn from(key: PublicComponentKey<'a>) -> Self {
        Self {
            key_id: key.public_key.legacy_key_id(),
            fingerprint: key.public_key.fingerprint(),
            algorithm: key.public_key.algorithm(),
        }
    }
}

pub trait PublicKeyExt: KeyDetails {
    /// Returns the key identifier of the `OpenPGP` key.
    fn generic_identifier(&self) -> GenericKeyIdentifier {
        GenericKeyIdentifier::Both(self.legacy_key_id(), self.fingerprint())
    }
}

impl<T: KeyDetails> PublicKeyExt for T {}
