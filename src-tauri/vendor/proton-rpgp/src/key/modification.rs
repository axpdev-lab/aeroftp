use pgp::{
    composed::SignedSecretKey,
    packet::{self, KeyFlags, PubKeyInner, SignatureType, UserId},
    types::{KeyDetails, SignedUser, Tag},
};

use crate::{
    check_signature_details, convert_user_ids, primary_key_flags, AsPublicKeyRef,
    CertificationSelectionExt, CheckUnixTime, KeyCertificationError, KeyGenerationProfileBuilder,
    KeyModificationError, KeyUserId, Lifetime, PacketPublicSubkeyExt, PacketUserIdExt, PrivateKey,
    PrivateKeySelectionExt, Profile, PublicKey, PublicKeyExt, PublicKeySelectionExt,
    SignatureError, SignatureExt, SignatureUsage, UnixTime, DEFAULT_PROFILE,
};

pub struct KeyModifier {
    /// The key to modify.
    key: SignedSecretKey,

    /// The profile to use for the key modification.
    profile: Profile,

    /// The new user-ids to add to the key.
    new_user_ids: Vec<KeyUserId>,

    /// Whether to remove the old user-ids from the key.
    clear_existing_user_ids: bool,

    /// Whether reset and create new signatures for user-ids/direct-key/subkeys.
    ///
    /// All existing signatures on these parts are removed and new signatures are created.
    reset_signatures: bool,

    /// The date of the key generation for the self-certifications and key creation time.
    modification_date: UnixTime,

    /// Override the key creation time of the primary key and subkeys.
    key_creation_time: Option<UnixTime>,
}

impl KeyModifier {
    pub fn new(key_to_modify: &PrivateKey) -> Self {
        Self::new_with_profile(key_to_modify, &DEFAULT_PROFILE)
    }

    pub fn new_with_profile(key_to_modify: &PrivateKey, profile: &Profile) -> Self {
        Self {
            key: key_to_modify.secret.clone(),
            profile: profile.clone(),
            new_user_ids: Vec::new(),
            clear_existing_user_ids: false,
            reset_signatures: false,
            modification_date: UnixTime::now().unwrap_or_default(),
            key_creation_time: None,
        }
    }

    /// Add a new user-id to the key.
    pub fn add_user_id(mut self, name: &str, email: &str) -> Self {
        self.new_user_ids.push(KeyUserId {
            name: name.to_string(),
            email: email.to_string(),
        });
        self
    }

    /// Remove all existing user-ids from the key.
    pub fn clear_existing_user_ids(mut self) -> Self {
        self.clear_existing_user_ids = true;
        self
    }

    /// Resets and regenerates signatures for user-IDs, direct-key info, and subkeys.
    ///
    /// When enabled, all existing signatures related to these components are removed,
    /// and new self-signatures are created according to the modification configuration.
    pub fn reset_signatures(mut self) -> Self {
        self.reset_signatures = true;
        self
    }

    /// Override the key creation time of the primary key and subkeys.  
    ///
    /// Automatically triggers a signature reset.
    pub fn with_key_creation_time(mut self, key_creation_time: UnixTime) -> Self {
        self.reset_signatures = true;
        self.key_creation_time = Some(key_creation_time);
        self
    }

    /// Set the date of the key modification.
    pub fn with_date(mut self, date: UnixTime) -> Self {
        self.modification_date = date;
        self
    }

    /// Apply the key modifications according to the configuration.
    pub fn apply(self) -> crate::Result<PrivateKey> {
        let sub_key_flags = self.collect_sub_key_flags()?;
        let modifier = self
            .apply_primary_key_modifications()?
            .apply_subkey_modifications(sub_key_flags)?;
        Ok(PrivateKey::new(modifier.key))
    }

