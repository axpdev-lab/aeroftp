use std::cmp::Ordering;

use pgp::{
    composed::{SignedPublicKey, SignedPublicSubKey, SignedSecretKey, SignedSecretSubKey},
    crypto::{ecc_curve::ECCCurve, public_key::PublicKeyAlgorithm},
    packet::{self, KeyFlags},
    ser::Serialize,
    types::{KeyDetails, KeyId, KeyVersion, PublicParams, SignedUser, VerifyingKey},
};

use crate::{
    key::params::PublicParamsExt, types::CheckUnixTime, AnyPublicKey, AnySecretKey,
    CertificationSelectionExt, GenericKeyIdentifier, KeyCertificationSelectionError,
    KeyRequirementError, KeyValidationError, PrivateComponentKey, Profile, PublicComponentKey,
    PublicKeyExt, SignatureExt, UnixTime,
};

use rsa::traits::PublicKeyParts;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(dead_code)]
pub enum SignatureUsage {
    /// The key can be used to certify other keys.
    Certify,
    /// The key can be used to sign data.
    Sign,
}

/// Extension trait for selecting a matching encryption/verification key from an `OpenPGP` key.
pub trait PublicKeySelectionExt: CertificationSelectionExt {
    /// Returns a reference to the primary key of the `OpenPGP` key.
    fn primary_key(&self) -> &packet::PublicKey;

    /// Iterator over the identities (also called `User IDs`) of the `OpenPGP` key.
    ///
    /// See [`User ID packet`](https://www.rfc-editor.org/rfc/rfc9580.html#section-5.11).
    fn iter_identities(&self) -> impl Iterator<Item = &SignedUser>;

    /// Iterator over the subkeys of the `OpenPGP` key.
    fn iter_subkeys(&self) -> impl Iterator<Item = &SignedPublicSubKey>;

