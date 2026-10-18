use pgp::{
    composed::KeyType,
    crypto::{
        aead::AeadAlgorithm, ecc_curve::ECCCurve, hash::HashAlgorithm, sym::SymmetricKeyAlgorithm,
    },
    packet::{Features, KeyFlags, UserId},
    types::{CompressionAlgorithm, KeyVersion},
};
use smallvec::SmallVec;

use crate::{KeyDetailsConfig, Profile, PREFERRED_SYMMETRIC_KEY_ALGORITHMS};

pub const PREFERRED_KEY_GEN_HASH_ALGORITHMS: &[HashAlgorithm] =
    &[HashAlgorithm::Sha512, HashAlgorithm::Sha256];

#[cfg(feature = "hazmat-key-gen")]
/// The minimum RSA modulus size in bits accepted for [`KeyGenerationType::RsaCustom`].
pub const MIN_RSA_KEY_GEN_BITS: u32 = 1023;

#[cfg(feature = "hazmat-key-gen")]
/// The maximum RSA modulus size in bits accepted for [`KeyGenerationType::RsaCustom`].
pub const MAX_RSA_KEY_GEN_BITS: u32 = 8192;

pub const PREFERRED_KEY_GEN_COMPRESSION_ALGORITHMS: &[CompressionAlgorithm] = &[
    CompressionAlgorithm::Uncompressed,
    CompressionAlgorithm::ZLIB,
];

pub const PREFERRED_KEY_GEN_AEAD_CIPHERSUITES: &[(SymmetricKeyAlgorithm, AeadAlgorithm)] = &[
    (SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Gcm),
    (SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Ocb),
    (SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Gcm),
    (SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Ocb),
];

/// The algorithm type to use for the key generation.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum KeyGenerationType {
    /// An RSA 4096-bit v4 signing and encryption key.
    RSA,

    /// An RSA v4 signing and encryption key with a custom modulus size in bits.
    ///
    /// HAZARD: This is intended for testing purposes.
    /// Use [`Self::RSA`] in clients.
    /// The size must be in [`MIN_RSA_KEY_GEN_BITS`]`..=`[`MAX_RSA_KEY_GEN_BITS`].
    /// Requires the `hazmat-key-gen` feature.
    #[cfg(feature = "hazmat-key-gen")]
    RsaCustom(u32),

    /// An ECC v4 signing (`EdDsaLegacy`) and encryption key (`ECDH` with `Curve25519` legacy).
    #[default]
    ECC,

    /// A PQC v6 signing (`ML-DSA`) and encryption key (`ML-KEM`).
    /// AeroFTP patch: only with the `pqc` feature.
    #[cfg(feature = "pqc")]
    PQC,
}

impl KeyGenerationType {
    pub(crate) fn primary_key_type(self) -> KeyType {
        match self {
            KeyGenerationType::RSA => KeyType::Rsa(4096),
            #[cfg(feature = "hazmat-key-gen")]
            KeyGenerationType::RsaCustom(bits) => KeyType::Rsa(bits),
            KeyGenerationType::ECC => KeyType::Ed25519Legacy,
            #[cfg(feature = "pqc")]
            KeyGenerationType::PQC => KeyType::MlDsa65Ed25519,
        }
    }

    pub(crate) fn encryption_key_type(self) -> KeyType {
        match self {
            KeyGenerationType::RSA => KeyType::Rsa(4096),
            #[cfg(feature = "hazmat-key-gen")]
            KeyGenerationType::RsaCustom(bits) => KeyType::Rsa(bits),
            KeyGenerationType::ECC => KeyType::ECDH(ECCCurve::Curve25519Legacy),
            #[cfg(feature = "pqc")]
            KeyGenerationType::PQC => KeyType::MlKem768X25519,
        }
    }

    pub(crate) fn key_generation_profile(self, profile: &Profile) -> KeyGenerationProfile {
        match self {
            KeyGenerationType::RSA | KeyGenerationType::ECC => {
                profile.default_key_generation_profile().build()
            }
            #[cfg(feature = "hazmat-key-gen")]
            KeyGenerationType::RsaCustom(_) => profile.default_key_generation_profile().build(),
            #[cfg(feature = "pqc")]
            KeyGenerationType::PQC => profile
                .default_key_generation_profile()
                .key_version(KeyVersion::V6)
                .build(),
        }
    }
}