    fn collect_sub_key_flags(&self) -> Result<Vec<KeyFlags>, KeyModificationError> {
        if self.reset_signatures {
            self.key
                .secret_subkeys
                .iter()
                .map(|sub_key| {
                    let self_sig = sub_key.latest_valid_self_certification(
                        self.key.primary_key(),
                        CheckUnixTime::disable(),
                        &self.profile,
                    )?;
                    Ok(self_sig.key_flags())
                })
                .collect()
        } else {
            Ok(Vec::default())
        }
    }

    fn apply_primary_key_modifications(mut self) -> Result<Self, KeyModificationError> {
        let mut rng = self.profile.rng();
        let preferred_hash = self.profile.key_hash_algorithm();

        if self.clear_existing_user_ids {
            self.key.details.users.clear();
        }

        if self.reset_signatures {
            if let Some(key_creation_time) = self.key_creation_time {
                let pub_key = PubKeyInner::new(
                    self.key.primary_key().version(),
                    self.key.primary_key().algorithm(),
                    key_creation_time.into(),
                    None,
                    self.key.primary_key().public_params().clone(),
                )
                .map_err(KeyModificationError::PrimaryKeyModification)?;
                let primary_pub_key = packet::PublicKey::from_inner(pub_key)
                    .map_err(KeyModificationError::PrimaryKeyModification)?;
                let primary_secret_key = packet::SecretKey::new(
                    primary_pub_key.clone(),
                    self.key.primary_secret_key().secret_params().clone(),
                )
                .map_err(KeyModificationError::PrimaryKeyModification)?;
                self.key.primary_key = primary_secret_key;
            }
        }

        let key_gen_config = KeyGenerationProfileBuilder::default()
            .key_version(self.key.version())
            .build();

        let (primary_user_id, non_primary_user_ids) = if self.key.details.users.is_empty() {
            convert_user_ids(&self.new_user_ids)?
        } else {
            let mut old_user_ids = if self.reset_signatures {
                std::mem::take(&mut self.key.details.users)
                    .into_iter()
                    .map(|user| user.id)
                    .collect::<Vec<_>>()
            } else {
                Vec::with_capacity(self.new_user_ids.len())
            };
            for user_id in &self.new_user_ids {
                old_user_ids.push(user_id.try_to_user_id()?);
            }

            (None, old_user_ids)
        };

        let key_details_config = key_gen_config.create_key_details_config(
            primary_user_id,
            non_primary_user_ids,
            primary_key_flags(),
        );

        let updated_signed_key_details = key_details_config
            .sign_with(
                self.key.primary_secret_key(),
                self.key.primary_key(),
                self.modification_date,
                preferred_hash,
                &mut rng,
                &self.profile,
            )
            .map_err(KeyModificationError::Signing)?;

        let mut new_users = Vec::with_capacity(
            updated_signed_key_details.users.len() + self.key.details.users.len(),
        );
        new_users.extend(updated_signed_key_details.users);
        new_users.extend(self.key.details.users);
        self.key.details.users = new_users;

        if self.reset_signatures {
            self.key.details.revocation_signatures.clear();
            self.key.details.direct_signatures.clear();
            self.key
                .details
                .direct_signatures
                .extend(updated_signed_key_details.direct_signatures);
        }

        Ok(self)
    }

