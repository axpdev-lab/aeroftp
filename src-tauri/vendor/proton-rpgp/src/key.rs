use std::{borrow::Cow, vec};

use pgp::{
    composed::{
        ArmorOptions, Deserializable, PlainSessionKey, RawSessionKey, SignedPublicKey,
        SignedSecretKey,
    },
    crypto::sym::SymmetricKeyAlgorithm,
    ser::Serialize,
    types::{KeyDetails, KeyVersion, Password, S2kParams, SecretParams},
};
use rand::RngCore as _;
use zeroize::Zeroizing;

use crate::{
    key::{params::PlainSecretParamsExt, preferences::RecipientsAlgorithms},
    CheckUnixTime, DataEncoding, EncryptionError, ExpectLockedError, KeyOperationError,
    KeySecretParamValidationError, Profile, ResolvedDataEncoding,
};

pub mod certifications;
pub(crate) use certifications::*;

pub mod component;
pub(crate) use component::*;

pub mod selection;
pub(crate) use selection::*;

pub(crate) mod preferences;
pub use preferences::EncryptionMechanism;

pub(crate) mod import;
pub(crate) use import::*;

mod generation;
pub use generation::*;

mod modification;
pub use modification::*;

mod info;
pub use info::*;

mod params;

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyLock {
    #[default]
    Expected,
    NotRequired,
}

/// A trait for types that can be converted to a `PublicKey` reference.
pub trait AsPublicKeyRef {
    fn as_public_key(&self) -> &PublicKey;
}

impl<T: AsPublicKeyRef> AsPublicKeyRef for &T {
    fn as_public_key(&self) -> &PublicKey {
        (*self).as_public_key()
    }
}

/// A generic `OpenPGP` public key.
/// An `OpenPGP` key consists of a primary key and zero or more subkeys.
#[derive(Debug, Clone)]
pub struct PublicKey {
    /// The inner type from rPGP.
    pub(crate) inner: SignedPublicKey,
}

impl AsPublicKeyRef for PublicKey {
    fn as_public_key(&self) -> &PublicKey {
        self
    }
}

impl AsRef<PublicKey> for PublicKey {
    fn as_ref(&self) -> &PublicKey {
        self
    }
}

impl From<PublicKey> for SignedPublicKey {
    fn from(public_key: PublicKey) -> Self {
        public_key.inner
    }
}

impl From<SignedPublicKey> for PublicKey {
    fn from(signed_public_key: SignedPublicKey) -> Self {
        Self {
            inner: signed_public_key,
        }
    }
}

impl PublicKey {
    /// Exposes the underlying rPGP signed public key.
    pub fn as_signed_public_key(&self) -> &SignedPublicKey {
        &self.inner
    }

    /// Import an `OpenPGP` public key from a byte slice.
    pub fn import(key_data: &[u8], encoding: DataEncoding) -> crate::Result<Self> {
        let resolved_encoding = encoding.resolve_for_read(key_data);
        let signed_public_key = match resolved_encoding {
            ResolvedDataEncoding::Armored => SignedPublicKey::from_armor_single(key_data)
                .map_err(KeyOperationError::Decode)
                .map(|(signed_public, _)| signed_public)?,
            ResolvedDataEncoding::Unarmored => {
                SignedPublicKey::from_bytes(key_data).map_err(KeyOperationError::Decode)?
            }
        };

        Ok(Self {
            inner: signed_public_key,
        })
    }

    /// Import an `OpenPGP` public key from a byte slice.
    ///
    /// Enforces that exactly one key is present in the input.
    pub fn import_single_enforce(key_data: &[u8], encoding: DataEncoding) -> crate::Result<Self> {
        let resolved_encoding = encoding.resolve_for_read(key_data);
        let signed_public_key = match resolved_encoding {
            ResolvedDataEncoding::Armored => SignedPublicKey::from_armor_single_enforce(key_data)?,
            ResolvedDataEncoding::Unarmored => {
                SignedPublicKey::from_bytes_single_enforce(key_data)?
            }
        };

        Ok(Self {
            inner: signed_public_key,
        })
    }

    /// Export the public key.
    pub fn export(&self, encoding: DataEncoding) -> crate::Result<Vec<u8>> {
        match encoding.resolve_for_write() {
            ResolvedDataEncoding::Armored => {
                let armored_bytes = self
                    .inner
                    .to_armored_bytes(ArmorOptions {
                        headers: None,
                        include_checksum: !(self.inner.version() == KeyVersion::V6),
                    })
                    .map_err(KeyOperationError::Encode)?;
                Ok(armored_bytes)
            }
            ResolvedDataEncoding::Unarmored => {
                let mut buf = Vec::new();
                self.inner
                    .to_writer(&mut buf)
                    .map_err(KeyOperationError::Encode)?;
                Ok(buf)
            }
        }
    }

