use std::collections::HashSet;

use pgp::{
    crypto::{
        aead::{AeadAlgorithm, ChunkSize},
        ecc_curve::ECCCurve,
        hash::HashAlgorithm,
        public_key::PublicKeyAlgorithm,
        sym::SymmetricKeyAlgorithm,
    },
    types::CompressionAlgorithm,
};

use super::{KeyGenerationProfile, KeyGenerationType};
use crate::{Profile, StringToKeyOption, PROTON_CONTEXT_NOTATION_NAME};

use super::Ciphersuite;

/// Preferred symmetric-key algorithms (in descending order of preference)
pub const PREFERRED_SYMMETRIC_KEY_ALGORITHMS: &[SymmetricKeyAlgorithm] =
    &[SymmetricKeyAlgorithm::AES256, SymmetricKeyAlgorithm::AES128];

/// Preferred AEAD algorithms (in descending order of preference)
pub const PREFERRED_AEAD_CIPHERSUITES: &[(SymmetricKeyAlgorithm, AeadAlgorithm)] = &[
    (SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Gcm),
    (SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Eax),
    (SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Ocb),
    (SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Gcm),
    (SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Eax),
    (SymmetricKeyAlgorithm::AES128, AeadAlgorithm::Ocb),
];

/// Preferred hash algorithms (in descending order of preference)
pub const PREFERRED_HASH_ALGORITHMS: &[HashAlgorithm] = &[
    HashAlgorithm::Sha512,
    HashAlgorithm::Sha256,
    HashAlgorithm::Sha3_512,
    HashAlgorithm::Sha3_256,
];

pub const PREFERRED_COMPRESSION_ALGORITHMS: &[CompressionAlgorithm] = &[
    CompressionAlgorithm::Uncompressed,
    CompressionAlgorithm::ZLIB,
    CompressionAlgorithm::ZIP,
];

pub const DEFAULT_MAX_READING_SIZE: usize = 50 * 1024 * 1024; // 50MB

pub type KeyGenerationForType = Box<dyn Fn(KeyGenerationType) -> KeyGenerationProfile>;

/// Represents the configuration options for `OpenPGP` operations.
///
/// This struct provides granular control over all `OpenPGP` settings
/// used throughout the library. The default configuration matches the recommended Proton profile,
/// but all options can be customized to suit specific requirements or interoperability needs.
#[derive(Debug, Clone)]
#[allow(clippy::struct_excessive_bools)]
pub struct ProfileSettings {
    /// Candidate hash algorithms, in descending order of preference.
    ///
    /// Used when selecting encryption algorithms based on recipient preferences.
    pub candidate_hash_algorithms: Vec<HashAlgorithm>,

    /// Candidate symmetric-key algorithms, in descending order of preference.
    ///
    /// Used when selecting encryption algorithms based on recipient preferences.
    pub candidate_symmetric_key_algorithms: Vec<SymmetricKeyAlgorithm>,

    /// Candidate compression algorithms, in descending order of preference.
    ///
    /// Used when selecting compression algorithms based on recipient preferences.
    pub candidate_compression_algorithms: Vec<CompressionAlgorithm>,

    /// Candidate AEAD cipher suites, in descending order of preference.
    ///
    /// Used when selecting AEAD cipher suites based on recipient preferences.
    pub candidate_aead_ciphersuites: Vec<(SymmetricKeyAlgorithm, AeadAlgorithm)>,

    /// The preferred hash algorithm for signatures.
    pub preferred_hash_algorithm: HashAlgorithm,

    /// The preferred AEAD ciphersuite for encryption, if any.
    ///
    /// If this option is `None`, `SEIPDv1` will be enforced for encrpytion.
    pub preferred_aead_ciphersuite: Option<Ciphersuite>,

    /// The preferred symmetric-key algorithm for encryption.
    pub preferred_symmetric_algorithm: SymmetricKeyAlgorithm,

    /// The preferred compression algorithm for message compression.
    pub preferred_compression: CompressionAlgorithm,

    /// String-to-key (S2K) parameters for message encryption.
    ///
    /// This is used in password based encryption.
    pub message_encryption_s2k_params: StringToKeyOption,

    /// String-to-key (S2K) parameters for key encryption.
    ///
    /// This is used when encrypting a key in the lock operation.
    pub key_encryption_s2k_params: StringToKeyOption,

    /// AEAD chunk size to use for chunked encryption.
    ///
    /// If AEAD is used, this setting allows to define the used chunk size.
    pub aead_chunk_size: ChunkSize,