/// The profile to use for the key generation.
#[derive(Debug, Clone)]
pub struct KeyGenerationProfile {
    /// The key version to use for the key generation.
    pub key_version: KeyVersion,

    /// The preferred symmetric algorithms to use for the key generation.
    pub preferred_symmetric_algorithms: SmallVec<[SymmetricKeyAlgorithm; 8]>,

    /// The preferred hash algorithms to use for the key generation.
    pub preferred_hash_algorithms: SmallVec<[HashAlgorithm; 8]>,

    /// The preferred compression algorithms to use for the key generation.
    pub preferred_compression_algorithms: SmallVec<[CompressionAlgorithm; 8]>,

    /// The preferred AEAD algorithms to use for the key generation.
    pub preferred_aead_ciphersuites: SmallVec<[(SymmetricKeyAlgorithm, AeadAlgorithm); 4]>,

    /// Whether to signal support for SEIPD v2.
    pub support_seipd_v2: bool,
}

impl Default for KeyGenerationProfile {
    fn default() -> Self {
        Self {
            key_version: KeyVersion::V4,
            preferred_symmetric_algorithms: PREFERRED_SYMMETRIC_KEY_ALGORITHMS.into(),
            preferred_hash_algorithms: PREFERRED_KEY_GEN_HASH_ALGORITHMS.into(),
            preferred_compression_algorithms: PREFERRED_KEY_GEN_COMPRESSION_ALGORITHMS.into(),
            preferred_aead_ciphersuites: SmallVec::new(),
            support_seipd_v2: false,
        }
    }
}

impl KeyGenerationProfile {
    pub(crate) fn create_key_details_config(
        self,
        primary_user_id: Option<UserId>,
        non_primary_user_ids: Vec<UserId>,
        keyflags: KeyFlags,
    ) -> KeyDetailsConfig {
        let mut features = Features::new();
        features.set_seipd_v1(true);
        features.set_seipd_v2(self.support_seipd_v2);

        KeyDetailsConfig {
            primary_user_id,
            non_primary_user_ids,
            keyflags,
            features,
            preferred_symmetric_algorithms: self.preferred_symmetric_algorithms,
            preferred_hash_algorithms: self.preferred_hash_algorithms,
            preferred_compression_algorithms: self.preferred_compression_algorithms,
            preferred_aead_algorithms: self.preferred_aead_ciphersuites,
        }
    }
}

/// A builder for the key generation profile.
#[derive(Default, Debug, Clone)]
pub struct KeyGenerationProfileBuilder {
    profile: KeyGenerationProfile,
}

impl KeyGenerationProfileBuilder {
    pub fn new() -> Self {
        Self {
            profile: KeyGenerationProfile::default(),
        }
    }

    pub fn key_version(mut self, key_version: KeyVersion) -> Self {
        self.profile.key_version = key_version;
        self
    }

    pub fn preferred_symmetric_algorithms(
        mut self,
        syms: impl Into<SmallVec<[SymmetricKeyAlgorithm; 8]>>,
    ) -> Self {
        self.profile.preferred_symmetric_algorithms = syms.into();
        self
    }

    pub fn preferred_hash_algorithms(
        mut self,
        hashes: impl Into<SmallVec<[HashAlgorithm; 8]>>,
    ) -> Self {
        self.profile.preferred_hash_algorithms = hashes.into();
        self
    }

    pub fn preferred_compression_algorithms(
        mut self,
        compressions: impl Into<SmallVec<[CompressionAlgorithm; 8]>>,
    ) -> Self {
        self.profile.preferred_compression_algorithms = compressions.into();
        self
    }

    pub fn preferred_aead_algorithms(
        mut self,
        aeads: impl Into<SmallVec<[(SymmetricKeyAlgorithm, AeadAlgorithm); 4]>>,
    ) -> Self {
        self.profile.preferred_aead_ciphersuites = aeads.into();
        self
    }

    pub fn with_preferred_aead_algorithms_default(mut self) -> Self {
        self.profile.preferred_aead_ciphersuites = PREFERRED_KEY_GEN_AEAD_CIPHERSUITES.into();
        self
    }

    pub fn seipd_v2(mut self, seipd_v2: bool) -> Self {
        self.profile.support_seipd_v2 = seipd_v2;
        self
    }

    pub fn build(self) -> KeyGenerationProfile {
        self.profile
    }
}