    fn apply_subkey_modifications(
        mut self,
        sub_key_flags: Vec<KeyFlags>,
    ) -> Result<Self, KeyModificationError> {
        if self.reset_signatures {
            let mut rng = self.profile.rng();
            let preferred_hash = self.profile.key_hash_algorithm();

            let updated_subkeys: Vec<_> = self
                .key
                .secret_subkeys
                .iter()
                .map(|sub_key| {
                    if let Some(key_creation_time) = self.key_creation_time {
                        let pub_key = PubKeyInner::new(
                            sub_key.key.version(),
                            sub_key.key.algorithm(),
                            key_creation_time.into(),
                            None,
                            sub_key.key.public_key().public_params().clone(),
                        )
                        .map_err(KeyModificationError::SubkeyModification)?;
                        let subkey_public = packet::PublicSubkey::from_inner(pub_key)
                            .map_err(KeyModificationError::SubkeyModification)?;
                        packet::SecretSubkey::new(subkey_public, sub_key.secret_params().clone())
                            .map_err(KeyModificationError::SubkeyModification)
                    } else {
                        Ok(sub_key.key.clone())
                    }
                })
                .collect::<Result<_, _>>()?;

            let new_subkey_signatures = updated_subkeys
                .iter()
                .zip(sub_key_flags)
                .map(|(sub_key, subkey_flag)| {
                    sub_key
                        .public_key()
                        .sign_with(
                            self.key.primary_secret_key(),
                            self.key.primary_key(),
                            self.modification_date,
                            preferred_hash,
                            subkey_flag,
                            None,
                            &mut rng,
                            &self.profile,
                        )
                        .map_err(KeyModificationError::Signing)
                })
                .collect::<Result<Vec<_>, _>>()?;

            for (sub_key, (updated_subkey, signature)) in self
                .key
                .secret_subkeys
                .iter_mut()
                .zip(updated_subkeys.into_iter().zip(new_subkey_signatures))
            {
                sub_key.key = updated_subkey;
                sub_key.signatures.clear();
                sub_key.signatures.push(signature);
            }
        }

        Ok(self)
    }
}

/// A builder to certify the single user-id of a key with an external `certifier` key.
///
/// For example: Proton CA
pub struct ExternalCertifier<'a, K> {
    /// The key whose user-id is being certified.
    key: K,

    /// The key used to issue the certification.
    certifier: &'a PrivateKey,

    /// The expected email address of the certified user-id.
    check_email: Option<String>,

    /// The date of the certification.
    date: UnixTime,

    /// The lifetime of the certification, after which it expires.
    lifetime: Option<Lifetime>,

    /// The profile to use for the certification.
    profile: Profile,
}

impl<'a, K> ExternalCertifier<'a, K> {
    pub(crate) fn new(key: K, certifier: &'a PrivateKey) -> Self {
        Self {
            key,
            certifier,
            check_email: None,
            date: UnixTime::now().unwrap_or_default(),
            lifetime: None,
            profile: DEFAULT_PROFILE.clone(),
        }
    }

    /// Enforce that the certified user-id contains the given email address.
    pub fn with_email(mut self, email: &str) -> Self {
        self.check_email = Some(email.to_string());
        self
    }

    /// Set the date of the certification.
    pub fn with_date(mut self, date: UnixTime) -> Self {
        self.date = date;
        self
    }

    /// Set the lifetime of the certification, after which it expires.
    ///
    /// If not set, the certification does not expire.
    pub fn with_lifetime(mut self, lifetime: Lifetime) -> Self {
        self.lifetime = Some(lifetime);
        self
    }

    /// Set the profile to use for the certification.
    pub fn with_profile(mut self, profile: &Profile) -> Self {
        self.profile = profile.clone();
        self
    }
}

impl ExternalCertifier<'_, PublicKey> {
    /// Apply the certification according to the configuration.
    pub fn apply(self) -> Result<PublicKey, KeyCertificationError> {
        let certified_user_ids = certify_user_id_with_external(
            &self.key.inner.primary_key,
            self.certifier,
            &self.key.inner.details.users,
            self.check_email.as_deref(),
            self.date,
            self.lifetime,
            &self.profile,
        )?;
        let mut key = self.key;
        key.inner.details.users = certified_user_ids;
        Ok(key)
    }
}

impl ExternalCertifier<'_, PrivateKey> {
    /// Apply the certification according to the configuration.
    pub fn apply(self) -> Result<PrivateKey, KeyCertificationError> {
        let certified_user_ids = certify_user_id_with_external(
            self.key.secret.primary_key.public_key(),
            self.certifier,
            &self.key.secret.details.users,
            self.check_email.as_deref(),
            self.date,
            self.lifetime,
            &self.profile,
        )?;
        let mut secret = self.key.secret;
        secret.details.users = certified_user_ids;
        Ok(PrivateKey::new(secret))
    }
}

