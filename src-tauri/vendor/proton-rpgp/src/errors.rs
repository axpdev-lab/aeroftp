use std::io;

use pgp::{
    armor::BlockType,
    crypto::{ecc_curve::ECCCurve, hash::HashAlgorithm, public_key::PublicKeyAlgorithm},
    packet::{self},
    types::{KeyId, KeyVersion, PkeskVersion},
};

use crate::{
    types::UnixTime, GenericKeyIdentifier, GenericKeyIdentifierList, PrettyKeyFlags,
    VerificationContext,
};

pub type Result<T> = std::result::Result<T, Error>;

pub(crate) const LIB_ERROR_PREFIX: &str = "Proton-rPGP";
pub(crate) const FUTURE_SIGNATURE_ERROR_MESSAGE: &str =
    "Signature has a creation time in the future";

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("{LIB_ERROR_PREFIX}: {0}")]
    KeyOperation(#[from] KeyOperationError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    KeyGeneration(#[from] KeyGenerationError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    KeyValidation(#[from] KeyValidationError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    KeyModification(#[from] KeyModificationError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    KeyCertification(#[from] KeyCertificationError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    Signing(#[from] SigningError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    Decryption(#[from] DecryptionError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    Encryption(#[from] EncryptionError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    Verification(#[from] MessageVerificationError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    Armor(#[from] ArmorError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    EncryptedMessage(#[from] EncryptedMessageError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    VerificationResultUtility(#[from] VerificationResultUtilityError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    ForwardingTransform(#[from] ForwardingTransformError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    ForwardingInstance(#[from] ForwardingInstanceError),

    #[error("{LIB_ERROR_PREFIX}: {0}")]
    ForwardingKeyGeneration(#[from] ForwardingKeyGenerationError),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyCertificationSelectionError {
    #[error("No valid self-certification found, error per self-signature: {0}")]
    NoSelfCertification(ErrorList<SignatureError>),

    #[error("No valid user-id with self-certification found, error per user-id: {0}")]
    NoIdentity(ErrorList<KeyCertificationSelectionError>),

    #[error("Invalid self-certification signature: {0}")]
    InvalidSignature(#[from] SignatureError),

    #[error("Latest user self-certification signature is revoked")]
    Revoked(Box<packet::Signature>),

    #[error(
        "Key is expired at unix time {date}, creation time {creation}, expiration: {expiration}"
    )]
    ExpiredKey {
        date: UnixTime,
        creation: UnixTime,
        expiration: UnixTime,
    },

    #[error("Key is in the future at unix time {date}, creation time {creation}")]
    FutureKey { date: UnixTime, creation: UnixTime },
}

#[derive(Debug, thiserror::Error)]
pub enum SignatureError {
    #[error("Signature has no creation time")]
    NoCreationTime,

    #[error("Signature does not specify a hash algorithm")]
    NoHash,

    #[error("Failed to compute hash for signature: {0}")]
    HashComputation(pgp::errors::Error),

    #[error("Raw signature verification failed: {0}")]
    Verification(#[from] pgp::errors::Error),

    #[error("Signature uses an invalid hash according to the profile: {0:?}")]
    InvalidHash(Option<HashAlgorithm>),

    #[error("Failed to access signature config")]
    ConfigAccess,

    #[error("{FUTURE_SIGNATURE_ERROR_MESSAGE}: {0}")]
    FutureSignature(UnixTime),

    #[error(
        "Signature is expired at unix time {date}, signature creation {creation}, expiration: {expiration}"
    )]
    Expired {
        date: UnixTime,
        creation: UnixTime,
        expiration: UnixTime,
    },

    #[error(
        "Signature with creation time {signature_date} is older than the verifying key with creation time {key_date}"
    )]
    SignatureOlderThanKey {
        signature_date: UnixTime,
        key_date: UnixTime,
    },

    #[error("Signature has a non-accepted critical notation with name: {name}")]
    CriticalNotation { name: String },

    #[error("Signing subkey {0} is missing a cross-signature")]
    MissingCrossSignature(KeyId),
}