    /// Selects the main user-id of the key at the given date.
    ///
    /// Only considers user-ids that are valid at the given date,
    /// and prefers user-ids marked as primary or that have the newest date.
    fn select_user_id_with_certification(
        &self,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(&SignedUser, &packet::Signature), KeyCertificationSelectionError> {
        let mut errors = Vec::new();
        // Filter user-ids that are valid at the given date.
        let mut valid_identities: Vec<_> = self
            .iter_identities()
            .filter_map(|identity| {
                match identity.check_validity(self.primary_key(), date, profile) {
                    Ok(signature) => Some((identity, signature)),
                    Err(err) => {
                        errors.push(err);
                        None
                    }
                }
            })
            .collect();

        // Determine the main user-id.
        valid_identities.sort_by(|(_, a), (_, b)| compare_identities(a, b));
        valid_identities
            .into_iter()
            .next()
            .ok_or(KeyCertificationSelectionError::NoIdentity(errors.into()))
    }

    /// Selects the primary self-certification of the `OpenPGP` key.
    ///
    /// If the key is a V6 key, it returns the latest valid direct key self-certification.
    /// Otherwise, it returns the user-id self-certification that is valid at the given date.
    fn primary_self_signature(
        &self,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<&packet::Signature, KeyCertificationSelectionError> {
        match self.primary_key().version() {
            KeyVersion::V6 => {
                self.latest_valid_self_certification(self.primary_key(), date, profile)
            }
            _ => self
                .select_user_id_with_certification(date, profile)
                .map(|(_, sig)| sig),
        }
    }

    /// Validates if the primary key is valid at the given date.
    ///
    /// Checks if the primary key has a valid self-certification at the given date,
    /// the self-certification is not revoked, and the key is not expired.
    fn check_primary_key(
        &self,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<&packet::Signature, KeyCertificationSelectionError> {
        let primary_self_certification = self.primary_self_signature(date, profile)?;

        if self.revoked(
            self.primary_key(),
            Some(primary_self_certification),
            date,
            profile,
        ) {
            return Err(KeyCertificationSelectionError::Revoked(Box::new(
                primary_self_certification.to_owned(),
            )));
        }

        check_key_not_expired(self.primary_key(), primary_self_certification, date)?;

        Ok(primary_self_certification)
    }

    /// Selects an encryption key from the `OpenPGP` key.
    fn encryption_key(
        &self,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<PublicComponentKey<'_>, KeyValidationError> {
        select_encryption_key(self, date, profile, CheckMode::Encryption)
    }

    /// Selects a key that can be forwarded from the `OpenPGP` key.
    fn forwardable_key(
        &self,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<PublicComponentKey<'_>, KeyValidationError> {
        select_encryption_key(self, date, profile, CheckMode::Forwarding)
    }

    /// Selects all valid keys to verify a signature from `OpenPGP` key.
    ///
    /// The verification keys can be filtered by `KeyId` or `usage`.
    /// If there are no verification keys, an error is returned.
    fn verification_keys(
        &self,
        date: CheckUnixTime,
        key_ids: Vec<GenericKeyIdentifier>,
        usage: SignatureUsage,
        profile: &Profile,
    ) -> Result<Vec<PublicComponentKey<'_>>, KeyValidationError> {
        let mut verification_keys = Vec::new();
        let mut errors = Vec::new();

        // Check if the primary key is a valid.
        let primary_self_certification = self.check_primary_key(date, profile)?;

        let primary_key = self.primary_key();
        check_key_requirements(primary_key, profile).map_err(|err| {
            KeyValidationError::PrimaryRequirement(primary_key.legacy_key_id(), err)
        })?;

        let primary_identifier = primary_key.generic_identifier();
        if !key_ids.is_empty() && !key_ids.iter().any(|key_id| key_id == &primary_identifier) {
            errors.push(KeyValidationError::NoMatchList(
                primary_key.generic_identifier(),
                key_ids.clone().into(),
            ));
        } else {
            match check_signing_key_flags(primary_key, primary_self_certification, profile, usage) {
                Ok(()) => {
                    verification_keys.push(PublicComponentKey::new(
                        AnyPublicKey::PrimaryPublicKey(primary_key),
                        primary_self_certification,
                        primary_self_certification,
                    ));
                }
                Err(err) => {
                    errors.push(KeyValidationError::PrimaryRequirement(
                        primary_key.legacy_key_id(),
                        err,
                    ));
                }
            }
        }

        for sub_key in self.iter_subkeys() {
            // Filter by key id if present.
            let generic_identifier = sub_key.key.generic_identifier();
            if !key_ids.is_empty() && !key_ids.iter().any(|key_id| key_id == &generic_identifier) {
                errors.push(KeyValidationError::NoMatchList(
                    generic_identifier,
                    key_ids.clone().into(),
                ));
                continue;
            }

            // Check subkey certifcations.
            let sub_key_self_certification =
                match sub_key.check_validity(primary_key, date, profile) {
                    Ok(self_certification) => self_certification,
                    Err(err) => {
                        errors.push(KeyValidationError::KeySelfCertification(err));
                        continue;
                    }
                };

            // Check if the subkey is a valid signing key.
            if let Err(err) =
                check_signing_key_flags(&sub_key.key, sub_key_self_certification, profile, usage)
            {
                errors.push(KeyValidationError::SubkeyRequirement(
                    sub_key.legacy_key_id(),
                    err,
                ));
                continue;
            }

            // Check key requirements enforced by the profile.
            if let Err(err) = check_key_requirements(&sub_key.key, profile) {
                errors.push(KeyValidationError::SubkeyRequirement(
                    sub_key.legacy_key_id(),
                    err,
                ));
                continue;
            }
            verification_keys.push(PublicComponentKey::new(
                AnyPublicKey::PublicSubKey(&sub_key.key),
                primary_self_certification,
                sub_key_self_certification,
            ));
        }

        if verification_keys.is_empty() {
            return Err(KeyValidationError::NoVerificationKeys(
                primary_key.legacy_key_id(),
                errors.into(),
            ));
        }
        Ok(verification_keys)
    }
}

/// Extension trait for selecting a matching singning/decryption key from an `OpenPGP` key.
pub trait PrivateKeySelectionExt: PublicKeySelectionExt {
    /// Return the primary secret key.
    fn primary_secret_key(&self) -> &packet::SecretKey;

    /// Returns an iterator over the private subkeys of the `OpenPGP` key.
    fn iter_private_subkeys(&self) -> impl Iterator<Item = &SignedSecretSubKey>;

    /// Selects a signing key from the `OpenPGP` key.
    ///
    /// The signing key can be filtered by `KeyId` or `usage`.
    /// If there are no signing keys, an error is returned.
    fn signing_key(
        &self,
        date: CheckUnixTime,
        key_id: Option<KeyId>,
        usage: SignatureUsage,
        profile: &Profile,
    ) -> Result<PrivateComponentKey<'_>, KeyValidationError> {
        // Check if the primary key is valid.
        let primary_self_certification = self.check_primary_key(date, profile)?;

        let primary_key = self.primary_key();
        check_key_requirements(primary_key, profile).map_err(|err| {
            KeyValidationError::PrimaryRequirement(primary_key.legacy_key_id(), err)
        })?;

        let mut signing_key = None;
        let mut max_time = None;
        let mut errors = Vec::new();
        for sub_key in self.iter_private_subkeys() {
            // Filter by key id if present.
            if let Some(key_id) = key_id {
                if sub_key.legacy_key_id() != key_id {
                    errors.push(KeyValidationError::NoMatch(sub_key.legacy_key_id(), key_id));
                    continue;
                }
            }

            // Check subkey certifications.
            let sub_key_self_certification =
                match sub_key.check_validity(primary_key, date, profile) {
                    Ok(self_certification) => self_certification,
                    Err(err) => {
                        errors.push(KeyValidationError::KeySelfCertification(err));
                        continue;
                    }
                };

            // Check if the subkey is a valid signing key.
            if let Err(err) = check_signing_key_flags(
                &sub_key.key.public_key(),
                sub_key_self_certification,
                profile,
                usage,
            ) {
                errors.push(KeyValidationError::SubkeyRequirement(
                    sub_key.legacy_key_id(),
                    err,
                ));
                continue;
            }

            // Check key requirements enforced by the profile.
            if let Err(err) = check_key_requirements(sub_key.key.public_key(), profile) {
                errors.push(KeyValidationError::SubkeyRequirement(
                    sub_key.legacy_key_id(),
                    err,
                ));
                continue;
            }

            let should_prefer = max_time.is_none_or(|current_max_time| {
                sub_key.public_key().created_at() > current_max_time
            });

            if should_prefer {
                signing_key = Some(PrivateComponentKey::new(
                    AnySecretKey::SecretSubKey(&sub_key.key),
                    primary_self_certification,
                    sub_key_self_certification,
                ));
                max_time = Some(sub_key.public_key().created_at());
            }
        }

        if let Some(signing_subkey) = signing_key {
            return Ok(signing_subkey);
        }

        // Check if the primary key is a valid signing key.
        if let Some(key_id) = key_id {
            if primary_key.legacy_key_id() != key_id {
                errors.push(KeyValidationError::NoMatch(
                    primary_key.legacy_key_id(),
                    key_id,
                ));
                return Err(KeyValidationError::NoSigningKey(
                    primary_key.legacy_key_id(),
                    errors.into(),
                ));
            }
        }

        if let Err(err) =
            check_signing_key_flags(primary_key, primary_self_certification, profile, usage)
        {
            errors.push(KeyValidationError::PrimaryRequirement(
                primary_key.legacy_key_id(),
                err,
            ));
            return Err(KeyValidationError::NoSigningKey(
                primary_key.legacy_key_id(),
                errors.into(),
            ));
        }

        Ok(PrivateComponentKey::new(
            AnySecretKey::PrimarySecretKey(self.primary_secret_key()),
            primary_self_certification,
            primary_self_certification,
        ))
    }

    /// Selects all valid keys to decrypt a message from the `OpenPGP` key.
    ///
    /// The decryption keys can be filtered by `KeyId`.
    /// If there are no decryption keys, an error is returned.
    fn decryption_keys(
        &self,
        date: CheckUnixTime,
        key_id: Option<GenericKeyIdentifier>,
        allow_forwarding_decryption: bool,
        profile: &Profile,
    ) -> Result<Vec<PrivateComponentKey<'_>>, KeyValidationError> {
        let primary_key = self.primary_key();
        let primary_self_certification = self.primary_self_signature(date, profile)?;

        let mut errors = Vec::new();
        let mut decryption_keys = Vec::new();

        let mode = CheckMode::Decryption {
            allow_forwarding_decryption,
        };

        for sub_key in self.iter_private_subkeys() {
            // Filter by key-id if present.
            if let Some(key_id) = &key_id {
                if &sub_key.key.generic_identifier() != key_id {
                    errors.push(KeyValidationError::NoMatchDecryption(
                        sub_key.legacy_key_id(),
                        key_id.clone(),
                    ));
                    continue;
                }
            }

            // Check subkey self-certification.
            let subkey_self_certification =
                match sub_key.latest_valid_self_certification(primary_key, date, profile) {
                    Ok(subkey_self_certification) => subkey_self_certification,
                    Err(err) => {
                        errors.push(KeyValidationError::KeySelfCertification(err));
                        continue;
                    }
                };

            // Check if the subkey is a valid decryption key.
            if let Err(err) = check_valid_encryption_key(
                sub_key.key.public_key(),
                subkey_self_certification,
                profile,
                mode,
            ) {
                errors.push(KeyValidationError::SubkeyRequirement(
                    sub_key.legacy_key_id(),
                    err,
                ));
                continue;
            }

            decryption_keys.push(PrivateComponentKey::new(
                AnySecretKey::SecretSubKey(&sub_key.key),
                primary_self_certification,
                subkey_self_certification,
            ));
        }

        // Check if we can decrypt with the primary key for compatibility.
        if let Err(err) =
            check_valid_encryption_key(primary_key, primary_self_certification, profile, mode)
        {
            errors.push(KeyValidationError::PrimaryRequirement(
                primary_key.legacy_key_id(),
                err,
            ));
        } else {
            decryption_keys.push(PrivateComponentKey::new(
                AnySecretKey::PrimarySecretKey(self.primary_secret_key()),
                primary_self_certification,
                primary_self_certification,
            ));
        }

        if decryption_keys.is_empty() {
            return Err(KeyValidationError::NoDecryptionKeys(
                primary_key.legacy_key_id(),
                errors.into(),
            ));
        }
        Ok(decryption_keys)
    }
}