/// A builder to verify that the single user-id of a key was certified by an external
/// `certifier` key.
pub struct ExternalVerifier<'a, K> {
    /// The key whose user-id certification is being verified.
    key: K,

    /// The primary key of the key that is expected to have issued the certification.
    certifier_primary_key: &'a PublicKey,

    /// The expected email address of the certified user-id.
    check_email: Option<String>,

    /// The date at which the certification must be valid.
    date: UnixTime,

    /// The profile to use for the verification.
    profile: Profile,
}

impl<'a, K> ExternalVerifier<'a, K> {
    pub(crate) fn new(key: K, certifier: &'a impl AsPublicKeyRef) -> Self {
        Self {
            key,
            certifier_primary_key: certifier.as_public_key(),
            check_email: None,
            date: UnixTime::now().unwrap_or_default(),
            profile: DEFAULT_PROFILE.clone(),
        }
    }

    /// Enforce that the certified user-id contains the given email address.
    pub fn with_email(mut self, email: &str) -> Self {
        self.check_email = Some(email.to_string());
        self
    }

    /// Set the date at which the certification must be valid.
    pub fn with_date(mut self, date: UnixTime) -> Self {
        self.date = date;
        self
    }

    /// Set the profile to use for the verification.
    pub fn with_profile(mut self, profile: &Profile) -> Self {
        self.profile = profile.clone();
        self
    }
}

impl ExternalVerifier<'_, PublicKey> {
    /// Verifies the certification according to the configuration.
    pub fn verify(self) -> Result<(), KeyCertificationError> {
        verify_user_id_with_external(
            &self.key.inner.primary_key,
            self.certifier_primary_key,
            &self.key.inner.details.users,
            self.check_email.as_deref(),
            self.date,
            &self.profile,
        )
    }
}

impl ExternalVerifier<'_, PrivateKey> {
    /// Verifies the certification according to the configuration.
    pub fn verify(self) -> Result<(), KeyCertificationError> {
        verify_user_id_with_external(
            self.key.secret.primary_key.public_key(),
            self.certifier_primary_key,
            &self.key.secret.details.users,
            self.check_email.as_deref(),
            self.date,
            &self.profile,
        )
    }
}

/// Verifies that the single user-id of a key carries a valid certification issued by the
/// `certifier` key, that is not expired at `date`.
fn verify_user_id_with_external(
    certified_primary_key: &packet::PublicKey,
    certifier: &PublicKey,
    user_ids: &[SignedUser],
    check_email: Option<&str>,
    date: UnixTime,
    profile: &Profile,
) -> Result<(), KeyCertificationError> {
    // Enforce that the key has a single user-id that matches the email.
    if user_ids.len() > 1 {
        return Err(KeyCertificationError::TooManyUserIds);
    }
    let Some(user) = user_ids.first() else {
        return Err(KeyCertificationError::NoUserId);
    };
    check_user_id_email(&user.id, check_email)?;

    // Time checks are disabled to also allow verifying expired keys.
    user.check_validity(certified_primary_key, CheckUnixTime::disable(), profile)
        .map_err(KeyCertificationError::UserIdSelfCertification)?;

    let verificaiton_keys = certifier
        .as_signed_public_key()
        .verification_keys(
            date.into(),
            Vec::default(),
            SignatureUsage::Certify,
            profile,
        )
        .map_err(KeyCertificationError::VerificationKeySelection)?;

    let mut failures: Vec<SignatureError> = Vec::new();
    for signature in user
        .signatures
        .iter()
        .filter(|sig| sig.is_certification() && !sig.is_revocation())
    {
        let sig_identifier = signature.issuer_generic_identifier();

        let matching_keys = verificaiton_keys.iter().filter(|key| {
            sig_identifier
                .iter()
                .any(|identifier| identifier == &key.public_key.generic_identifier())
        });

        for verification_key in matching_keys {
            if let Err(err) = signature.verify_third_party_certification(
                certified_primary_key,
                &verification_key.public_key,
                Tag::UserId,
                &user.id,
            ) {
                failures.push(SignatureError::Verification(err));
                continue;
            }

            match check_signature_details(signature, CheckUnixTime::enable(date), profile) {
                Ok(()) => return Ok(()),
                Err(err) => failures.push(err),
            }
        }
    }

    Err(KeyCertificationError::NoValidCertification(failures.into()))
}