#[derive(Debug, thiserror::Error)]
pub enum SignatureContextError {
    #[error("Signature context is required but is not provided: {0}")]
    MissingContext(VerificationContext),

    #[error("Signature context {0} does not match the verification context: {1}")]
    WrongContext(String, VerificationContext),

    #[error("Signature has multiple context notations: {0:?}")]
    MultipleContexts(Vec<String>),

    #[error(
        "Signature contains a critical context {0}, but no matching verification context was provided"
    )]
    CriticialContext(String),
}

#[derive(Debug, thiserror::Error)]
pub enum MessageSignatureError {
    #[error("Message signature verification failed: {0}")]
    Failed(#[from] SignatureError),

    #[error("No key found to verify signature: {0}")]
    NoMatchingKey(ErrorList<KeyValidationError>),

    #[error(transparent)]
    Context(SignatureContextError),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyOperationError {
    #[error("Failed to lock private OpenPGP key with key id {0}: {1}")]
    Lock(KeyId, pgp::errors::Error),

    #[error("Failed to unlock private OpenPGP key with key id {0}: {1}")]
    Unlock(KeyId, pgp::errors::Error),

    #[error("Failed to encode OpenPGP key: {0}")]
    Encode(pgp::errors::Error),

    #[error("Failed to decode OpenPGP key: {0}")]
    Decode(pgp::errors::Error),

    #[error("Failed to find OpenPGP key")]
    DecodeNotFound,

    #[error("Multiple keys found in the input")]
    DecodeMultipleKeys,

    #[error("Key is locked")]
    Locked,

    #[error("Unlocking an expected locked key failed: {0}")]
    ExpectLocked(#[from] ExpectLockedError),

    #[error("Failed to validate public parts of private OpenPGP key: {0}")]
    ValidatePublicParts(#[from] KeySecretParamValidationError),
}

#[derive(Debug, thiserror::Error)]
pub enum ExpectLockedError {
    #[error("Primary key is already unlocked")]
    PrimaryKeyUnlocked,

    #[error("Subkey {0} is already unlocked")]
    SubkeyUnlocked(KeyId),

    #[error("Found public subkey in a key supposed to be locked")]
    NoPublicSubkeys,
}

#[derive(Debug, thiserror::Error)]
pub enum KeySecretParamValidationError {
    #[error("Failed to validate public parts because the key is locked")]
    ValidatePublicPartsOnLocked,

    #[error("Failed to compute public parameters from secret parameters")]
    PublicParamsConversionFailed(#[from] pgp::errors::Error),

    #[error("The private parameters do not match the public parameters for the secret key")]
    ValidatePublicPartsFailed,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyValidationError {
    #[error(transparent)]
    KeySelfCertification(#[from] KeyCertificationSelectionError),

    #[error("Primary key {0} does not meet requirements: {1}")]
    PrimaryRequirement(KeyId, KeyRequirementError),

    #[error("Subkey {0} does not meet requirements: {1}")]
    SubkeyRequirement(KeyId, KeyRequirementError),

    #[error("Key {0} does not match requested key-id: {1}")]
    NoMatch(KeyId, KeyId),

    #[error("Key {0} does not match requested key-id: {1}")]
    NoMatchDecryption(KeyId, GenericKeyIdentifier),

    #[error("Key {0} does not match requested key-ids: {1}")]
    NoMatchList(GenericKeyIdentifier, GenericKeyIdentifierList),

    #[error("No valid encryption key found in key with primary key-id {0}: {1}")]
    NoEncryptionKey(KeyId, ErrorList<KeyValidationError>),

    #[error("No valid verification keys found in key with primary key-id {0}: {1}")]
    NoVerificationKeys(KeyId, ErrorList<KeyValidationError>),

    #[error("No valid decryption keys found in key with primary key-id {0}: {1}")]
    NoDecryptionKeys(KeyId, ErrorList<KeyValidationError>),

    #[error("No valid signing key found in key with primary key-id {0}: {1}")]
    NoSigningKey(KeyId, ErrorList<KeyValidationError>),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyRequirementError {
    #[error("Rejected public key algorithm: {0:?}")]
    WeakAlgorithm(PublicKeyAlgorithm),

    #[error("Rejected RSA public key algorithm: insufficient bits (got: {0}, required: {1})")]
    WeakRsaAlgorithm(usize, usize),

    #[error("Rejected ecc curve: {0:?}")]
    WeakEccAlgorithm(ECCCurve),

    #[error("Invalid legacy usage in v6 for curve: {0}")]
    MixedLegacyAlgorithms(ECCCurve),

    #[error("Invalid signing key usage flags: {0}")]
    InvalidSigningKeyFlags(PrettyKeyFlags),

    #[error("Invalid encryption key usage flags: {0}")]
    InvalidEncryptionKeyFlags(PrettyKeyFlags),

    #[error("Invalid algorithm for usage: {0:?}")]
    InvalidUsageAlgorithm(PublicKeyAlgorithm),

    #[error("Algorithm is not supported by the library")]
    UnsupportedAlgorithm,
}

#[derive(Debug, thiserror::Error)]
pub enum KeyGenerationError {
    #[error("No user id provided")]
    NoUserId,

    #[error("Failed to convert user id: {0}")]
    InvalidUserId(#[from] UserIdError),

    #[error("Failed to generate subkey: {0}")]
    SubkeyGeneration(#[from] pgp::composed::SubkeyParamsBuilderError),

    #[error("Failed to perpare primary key: {0}")]
    PrimaryKeyPreparation(#[from] pgp::composed::SecretKeyParamsBuilderError),

    #[error("Failed to generate key: {0}")]
    Generation(#[from] pgp::errors::Error),

    #[cfg(feature = "hazmat-key-gen")]
    #[error("Invalid RSA key size {0}: must be between {min} and {max} bits", min = crate::MIN_RSA_KEY_GEN_BITS, max = crate::MAX_RSA_KEY_GEN_BITS)]
    InvalidRsaBits(u32),

    #[cfg(feature = "hazmat-key-gen")]
    #[error("Failed to generate RSA key: {0}")]
    RsaGeneration(#[from] rsa::Error),

    #[error("Failed to self-sign key: {0}")]
    Signing(#[from] SigningError),

    #[error("Invalid state in key generation: {0}")]
    InvalidState(&'static str),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyModificationError {
    #[error("Failed to convert user id: {0}")]
    InvalidUserId(#[from] UserIdError),

    #[error("Failed to self-sign key: {0}")]
    Signing(#[from] SigningError),

    #[error("Failed load self signature for subkey: {0}")]
    SubkeyCertification(#[from] KeyCertificationSelectionError),

    #[error("Failed to modify subkey params: {0}")]
    SubkeyModification(pgp::errors::Error),

    #[error("Failed to modify primary key params: {0}")]
    PrimaryKeyModification(pgp::errors::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum KeyCertificationError {
    #[error("The key has too many user ids to certify with an external certifier")]
    TooManyUserIds,

    #[error("The key has no user id to certify")]
    NoUserId,

    #[error("The user id of the key is not valid utf-8")]
    InvalidUserId,

    #[error("The user id of the key does not contain the email address {0}")]
    EmailMismatch(String),

    #[error("Failed to load a valid self-certification for the user id to certify: {0}")]
    UserIdSelfCertification(KeyCertificationSelectionError),

    #[error("Failed to load a valid user id in the certifier key: {0}")]
    CertifierUserId(KeyCertificationSelectionError),

    #[error("Failed to select a certification key in the certifier key: {0}")]
    CertificationKeySelection(KeyValidationError),

    #[error("Failed to select key for verifiying certifications: {0}")]
    VerificationKeySelection(KeyValidationError),

    #[error("Failed to create the certification signature: {0}")]
    Signing(#[from] SigningError),

    #[error("No valid certification signature from the given certifier was found: {0}")]
    NoValidCertification(ErrorList<SignatureError>),
}

#[derive(Debug, thiserror::Error)]
pub enum UserIdError {
    #[error("Invalid user id {0}: {1}")]
    InvalidUserId(String, pgp::errors::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum EncryptionError {
    #[error("Failed to select encryption key: {0}")]
    EncryptionKeySelection(#[from] KeyValidationError),

    #[error("Failed to select signing key in encryption: {0}")]
    SigningKeySelection(KeyValidationError),

    #[error("Failed to encrypt session key with a public key: {0}")]
    PkeskEncryption(pgp::errors::Error),

    #[error("Missing algorithm in session key to encrypt key packet")]
    KeyPacketEncryptionNoAlgorithm,

    #[error("Failed to encrypt session key with a passphrase: {0}")]
    SkeskEncryption(pgp::errors::Error),

    #[error("Failed to encrypt or sign data: {0}")]
    DataEncryption(pgp::errors::Error),

    #[error("Failed to sign data before encryption: {0}")]
    Signing(#[from] SigningError),

    #[error("Not supported: {0}")]
    NotSupported(String),

    #[error("Missing encryption tools: no encryption keys or passphrases provided")]
    MissingEncryptionTools,

    #[error("Failed to armor detached signature: {0}")]
    DetachedSignature(#[from] ArmorError),

    #[error("Unexpected error: {0}")]
    Unexpected(Box<Error>),
}

#[derive(Debug, thiserror::Error)]
pub enum DecryptionError {
    #[error("Unexpected locked key")]
    LockedKey,

    #[error("No encrypted data found")]
    UnexpectedPlaintext,

    #[error("Message is not encrypted")]
    NoEncryption,

    #[error("Failed to process message before or after decryption: {0}")]
    MessageProcessing(#[from] MessageProcessingError),

    #[error("PKESK decryption for key {0} failed: {1}")]
    PkeskDecryption(Box<GenericKeyIdentifier>, ErrorList<DecryptionError>),

    #[error("SKESK decryption with a password failed: {0}")]
    SkeskDecryption(pgp::errors::Error),

    #[error(transparent)]
    SinglePkeskDecryption(#[from] PkeskDecryptionError),

    #[error("Invalid PKESK packet without a key-id or issuer")]
    PkeskNoIssuer,

    #[error("No matching key found for PKESK using key identifier {0}")]
    PkeskNoMatchingKey(Box<GenericKeyIdentifier>),

    #[error("Failed to decrypt any session key: {0}")]
    SessionKeyDecryption(ErrorList<DecryptionError>),

    #[error("Failed to decrypt with session key: {0}")]
    InvalidSessionKey(#[from] pgp::errors::Error),

    #[error("Failed to select verified decryption keys for id {0}: {1}")]
    KeySelection(Box<GenericKeyIdentifier>, KeyValidationError),

    #[error("No valid key packets found")]
    NoKeyPackets,

    #[error("Maximum number of S2K trials per passphrase exceeded")]
    MaxS2KTrialsPerPassphraseExceeded,

    #[error("No passphrase found to decrypt SKESK packet")]
    NoPassphraseForSkesk,

    #[error("Unexpected error: {0}")]
    Unexpected(Box<Error>),
}

#[derive(Debug, thiserror::Error)]
pub enum MessageVerificationError {
    #[error("Failed to verify message: {0}")]
    MessageProcessing(#[from] MessageProcessingError),
}

#[derive(Debug, thiserror::Error)]
pub enum MessageProcessingError {
    #[error("Failed to parse message: {0}")]
    MessageParsing(pgp::errors::Error),

    #[error("Cannot process message: the message is encrypted")]
    Encrypted,

    #[error("Failed to decompress message: {0}")]
    Decompression(pgp::errors::Error),

    #[error("Multiple compression layers found in message")]
    Compression,

    #[error(transparent)]
    TextSanitization(#[from] TextSanitizationError),

    #[error("Failed to read data: {0}")]
    Read(#[from] io::Error),

    #[error("Failed to unarmor data: {0}")]
    Unarmor(#[from] ArmorError),

    #[error("Message is not fully read for verification")]
    NotFullyRead,
}

#[derive(Debug, thiserror::Error)]
pub enum PkeskDecryptionError {
    #[error("Unexpected locked key")]
    LockedKey,

    #[error("PKESK decryption failed: {0}")]
    Pkesk(#[from] pgp::errors::Error),

    #[error("Invalid PKESK version: {0:?}")]
    InvalidPkesk(PkeskVersion),
}

#[derive(Debug, thiserror::Error)]
pub enum SigningError {
    #[error("Failed to set data mode: {0}")]
    DataMode(pgp::errors::Error),

    #[error("Failed to serialize signatures: {0}")]
    Serialize(pgp::errors::Error),

    #[error("Failed to select signing key: {0}")]
    KeySelection(#[from] KeyValidationError),

    #[error("Invalid signing key version")]
    InvalidKeyVersion,

    #[error("{0}")]
    HashAlgorithm(#[from] SignHashSelectionError),

    #[error("Failed to sign data: {0}")]
    Sign(pgp::errors::Error),

    #[error("Invalid input encoding for text signature: {0}")]
    InvalidInputData(#[from] std::str::Utf8Error),

    #[error("Invalid input encoding for text signature: {0}")]
    InvalidInputDataLineEnding(#[from] io::Error),

    #[error("Unexpected error: {0}")]
    Unexpected(Box<Error>),

    #[error("Cannot sign data: not all data has been read")]
    NotAllDataRead,
}

#[derive(Debug, thiserror::Error)]
pub enum SignHashSelectionError {
    #[error("Failed to load valid primary self-certification for hash selection: {0}")]
    PrimaryCertification(#[from] KeyCertificationSelectionError),

    #[error("Failed to select hash algorithm")]
    HashAlgorithm,
}

#[derive(Debug, thiserror::Error)]
pub enum FingerprintError {
    #[error("Failed to decode hex string: {0}")]
    HexDecode(#[from] hex::FromHexError),

    #[error("Invalid fingerprint length: {0}")]
    InvalidLength(usize),
}

#[derive(Debug, thiserror::Error)]
pub enum ArmorError {
    #[error("No armor header found")]
    DecodeHeader,

    #[error("Wrong header, got: {0} expected {1}")]
    DecodeWrongHeader(String, BlockType),

    #[error("Failed to decode armor due to io: {0}")]
    Decode(io::Error),

    #[error("Failed to armor: {0}")]
    Encode(pgp::errors::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum EncryptedMessageError {
    #[error("Failed to split encrypted message into key and data packets: {0}")]
    ParseSplit(pgp::errors::Error),

    #[error("Non expected packet type found while splitting encrypted OpenPGP message")]
    NonExpectedPacketSplit,

    #[error("Failed to armor encrypted OpenPGP message: {0}")]
    Armor(#[from] ArmorError),
}

#[derive(Debug, thiserror::Error)]
pub enum TextSanitizationError {
    #[error("Failed to normalize line endings or encode as utf-8: {0}")]
    Normalization(#[from] io::Error),
}

#[derive(Debug, thiserror::Error)]
pub enum VerificationResultUtilityError {
    #[error("Failed to serialize signature bytes: {0}")]
    SignatureBytes(#[from] pgp::errors::Error),
}

/// Errors raised while transforming a PKESK for a forwardee.
#[derive(Debug, thiserror::Error)]
pub enum ForwardingTransformError {
    #[error("Invalid PKESK version for forwarding: {0:?}")]
    VersionMismatch(PkeskVersion),

    #[error("Invalid PKESK algorithm for forwarding: {0:?}")]
    AlgorithmMismatch(PublicKeyAlgorithm),

    #[error("Failed to parse PKESK for forwarding: {0}")]
    Parsing(pgp::errors::Error),

    #[error("No PKESK packet found in the input")]
    NoPkeskFound,

    #[error("Multiple PKESK packets found in the input")]
    MultiplePkesks,

    #[error("No forwarding instance matches the PKESK recipient key id {0}")]
    NoMatchingInstance(KeyId),

    #[error("Failed to encode the forwarded PKESK: {0}")]
    Encode(pgp::errors::Error),

    #[error("Failed to transform PKESK with proxy parameter: {0}")]
    ProxyTransform(pgp::errors::Error),
}

/// Errors raised while constructing a forwarding instance.
#[derive(Debug, thiserror::Error)]
pub enum ForwardingInstanceError {
    #[error("Forwarding requires v4 keys, got a forwarder fingerprint of version {0:?}")]
    UnsupportedForwarderKeyVersion(Option<KeyVersion>),

    #[error("Forwarding requires v4 keys, got a forwardee fingerprint of version {0:?}")]
    UnsupportedForwardeeKeyVersion(Option<KeyVersion>),
}

/// Errors raised while generating a forwarding key.
#[derive(Debug, thiserror::Error)]
pub enum ForwardingKeyGenerationError {
    #[error("Invalid key version to create a forwarding key from, version {0}")]
    VersionMismatch(u8),

    #[error("Unable to get the time")]
    UnableToGetTime,

    #[error("No user id provided")]
    NoUserId,

    #[error("Failed to validate the forwarder key for forwarding: {0}")]
    KeyValidation(#[from] ForwardingKeyValidationError),

    #[error("Failed to find a valid encryption sub-key for forwarding: {0}")]
    SubKeyValidation(ErrorList<ForwardingKeyValidationError>),

    #[error("No subkeys found")]
    NoSubkeys,

    #[error("Failed to generate forwarding sub-key: {0}")]
    GenerationSubkey(KeyGenerationError),

    #[error("Failed to generate forwarding primary key: {0}")]
    GenerationPrimary(KeyGenerationError),

    #[error("Failed to generate forwarding proxy parameter: {0}")]
    ProxyParamGeneration(pgp::errors::Error),

    #[error("Failed to create user id: {0}")]
    UserId(#[from] UserIdError),

    #[error("Failed to create the forwarding instance: {0}")]
    Instance(#[from] ForwardingInstanceError),
}

/// Errors raised while checking that a key is eligible for forwarding.
#[derive(Debug, thiserror::Error)]
pub enum ForwardingKeyValidationError {
    #[error("Failed to validate key for forwarding: {0}")]
    KeyValidation(#[from] KeyValidationError),

    #[error("Algorithm invalid for forwarding: {0:?}")]
    SubKeyNoMatchingAlgorithm(PublicKeyAlgorithm),

    #[error("Forwarding requires v4 sub-keys, got a fingerprint of version {0:?}")]
    SubKeyUnsupportedKeyVersion(Option<KeyVersion>),
}

#[derive(Debug)]
pub struct ErrorList<E: std::error::Error>(pub Vec<E>);

impl<E: std::error::Error> std::fmt::Display for ErrorList<E> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let mut errors = self.0.iter();

        write!(f, "[")?;
        if let Some(first) = errors.next() {
            write!(f, "{first}")?;
            for err in errors {
                write!(f, ", {err}")?;
            }
        }
        write!(f, "]")
    }
}

impl<E: std::error::Error> std::error::Error for ErrorList<E> {}

impl<E: std::error::Error> From<Vec<E>> for ErrorList<E> {
    fn from(errors: Vec<E>) -> Self {
        Self(errors)
    }
}

impl From<Error> for EncryptionError {
    fn from(value: Error) -> Self {
        match value {
            Error::Encryption(err) => err,
            _ => EncryptionError::Unexpected(Box::new(value)),
        }
    }
}

impl From<Error> for DecryptionError {
    fn from(value: Error) -> Self {
        match value {
            Error::Decryption(err) => err,
            _ => DecryptionError::Unexpected(Box::new(value)),
        }
    }
}

impl From<Error> for SigningError {
    fn from(value: Error) -> Self {
        match value {
            Error::Signing(err) => err,
            _ => SigningError::Unexpected(Box::new(value)),
        }
    }
}
