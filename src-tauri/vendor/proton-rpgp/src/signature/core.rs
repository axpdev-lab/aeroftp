use pgp::{
    bytes::Bytes,
    crypto::hash::HashAlgorithm,
    packet::{
        KeyFlags, Notation, SignatureConfig, SignatureType, Subpacket, SubpacketData, UserId,
    },
    types::{Duration, KeyVersion, SigningKey, VerifyingKey},
};
use rand::{CryptoRng, Rng};

use crate::{
    preferences::select_hash_to_sign_key_signatures, types::UnixTime, AnySecretKey,
    KeyDetailsConfig, Profile, SignatureContext, SignatureMode, SigningError,
};

const SALT_NOTATION_NAME: &[u8] = b"salt@notations.openpgpjs.org";

/// Configures a Proton-specific signature config for a given private key.
///
/// The config is then used to create an `OpenPGP` signature over the input data.
pub fn configure_message_signature<R: Rng + CryptoRng>(
    private_key: &AnySecretKey<'_>,
    at_date: UnixTime,
    signature_mode: SignatureMode,
    hash_algorithm: HashAlgorithm,
    signature_context: Option<&SignatureContext>,
    mut rng: R,
) -> Result<SignatureConfig, SigningError> {
    // Create a signature config based on the key version and hash algorithm.
    let mut config =
        signature_config_from_key(private_key, signature_mode.into(), hash_algorithm, &mut rng)?;
    let (hashed_subpackets, unhashed_subpackets) = message_signature_subpackets(
        private_key,
        at_date,
        hash_algorithm,
        signature_context,
        &mut rng,
    )?;
    config.hashed_subpackets = hashed_subpackets;
    config.unhashed_subpackets = unhashed_subpackets;
    Ok(config)
}

pub fn message_signature_subpackets<R: Rng + CryptoRng>(
    private_key: &AnySecretKey<'_>,
    at_date: UnixTime,
    hash_algorithm: HashAlgorithm,
    signature_context: Option<&SignatureContext>,
    mut rng: R,
) -> Result<(Vec<Subpacket>, Vec<Subpacket>), SigningError> {
    let mut hashed_subpackets = Vec::with_capacity(4);

    push_signature_creation_time_subpacket(&mut hashed_subpackets, at_date)?;

    push_v4_issuer_and_salt(
        &mut hashed_subpackets,
        private_key,
        hash_algorithm,
        &mut rng,
    )?;

    push_issuer_fingerprint_subpacket(&mut hashed_subpackets, private_key)?;

    // Add the signature context if any.
    if let Some(signature_context) = signature_context {
        let notation = SubpacketData::Notation(Notation::from(signature_context.clone()));
        let subpacket_result = if signature_context.is_critical {
            Subpacket::critical(notation)
        } else {
            Subpacket::regular(notation)
        };
        hashed_subpackets.push(subpacket_result.map_err(SigningError::Sign)?);
    }

    Ok((hashed_subpackets, Vec::new()))
}

#[allow(clippy::too_many_arguments)]
pub fn configure_key_details_signature<K, R, P>(
    private_key: &K,
    public_key: &P,
    at_date: UnixTime,
    preferred_hash: HashAlgorithm,
    signature_mode: SignatureType,
    key_details: &KeyDetailsConfig,
    primary_user_id: bool,
    include_details: bool,
    profile: &Profile,
    mut rng: R,
) -> Result<SignatureConfig, SigningError>
where
    P: VerifyingKey,
    K: SigningKey,
    R: Rng + CryptoRng,
{
    let hash_algorithm =
        select_hash_to_sign_key_signatures(preferred_hash, public_key.public_params(), profile);
    let mut config =
        signature_config_from_key(private_key, signature_mode, hash_algorithm, &mut rng)?;
    config.hashed_subpackets = key_details_signature_subpackets(
        private_key,
        at_date,
        hash_algorithm,
        key_details,
        primary_user_id,
        include_details,
        &mut rng,
    )?;
    Ok(config)
}