/// Selects the best valid encryption key of `key` for the given `mode`.
fn select_encryption_key<'a, T: PublicKeySelectionExt + ?Sized>(
    key: &'a T,
    date: CheckUnixTime,
    profile: &Profile,
    mode: CheckMode,
) -> Result<PublicComponentKey<'a>, KeyValidationError> {
    // Disable time checks on the key if the profile enables encryption with future/expired keys.
    let encryption_date = if profile.allow_encryption_with_future_or_expired_keys() {
        CheckUnixTime::disable()
    } else {
        date
    };

    // Check if the primary key is valid.
    let primary_self_certification = key.check_primary_key(encryption_date, profile)?;

    let primary_key = key.primary_key();
    check_key_requirements(primary_key, profile)
        .map_err(|err| KeyValidationError::PrimaryRequirement(primary_key.legacy_key_id(), err))?;

    // Select the best subkey that is a valid encryption key.
    let mut errors = Vec::new();
    let mut subkey_selection = None;
    let mut is_pq = false;
    let mut max_time = None;

    for sub_key in key.iter_subkeys() {
        // Check subkey certifications.
        let sub_key_self_certification =
            match sub_key.check_validity(primary_key, encryption_date, profile) {
                Ok(self_certification) => self_certification,
                Err(err) => {
                    errors.push(KeyValidationError::KeySelfCertification(err));
                    continue;
                }
            };

        // Check if the subkey is a valid encryption key.
        if let Err(err) =
            check_valid_encryption_key(sub_key, sub_key_self_certification, profile, mode)
        {
            errors.push(KeyValidationError::SubkeyRequirement(
                sub_key.legacy_key_id(),
                err,
            ));
            continue;
        }

        // Check key requirements enforced by the profile.
        if let Err(err) = check_key_requirements(&sub_key.key, profile) {
            errors.push(KeyValidationError::SubkeyRequirement(
                sub_key.legacy_key_id(),
                err,
            ));
            continue;
        }

        // Prefer newer subkeys.
        let should_prefer = max_time.is_none_or(|current_max_time| {
            sub_key.created_at() > current_max_time || (!is_pq && sub_key.algorithm().is_pqc())
        });

        if should_prefer {
            subkey_selection = Some(PublicComponentKey::new(
                AnyPublicKey::PublicSubKey(&sub_key.key),
                primary_self_certification,
                sub_key_self_certification,
            ));
            max_time = Some(sub_key.created_at());
            is_pq = sub_key.algorithm().is_pqc();
        }
    }

    // If we have found a subkey that is a valid encryption key, return it.
    if let Some(subkey_selection) = subkey_selection {
        return Ok(subkey_selection);
    }

    // Check if the primary key is a valid encryption key.
    if let Err(err) =
        check_valid_encryption_key(primary_key, primary_self_certification, profile, mode)
    {
        errors.push(KeyValidationError::PrimaryRequirement(
            primary_key.legacy_key_id(),
            err,
        ));
        return Err(KeyValidationError::NoEncryptionKey(
            primary_key.legacy_key_id(),
            errors.into(),
        ));
    }

    Ok(PublicComponentKey::new(
        AnyPublicKey::PrimaryPublicKey(primary_key),
        primary_self_certification,
        primary_self_certification,
    ))
}

