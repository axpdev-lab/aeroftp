use pgp::{
    composed::{KeyType, SignedSecretKey, SignedSecretSubKey},
    crypto::{ecc_curve::ECCCurve, hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::KeyFlags,
    ser::Serialize,
    types::{
        EcdhKdfType, EcdhPublicParams, Fingerprint, KeyDetails, KeyVersion, Password, PublicParams,
        SigningKey, VerifyingKey,
    },
};
use rand::{CryptoRng, Rng};

use crate::{
    check_subkey_for_forwarding, convert_user_ids, generate_primary_key, primary_key_flags,
    sign_subkey_with_params, AccessKeyInfo, ForwardingInstance, ForwardingKeyGenerationError,
    ForwardingKeyValidationError, KeyGenerationError, KeyUserId, PrivateKey, Profile,
    PublicKeySelectionExt, SubkeySpec, UnixTime, DEFAULT_PROFILE,
};

const FORWARDING_KEY_VERSION: KeyVersion = KeyVersion::V4;

const FORWARDING_SUBKEY_TYPE: KeyType = KeyType::ECDH(ECCCurve::Curve25519Legacy);

impl PrivateKey {
    /// Returns a generator for a forwarding key that decrypts messages
    /// originally encrypted to this key after transformation.
    pub fn forwarding_key_generator(&self) -> ForwardingKeyGenerator<'_> {
        ForwardingKeyGenerator::new(self)
    }

    /// Returns a generator for a forwarding key that decrypts messages
    /// originally encrypted to this key after transformation, using `profile`.
    pub fn forwarding_key_generator_with_profile(
        &self,
        profile: &Profile,
    ) -> ForwardingKeyGenerator<'_> {
        ForwardingKeyGenerator::new_with_profile(self, profile)
    }
}

/// A generator for `OpenPGP` forwarding keys.
///
/// # Example
///
/// ```rust
/// use proton_rpgp::{KeyGenerationType, KeyGenerator};
///
/// let forwarder = KeyGenerator::default()
///     .with_user_id("forwarder", "forwarder@test.test")
///     .with_key_type(KeyGenerationType::ECC)
///     .generate()
///     .unwrap();
///
/// let (forwardee_key, instances) = forwarder
///     .forwarding_key_generator()
///     .with_user_id("forwardee", "forwardee@test.test")
///     .generate()
///     .unwrap();
/// ```
#[derive(Debug)]
pub struct ForwardingKeyGenerator<'a> {
    /// The key whose encryption subkeys are being forwarded.
    forwarder: &'a PrivateKey,

    /// The profile to use for the key generation.
    profile: Profile,

    /// The user-ids to use for the generated forwarding key.
    user_ids: Vec<KeyUserId>,

    /// The date of the key generation for the self-certifications and key
    /// creation time. Resolved to the current time when not set.
    date: Option<UnixTime>,
}

impl<'a> ForwardingKeyGenerator<'a> {
    pub fn new(forwarder: &'a PrivateKey) -> Self {
        Self::new_with_profile(forwarder, &DEFAULT_PROFILE)
    }

    pub fn new_with_profile(forwarder: &'a PrivateKey, profile: &Profile) -> Self {
        Self {
            forwarder,
            profile: profile.clone(),
            user_ids: Vec::new(),
            date: None,
        }
    }

    /// Add a user-id to the generated forwarding key.
    ///
    /// The user-id will be included as a `name <email>` formatted string.
    pub fn with_user_id(mut self, name: &str, email: &str) -> Self {
        self.user_ids.push(KeyUserId {
            name: name.to_string(),
            email: email.to_string(),
        });
        self
    }

    /// Set the date of the key generation for the self-certifications and key
    /// creation time.
    pub fn at_date(mut self, date: UnixTime) -> Self {
        self.date = Some(date);
        self
    }

    /// Generates the forwarding key and the forwarding instances for the proxy.
    ///
    /// One instance is returned per forwarded encryption subkey, in the same
    /// order as the subkeys of the generated key.
    pub fn generate(self) -> crate::Result<(PrivateKey, Vec<ForwardingInstance>)> {
        self.generate_inner().map_err(Into::into)
    }