    /// Certifies the user-id of this key with an external `certifier` key.
    ///
    /// For example: Proton CA
    ///
    /// Returns a builder to configure the optional certification parameters,
    /// such as the expected email of the user-id, the certification date, the
    /// lifetime after which the certification expires, and the profile to use.
    pub fn certify_with_external<'a>(
        &self,
        certifier: &'a PrivateKey,
    ) -> ExternalCertifier<'a, Self> {
        ExternalCertifier::new(self.clone(), certifier)
    }

    /// Verifies that the user-id of this key was certified by an external `certifier` key.
    ///
    /// For example: Proton CA
    ///
    /// Returns a builder to configure the optional verification parameters, such as the
    /// expected email of the user-id, the verification date, and the profile to use.
    pub fn verify_with_external<'a>(
        &self,
        certifier: &'a impl AsPublicKeyRef,
    ) -> ExternalVerifier<'a, Self> {
        ExternalVerifier::new(self.clone(), certifier)
    }
}

/// A generic locked `OpenPGP` secret key.
/// An `OpenPGP` key consists of a primary key and zero or more subkeys.
/// The secret key's private key material might be encrypted with a password.
#[derive(Debug, Clone)]
pub struct LockedPrivateKey(PrivateKey);

impl AsRef<PublicKey> for LockedPrivateKey {
    fn as_ref(&self) -> &PublicKey {
        &self.0.public
    }
}

impl AsPublicKeyRef for LockedPrivateKey {
    fn as_public_key(&self) -> &PublicKey {
        &self.0.public
    }
}

impl From<LockedPrivateKey> for PublicKey {
    fn from(locked: LockedPrivateKey) -> Self {
        locked.0.public
    }
}

impl From<&LockedPrivateKey> for PublicKey {
    fn from(value: &LockedPrivateKey) -> Self {
        value.0.public.clone()
    }
}

impl From<PrivateKey> for PublicKey {
    fn from(private: PrivateKey) -> Self {
        private.public
    }
}

impl From<&PrivateKey> for PublicKey {
    fn from(value: &PrivateKey) -> Self {
        value.public.clone()
    }
}

impl LockedPrivateKey {
    fn new(secret: SignedSecretKey) -> Self {
        Self(PrivateKey::new(secret))
    }

    /// Exposes the underlying rPGP signed public key.
    pub fn as_signed_public_key(&self) -> &SignedPublicKey {
        &self.0.public.inner
    }

    /// Returns `true` if the secret key or any of its subkeys are locked,
    ///
    /// A "locked" key means the secret/private key data is protected and cannot be used for cryptographic operations
    /// until it is unlocked with the correct password.
    pub fn is_locked(&self) -> bool {
        self.primary_is_locked()
            || self
                .0
                .secret
                .secret_subkeys
                .iter()
                .any(|sub_key| sub_key.secret_params().is_encrypted())
    }

    /// Determines whether the primary secret key and all associated subkeys are locked (encrypted)
    /// and there are no public subkeys.
    ///
    /// Checks that all secret components (primary key and subkeys) are locked (encrypted) and that there are no public subkeys.
    ///
    /// Returns `Ok(())` if every secret component requires unlocking.
    /// Returns an error indicating which component is not locked or if public subkeys are present.
    pub fn is_fully_locked(&self) -> Result<(), KeyOperationError> {
        if !self.primary_is_locked() {
            return Err(ExpectLockedError::PrimaryKeyUnlocked.into());
        }
        for subkey in &self.0.secret.secret_subkeys {
            if !subkey.secret_params().is_encrypted() {
                return Err(ExpectLockedError::SubkeyUnlocked(subkey.legacy_key_id()).into());
            }
        }
        if !self.0.secret.public_subkeys.is_empty() {
            return Err(ExpectLockedError::NoPublicSubkeys.into());
        }
        Ok(())
    }

    fn primary_is_locked(&self) -> bool {
        self.0.secret.secret_params().is_encrypted()
    }