fn check_signing_key_flags(
    public_key: &impl VerifyingKey,
    self_certification: &packet::Signature,
    profile: &Profile,
    usage: SignatureUsage,
) -> Result<(), KeyRequirementError> {
    match usage {
        SignatureUsage::Certify => {
            check_valid_certification_key(public_key, self_certification, profile)
        }
        SignatureUsage::Sign => check_valid_signing_key(public_key, self_certification, profile),
    }
}

#[derive(Debug, Clone, Copy)]
enum CheckMode {
    Encryption,
    Decryption {
        allow_forwarding_decryption: bool,
    },
    /// Selecting a key to derive a forwarding key from.
    Forwarding,
}

impl CheckMode {
    fn allow_forwarding_keys(self) -> bool {
        match self {
            CheckMode::Encryption => false,
            CheckMode::Forwarding => true,
            CheckMode::Decryption {
                allow_forwarding_decryption,
            } => allow_forwarding_decryption,
        }
    }

    fn is_decryption(self) -> bool {
        matches!(self, CheckMode::Decryption { .. })
    }
}

fn check_valid_encryption_key(
    public_key: &impl KeyDetails,
    signature: &packet::Signature,
    profile: &Profile,
    mode: CheckMode,
) -> Result<(), KeyRequirementError> {
    // Check the key algorithm.
    if !public_key.algorithm().can_encrypt() {
        return Err(KeyRequirementError::InvalidUsageAlgorithm(
            public_key.algorithm(),
        ));
    }

    // Check if the library supports performing encryption or decryption with the key.
    if !public_key.public_params().can_perform_operation() {
        return Err(KeyRequirementError::UnsupportedAlgorithm);
    }

    // Check the key flags if the profile does not ignore them.
    if profile.ignore_key_flags() {
        return Ok(());
    }

    // Check the key flags.
    let key_flags = signature.key_flags();
    let allow_signing_flags =
        mode.is_decryption() && profile.allow_insecure_decryption_with_signing_keys();

    let valid_for_encryption = key_flags.encrypt_comms()
        || key_flags.encrypt_storage()
        || (allow_signing_flags && key_flags.sign())
        || (mode.allow_forwarding_keys() && key_flags.draft_decrypt_forwarded());
    if !valid_for_encryption {
        return Err(KeyRequirementError::InvalidEncryptionKeyFlags(
            key_flags.into(),
        ));
    }

    Ok(())
}