    fn generate_inner(
        self,
    ) -> Result<(PrivateKey, Vec<ForwardingInstance>), ForwardingKeyGenerationError> {
        let forwarder = self.forwarder;
        let profile = &self.profile;

        if forwarder.version() != u8::from(FORWARDING_KEY_VERSION) {
            return Err(ForwardingKeyGenerationError::VersionMismatch(
                forwarder.version(),
            ));
        }

        if self.user_ids.is_empty() {
            return Err(ForwardingKeyGenerationError::NoUserId);
        }

        let date = match self.date {
            Some(date) => date,
            None => UnixTime::now().ok_or(ForwardingKeyGenerationError::UnableToGetTime)?,
        };

        // Check that the forwarder key has a forwardable key at all before
        // generating anything, so a fully unusable key fails with a single clear error.
        forwarder
            .as_signed_public_key()
            .forwardable_key(date.into(), profile)
            .map_err(ForwardingKeyValidationError::KeyValidation)
            .map_err(ForwardingKeyGenerationError::KeyValidation)?;

        let mut rng = profile.rng();
        let (primary_secret_key, primary_pub_key) = generate_primary_key(
            KeyType::Ed25519Legacy,
            FORWARDING_KEY_VERSION,
            date,
            &mut rng,
        )
        .map_err(ForwardingKeyGenerationError::GenerationPrimary)?;

        let forwarder_primary = forwarder.secret.primary_key.public_key();
        let mut errors = Vec::new();
        let mut forwardee_subkeys = Vec::new();
        let mut instances = Vec::new();

        for forwarder_subkey in &forwarder.secret.secret_subkeys {
            let kdf_params =
                match forwarding_kdf_params(forwarder_subkey, forwarder_primary, date, profile) {
                    Ok(kdf_params) => kdf_params,
                    Err(err) => {
                        errors.push(err);
                        continue;
                    }
                };

            let forwarder_fingerprint = forwarder_subkey.fingerprint();
            let forwardee_subkey = generate_forwarding_subkey(
                &primary_secret_key,
                &primary_pub_key,
                &kdf_params,
                date,
                &mut rng,
                profile,
            )
            .map_err(ForwardingKeyGenerationError::GenerationSubkey)?;

            let proxy_parameter = forwarder_subkey
                .key
                .generate_proxy_parameter(
                    &forwardee_subkey.key,
                    &Password::empty(),
                    &Password::empty(),
                )
                .map_err(ForwardingKeyGenerationError::ProxyParamGeneration)?;

            instances.push(ForwardingInstance::new_inner(
                forwarder_fingerprint,
                forwardee_subkey.fingerprint(),
                proxy_parameter,
            )?);
            forwardee_subkeys.push(forwardee_subkey);
        }

        if forwardee_subkeys.is_empty() {
            if errors.is_empty() {
                return Err(ForwardingKeyGenerationError::NoSubkeys);
            }
            return Err(ForwardingKeyGenerationError::SubKeyValidation(
                errors.into(),
            ));
        }

        let (primary_user_id, non_primary_user_ids) = convert_user_ids(&self.user_ids)?;
        let key_details_config = profile
            .default_key_generation_profile()
            .build()
            .create_key_details_config(primary_user_id, non_primary_user_ids, primary_key_flags());

        let signed_key_details = key_details_config
            .sign_with(
                &primary_secret_key,
                &primary_pub_key,
                date,
                profile.key_hash_algorithm(),
                &mut rng,
                profile,
            )
            .map_err(|err| ForwardingKeyGenerationError::GenerationPrimary(err.into()))?;

        let signed_secret_key = SignedSecretKey::new(
            primary_secret_key,
            signed_key_details,
            Vec::new(),
            forwardee_subkeys,
        );

        Ok((PrivateKey::new(signed_secret_key), instances))
    }
}

