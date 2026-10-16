use pgp::{
    crypto::{aead::AeadAlgorithm, hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::Features,
    types::{CompressionAlgorithm, KeyDetails, PublicParams},
};

use crate::{Ciphersuite, PrivateComponentKey, Profile, PublicComponentKey};

const HASH_ALGORITHMS_MID: &[HashAlgorithm] = &[
    HashAlgorithm::Sha512,
    HashAlgorithm::Sha3_512,
    HashAlgorithm::Sha384,
];

const HASH_ALGORITHMS_HIGH: &[HashAlgorithm] = &[HashAlgorithm::Sha512, HashAlgorithm::Sha3_512];

/// The algorithms determined based on the recipients.
#[derive(Debug, Clone)]
pub(crate) struct RecipientsAlgorithms {
    /// The hash algorithms that could be used to sign the message.
    pub signing_hash_candidates: Vec<HashAlgorithm>,

    /// The compression algorithm to use.
    pub compression_algorithm: CompressionAlgorithm,

    /// The symmetric key algorithm to use.
    pub symmetric_algorithm: SymmetricKeyAlgorithm,

    /// The AEAD ciphersuite to use if any.
    pub aead_ciphersuite: Option<(SymmetricKeyAlgorithm, AeadAlgorithm)>,

    /// Whether the recipients support AEAD (SEIPD v2) encryption.
    pub support_seipdv2: bool,
}

/// The encryption mechanism to use by the encryptor.
#[derive(Debug, Copy, Clone, PartialEq)]
pub enum EncryptionMechanism {
    SeipdV1(SymmetricKeyAlgorithm),
    SeipdV2(SymmetricKeyAlgorithm, AeadAlgorithm),
}

impl RecipientsAlgorithms {
    pub fn select(
        message_symmetric_algorithm: SymmetricKeyAlgorithm,
        message_cipher_suite: Option<Ciphersuite>,
        message_compression: CompressionAlgorithm,
        keys: &[PublicComponentKey<'_>],
        profile: &Profile,
    ) -> Self {
        let mut candidate_hashes_to_sign = profile.candidate_hash_algorithms().to_vec();
        let mut candidate_symmetric_algorithms =
            profile.candidate_symmetric_key_algorithms().to_vec();
        let mut candidate_compression_algorithms =
            profile.candidate_compression_algorithms().to_vec();
        let mut candidate_aead_algorithms = profile.candidate_aead_ciphersuites().to_vec();
        // We only allow AEAD encryption if the encryptor specifies an AEAD algorithm.
        let mut aead_support = message_cipher_suite.is_some();

        // Intersect the candidate algorithms with the preferences of the recipients.
        for key in keys {
            let self_sig = key.primary_self_certification;
            intersect(
                &mut candidate_hashes_to_sign,
                self_sig.preferred_hash_algs(),
            );
            intersect(
                &mut candidate_symmetric_algorithms,
                self_sig.preferred_symmetric_algs(),
            );
            intersect(
                &mut candidate_compression_algorithms,
                self_sig.preferred_compression_algs(),
            );
            intersect(
                &mut candidate_aead_algorithms,
                self_sig.preferred_aead_algs(),
            );

            if !self_sig.features().is_some_and(Features::seipd_v2) {
                aead_support = false;
            }
        }
        let symmetric_algorithm =
            if candidate_symmetric_algorithms.contains(&message_symmetric_algorithm) {
                message_symmetric_algorithm
            } else {
                candidate_symmetric_algorithms
                    .into_iter()
                    .next()
                    .unwrap_or(SymmetricKeyAlgorithm::AES128)
            };

        let compression_algorithm = if message_compression == CompressionAlgorithm::Uncompressed {
            // We only allow compression if explicitly enabled via message_compression.
            CompressionAlgorithm::Uncompressed
        } else if candidate_compression_algorithms.contains(&message_compression) {
            message_compression
        } else {
            candidate_compression_algorithms
                .into_iter()
                .next()
                .unwrap_or(CompressionAlgorithm::Uncompressed)
        };

        // Select the AEAD ciphersuite to use if all recipients support AEAD, i.e, support_seipdv2 is true.
        let aead_ciphersuite = if aead_support {
            Some(
                message_cipher_suite
                    .filter(|suite| candidate_aead_algorithms.contains(suite))
                    .or(candidate_aead_algorithms.first().copied())
                    .unwrap_or((SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Gcm)),
            )
        } else {
            None
        };

        Self {
            signing_hash_candidates: candidate_hashes_to_sign,
            compression_algorithm,
            symmetric_algorithm,
            aead_ciphersuite,
            support_seipdv2: aead_support,
        }
    }

    pub fn encryption_mechanism(&self) -> EncryptionMechanism {
        match (self.support_seipdv2, self.aead_ciphersuite) {
            (true, Some((sym, aead))) => EncryptionMechanism::SeipdV2(sym, aead),
            _ => EncryptionMechanism::SeipdV1(self.symmetric_algorithm),
        }
    }

    pub fn select_hash_algorithm<'a>(
        &self,
        preferred_hash: HashAlgorithm,
        keys: &'a [PrivateComponentKey<'a>],
        profile: &Profile,
    ) -> Vec<HashAlgorithm> {
        select_hash_algorithm_from_keys(preferred_hash, keys, Some(self), profile)
    }
}