pub fn key_details_signature_subpackets<K, R>(
    private_key: &K,
    at_date: UnixTime,
    hash_algorithm: HashAlgorithm,
    key_details: &KeyDetailsConfig,
    primary_user_id: bool,
    include_details: bool,
    mut rng: R,
) -> Result<Vec<Subpacket>, SigningError>
where
    K: SigningKey,
    R: Rng + CryptoRng,
{
    let mut hashed_subpackets = Vec::with_capacity(11);

    push_signature_creation_time_subpacket(&mut hashed_subpackets, at_date)?;

    if include_details {
        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::PreferredSymmetricAlgorithms(
                key_details.preferred_symmetric_algorithms.clone(),
            ))
            .map_err(SigningError::Sign)?,
        );
    }

    push_v4_issuer_and_salt(
        &mut hashed_subpackets,
        private_key,
        hash_algorithm,
        &mut rng,
    )?;

    if include_details {
        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::PreferredHashAlgorithms(
                key_details.preferred_hash_algorithms.clone(),
            ))
            .map_err(SigningError::Sign)?,
        );

        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::PreferredCompressionAlgorithms(
                key_details.preferred_compression_algorithms.clone(),
            ))
            .map_err(SigningError::Sign)?,
        );
    }

    if primary_user_id {
        hashed_subpackets
            .push(Subpacket::regular(SubpacketData::IsPrimary(true)).map_err(SigningError::Sign)?);
    }

    if include_details {
        hashed_subpackets.push(
            Subpacket::critical(SubpacketData::KeyFlags(key_details.keyflags.clone()))
                .map_err(SigningError::Sign)?,
        );

        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::Features(key_details.features.clone()))
                .map_err(SigningError::Sign)?,
        );
    }

    push_issuer_fingerprint_subpacket(&mut hashed_subpackets, private_key)?;

    if include_details && !key_details.preferred_aead_algorithms.is_empty() {
        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::PreferredAeadAlgorithms(
                key_details.preferred_aead_algorithms.clone(),
            ))
            .map_err(SigningError::Sign)?,
        );
    }

    Ok(hashed_subpackets)
}

/// Configures a third-party certification signature over a user-id of another key.
///
/// If a `lifetime` in seconds is given, the certification expires that many
/// seconds after `at_date`.
pub(crate) fn configure_third_party_certification_signature<K, R>(
    certifier_secret_key: &K,
    at_date: UnixTime,
    preferred_hash: HashAlgorithm,
    certifier_user_id: Option<&UserId>,
    lifetime: Option<crate::Lifetime>,
    profile: &Profile,
    mut rng: R,
) -> Result<SignatureConfig, SigningError>
where
    K: SigningKey,
    R: Rng + CryptoRng,
{
    let hash_algorithm = select_hash_to_sign_key_signatures(
        preferred_hash,
        certifier_secret_key.public_params(),
        profile,
    );
    let mut config = signature_config_from_key(
        certifier_secret_key,
        SignatureType::CertGeneric,
        hash_algorithm,
        &mut rng,
    )?;

    let mut hashed_subpackets = Vec::with_capacity(6);

    push_signature_creation_time_subpacket(&mut hashed_subpackets, at_date)?;

    // Let the certification expire after the given lifetime.
    if let Some(lifetime) = lifetime {
        hashed_subpackets.push(
            Subpacket::critical(SubpacketData::SignatureExpirationTime(Duration::from(
                lifetime,
            )))
            .map_err(SigningError::Sign)?,
        );
    }

    push_v4_issuer_and_salt(
        &mut hashed_subpackets,
        certifier_secret_key,
        hash_algorithm,
        &mut rng,
    )?;

    // Indicate with which identity of the certifier key the certification was made.
    if let Some(certifier_user_id) = certifier_user_id {
        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::SignersUserID(Bytes::from(
                certifier_user_id.id().to_vec(),
            )))
            .map_err(SigningError::Sign)?,
        );
    }

    push_issuer_fingerprint_subpacket(&mut hashed_subpackets, certifier_secret_key)?;

    config.hashed_subpackets = hashed_subpackets;
    Ok(config)
}

#[allow(clippy::too_many_arguments)]
pub fn configure_subkey_signature<K, R, P>(
    primary_secret_key: &K,
    primary_public_key: &P,
    at_date: UnixTime,
    preferred_hash: HashAlgorithm,
    signature_mode: SignatureType,
    keyflags: KeyFlags,
    profile: &Profile,
    mut rng: R,
) -> Result<SignatureConfig, SigningError>
where
    P: VerifyingKey,
    K: SigningKey,
    R: Rng + CryptoRng,
{
    let hash_algorithm = select_hash_to_sign_key_signatures(
        preferred_hash,
        primary_public_key.public_params(),
        profile,
    );
    let mut config =
        signature_config_from_key(primary_secret_key, signature_mode, hash_algorithm, &mut rng)?;

    config.hashed_subpackets = subkey_signature_subpackets(
        primary_secret_key,
        at_date,
        hash_algorithm,
        keyflags,
        &mut rng,
    )?;
    Ok(config)
}