fn check_valid_certification_key(
    public_key: &impl VerifyingKey,
    signature: &packet::Signature,
    profile: &Profile,
) -> Result<(), KeyRequirementError> {
    check_valid_key_with_flags(public_key, signature, profile, KeyFlags::certify)
}

fn check_valid_signing_key(
    public_key: &impl VerifyingKey,
    signature: &packet::Signature,
    profile: &Profile,
) -> Result<(), KeyRequirementError> {
    check_valid_key_with_flags(public_key, signature, profile, KeyFlags::sign)
}

fn check_valid_key_with_flags<F>(
    public_key: &impl VerifyingKey,
    signature: &packet::Signature,
    profile: &Profile,
    check_flag: F,
) -> Result<(), KeyRequirementError>
where
    F: Fn(&KeyFlags) -> bool,
{
    // Check the key algorithm.
    if !public_key.algorithm().can_sign() {
        return Err(KeyRequirementError::InvalidUsageAlgorithm(
            public_key.algorithm(),
        ));
    }

    // Check if the library supports to perform the operation with the key.
    if !public_key.public_params().can_perform_operation() {
        return Err(KeyRequirementError::UnsupportedAlgorithm);
    }

    // Check the key flags if the profile does not ignore them.
    if profile.ignore_key_flags() {
        return Ok(());
    }

    // Check the key flags.
    let key_flags = signature.key_flags();
    if !check_flag(&key_flags) {
        return Err(KeyRequirementError::InvalidSigningKeyFlags(
            key_flags.into(),
        ));
    }

    Ok(())
}

