use std::fmt::Display;

use pgp::{
    composed::{KeyType, SignedSecretKey, SignedSecretSubKey},
    packet::{self, KeyFlags, PubKeyInner, UserId},
    ser::Serialize,
    types::{
        KeyVersion, PacketHeaderVersion, PublicParams, SecretParams, SigningKey, VerifyingKey,
    },
};
use rand::{CryptoRng, Rng};

use crate::{
    KeyGenerationError, KeyGenerationType, PacketPublicSubkeyExt, PrivateKey, Profile, UnixTime,
    UserIdError, DEFAULT_PROFILE,
};
#[cfg(feature = "hazmat-key-gen")]
use crate::{MAX_RSA_KEY_GEN_BITS, MIN_RSA_KEY_GEN_BITS};

/// Internal representation of a user-id.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct KeyUserId {
    pub name: String,
    pub email: String,
}

impl KeyUserId {
    pub(crate) fn try_to_user_id(&self) -> Result<UserId, UserIdError> {
        UserId::from_str(PacketHeaderVersion::default(), self.to_string())
            .map_err(|err| UserIdError::InvalidUserId(self.to_string(), err))
    }
}

impl Display for KeyUserId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} <{}>", self.name, self.email)
    }
}

/// A key generator that can be used to generate `OpenPGP` private keys.
#[derive(Debug)]
pub struct KeyGenerator {
    /// The profile to use for the key generation.
    profile: Profile,

    /// The user-ids to use for the key generation.
    user_ids: Vec<KeyUserId>,

    /// The algorithm type to use for the key generation.
    algorithm: KeyGenerationType,

    /// The date of the key generation for the self-certifications and key creation time.
    date: UnixTime,
}

impl KeyGenerator {
    /// Create a new key generator with the given profile.
    pub fn new(profile: Profile) -> Self {
        Self {
            profile,
            user_ids: Vec::new(),
            algorithm: KeyGenerationType::default(),
            date: UnixTime::now().unwrap_or_default(),
        }
    }

    /// Set the user-id to use for the key generation.
    ///
    /// The user-id will be included as a `name <email>` formatted string.
    pub fn with_user_id(mut self, name: &str, email: &str) -> Self {
        self.user_ids.push(KeyUserId {
            name: name.to_string(),
            email: email.to_string(),
        });
        self
    }

    /// Set the algorithm type to use for the key generation.
    pub fn with_key_type(mut self, algorithm: KeyGenerationType) -> Self {
        self.algorithm = algorithm;
        self
    }

    /// Set the date of the key generation for the self-certifications and key creation time.
    pub fn at_date(mut self, date: UnixTime) -> Self {
        self.date = date;
        self
    }

    /// Generate a `OpenPGP` private key for the given generation configuration.
    ///
    /// # Example
    ///
    /// ```rust
    /// use proton_rpgp::{KeyGenerator, KeyGenerationType};
    ///
    /// let key = KeyGenerator::default()
    ///     .with_user_id("test", "test@test.test")
    ///     .with_key_type(KeyGenerationType::ECC)
    ///     .generate()
    ///     .unwrap();
    /// ```
    pub fn generate(self) -> crate::Result<PrivateKey> {
        let rng = self.profile.rng();
        self.hazardous_generate_with_rng(rng)
    }