struct ForwardingKdfParams {
    /// The fingerprint that replaces the forwardee fingerprint in the KDF.
    replacement_fingerprint: [u8; 20],

    /// The KDF hash algorithm of the forwarder subkey.
    hash: HashAlgorithm,

    /// The KDF wrapping algorithm of the forwarder subkey.
    alg_sym: SymmetricKeyAlgorithm,
}

/// Checks that `forwarder_subkey` can be forwarded and extracts the KDF
/// parameters its forwardee subkey has to reuse.
fn forwarding_kdf_params<K>(
    forwarder_subkey: &SignedSecretSubKey,
    forwarder_primary: &K,
    date: UnixTime,
    profile: &Profile,
) -> Result<ForwardingKdfParams, ForwardingKeyValidationError>
where
    K: VerifyingKey + Serialize,
{
    check_subkey_for_forwarding(forwarder_subkey, forwarder_primary, date.into(), profile)?;

    let PublicParams::ECDH(EcdhPublicParams::Curve25519Legacy {
        hash,
        alg_sym,
        ecdh_kdf_type,
        ..
    }) = forwarder_subkey.public_params()
    else {
        return Err(ForwardingKeyValidationError::SubKeyNoMatchingAlgorithm(
            forwarder_subkey.algorithm(),
        ));
    };

    let forwarder_fingerprint = forwarder_subkey.fingerprint();
    let Fingerprint::V4(forwarder_fingerprint_bytes) = forwarder_fingerprint else {
        return Err(ForwardingKeyValidationError::SubKeyUnsupportedKeyVersion(
            forwarder_fingerprint.version(),
        ));
    };

    // If the forwarder subkey is itself a forwarding key (recursive forwarding),
    // messages were originally encrypted to the key it replaces. The forwardee
    // has to mix in that original fingerprint into the KDF as well.
    let replacement_fingerprint = match ecdh_kdf_type {
        EcdhKdfType::Replaced {
            replacement_fingerprint,
        } => *replacement_fingerprint,
        EcdhKdfType::Native => forwarder_fingerprint_bytes,
    };

    Ok(ForwardingKdfParams {
        replacement_fingerprint,
        hash: *hash,
        alg_sym: *alg_sym,
    })
}

/// Generates a forwardee encryption subkey and binds it to the forwardee
/// primary key.
#[allow(clippy::needless_pass_by_value)]
fn generate_forwarding_subkey<K, P, R>(
    primary_secret_key: &K,
    primary_pub_key: &P,
    kdf_params: &ForwardingKdfParams,
    date: UnixTime,
    mut rng: R,
    profile: &Profile,
) -> Result<SignedSecretSubKey, KeyGenerationError>
where
    K: SigningKey,
    P: VerifyingKey + Serialize,
    R: CryptoRng + Rng,
{
    let (mut public_params, secret_params) = FORWARDING_SUBKEY_TYPE.generate(&mut rng)?;

    let PublicParams::ECDH(EcdhPublicParams::Curve25519Legacy {
        ref mut hash,
        ref mut alg_sym,
        ref mut ecdh_kdf_type,
        ..
    }) = public_params
    else {
        return Err(KeyGenerationError::InvalidState("expected ECDH/Curve25519"));
    };

    *hash = kdf_params.hash;
    *alg_sym = kdf_params.alg_sym;
    *ecdh_kdf_type = EcdhKdfType::Replaced {
        replacement_fingerprint: kdf_params.replacement_fingerprint,
    };

    let spec = SubkeySpec {
        key_type: FORWARDING_SUBKEY_TYPE,
        key_version: FORWARDING_KEY_VERSION,
        key_flags: forwarding_subkey_flags(),
        date,
    };

    sign_subkey_with_params(
        primary_secret_key,
        primary_pub_key,
        &spec,
        (public_params, secret_params),
        &mut rng,
        profile,
    )
}

fn forwarding_subkey_flags() -> KeyFlags {
    let mut flags = KeyFlags::default();
    flags.set_shared(true);
    flags.set_draft_decrypt_forwarded(true);
    flags
}