    /// Unlock the secret key with a key password.
    pub fn unlock(&self, password: &[u8], lock_expectation: KeyLock) -> crate::Result<PrivateKey> {
        if lock_expectation == KeyLock::Expected {
            if let Err(err) = self.is_fully_locked() {
                return Err(err.into());
            }
        }
        let local_password = Password::from(password);
        let mut secret_copy = self.0.secret.clone();
        let primary_was_aead_encrypted = matches!(
            secret_copy.primary_key.secret_params(),
            SecretParams::Encrypted(params) if matches!(params.string_to_key_params(), S2kParams::Aead { .. })
        );
        secret_copy
            .primary_key
            .remove_password(&local_password)
            .map_err(|e| KeyOperationError::Unlock(secret_copy.primary_key.legacy_key_id(), e))?;
        for subkey in &mut secret_copy.secret_subkeys {
            subkey
                .key
                .remove_password(&local_password)
                .map_err(|err| KeyOperationError::Unlock(subkey.key.legacy_key_id(), err))?;
        }
        let private_key = PrivateKey::new(secret_copy);
        if self.primary_is_locked() {
            // If we unlock the primary secret key, we validate that the public parts match the private parts.
            // Subkeys are verified via binding signatures from the primary key.
            // This prevents an attacker from modifying the public parts of a locked key.
            // For aead encrypted primary secret keys, the public parts are authenticated.
            private_key
                .validate_primary_secret_key(primary_was_aead_encrypted)
                .map_err(KeyOperationError::ValidatePublicParts)?;
        }
        Ok(private_key)
    }

    /// Import a locked `OpenPGP` secret key from a byte slice.
    ///
    /// Does not check if the key is locked or not.
    pub fn import(key_data: &[u8], encoding: DataEncoding) -> crate::Result<Self> {
        let resolved_encoding = encoding.resolve_for_read(key_data);
        let secret = match resolved_encoding {
            ResolvedDataEncoding::Armored => SignedSecretKey::from_armor_single(key_data)
                .map_err(KeyOperationError::Decode)
                .map(|(secret, _)| secret)?,
            ResolvedDataEncoding::Unarmored => {
                SignedSecretKey::from_bytes(key_data).map_err(KeyOperationError::Decode)?
            }
        };
        Ok(Self::new(secret))
    }

    /// Import a locked `OpenPGP` secret key from a byte slice.
    ///
    /// Does not check if the key is locked or not.
    /// Enforces that exactly one key is encoded in the input.
    pub fn import_single_enforce(key_data: &[u8], encoding: DataEncoding) -> crate::Result<Self> {
        let resolved_encoding = encoding.resolve_for_read(key_data);
        let secret = match resolved_encoding {
            ResolvedDataEncoding::Armored => SignedSecretKey::from_armor_single_enforce(key_data)?,
            ResolvedDataEncoding::Unarmored => {
                SignedSecretKey::from_bytes_single_enforce(key_data)?
            }
        };
        Ok(Self::new(secret))
    }

    /// Allows to import multiple locked secret keys from a single binary blob.
    pub fn import_many(key_data: &[u8]) -> crate::Result<Vec<Self>> {
        let mut locked_keys = Vec::new();
        for signed_secret_key in
            SignedSecretKey::from_bytes_many(key_data).map_err(KeyOperationError::Decode)?
        {
            locked_keys.push(Self::new(
                signed_secret_key.map_err(KeyOperationError::Decode)?,
            ));
        }
        Ok(locked_keys)
    }

    /// Export the locked key.
    pub fn export(&self, encoding: DataEncoding) -> crate::Result<Vec<u8>> {
        // The key is already locked.
        self.0.export_unlocked(encoding)
    }

    /// Checks if the secret key is a `Proton` forwarding key.
    pub fn is_forwarding_key(&self, profile: &Profile) -> bool {
        self.0.is_forwarding_key(profile)
    }
}

/// A generic unlocked `OpenPGP` secret key.
/// An `OpenPGP` key consists of a primary key and zero or more subkeys.
/// The secret key contains all the unlocked private key material.
#[derive(Debug, Clone)]
pub struct PrivateKey {
    /// We keep a copy of the public key part of the secret key.
    /// This allows to pass a secret key to act as a public key in verification and encryption operations.
    pub(crate) public: PublicKey,

    /// The inner secret key type from rPGP.
    pub(crate) secret: SignedSecretKey,
}

impl AsRef<PublicKey> for PrivateKey {
    fn as_ref(&self) -> &PublicKey {
        &self.public
    }
}

impl AsPublicKeyRef for PrivateKey {
    fn as_public_key(&self) -> &PublicKey {
        &self.public
    }
}