/// Helper comparison function to determine the order of user-ids.
///
/// Is used to sort the user-ids in ascending order, and select the first one in the list.
fn compare_identities(current: &packet::Signature, potential: &packet::Signature) -> Ordering {
    match (current.is_primary(), potential.is_primary()) {
        (true, false) => Ordering::Less,    // Prefer current
        (false, true) => Ordering::Greater, // Prefer potential
        _ => {
            // Both have same primary status, compare by creation time
            match (current.unix_created_at(), potential.unix_created_at()) {
                (Ok(current_time), Ok(potential_time)) => potential_time.cmp(&current_time), // Prefer newer
                (Err(_), Ok(_)) => Ordering::Greater, // Prefer potential if current has no valid time
                _ => Ordering::Less, // Prefer current if potential has no valid time
            }
        }
    }
}

/// Checks if the key is expired at the given date based on its self-certification.
///
/// Returns an error if the key is expired.
/// The key is expired if it is in the future or if it has an expiration time
/// that is before the given date.
pub(crate) fn check_key_not_expired<K: VerifyingKey + Serialize>(
    key: &K,
    self_signature: &packet::Signature,
    check_date: CheckUnixTime,
) -> Result<(), KeyCertificationSelectionError> {
    let Some(date) = check_date.at() else {
        return Ok(());
    };
    let key_creation_time = key.created_at();
    if UnixTime::from(key_creation_time) > date {
        return Err(KeyCertificationSelectionError::FutureKey {
            date,
            creation: key_creation_time.into(),
        });
    }
    if let Some(expiration_time) = self_signature.key_expiration_time() {
        if expiration_time.as_secs() == 0 {
            // If the key expiration time is zero, it means that the key has no expiration time,
            // and is thus not expired.
            return Ok(());
        }
        let expiration_date = UnixTime::new(
            u64::from(key_creation_time.as_secs()) + u64::from(expiration_time.as_secs()),
        );
        if expiration_date < date {
            return Err(KeyCertificationSelectionError::ExpiredKey {
                date,
                creation: key_creation_time.into(),
                expiration: expiration_date,
            });
        }
    }
    Ok(())
}