    /// Hash algorithms that are explicitly rejected for any use.
    pub rejected_hashes: HashSet<HashAlgorithm>,

    /// Hash algorithms that are rejected for message signatures.
    ///
    /// This must be a superset of `rejected_hashes`
    pub rejected_message_hashes: HashSet<HashAlgorithm>,

    /// Public key algorithms that are rejected.
    pub rejected_public_key_algorithms: Vec<PublicKeyAlgorithm>,

    /// ECC curves that are rejected.
    pub rejected_ecc_curves: Vec<ECCCurve>,

    /// Set of critical notation names that are recognized as known.
    pub known_notation_names: HashSet<String>,

    /// Minimum number of bits required for RSA keys.
    pub min_rsa_bits: usize,

    /// Maximum number of signatures that are verified in a message.
    pub max_number_of_signatures: usize,

    /// If true, ignore key flags in key usage checks.
    pub ignore_key_flags: bool,

    /// Allow verification of message signatures with keys whose validity at the time of signing cannot be determined.
    ///
    /// Instead, a verification key will also be considered valid as long as it is valid at the current time.
    /// This setting is potentially insecure, but it is needed to verify messages signed with keys that were later reformatted,
    /// and have self-signature's creation date that does not match the primary key creation date.
    pub allow_insecure_verification_with_reformatted_keys: bool,

    /// If true, allows encryption to expired or not yet valid keys.
    pub allow_encryption_with_future_or_expired_keys: bool,

    /// If true, allows decryption with keys that are only marked as signing keys.
    pub allow_insecure_decryption_with_signing_keys: bool,

    /// The maximum reading size in bytes for reading messages in decryption and verification.
    ///
    /// This allow to prevent denial of service attacks by limiting the amount of data that can be read from a message.
    /// E.g., via compressed messages.
    pub max_reading_size: Option<usize>,

    /// The maximum number of S2K trials per passphrase.
    ///
    /// This allows to limit resource usage by limiting the number of S2K trials per passphrase.
    pub max_s2k_trials_per_passphrase: Option<usize>,
}

impl ProfileSettings {
    pub fn builder() -> ProfileSettingsBuilder {
        ProfileSettingsBuilder::new()
    }
}

impl Default for ProfileSettings {
    fn default() -> Self {
        Self {
            candidate_hash_algorithms: PREFERRED_HASH_ALGORITHMS.to_vec(),
            candidate_symmetric_key_algorithms: PREFERRED_SYMMETRIC_KEY_ALGORITHMS.to_vec(),
            candidate_compression_algorithms: PREFERRED_COMPRESSION_ALGORITHMS.to_vec(),
            candidate_aead_ciphersuites: PREFERRED_AEAD_CIPHERSUITES.to_vec(),
            preferred_hash_algorithm: HashAlgorithm::Sha512,
            preferred_aead_ciphersuite: None,
            preferred_symmetric_algorithm: SymmetricKeyAlgorithm::AES256,
            preferred_compression: CompressionAlgorithm::Uncompressed,
            message_encryption_s2k_params: StringToKeyOption::IteratedAndSalted {
                sym_alg: SymmetricKeyAlgorithm::AES256,
                hash_alg: HashAlgorithm::Sha256,
                count: 224,
            },
            aead_chunk_size: ChunkSize::C256KiB,
            key_encryption_s2k_params: StringToKeyOption::IteratedAndSalted {
                sym_alg: SymmetricKeyAlgorithm::AES256,
                hash_alg: HashAlgorithm::Sha256,
                count: 96,
            },
            rejected_hashes: HashSet::from([HashAlgorithm::Md5, HashAlgorithm::Ripemd160]),
            rejected_message_hashes: HashSet::from([
                HashAlgorithm::Md5,
                HashAlgorithm::Ripemd160,
                HashAlgorithm::Sha1,
            ]),
            rejected_public_key_algorithms: vec![
                PublicKeyAlgorithm::Elgamal,
                PublicKeyAlgorithm::ElgamalEncrypt,
                PublicKeyAlgorithm::DSA,
            ],
            rejected_ecc_curves: vec![ECCCurve::Secp256k1],
            min_rsa_bits: 1023,
            max_number_of_signatures: 16,
            ignore_key_flags: false,
            known_notation_names: HashSet::from([PROTON_CONTEXT_NOTATION_NAME.to_string()]),
            allow_insecure_verification_with_reformatted_keys: true,
            allow_encryption_with_future_or_expired_keys: true,
            allow_insecure_decryption_with_signing_keys: true,
            max_reading_size: Some(DEFAULT_MAX_READING_SIZE),
            max_s2k_trials_per_passphrase: Some(5),
        }
    }
}