impl PrivateKey {
    pub(crate) fn new(secret: SignedSecretKey) -> Self {
        let signed_public = SignedPublicKey::from(secret.clone());
        Self {
            public: PublicKey {
                inner: signed_public,
            },
            secret,
        }
    }

    /// Exposes the underlying rPGP signed public key.
    pub fn as_signed_public_key(&self) -> &SignedPublicKey {
        &self.public.inner
    }

    /// Import and unlock `OpenPGP` secret key from a byte slice.
    pub fn import(
        key_data: &[u8],
        password: &[u8],
        encoding: DataEncoding,
    ) -> crate::Result<PrivateKey> {
        let locked = LockedPrivateKey::import(key_data, encoding)?;
        locked.unlock(password, KeyLock::Expected)
    }

    /// Import and unlock `OpenPGP` secret key from a byte slice.
    ///
    /// Enforces that exactly one key is present in the input.
    pub fn import_single_enforce(
        key_data: &[u8],
        password: &[u8],
        encoding: DataEncoding,
    ) -> crate::Result<PrivateKey> {
        let locked = LockedPrivateKey::import_single_enforce(key_data, encoding)?;
        locked.unlock(password, KeyLock::Expected)
    }

    /// Imports multiple unlocked `OpenPGP` secret keys from a single binary blob.
    pub fn import_unlocked_many(key_data: &[u8]) -> crate::Result<Vec<PrivateKey>> {
        let locked_keys = LockedPrivateKey::import_many(key_data)?;
        if locked_keys.iter().any(LockedPrivateKey::is_locked) {
            return Err(KeyOperationError::Locked.into());
        }
        locked_keys
            .into_iter()
            .map(|key| key.unlock("".as_bytes(), KeyLock::NotRequired))
            .collect()
    }

    /// Import an unlocked `OpenPGP` secret key from a byte slice.
    ///
    /// Returns an [`KeyOperationError::Locked`] if the imported key is locked.
    pub fn import_unlocked(key_data: &[u8], encoding: DataEncoding) -> crate::Result<PrivateKey> {
        let locked = LockedPrivateKey::import(key_data, encoding)?;
        if locked.is_locked() {
            return Err(KeyOperationError::Locked.into());
        }
        locked.unlock("".as_bytes(), KeyLock::NotRequired)
    }

    /// Import an unlocked `OpenPGP` secret key from a byte slice.
    ///
    /// Returns an [`KeyOperationError::Locked`] if the imported key is locked.
    /// Enforces that exactly one key is present in the input.
    pub fn import_unlocked_single_enforce(
        key_data: &[u8],
        encoding: DataEncoding,
    ) -> crate::Result<PrivateKey> {
        let locked = LockedPrivateKey::import_single_enforce(key_data, encoding)?;
        if locked.is_locked() {
            return Err(KeyOperationError::Locked.into());
        }
        locked.unlock("".as_bytes(), KeyLock::NotRequired)
    }

    /// Lock the secret key with a password and export it.
    pub fn export(
        &self,
        profile: &Profile,
        password: &[u8],
        encoding: DataEncoding,
    ) -> crate::Result<Vec<u8>> {
        let locked_key = self.lock(profile, password)?;
        match encoding.resolve_for_write() {
            ResolvedDataEncoding::Armored => {
                let armored_bytes = locked_key
                    .0
                    .secret
                    .to_armored_bytes(ArmorOptions {
                        headers: None,
                        include_checksum: !(self.secret.version() == KeyVersion::V6),
                    })
                    .map_err(KeyOperationError::Encode)?;
                Ok(armored_bytes)
            }
            ResolvedDataEncoding::Unarmored => {
                let mut buf = Vec::new();
                locked_key
                    .0
                    .secret
                    .to_writer(&mut buf)
                    .map_err(KeyOperationError::Encode)?;
                Ok(buf)
            }
        }
    }

    /// Export the key in unlocked format.
    ///
    /// # Security
    /// Note that a key exported in unlocked format is not protected by a password.
    /// If unsure use [`Self::export`] instead.
    pub fn export_unlocked(&self, encoding: DataEncoding) -> crate::Result<Vec<u8>> {
        match encoding.resolve_for_write() {
            ResolvedDataEncoding::Armored => {
                let armored_bytes = self
                    .secret
                    .to_armored_bytes(ArmorOptions {
                        headers: None,
                        include_checksum: !(self.secret.version() == KeyVersion::V6),
                    })
                    .map_err(KeyOperationError::Encode)?;
                Ok(armored_bytes)
            }
            ResolvedDataEncoding::Unarmored => {
                let mut buf = Vec::new();
                self.secret
                    .to_writer(&mut buf)
                    .map_err(KeyOperationError::Encode)?;
                Ok(buf)
            }
        }
    }