pub(crate) fn select_hash_algorithm_from_keys<'a>(
    preferred_hash: HashAlgorithm,
    keys: &'a [PrivateComponentKey<'a>],
    preference: Option<&RecipientsAlgorithms>,
    profile: &'a Profile,
) -> Vec<HashAlgorithm> {
    let mut selected_hashes = Vec::with_capacity(keys.len());
    for key in keys {
        let mut candidates = if let Some(selection) = preference {
            selection.signing_hash_candidates.clone()
        } else {
            profile.candidate_hash_algorithms().to_vec()
        };

        intersect(
            &mut candidates,
            key.primary_self_certification.preferred_hash_algs(),
        );

        let selected_hash = select_hash_to_sign(
            candidates,
            preferred_hash,
            key.private_key.public_params(),
            profile,
        );
        selected_hashes.push(selected_hash);
    }
    selected_hashes
}

fn select_hash_to_sign(
    mut candidates: Vec<HashAlgorithm>,
    preferred_hash: HashAlgorithm,
    public_params: &PublicParams,
    profile: &Profile,
) -> HashAlgorithm {
    let acceptable_hashes = acceptable_sign_hash_algorithms(public_params, profile);
    intersect(&mut candidates, acceptable_hashes);

    if candidates.contains(&preferred_hash) {
        return preferred_hash;
    }

    if let Some(selection) = candidates.first() {
        *selection
    } else {
        *acceptable_hashes.first().unwrap_or(&HashAlgorithm::Sha256)
    }
}

pub(crate) fn select_hash_to_sign_key_signatures(
    preferred_hash: HashAlgorithm,
    public_params: &PublicParams,
    profile: &Profile,
) -> HashAlgorithm {
    let mut candidates = profile.candidate_hash_algorithms().to_vec();
    let acceptable_hashes = acceptable_sign_hash_algorithms(public_params, profile);
    intersect(&mut candidates, acceptable_hashes);

    if candidates.contains(&preferred_hash) {
        return preferred_hash;
    }

    if let Some(selection) = candidates.first() {
        *selection
    } else {
        *acceptable_hashes.first().unwrap_or(&HashAlgorithm::Sha256)
    }
}

fn intersect<T: Copy + PartialEq>(order_determining: &mut Vec<T>, to_intersect: &[T]) {
    order_determining.retain(|alg| to_intersect.contains(alg));
}

fn acceptable_sign_hash_algorithms<'a>(
    public_params: &'a PublicParams,
    profile: &'a Profile,
) -> &'a [HashAlgorithm] {
    match public_params {
        PublicParams::ECDSA(ecdsa_public_params) => match ecdsa_public_params {
            pgp::types::EcdsaPublicParams::P384 { key: _ } => HASH_ALGORITHMS_MID,
            pgp::types::EcdsaPublicParams::P521 { key: _ } => HASH_ALGORITHMS_HIGH,
            _ => profile.candidate_hash_algorithms(),
        },
        PublicParams::Ed448(_) => HASH_ALGORITHMS_HIGH,
        // AeroFTP patch: post-quantum variants exist only with `pqc`.
        #[cfg(feature = "pqc")]
        PublicParams::MlDsa87Ed448(_) => HASH_ALGORITHMS_HIGH,
        _ => profile.candidate_hash_algorithms(),
    }
}