/// Checks if the key meets the requirements of the profile.
///
/// The requirements are:
/// - The key is supported by the library.
/// - The key algorithm is accepted by the profile.
/// - The key has enough bits for the RSA algorithm.
/// - The key has a valid curve for the ECC algorithm.
fn check_key_requirements(
    public_key: impl VerifyingKey,
    profile: &Profile,
) -> Result<(), KeyRequirementError> {
    if profile.reject_public_key_algorithm(public_key.algorithm()) {
        return Err(KeyRequirementError::WeakAlgorithm(public_key.algorithm()));
    }
    if !public_key.public_params().can_perform_operation() {
        return Err(KeyRequirementError::UnsupportedAlgorithm);
    }
    match public_key.algorithm() {
        PublicKeyAlgorithm::RSA | PublicKeyAlgorithm::RSASign | PublicKeyAlgorithm::RSAEncrypt => {
            let params = public_key.public_params();
            if let PublicParams::RSA(params) = params {
                if params.key.n().bits() < profile.min_rsa_bits() {
                    return Err(KeyRequirementError::WeakRsaAlgorithm(
                        params.key.n().bits(),
                        profile.min_rsa_bits(),
                    ));
                }
            }
            Ok(())
        }
        PublicKeyAlgorithm::ECDH | PublicKeyAlgorithm::EdDSALegacy | PublicKeyAlgorithm::ECDSA => {
            let curve = match public_key.public_params() {
                PublicParams::EdDSALegacy(params) => Some(params.curve()),
                PublicParams::ECDH(params) => Some(params.curve()),
                PublicParams::ECDSA(params) => Some(params.curve()),
                _ => None,
            };

            if let Some(curve) = curve {
                if profile.reject_ecc_curve(&curve) {
                    return Err(KeyRequirementError::WeakEccAlgorithm(curve));
                }
                if public_key.version() == KeyVersion::V6
                    && (curve == ECCCurve::Ed25519Legacy || curve == ECCCurve::Curve25519Legacy)
                {
                    return Err(KeyRequirementError::MixedLegacyAlgorithms(curve));
                }
            }
            Ok(())
        }
        _ => Ok(()),
    }
}

/// Checks that `sub_key` is a valid subkey of `primary_key` to derive a
/// forwarding key from.
///
/// Accepts regular encryption subkeys as well as forwarding subkeys.
/// Unlike [`PrivateKeySelectionExt::decryption_keys`] this checks a single,
/// caller-chosen subkey and does not fall back to the primary key.
pub(crate) fn check_subkey_for_forwarding<K>(
    sub_key: &SignedSecretSubKey,
    primary_key: &K,
    encryption_date: CheckUnixTime,
    profile: &Profile,
) -> Result<(), KeyValidationError>
where
    K: VerifyingKey + Serialize,
{
    // Check subkey certifications.
    let sub_key_self_certification = sub_key
        .check_validity(primary_key, encryption_date, profile)
        .map_err(KeyValidationError::KeySelfCertification)?;

    // Check if the subkey is a valid encryption key.
    check_valid_encryption_key(
        sub_key.public_key(),
        sub_key_self_certification,
        profile,
        CheckMode::Forwarding,
    )
    .map_err(|err| KeyValidationError::SubkeyRequirement(sub_key.legacy_key_id(), err))?;

    // Check key requirements enforced by the profile.
    check_key_requirements(sub_key.public_key(), profile)
        .map_err(|err| KeyValidationError::SubkeyRequirement(sub_key.legacy_key_id(), err))?;

    Ok(())
}

impl PublicKeySelectionExt for SignedPublicKey {
    fn primary_key(&self) -> &packet::PublicKey {
        &self.primary_key
    }

    fn iter_identities(&self) -> impl Iterator<Item = &SignedUser> {
        self.details.users.iter()
    }

    fn iter_subkeys(&self) -> impl Iterator<Item = &SignedPublicSubKey> {
        self.public_subkeys.iter()
    }
}

impl PublicKeySelectionExt for SignedSecretKey {
    fn primary_key(&self) -> &packet::PublicKey {
        self.primary_key.public_key()
    }

    fn iter_identities(&self) -> impl Iterator<Item = &SignedUser> {
        self.details.users.iter()
    }

    fn iter_subkeys(&self) -> impl Iterator<Item = &SignedPublicSubKey> {
        self.public_subkeys.iter()
    }
}

impl PrivateKeySelectionExt for SignedSecretKey {
    fn primary_secret_key(&self) -> &packet::SecretKey {
        &self.primary_key
    }

    fn iter_private_subkeys(&self) -> impl Iterator<Item = &SignedSecretSubKey> {
        self.secret_subkeys.iter()
    }
}