    /// Lock the secret key with a password.
    pub fn lock(&self, profile: &Profile, password: &[u8]) -> crate::Result<LockedPrivateKey> {
        let mut secret_copy = self.secret.clone();
        let password = Password::from(password);
        secret_copy
            .primary_key
            .set_password_with_s2k(&password, profile.key_s2k_params())
            .map_err(|e| KeyOperationError::Lock(self.key_id(), e))?;
        for subkey in &mut secret_copy.secret_subkeys {
            subkey
                .key
                .set_password_with_s2k(&password, profile.key_s2k_params())
                .map_err(|e| KeyOperationError::Lock(self.key_id(), e))?;
        }
        Ok(LockedPrivateKey::new(secret_copy))
    }

    /// Create a new key modifier with the default profile.
    ///
    /// The returned modifier allows to motify a copy of the secret key.
    pub fn modify(&self) -> KeyModifier {
        KeyModifier::new(self)
    }

    /// Create a new key modifier with the given profile.
    ///
    /// The returned modifier allows to motify a copy of the secret key.
    pub fn modify_with_profile(&self, profile: &Profile) -> KeyModifier {
        KeyModifier::new_with_profile(self, profile)
    }

    /// Certifies the user-id of this key with an external `certifier` key.
    ///
    /// For example: Proton CA
    ///
    /// Returns a builder to configure the optional certification parameters,
    /// such as the expected email of the user-id, the certification date, the
    /// lifetime after which the certification expires, and the profile to use.
    pub fn certify_with_external<'a>(
        &self,
        certifier: &'a PrivateKey,
    ) -> ExternalCertifier<'a, Self> {
        ExternalCertifier::new(self.clone(), certifier)
    }

    /// Verifies that the user-id of this key was certified by an external `certifier` key.
    ///
    /// For example: Proton CA
    ///
    /// Returns a builder to configure the optional verification parameters, such as the
    /// expected email of the user-id, the verification date, and the profile to use.
    pub fn verify_with_external<'a>(
        &self,
        certifier: &'a impl AsPublicKeyRef,
    ) -> ExternalVerifier<'a, Self> {
        ExternalVerifier::new(self.clone(), certifier)
    }

    /// Checks if the secret key is a `Proton` forwarding key.
    pub fn is_forwarding_key(&self, profile: &Profile) -> bool {
        let Ok(decryption_keys) =
            self.secret
                .decryption_keys(CheckUnixTime::disable(), None, true, profile)
        else {
            return false;
        };
        decryption_keys
            .iter()
            .all(PrivateComponentKey::is_forwarding_key)
    }

    /// Checks that the unlocked primary secret key material matches the parsed public key material.
    ///
    /// For AEAD encrypted primary secret keys, the public parts are authenticated
    /// and validation always succeeds.
    pub(crate) fn validate_primary_secret_key(
        &self,
        used_aead_encryption: bool,
    ) -> Result<(), KeySecretParamValidationError> {
        if used_aead_encryption {
            return Ok(());
        }
        let SecretParams::Plain(secret_params) = self.secret.secret_params() else {
            return Err(KeySecretParamValidationError::ValidatePublicPartsOnLocked);
        };
        secret_params.validate_public_params(self.secret.public_params())
    }
}

pub type SessionKeyBytes = Zeroizing<Vec<u8>>;

#[derive(Debug, Clone)]
pub struct SessionKey {
    pub(crate) inner: PlainSessionKey,
}

impl SessionKey {
    pub fn new(key: &[u8], algorithm: SymmetricKeyAlgorithm) -> Self {
        // Default is seipdv1
        Self::new_for_seipdv1(key, algorithm)
    }

    pub fn new_for_seipdv1(key: &[u8], algorithm: SymmetricKeyAlgorithm) -> Self {
        Self {
            inner: PlainSessionKey::V3_4 {
                sym_alg: algorithm,
                key: key.into(),
            },
        }
    }

    pub fn new_for_seipdv2(key: &[u8]) -> Self {
        Self {
            inner: PlainSessionKey::V6 { key: key.into() },
        }
    }

    /// Export the raw session key bytes.
    pub fn export_bytes(&self) -> RawSessionKey {
        match &self.inner {
            PlainSessionKey::V3_4 { key, sym_alg: _ }
            | PlainSessionKey::V5 { key }
            | PlainSessionKey::V6 { key } => key.clone(),
        }
    }