pub fn subkey_signature_subpackets<K, R>(
    private_key: &K,
    at_date: UnixTime,
    hash_algorithm: HashAlgorithm,
    keyflags: KeyFlags,
    mut rng: R,
) -> Result<Vec<Subpacket>, SigningError>
where
    K: SigningKey,
    R: Rng + CryptoRng,
{
    let mut hashed_subpackets = Vec::with_capacity(5);

    push_signature_creation_time_subpacket(&mut hashed_subpackets, at_date)?;

    push_v4_issuer_and_salt(
        &mut hashed_subpackets,
        private_key,
        hash_algorithm,
        &mut rng,
    )?;

    hashed_subpackets
        .push(Subpacket::critical(SubpacketData::KeyFlags(keyflags)).map_err(SigningError::Sign)?);

    push_issuer_fingerprint_subpacket(&mut hashed_subpackets, private_key)?;

    Ok(hashed_subpackets)
}

/// Creates a salt notation for a v4 signature.
///
/// Example:
/// ```text
///   Notation: salt@notations.openpgpjs.org
///     00000000  33 fd e5 72 fa b0 1c 75  fa 59 a7 19 ab 09 f5 35
///     00000010  9a e8 81 b3 af f6 49 98  ec 1e c0 11 c1 10 1b 5b
/// ```
fn salt_notation<R: Rng + CryptoRng>(
    hash_algorithm: HashAlgorithm,
    rng: &mut R,
) -> Result<Subpacket, pgp::errors::Error> {
    let salt_size = hash_algorithm.salt_len().unwrap_or(16);
    let mut salt = vec![0; salt_size];
    rng.fill_bytes(&mut salt);

    Subpacket::regular(SubpacketData::Notation(Notation {
        name: Bytes::from(SALT_NOTATION_NAME.to_vec()),
        value: Bytes::from(salt),
        readable: false,
    }))
}

fn push_signature_creation_time_subpacket(
    hashed_subpackets: &mut Vec<Subpacket>,
    at_date: UnixTime,
) -> Result<(), SigningError> {
    hashed_subpackets.push(
        Subpacket::critical(SubpacketData::SignatureCreationTime(at_date.into()))
            .map_err(SigningError::Sign)?,
    );
    Ok(())
}

fn push_v4_issuer_and_salt<K, R>(
    hashed_subpackets: &mut Vec<Subpacket>,
    private_key: &K,
    hash_algorithm: HashAlgorithm,
    rng: &mut R,
) -> Result<(), SigningError>
where
    K: SigningKey,
    R: Rng + CryptoRng,
{
    if private_key.version() < KeyVersion::V6 {
        for subpacket in [
            Subpacket::regular(SubpacketData::IssuerKeyId(private_key.legacy_key_id())),
            salt_notation(hash_algorithm, rng),
        ] {
            hashed_subpackets.push(subpacket.map_err(SigningError::Sign)?);
        }
    }
    Ok(())
}

fn push_issuer_fingerprint_subpacket<K>(
    hashed_subpackets: &mut Vec<Subpacket>,
    private_key: &K,
) -> Result<(), SigningError>
where
    K: SigningKey,
{
    if private_key.version() < KeyVersion::V6 {
        hashed_subpackets.push(
            Subpacket::regular(SubpacketData::IssuerFingerprint(private_key.fingerprint()))
                .map_err(SigningError::Sign)?,
        );
    } else {
        hashed_subpackets.push(
            Subpacket::critical(SubpacketData::IssuerFingerprint(private_key.fingerprint()))
                .map_err(SigningError::Sign)?,
        );
    }
    Ok(())
}

fn signature_config_from_key(
    private_key: &impl SigningKey,
    signature_mode: SignatureType,
    hash_algorithm: HashAlgorithm,
    rng: impl Rng + CryptoRng,
) -> Result<SignatureConfig, SigningError> {
    match private_key.version() {
        KeyVersion::V4 => Ok(SignatureConfig::v4(
            signature_mode,
            private_key.algorithm(),
            hash_algorithm,
        )),
        KeyVersion::V6 => {
            SignatureConfig::v6(rng, signature_mode, private_key.algorithm(), hash_algorithm)
                .map_err(SigningError::Sign)
        }
        _ => Err(SigningError::InvalidKeyVersion),
    }
}