/// Certifies the single user-id of a key with an external `certifier` key.
///
/// If a `lifetime` in seconds is given, the certification expires that many
/// seconds after `date`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn certify_user_id_with_external(
    certified_primary_key: &packet::PublicKey,
    certifier: &PrivateKey,
    user_ids: &[SignedUser],
    check_email: Option<&str>,
    date: UnixTime,
    lifetime: Option<Lifetime>,
    profile: &Profile,
) -> Result<Vec<SignedUser>, KeyCertificationError> {
    // Enforce that the key has a single user-id that matches the email.
    if user_ids.len() > 1 {
        return Err(KeyCertificationError::TooManyUserIds);
    }
    let Some(user) = user_ids.first() else {
        return Err(KeyCertificationError::NoUserId);
    };
    check_user_id_email(&user.id, check_email)?;

    // Time checks are disabled to also allow certifying expired keys.
    user.check_validity(certified_primary_key, CheckUnixTime::disable(), profile)
        .map_err(KeyCertificationError::UserIdSelfCertification)?;

    let certifier_primary_key = certifier.secret.primary_key.public_key();
    let certification_key = certifier
        .secret
        .signing_key(
            date.into(),
            Some(certifier_primary_key.legacy_key_id()),
            SignatureUsage::Certify,
            profile,
        )
        .map_err(KeyCertificationError::CertificationKeySelection)?;
    let (certifier_user, _) = certifier
        .secret
        .select_user_id_with_certification(CheckUnixTime::disable(), profile)
        .map_err(KeyCertificationError::CertifierUserId)?;

    let certification = user.id.sign_third_party_with(
        &certification_key.private_key,
        certified_primary_key,
        Some(&certifier_user.id),
        date,
        profile.key_hash_algorithm(),
        lifetime,
        profile.rng(),
        profile,
    )?;

    // Replace existing generic certifications from the same certifier.
    let mut certified_user = user.clone();
    certified_user.signatures.retain(|signature| {
        signature.typ() != Some(SignatureType::CertGeneric)
            || !signature.is_issued_by(certifier_primary_key)
    });
    certified_user.signatures.push(certification);

    Ok(vec![certified_user])
}

/// Checks that the user-id contains the given email address.
fn check_user_id_email(
    user_id: &UserId,
    check_email: Option<&str>,
) -> Result<(), KeyCertificationError> {
    let Some(email) = check_email else {
        return Ok(());
    };
    let user_id_string = user_id
        .as_str()
        .ok_or(KeyCertificationError::InvalidUserId)?;
    if extract_email(user_id_string) != Some(email) {
        return Err(KeyCertificationError::EmailMismatch(email.to_owned()));
    }
    Ok(())
}

/// Extracts the email address of a user-id of the form `name (comment) <email>`.
fn extract_email(user_id: &str) -> Option<&str> {
    let (_, remainder) = user_id.split_once('<')?;
    let (email, _) = remainder.split_once('>')?;
    Some(email)
}

#[cfg(test)]
mod tests {
    use crate::{
        AccessKeyInfo, DataEncoding, PrivateKey, UnixTime, DEFAULT_PROFILE,
        PREFERRED_KEY_GEN_COMPRESSION_ALGORITHMS, PREFERRED_KEY_GEN_HASH_ALGORITHMS,
    };