    /// Generate a `OpenPGP` private key for the given generation configuration and CSPRNG.
    ///
    /// # Security Hazard
    ///
    /// The randomness source is security critical and must be provided by a cryptographically secure random number generator.
    /// If unsure, use [`Self::generate`] instead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use rand::rngs::OsRng;
    /// use proton_rpgp::{KeyGenerator, KeyGenerationType};
    ///
    /// let mut rng = OsRng;
    ///
    /// let key = KeyGenerator::default()
    ///     .with_user_id("test", "test@test.test")
    ///     .with_key_type(KeyGenerationType::ECC)
    ///     .hazardous_generate_with_rng(&mut rng)
    ///     .unwrap();
    /// ```
    pub fn hazardous_generate_with_rng<R>(self, mut rng: R) -> crate::Result<PrivateKey>
    where
        R: Rng + CryptoRng,
    {
        #[cfg(feature = "hazmat-key-gen")]
        if let KeyGenerationType::RsaCustom(bits) = self.algorithm {
            if !(MIN_RSA_KEY_GEN_BITS..=MAX_RSA_KEY_GEN_BITS).contains(&bits) {
                return Err(KeyGenerationError::InvalidRsaBits(bits).into());
            }
        }
        let key_generation_options = self.algorithm.key_generation_profile(&self.profile);
        let preferred_hash = self.profile.key_hash_algorithm();
        let (primary_user_id, non_primary_user_ids) =
            convert_user_ids(&self.user_ids).map_err(KeyGenerationError::InvalidUserId)?;

        if primary_user_id.is_none() && key_generation_options.key_version == KeyVersion::V4 {
            return Err(KeyGenerationError::NoUserId.into());
        }
        // Generate the primary key for signing.
        let primary_flags = primary_key_flags();
        let key_version = key_generation_options.key_version;
        let key_details_config = key_generation_options.create_key_details_config(
            primary_user_id,
            non_primary_user_ids,
            primary_flags,
        );

        let (primary_secret_key, primary_pub_key) = generate_primary_key(
            self.algorithm.primary_key_type(),
            key_version,
            self.date,
            &mut rng,
        )?;

        // Generate a single subkey for encryption.
        let subkey_spec = SubkeySpec {
            key_type: self.algorithm.encryption_key_type(),
            key_version,
            key_flags: encryption_subkey_flags(),
            date: self.date,
        };
        let signed_subkey_secret = generate_signed_subkey(
            &primary_secret_key,
            &primary_pub_key,
            &subkey_spec,
            &mut rng,
            &self.profile,
        )?;

        // Create all self-certifications.
        let signed_key_details = key_details_config.sign_with(
            &primary_secret_key,
            &primary_pub_key,
            self.date,
            preferred_hash,
            &mut rng,
            &self.profile,
        )?;

        let signed_secret_key = SignedSecretKey::new(
            primary_secret_key,
            signed_key_details,
            Vec::new(),
            vec![signed_subkey_secret],
        );

        Ok(PrivateKey::new(signed_secret_key))
    }
}

impl Default for KeyGenerator {
    fn default() -> Self {
        Self::new(DEFAULT_PROFILE.clone())
    }
}

#[allow(clippy::needless_pass_by_value)]
pub(crate) fn generate_primary_key(
    key_type: KeyType,
    key_version: KeyVersion,
    date: UnixTime,
    rng: impl Rng + CryptoRng,
) -> Result<(packet::SecretKey, packet::PublicKey), KeyGenerationError> {
    let (primary_public_params, primary_secret_params) = generate_key_params(&key_type, rng)?;
    let pub_key = PubKeyInner::new(
        key_version,
        key_type.to_alg(),
        date.into(),
        None,
        primary_public_params,
    )?;
    let primary_pub_key = packet::PublicKey::from_inner(pub_key)?;
    let primary_secret_key =
        packet::SecretKey::new(primary_pub_key.clone(), primary_secret_params)?;
    Ok((primary_secret_key, primary_pub_key))
}

/// Generates key material for the given key type.
///
/// With the `hazmat-key-gen` feature, RSA keys are generated through the `rsa` crate
/// directly to support sizes above the 4096-bit generation limit of rpgp.
pub(crate) fn generate_key_params(
    key_type: &KeyType,
    rng: impl Rng + CryptoRng,
) -> Result<(PublicParams, SecretParams), KeyGenerationError> {
    match key_type {
        #[cfg(feature = "hazmat-key-gen")]
        KeyType::Rsa(bits) => {
            let mut rng = rng;
            let key = rsa::RsaPrivateKey::new(&mut rng, *bits as usize)?;
            let secret = pgp::crypto::rsa::SecretKey::from(key);
            let public_params = PublicParams::RSA((&secret).into());
            let secret_params = SecretParams::Plain(pgp::types::PlainSecretParams::RSA(secret));
            Ok((public_params, secret_params))
        }
        _ => Ok(key_type.generate(rng)?),
    }
}