/// Builder for `ProfileSettings`.
#[derive(Default, Debug, Clone)]
pub struct ProfileSettingsBuilder {
    settings: ProfileSettings,
}

impl ProfileSettingsBuilder {
    fn new() -> Self {
        Self::default()
    }

    /// Sets the candidate hash algorithms to consider for signatures.
    ///
    /// These are the hash algorithms that will be considered when selecting the hash algorithm for creating signatures.
    pub fn candidate_hash_algorithms<I>(mut self, algs: I) -> Self
    where
        I: IntoIterator<Item = HashAlgorithm>,
    {
        self.settings.candidate_hash_algorithms = algs.into_iter().collect();
        self
    }

    /// Sets the candidate symmetric key algorithms to consider for encryption.
    ///
    /// These are the symmetric algorithms that will be considered when selecting the symmetric algorithm for encryption.
    pub fn candidate_symmetric_key_algorithms<I>(mut self, algs: I) -> Self
    where
        I: IntoIterator<Item = SymmetricKeyAlgorithm>,
    {
        self.settings.candidate_symmetric_key_algorithms = algs.into_iter().collect();
        self
    }

    /// Sets the candidate compression algorithms to consider for message compression.
    ///
    /// These are the compression algorithms that will be considered when selecting the compression algorithm for compressing messages.
    pub fn candidate_compression_algorithms<I>(mut self, algs: I) -> Self
    where
        I: IntoIterator<Item = CompressionAlgorithm>,
    {
        self.settings.candidate_compression_algorithms = algs.into_iter().collect();
        self
    }

    /// Sets the candidate AEAD cipher suites to consider for AEAD encryption.
    ///
    /// These are the (symmetric, AEAD) algorithm pairs that will be considered when selecting the AEAD cipher suite for encryption.
    /// If not set, `SEIPDv1` will be enforced for encrpytion.
    pub fn candidate_aead_ciphersuites<I>(mut self, suites: I) -> Self
    where
        I: IntoIterator<Item = (SymmetricKeyAlgorithm, AeadAlgorithm)>,
    {
        self.settings.candidate_aead_ciphersuites = suites.into_iter().collect();
        self
    }

    /// Sets the preferred hash algorithm for signatures.
    ///
    /// This is the hash algorithm that will be preferred for signing operations.
    pub fn preferred_hash_algorithm(mut self, alg: HashAlgorithm) -> Self {
        self.settings.preferred_hash_algorithm = alg;
        self
    }

    /// Sets the preferred AEAD ciphersuite for AEAD encryption.
    ///
    /// This is the (symmetric, AEAD) algorithm pair that will be preferred for AEAD encryption.
    pub fn preferred_aead_ciphersuite(
        mut self,
        suite: Option<(SymmetricKeyAlgorithm, AeadAlgorithm)>,
    ) -> Self {
        self.settings.preferred_aead_ciphersuite = suite;
        self
    }

    /// Sets the preferred symmetric key algorithm for encryption.
    ///
    /// This is the symmetric algorithm that will be preferred for message encryption.
    pub fn preferred_symmetric_algorithm(mut self, alg: SymmetricKeyAlgorithm) -> Self {
        self.settings.preferred_symmetric_algorithm = alg;
        self
    }

    /// Sets the preferred compression algorithm for message compression.
    ///
    /// This is the compression algorithm that will be preferred for compressing messages.
    pub fn preferred_compression(mut self, alg: CompressionAlgorithm) -> Self {
        self.settings.preferred_compression = alg;
        self
    }

    /// Sets the S2K (String-to-Key) parameters for message encryption.
    ///
    /// These parameters control how passphrases are converted to keys for message encryption.
    pub fn message_encryption_s2k_params(mut self, params: StringToKeyOption) -> Self {
        self.settings.message_encryption_s2k_params = params;
        self
    }

    /// Sets the AEAD chunk size for AEAD-encrypted messages.
    ///
    /// This controls the chunk size used for AEAD encryption.
    pub fn aead_chunk_size(mut self, size: ChunkSize) -> Self {
        self.settings.aead_chunk_size = size;
        self
    }

    /// Sets the S2K (String-to-Key) parameters for key encryption.
    ///
    /// These parameters control how passphrases are converted to keys for key encryption.
    pub fn key_encryption_s2k_params(mut self, params: StringToKeyOption) -> Self {
        self.settings.key_encryption_s2k_params = params;
        self
    }