    const TEST_PRIVATE_KEY: &str = include_str!("../../test-data/keys/locked_private_key_v6.asc");
    const TEST_PRIVATE_KEY_PASSWORD: &str = "password";

    #[test]
    fn key_modification_user_id_v4() {
        let key = PrivateKey::import_unlocked(
            include_str!("../../test-data/keys/private_key_v4.asc").as_bytes(),
            DataEncoding::Armored,
        )
        .expect("Failed to import key");

        let date = UnixTime::new(1_756_196_260);

        let modified_key = key
            .modify()
            .clear_existing_user_ids()
            .add_user_id("test2", "test2@test.com")
            .with_date(date)
            .apply()
            .expect("Failed to modify key");

        assert!(modified_key
            .check_can_encrypt(&DEFAULT_PROFILE, date.into())
            .is_ok());

        assert_eq!(modified_key.as_signed_public_key().details.users.len(), 1);
        let user_id = modified_key
            .as_signed_public_key()
            .details
            .users
            .first()
            .unwrap();
        assert_eq!(user_id.id.as_str().unwrap(), "test2 <test2@test.com>");
        let user_id_signature = user_id.signatures.first().unwrap();
        assert_eq!(
            user_id_signature.preferred_hash_algs(),
            PREFERRED_KEY_GEN_HASH_ALGORITHMS
        );
        assert_eq!(
            user_id_signature.preferred_compression_algs(),
            PREFERRED_KEY_GEN_COMPRESSION_ALGORITHMS,
        );

        assert_eq!(UnixTime::from(user_id_signature.created().unwrap()), date);
    }

    #[test]
    fn key_modification_user_id_v6() {
        let key = PrivateKey::import(
            TEST_PRIVATE_KEY.as_bytes(),
            TEST_PRIVATE_KEY_PASSWORD.as_bytes(),
            DataEncoding::Armored,
        )
        .expect("Failed to import key");

        let date = UnixTime::new(1_756_196_260);

        let modified_key = key
            .modify()
            .clear_existing_user_ids()
            .add_user_id("test2", "test2@test.com")
            .with_date(date)
            .apply()
            .expect("Failed to modify key");

        assert!(modified_key
            .check_can_encrypt(&DEFAULT_PROFILE, date.into())
            .is_ok());

        assert_eq!(modified_key.as_signed_public_key().details.users.len(), 1);
        let user_id = modified_key
            .as_signed_public_key()
            .details
            .users
            .first()
            .unwrap();
        assert_eq!(user_id.id.as_str().unwrap(), "test2 <test2@test.com>");

        let user_id_signature = user_id.signatures.first().unwrap();
        assert!(user_id_signature.preferred_hash_algs().is_empty());
        assert_eq!(UnixTime::from(user_id_signature.created().unwrap()), date);
    }

    #[test]
    #[allow(clippy::indexing_slicing)]
    fn key_modification_user_id_v4_reset() {
        let key = PrivateKey::import_unlocked(
            include_str!("../../test-data/keys/private_key_v4.asc").as_bytes(),
            DataEncoding::Armored,
        )
        .expect("Failed to import key");

        let date = UnixTime::new(1_756_196_260);

        let modified_key = key
            .modify()
            .add_user_id("test2", "test2@test.com")
            .reset_signatures()
            .with_key_creation_time(date)
            .with_date(date)
            .apply()
            .expect("Failed to modify key");

        assert!(modified_key
            .check_can_encrypt(&DEFAULT_PROFILE, date.into())
            .is_ok());

        assert_eq!(modified_key.as_signed_public_key().details.users.len(), 2);
        let users = &modified_key.as_signed_public_key().details.users;
        let new_user_id = &users[1].id;
        let old_user_id = &users[0].id;
        assert_eq!(new_user_id.as_str().unwrap(), "test2 <test2@test.com>");
        assert_eq!(
            old_user_id.as_str().unwrap(),
            "rust-test <rust-test@test.test>"
        );
    }
}