    /// Get the algorithm of the session key.
    ///
    /// A session key extracted from a V5/V6 PKESK packet will not contain
    /// an algorithm.
    pub fn algorithm(&self) -> Option<SymmetricKeyAlgorithm> {
        self.inner.sym_algorithm()
    }

    /// Generate a session key that is used with `OpenPGP` `PKESKv3` and `SEIPDv1` packets.
    pub fn generate_for_seipdv1(algorithm: SymmetricKeyAlgorithm, profile: &Profile) -> Self {
        Self {
            inner: PlainSessionKey::V3_4 {
                sym_alg: algorithm,
                key: generate_session_key_bytes(algorithm, profile),
            },
        }
    }

    /// Generate a session key that is used with `OpenPGP` `PKESKv6` and `SEIPDv2` packets.
    pub fn generate_for_seipdv2(algorithm: SymmetricKeyAlgorithm, profile: &Profile) -> Self {
        Self {
            inner: PlainSessionKey::V6 {
                key: generate_session_key_bytes(algorithm, profile),
            },
        }
    }

    /// Indicates if the session key is for AEAD encryption with `OpenPGP` (`SEIPDv2` RFC9580).
    ///
    /// A session key is marked as AEAD use if extracted from a v6 PKESK packet or
    /// if it is explicitly imported/generated as AEAD.
    pub fn is_seipdv2_aead(&self) -> bool {
        matches!(&self.inner, PlainSessionKey::V6 { .. })
    }

    pub(crate) fn as_raw_session_key(&self) -> &RawSessionKey {
        match &self.inner {
            PlainSessionKey::V3_4 { key, .. }
            | PlainSessionKey::V5 { key }
            | PlainSessionKey::V6 { key } => key,
        }
    }

    /// Helper function to determine the encryption mechanism of the session key.
    ///
    /// This is useful in pure session-key encryption.
    pub(crate) fn encryption_mechanism(
        &self,
        recipients_algo: &RecipientsAlgorithms,
        profile: &Profile,
    ) -> Result<EncryptionMechanism, EncryptionError> {
        match &self.inner {
            PlainSessionKey::V3_4 { sym_alg, .. } => Ok(EncryptionMechanism::SeipdV1(*sym_alg)),
            PlainSessionKey::V6 { key } => {
                let (symmetric_algorithm, aead_algorithm) = match recipients_algo.aead_ciphersuite {
                    Some(aead_ciphersuite) if aead_ciphersuite.0.key_size() == key.len() => {
                        aead_ciphersuite
                    }
                    _ => profile
                        .fallback_ciphersuite_for_key_length(key.len())
                        .ok_or(EncryptionError::NotSupported(
                            "missing aead algorithm for v6 session key".to_owned(),
                        ))?,
                };

                Ok(EncryptionMechanism::SeipdV2(
                    symmetric_algorithm,
                    aead_algorithm,
                ))
            }
            PlainSessionKey::V5 { .. } => Err(EncryptionError::NotSupported(
                "V5 session key is not supported for encryption".to_string(),
            )),
        }
    }
}

impl From<PlainSessionKey> for SessionKey {
    fn from(key: PlainSessionKey) -> Self {
        Self { inner: key }
    }
}

impl From<SessionKey> for PlainSessionKey {
    fn from(value: SessionKey) -> Self {
        value.inner
    }
}

impl<'a> From<&'a SessionKey> for Cow<'a, SessionKey> {
    fn from(key: &'a SessionKey) -> Self {
        Cow::Borrowed(key)
    }
}

impl From<SessionKey> for Cow<'_, SessionKey> {
    fn from(key: SessionKey) -> Self {
        Cow::Owned(key)
    }
}

impl AsRef<[u8]> for SessionKey {
    fn as_ref(&self) -> &[u8] {
        self.as_raw_session_key().as_ref()
    }
}

fn generate_session_key_bytes(
    algorithm: SymmetricKeyAlgorithm,
    profile: &Profile,
) -> RawSessionKey {
    let mut rng = profile.rng();
    let mut key = Zeroizing::new(vec![0_u8; algorithm.key_size()]);
    rng.fill_bytes(&mut key);
    key.into()
}

#[cfg(test)]
mod tests {
    use pgp::types::{KeyDetails, S2kParams};

    use crate::{
        types::UnixTime, DataEncoding, KeyCertificationSelectionError, KeyLock, KeyValidationError,
        LockedPrivateKey, PrivateKey, PrivateKeySelectionExt, Profile, ProfileSettings, PublicKey,
        SignatureUsage,
    };