/// Describes an encryption subkey to generate and bind to a primary key.
pub(crate) struct SubkeySpec {
    /// The type of key material to generate.
    pub key_type: KeyType,

    /// The key version of the generated subkey packet.
    pub key_version: KeyVersion,

    /// The key flags to certify the subkey with.
    pub key_flags: KeyFlags,

    /// The creation time of the subkey and of its binding signature.
    pub date: UnixTime,
}

/// Generates an encryption subkey and binds it to the primary key.
pub(crate) fn generate_signed_subkey<K, P, R>(
    primary_secret_key: &K,
    primary_pub_key: &P,
    spec: &SubkeySpec,
    mut rng: R,
    profile: &Profile,
) -> Result<SignedSecretSubKey, KeyGenerationError>
where
    K: SigningKey,
    P: VerifyingKey + Serialize,
    R: CryptoRng + Rng,
{
    let params = generate_key_params(&spec.key_type, &mut rng)?;
    sign_subkey_with_params(
        primary_secret_key,
        primary_pub_key,
        spec,
        params,
        rng,
        profile,
    )
}

/// Builds a subkey packet from already generated key material and binds it to
/// the primary key with a subkey binding signature.
///
/// Callers that have to adjust the generated public parameters before the subkey
/// packet is built generate the key material themselves and call this directly.
#[allow(clippy::needless_pass_by_value)]
pub(crate) fn sign_subkey_with_params<K, P, R>(
    primary_secret_key: &K,
    primary_pub_key: &P,
    spec: &SubkeySpec,
    params: (PublicParams, SecretParams),
    mut rng: R,
    profile: &Profile,
) -> Result<SignedSecretSubKey, KeyGenerationError>
where
    K: SigningKey,
    P: VerifyingKey + Serialize,
    R: CryptoRng + Rng,
{
    let (subkey_public_params, subkey_secret_params) = params;
    let pub_key = PubKeyInner::new(
        spec.key_version,
        spec.key_type.to_alg(),
        spec.date.into(),
        None,
        subkey_public_params,
    )?;
    let subkey_public = packet::PublicSubkey::from_inner(pub_key)?;
    let subkey_secret = packet::SecretSubkey::new(subkey_public.clone(), subkey_secret_params)?;

    let subkey_binding_signature = subkey_public.sign_with(
        primary_secret_key,
        primary_pub_key,
        spec.date,
        profile.key_hash_algorithm(),
        spec.key_flags.clone(),
        None,
        &mut rng,
        profile,
    )?;

    Ok(SignedSecretSubKey::new(
        subkey_secret,
        vec![subkey_binding_signature],
    ))
}

pub(crate) fn primary_key_flags() -> KeyFlags {
    let mut flags = KeyFlags::default();
    flags.set_sign(true);
    flags.set_certify(true);
    flags
}

fn encryption_subkey_flags() -> KeyFlags {
    let mut flags = KeyFlags::default();
    flags.set_encrypt_comms(true);
    flags.set_encrypt_storage(true);
    flags
}

pub(crate) fn convert_user_ids(
    user_ids: &[KeyUserId],
) -> Result<(Option<UserId>, Vec<UserId>), UserIdError> {
    let primary = user_ids
        .first()
        .map(KeyUserId::try_to_user_id)
        .transpose()?;
    let non_primary = user_ids
        .iter()
        .skip(1)
        .map(KeyUserId::try_to_user_id)
        .collect::<Result<Vec<_>, _>>()?;
    Ok((primary, non_primary))
}

#[cfg(test)]
mod tests {
    use pgp::{
        crypto::hash::HashAlgorithm,
        packet::{Packet, PacketParser, Signature, SignatureType, SignatureVersion},
        types::KeyDetails,
    };

    use crate::{
        AccessKeyInfo, DataEncoding, SignatureExt, HAZARD_AEAD_PROFILE,
        PREFERRED_KEY_GEN_AEAD_CIPHERSUITES,
    };

    use super::*;

