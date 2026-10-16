use pgp::{
    crypto::{
        aead::{AeadAlgorithm, ChunkSize},
        ecc_curve::ECCCurve,
        hash::HashAlgorithm,
        public_key::PublicKeyAlgorithm,
        sym::SymmetricKeyAlgorithm,
    },
    packet::Notation,
    types::{CompressionAlgorithm, S2kParams, StringToKey},
};
use rand::{CryptoRng, Rng};

mod settings;
pub use settings::*;
mod s2k;
pub use s2k::*;
mod keygen;
pub use keygen::*;

use std::sync::{Arc, LazyLock};

use crate::AeadCiphersuite;

/// AEAD ciphersuite.
pub type Ciphersuite = (SymmetricKeyAlgorithm, AeadAlgorithm);

/// The default profile.
pub static DEFAULT_PROFILE: LazyLock<Profile> = LazyLock::new(Profile::default);

/// A profile with AEAD enabled, meaning using AEAD (`SEIPDv2`, RFC 9580) encryption if possbile.
///
/// HAZARD: Use this profile only if backward compatibility is not required.
/// This profile breaks backward compatibility.
pub static HAZARD_AEAD_PROFILE: LazyLock<Profile> = LazyLock::new(|| {
    ProfileSettings::builder()
        .preferred_aead_ciphersuite(Some(AeadCiphersuite::default().into()))
        .build_into_profile()
});

#[derive(Debug, Clone)]
pub struct Profile {
    settings: Arc<ProfileSettings>,
}

impl Profile {
    pub fn new(settings: ProfileSettings) -> Self {
        Self {
            settings: Arc::new(settings),
        }
    }

    pub fn rng(&self) -> impl Rng + CryptoRng {
        rand::thread_rng()
    }

    pub fn candidate_compression_algorithms(&self) -> &[CompressionAlgorithm] {
        &self.settings.candidate_compression_algorithms
    }

    pub fn candidate_hash_algorithms(&self) -> &[HashAlgorithm] {
        &self.settings.candidate_hash_algorithms
    }

    pub fn candidate_symmetric_key_algorithms(&self) -> &[SymmetricKeyAlgorithm] {
        &self.settings.candidate_symmetric_key_algorithms
    }

    pub fn candidate_aead_ciphersuites(&self) -> &[(SymmetricKeyAlgorithm, AeadAlgorithm)] {
        &self.settings.candidate_aead_ciphersuites
    }

    pub fn message_hash_algorithm(&self) -> HashAlgorithm {
        self.settings.preferred_hash_algorithm
    }

    pub fn key_hash_algorithm(&self) -> HashAlgorithm {
        self.settings.preferred_hash_algorithm
    }

    pub fn message_aead_cipher_suite(&self) -> Option<Ciphersuite> {
        self.settings.preferred_aead_ciphersuite
    }

    pub fn message_symmetric_algorithm(&self) -> SymmetricKeyAlgorithm {
        self.settings.preferred_symmetric_algorithm
    }

    pub fn message_compression(&self) -> CompressionAlgorithm {
        self.settings.preferred_compression
    }

    pub fn message_aead_chunk_size(&self) -> ChunkSize {
        self.settings.aead_chunk_size
    }

    pub fn reject_hash_algorithm(&self, hash_opt: Option<HashAlgorithm>) -> bool {
        hash_opt.is_some_and(|hash| self.settings.rejected_hashes.contains(&hash))
    }

    pub fn reject_message_hash_algorithm(&self, hash_opt: Option<HashAlgorithm>) -> bool {
        hash_opt.is_some_and(|hash| self.settings.rejected_message_hashes.contains(&hash))
    }

    pub fn reject_public_key_algorithm(&self, algorithm: PublicKeyAlgorithm) -> bool {
        self.settings
            .rejected_public_key_algorithms
            .contains(&algorithm)
    }

    pub fn reject_ecc_curve(&self, curve: &ECCCurve) -> bool {
        self.settings.rejected_ecc_curves.contains(curve)
    }

    pub fn accept_critical_notation(&self, notation: &Notation) -> bool {
        let Ok(notation_name) = std::str::from_utf8(notation.name.as_ref()) else {
            return false;
        };
        self.settings.known_notation_names.contains(notation_name)
    }

    pub fn max_number_of_message_signatures(&self) -> usize {
        self.settings.max_number_of_signatures
    }

    pub fn ignore_key_flags(&self) -> bool {
        self.settings.ignore_key_flags
    }

    pub fn min_rsa_bits(&self) -> usize {
        self.settings.min_rsa_bits
    }

    pub fn fallback_ciphersuite_for_key_length(&self, length: usize) -> Option<Ciphersuite> {
        match length {
            16 => Some((SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Gcm)),
            24 => Some((SymmetricKeyAlgorithm::AES192, AeadAlgorithm::Gcm)),
            32 => Some((SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Gcm)),
            _ => None,
        }
    }

    pub fn key_s2k_params(&self) -> S2kParams {
        self.settings
            .key_encryption_s2k_params
            .generate_s2k_encryption_params(self.rng())
    }

    pub fn message_s2k_params(&self) -> StringToKey {
        self.settings
            .message_encryption_s2k_params
            .generate_s2k_params(self.rng())
    }

    pub fn allow_insecure_verification_with_reformatted_keys(&self) -> bool {
        self.settings
            .allow_insecure_verification_with_reformatted_keys
    }

    pub fn allow_encryption_with_future_or_expired_keys(&self) -> bool {
        self.settings.allow_encryption_with_future_or_expired_keys
    }

    pub fn allow_insecure_decryption_with_signing_keys(&self) -> bool {
        self.settings.allow_insecure_decryption_with_signing_keys
    }

    pub fn max_reading_size(&self) -> Option<usize> {
        self.settings.max_reading_size
    }

    pub fn max_s2k_trials_per_passphrase(&self) -> Option<usize> {
        self.settings.max_s2k_trials_per_passphrase
    }

    pub(crate) fn default_key_generation_profile(&self) -> KeyGenerationProfileBuilder {
        if self.message_aead_cipher_suite().is_some() {
            // Signal support for SEIPD v2 if the profile prefers AEAD.
            KeyGenerationProfileBuilder::default()
                .with_preferred_aead_algorithms_default()
                .seipd_v2(true)
        } else {
            KeyGenerationProfileBuilder::default()
        }
    }
}

impl Default for Profile {
    fn default() -> Self {
        Self::new(ProfileSettings::default())
    }
}

impl From<ProfileSettings> for Profile {
    fn from(settings: ProfileSettings) -> Self {
        Self::new(settings)
    }
}