    use super::PublicKeySelectionExt;

    #[test]
    fn multiple_user_ids() {
        const TEST_KEY: &str = include_str!("../test-data/keys/public_key_v4_multi_user_id.asc");
        let time = UnixTime::new(1_751_881_317);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let (primary_user_id, _) = public_key
            .as_signed_public_key()
            .select_user_id_with_certification(time.into(), &profile)
            .expect("Failed to select primary user id");

        assert_eq!(
            primary_user_id.id.id(),
            b"Bob Babbage <bob@openpgp.example>"
        );
    }

    #[test]
    fn multiple_user_ids_first_revoked() {
        const TEST_KEY: &str =
            include_str!("../test-data/keys/public_key_v4_multi_user_id_revoked.asc");
        let time = UnixTime::new(1_751_881_317);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let (primary_user_id, _) = public_key
            .as_signed_public_key()
            .select_user_id_with_certification(time.into(), &profile)
            .expect("Failed to select primary user id");

        assert_eq!(
            primary_user_id.id.id(),
            b"Golang Gopher <no-reply@golang.com>"
        );
    }

    #[test]
    fn no_user_id() {
        const TEST_KEY: &str = include_str!("../test-data/keys/private_key_v4_no_user_id.asc");
        let date = UnixTime::new(1_751_881_317);
        let profile = Profile::default();

        let private_key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let user_id_result = private_key
            .as_signed_public_key()
            .select_user_id_with_certification(date.into(), &profile);

        assert!(matches!(
            user_id_result,
            Err(KeyCertificationSelectionError::NoIdentity(_))
        ));
    }

    #[test]
    fn primary_key_expired() {
        const TEST_KEY: &str = include_str!("../test-data/keys/public_key_v4_expired.asc");
        let not_expired = UnixTime::new(1_635_464_783);
        let expired = UnixTime::new(1_751_881_317);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let check_result = public_key
            .as_signed_public_key()
            .check_primary_key(not_expired.into(), &profile);

        assert!(check_result.is_ok());

        let check_result = public_key
            .as_signed_public_key()
            .check_primary_key(expired.into(), &profile);

        assert!(matches!(
            check_result,
            Err(KeyCertificationSelectionError::ExpiredKey {
                date: _,
                creation: _,
                expiration: _,
            })
        ));
    }

    #[test]
    fn primary_key_revoked() {
        const TEST_KEY: &str = include_str!("../test-data/keys/public_key_v4_revoked.asc");
        let date = UnixTime::new(1_751_881_317);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let check_result = public_key
            .as_signed_public_key()
            .check_primary_key(date.into(), &profile);

        assert!(matches!(
            check_result,
            Err(KeyCertificationSelectionError::Revoked(_))
        ));
    }

    #[test]
    fn enc_key_selection_subkey_expired_binding_signature() {
        const TEST_KEY: &str =
            include_str!("../test-data/keys/public_key_v4_subkey_expired_binding_signature.asc");
        let expired = UnixTime::new(1_751_881_317);
        let profile = ProfileSettings::builder()
            .allow_encryption_with_future_and_expired_keys(false)
            .build_into_profile();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let selection_result = public_key
            .as_signed_public_key()
            .encryption_key(expired.into(), &profile);

        match selection_result {
            Err(KeyValidationError::NoEncryptionKey(_, selection_errors)) => {
                let selection_error = selection_errors.0.first().expect("No subkey error");
                assert!(matches!(
                    selection_error,
                    KeyValidationError::KeySelfCertification(
                        KeyCertificationSelectionError::NoSelfCertification(_)
                    )
                ));
            }
            _ => panic!("Expected NoEncryptionKey"),
        }
    }

    #[test]
    fn enc_key_selection_revoked() {
        const TEST_KEY: &str = include_str!("../test-data/keys/public_key_v4_subkey_revoked.asc");
        let date = UnixTime::new(1_751_881_317);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let selection_result = public_key
            .as_signed_public_key()
            .encryption_key(date.into(), &profile);

        match selection_result {
            Err(KeyValidationError::KeySelfCertification(
                KeyCertificationSelectionError::NoIdentity(selection_errors),
            )) => {
                let selection_error = selection_errors.0.first().expect("No subkey error");
                assert!(matches!(
                    selection_error,
                    KeyCertificationSelectionError::Revoked(_)
                ));
            }
            _ => panic!("Expected KeySelectionError with Certification"),
        }
    }