    #[test]
    fn test_key_generation_details_v4() {
        let date = UnixTime::new(1_756_196_260);
        let key = KeyGenerator::default()
            .with_user_id("test", "test@test.test")
            .with_key_type(KeyGenerationType::ECC)
            .at_date(date)
            .generate()
            .unwrap();

        assert_eq!(key.version(), 4);
        assert_eq!(UnixTime::from(key.public.inner.created_at()), date);

        let exported = key
            .export_unlocked(DataEncoding::Unarmored)
            .expect("Failed to export key");

        let key_info_signature = load_user_id_signature(&exported);
        assert_eq!(key_info_signature.version(), SignatureVersion::V4);
        assert_eq!(key_info_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            key_info_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(
            key_info_signature.issuer_key_id().first().copied(),
            Some(&key.key_id())
        );
        assert_eq!(key_info_signature.unix_created_at().unwrap(), date);
        assert!(key_info_signature.key_flags().certify() && key_info_signature.key_flags().sign());
        assert!(
            key_info_signature.features().unwrap().seipd_v1()
                && !key_info_signature.features().unwrap().seipd_v2()
        );

        let user_id = load_user_id(&exported).unwrap();
        assert_eq!(user_id.as_str().unwrap(), "test <test@test.test>");

        let subkey_signature = load_subkey_signature(&exported);
        assert_eq!(subkey_signature.version(), SignatureVersion::V4);
        assert_eq!(subkey_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(subkey_signature.unix_created_at().unwrap(), date);
        assert_eq!(
            subkey_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert!(
            subkey_signature.key_flags().encrypt_comms()
                && subkey_signature.key_flags().encrypt_storage()
        );
    }

    #[test]
    fn test_key_generation_details_v4_with_aead() {
        let date = UnixTime::new(1_756_196_260);

        let key = KeyGenerator::new(HAZARD_AEAD_PROFILE.clone())
            .with_user_id("test", "test@test.test")
            .with_key_type(KeyGenerationType::ECC)
            .at_date(date)
            .generate()
            .unwrap();

        assert_eq!(key.version(), 4);
        assert_eq!(UnixTime::from(key.public.inner.created_at()), date);

        let exported = key
            .export_unlocked(DataEncoding::Unarmored)
            .expect("Failed to export key");

        let key_info_signature = load_user_id_signature(&exported);
        assert_eq!(key_info_signature.version(), SignatureVersion::V4);
        assert_eq!(key_info_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            key_info_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(
            key_info_signature.issuer_key_id().first().copied(),
            Some(&key.key_id())
        );
        assert_eq!(key_info_signature.unix_created_at().unwrap(), date);
        assert!(key_info_signature.key_flags().certify() && key_info_signature.key_flags().sign());
        assert!(
            key_info_signature.features().unwrap().seipd_v1()
                && key_info_signature.features().unwrap().seipd_v2()
        );

        assert_eq!(
            key_info_signature.preferred_aead_algs(),
            PREFERRED_KEY_GEN_AEAD_CIPHERSUITES
        );
        let user_id = load_user_id(&exported).unwrap();
        assert_eq!(user_id.as_str().unwrap(), "test <test@test.test>");

        let subkey_signature = load_subkey_signature(&exported);
        assert_eq!(subkey_signature.version(), SignatureVersion::V4);
        assert_eq!(subkey_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(subkey_signature.unix_created_at().unwrap(), date);
        assert_eq!(
            subkey_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert!(
            subkey_signature.key_flags().encrypt_comms()
                && subkey_signature.key_flags().encrypt_storage()
        );
    }

    #[test]
    #[cfg(feature = "pqc")]
    fn test_key_generation_details_v6() {
        let date = UnixTime::new(1_756_196_260);
        let key = KeyGenerator::default()
            .with_user_id("test", "test@test.test")
            .with_key_type(KeyGenerationType::PQC)
            .at_date(date)
            .generate()
            .unwrap();

        assert_eq!(key.version(), 6);
        assert_eq!(UnixTime::from(key.public.inner.created_at()), date);

        let exported = key
            .export_unlocked(DataEncoding::Unarmored)
            .expect("Failed to export key");

        let key_info_signature = load_direct_key_info_signature(&exported);
        assert_eq!(key_info_signature.version(), SignatureVersion::V6);
        assert_eq!(key_info_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            key_info_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(key_info_signature.unix_created_at().unwrap(), date);
        assert!(key_info_signature.key_flags().certify() && key_info_signature.key_flags().sign());
        assert!(
            key_info_signature.features().unwrap().seipd_v1()
                && !key_info_signature.features().unwrap().seipd_v2()
        );

        let user_id = load_user_id(&exported).unwrap();
        assert_eq!(user_id.as_str().unwrap(), "test <test@test.test>");

        let subkey_signature = load_subkey_signature(&exported);
        assert_eq!(subkey_signature.version(), SignatureVersion::V6);
        assert_eq!(subkey_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(subkey_signature.unix_created_at().unwrap(), date);
        assert_eq!(
            subkey_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert!(
            subkey_signature.key_flags().encrypt_comms()
                && subkey_signature.key_flags().encrypt_storage()
        );

        let user_id_signature = load_user_id_signature(&exported);
        assert_eq!(user_id_signature.version(), SignatureVersion::V6);
        assert_eq!(user_id_signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            user_id_signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(user_id_signature.unix_created_at().unwrap(), date);
        assert!(user_id_signature.features().is_none());
        assert!(user_id_signature.preferred_symmetric_algs().is_empty());
    }

    fn load_user_id_signature(signature: &[u8]) -> Signature {
        let parser = PacketParser::new(signature);
        for packet in parser.flatten() {
            if let Packet::Signature(signature) = packet {
                if let Some(SignatureType::CertPositive) = signature.typ() {
                    return signature;
                }
            }
        }
        panic!("Expected a signature packet");
    }

    fn load_direct_key_info_signature(signature: &[u8]) -> Signature {
        let parser = PacketParser::new(signature);
        for packet in parser.flatten() {
            if let Packet::Signature(signature) = packet {
                if let Some(SignatureType::Key) = signature.typ() {
                    return signature;
                }
            }
        }
        panic!("Expected a signature packet");
    }

    fn load_subkey_signature(signature: &[u8]) -> Signature {
        let parser = PacketParser::new(signature);
        for packet in parser.flatten() {
            if let Packet::Signature(signature) = packet {
                if let Some(SignatureType::SubkeyBinding) = signature.typ() {
                    return signature;
                }
            }
        }
        panic!("Expected a signature packet");
    }

    fn load_user_id(signature: &[u8]) -> Option<UserId> {
        let parser = PacketParser::new(signature);
        for packet in parser.flatten() {
            if let Packet::UserId(user_id) = packet {
                return Some(user_id);
            }
        }
        None
    }

    #[cfg(feature = "hazmat-key-gen")]
    mod hazmat_key_gen {
        use rsa::traits::PublicKeyParts;

        use super::*;

        fn rsa_modulus_bits(params: &PublicParams) -> usize {
            match params {
                PublicParams::RSA(params) => params.key.n().bits(),
                _ => panic!("expected RSA public params"),
            }
        }

        fn assert_rsa_key_bits(key: &PrivateKey, expected_bits: usize) {
            let public = &key.public.inner;
            assert_eq!(
                rsa_modulus_bits(public.primary_key.public_params()),
                expected_bits
            );
            let [subkey] = public.public_subkeys.as_slice() else {
                panic!("expected exactly one subkey");
            };
            assert_eq!(rsa_modulus_bits(subkey.key.public_params()), expected_bits);
        }

        fn generate_rsa_key(key_type: KeyGenerationType) -> crate::Result<PrivateKey> {
            KeyGenerator::default()
                .with_user_id("test", "test@test.test")
                .with_key_type(key_type)
                .at_date(UnixTime::new(1_756_196_260))
                .generate()
        }

        #[test]
        fn test_key_generation_rsa_custom_bits() {
            for bits in [1023, 1024] {
                let key = generate_rsa_key(KeyGenerationType::RsaCustom(bits)).unwrap();
                assert_eq!(key.version(), 4);
                assert_rsa_key_bits(&key, bits as usize);
            }
        }
    }
}