    /// Sets the hash algorithms that should be rejected for any use.
    ///
    /// These hash algorithms will not be used for any cryptographic operation.
    pub fn rejected_hashes<I>(mut self, hashes: I) -> Self
    where
        I: IntoIterator<Item = HashAlgorithm>,
    {
        self.settings.rejected_hashes = hashes.into_iter().collect();
        self
    }

    /// Sets the hash algorithms that should be rejected for message signatures.
    ///
    /// These hash algorithms will not be used for message signatures.
    pub fn rejected_message_hashes<I>(mut self, hashes: I) -> Self
    where
        I: IntoIterator<Item = HashAlgorithm>,
    {
        self.settings.rejected_message_hashes = hashes.into_iter().collect();
        self
    }

    /// Sets the public key algorithms that should be rejected.
    ///
    /// These public key algorithms will not be used for any cryptographic operation.
    pub fn rejected_public_key_algorithms<I>(mut self, algs: I) -> Self
    where
        I: IntoIterator<Item = PublicKeyAlgorithm>,
    {
        self.settings.rejected_public_key_algorithms = algs.into_iter().collect();
        self
    }

    /// Sets the ECC curves that should be rejected.
    ///
    /// These elliptic curves will not be used for any cryptographic operation.
    pub fn rejected_ecc_curves<I>(mut self, curves: I) -> Self
    where
        I: IntoIterator<Item = ECCCurve>,
    {
        self.settings.rejected_ecc_curves = curves.into_iter().collect();
        self
    }

    /// Sets the minimum number of bits required for RSA keys.
    ///
    /// RSA keys with fewer bits than this value will be rejected.
    pub fn min_rsa_bits(mut self, bits: usize) -> Self {
        self.settings.min_rsa_bits = bits;
        self
    }

    /// Sets the maximum number of signatures that are verified in a message.
    pub fn max_number_of_signatures(mut self, num: usize) -> Self {
        self.settings.max_number_of_signatures = num;
        self
    }

    /// Sets whether to ignore key flags during verification.
    ///
    /// If true, key flags will be ignored when verifying signatures.
    pub fn ignore_key_flags(mut self, ignore: bool) -> Self {
        self.settings.ignore_key_flags = ignore;
        self
    }

    /// Sets whether to allow insecure verification with reformatted keys.
    ///
    /// If true, verification will allow keys whose validity at the time of signing cannot be determined,
    /// which is needed for some reformatted or migrated keys but may be less secure.
    pub fn allow_insecure_verification_with_reformatted_keys(mut self, allow: bool) -> Self {
        self.settings
            .allow_insecure_verification_with_reformatted_keys = allow;
        self
    }

    /// Sets whether to allow encryption with future and expired keys.
    ///
    /// If true, no time checks are performed for encryption key selection.
    pub fn allow_encryption_with_future_and_expired_keys(mut self, allow: bool) -> Self {
        self.settings.allow_encryption_with_future_or_expired_keys = allow;
        self
    }

    /// Sets whether to allow decryption with signing keys.
    ///
    /// If true, decryption will allow using keys that are only marked as signing keys.
    pub fn allow_insecure_decryption_with_signing_keys(mut self, allow: bool) -> Self {
        self.settings.allow_insecure_decryption_with_signing_keys = allow;
        self
    }

    /// Sets the maximum reading size in bytes for reading messages in decryption and verification.
    ///
    /// This allows to prevent denial of service attacks by limiting the amount of data that can be read from a message.
    /// E.g., via compressed messages.
    pub fn max_reading_size(mut self, size: Option<usize>) -> Self {
        self.settings.max_reading_size = size;
        self
    }

    /// Sets the maximum number of S2K trials per passphrase.
    ///
    /// This allows to limit resource usage by limiting the number of S2K trials per passphrase.
    pub fn max_s2k_trials_per_passphrase(mut self, trials: usize) -> Self {
        self.settings.max_s2k_trials_per_passphrase = Some(trials);
        self
    }

    /// Builds the `ProfileSettings` from the builder.
    ///
    /// This will also ensure that all rejected hashes are included in the set of rejected message hashes.
    pub fn build(mut self) -> ProfileSettings {
        self.settings
            .rejected_message_hashes
            .extend(self.settings.rejected_hashes.iter());
        self.settings
    }

    /// Directly builds the `Profile` based on `ProfileSettings` from the builder.
    pub fn build_into_profile(self) -> Profile {
        self.build().into()
    }
}