    #[test]
    fn enc_key_selection() {
        const TEST_KEY: &str = include_str!("../test-data/keys/public_key_v4.asc");
        let date = UnixTime::new(1_751_984_424);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let selection_result = public_key
            .as_signed_public_key()
            .encryption_key(date.into(), &profile)
            .expect("key selected");

        assert_eq!(
            selection_result.public_key.fingerprint().to_string(),
            "b21caed66abfe03ae31fcf4a27b3a9160a712c96"
        );
    }

    #[test]
    fn signing_key_selection() {
        const TEST_KEY: &str = include_str!("../test-data/keys/private_key_v4.asc");
        let date = UnixTime::new(1_751_984_424);
        let profile = Profile::default();

        let private_key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let selection_result = private_key
            .secret
            .signing_key(date.into(), None, SignatureUsage::Sign, &profile)
            .expect("key selected");
        assert_eq!(
            selection_result.private_key.fingerprint().to_string(),
            "c8e74badf4d2221719212f994faefe8fff37c1e7"
        );
    }

    #[test]
    fn verification_key_selection() {
        const TEST_KEY: &str = include_str!("../test-data/keys/public_key_v4.asc");
        let date = UnixTime::new(1_751_984_424);
        let profile = Profile::default();

        let public_key = PublicKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let selection_result = public_key
            .as_signed_public_key()
            .verification_keys(date.into(), Vec::default(), SignatureUsage::Sign, &profile)
            .expect("key selected");

        let selected = selection_result.into_iter().next().unwrap();
        assert_eq!(
            selected.public_key.fingerprint().to_string(),
            "c8e74badf4d2221719212f994faefe8fff37c1e7"
        );
    }

    #[test]
    fn decryption_key_selection() {
        const TEST_KEY: &str = include_str!("../test-data/keys/private_key_v4.asc");
        let date = UnixTime::new(1_751_984_424);
        let profile = Profile::default();

        let private_key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let selection_result = private_key
            .secret
            .decryption_keys(date.into(), None, false, &profile)
            .expect("key selected");

        let selected = selection_result.into_iter().next().unwrap();
        assert_eq!(
            selected.private_key.fingerprint().to_string(),
            "b21caed66abfe03ae31fcf4a27b3a9160a712c96"
        );
    }

    #[test]
    fn validation_does_not_consider_aead_encrypted_primary_secret_key() {
        use pgp::{
            composed::KeyType,
            crypto::{ecc_curve::ECCCurve, public_key::PublicKeyAlgorithm},
            types::SecretParams,
        };
        use rand::{rngs::StdRng, SeedableRng};

        const TEST_KEY: &str = include_str!("../test-data/keys/locked_private_key_v4_aead.asc");
        const PASSPHRASE: &[u8] = b"passphrase";

        let locked = LockedPrivateKey::import(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import AEAD key");
        assert!(locked.is_locked());

        let SecretParams::Encrypted(encrypted_params) = locked.0.secret.primary_key.secret_params()
        else {
            panic!("expected locked primary secret key");
        };
        assert_eq!(encrypted_params.string_to_key_id(), 253);
        assert!(matches!(
            encrypted_params.string_to_key_params(),
            S2kParams::Aead { .. }
        ));

        let private_key = locked
            .unlock(PASSPHRASE, KeyLock::Expected)
            .expect("unlock should succeed without primary secret key validation");

        let mut corrupted_secret = private_key.secret.clone();
        let key_type = match corrupted_secret.primary_key.public_key().algorithm() {
            PublicKeyAlgorithm::Ed25519 => KeyType::Ed25519,
            PublicKeyAlgorithm::EdDSALegacy => KeyType::Ed25519Legacy,
            PublicKeyAlgorithm::ECDH => KeyType::ECDH(ECCCurve::Curve25519Legacy),
            other => panic!("unsupported test key algorithm: {other:?}"),
        };
        let mut rng = StdRng::seed_from_u64(0xDEAD_BEEF);
        let (_, wrong_secret_params) = key_type.generate(&mut rng).expect("key generation");
        let SecretParams::Plain(wrong_plain) = &wrong_secret_params else {
            panic!("expected plain secret params");
        };
        corrupted_secret.primary_key = pgp::packet::SecretKey::new(
            corrupted_secret.primary_key.public_key().clone(),
            SecretParams::Plain(wrong_plain.clone()),
        )
        .expect("corrupted primary key");
        let corrupted = PrivateKey::new(corrupted_secret);

        corrupted
            .validate_primary_secret_key(true)
            .expect("validation should succeed");
    }
}
