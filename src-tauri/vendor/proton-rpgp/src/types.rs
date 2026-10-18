use std::{
    borrow::Cow,
    fmt::{self, Display},
    io,
    ops::Deref,
};

use pgp::{
    crypto::{aead::AeadAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::KeyFlags,
    types::{Fingerprint, KeyId, Password, Timestamp},
};

use crate::{armor, Ciphersuite, FingerprintError};

/// Possible encodings of an `OpenPGP` message.
///
/// The data is either armored i.e., base64 encoded with a header
/// -----BEGIN PGP ... -----
/// ...
/// -----BEGIN PGP ... -----
/// or encoded as raw bytes.
/// Auto is used to indicate that encoding is unknown and the function
/// should detect the encoding automatically.
#[derive(Default, PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum DataEncoding {
    /// The data is armored.
    #[default]
    Armored,
    /// The data is encoded as raw bytes.
    Unarmored,
    /// Try to detect the encoding automatically.
    ///
    /// On read:
    /// - Tries to detect the encoding (armored or unarmored) automatically.
    ///
    /// On write:
    /// - Auto will be resolved to [`DataEncoding::default`].
    Auto,
}

impl DataEncoding {
    pub fn is_armor(&self) -> bool {
        *self == DataEncoding::Armored
    }

    pub(crate) fn resolve_for_read(self, data: &[u8]) -> ResolvedDataEncoding {
        match self {
            DataEncoding::Armored => ResolvedDataEncoding::Armored,
            DataEncoding::Unarmored => ResolvedDataEncoding::Unarmored,
            DataEncoding::Auto => armor::detect_encoding(data),
        }
    }

    pub(crate) fn resolve_for_read_stream(
        self,
        data: &mut impl io::BufRead,
    ) -> ResolvedDataEncoding {
        match self {
            DataEncoding::Armored => ResolvedDataEncoding::Armored,
            DataEncoding::Unarmored => ResolvedDataEncoding::Unarmored,
            DataEncoding::Auto => {
                armor::detect_encoding_reader(data).unwrap_or(ResolvedDataEncoding::Unarmored)
            }
        }
    }

    pub(crate) fn resolve_for_write(self) -> ResolvedDataEncoding {
        match self {
            DataEncoding::Armored => ResolvedDataEncoding::Armored,
            DataEncoding::Unarmored => ResolvedDataEncoding::Unarmored,
            DataEncoding::Auto => match DataEncoding::default() {
                DataEncoding::Armored => ResolvedDataEncoding::Armored,
                _ => ResolvedDataEncoding::Unarmored,
            },
        }
    }
}

#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug)]
pub enum ResolvedDataEncoding {
    /// The data is armored.
    Armored,
    /// The data is encoded as raw bytes.
    Unarmored,
}

impl From<ResolvedDataEncoding> for DataEncoding {
    fn from(value: ResolvedDataEncoding) -> Self {
        match value {
            ResolvedDataEncoding::Armored => DataEncoding::Armored,
            ResolvedDataEncoding::Unarmored => DataEncoding::Unarmored,
        }
    }
}

/// `UnixTimestamp` represents a unix timestamp within `OpenPGP`.
#[derive(Ord, PartialOrd, PartialEq, Eq, Hash, Clone, Copy, Debug, Default)]
pub struct UnixTime(u64);

impl UnixTime {
    /// Creates a new unix timestamp.
    pub fn new(unix_time: u64) -> Self {
        Self(unix_time)
    }

    pub fn now() -> Option<Self> {
        use web_time::{SystemTime, UNIX_EPOCH};
        match SystemTime::now().duration_since(UNIX_EPOCH) {
            Ok(n) => Some(UnixTime::new(n.as_secs())),
            Err(_) => None,
        }
    }

    pub fn now_unchecked() -> Self {
        Self::now().unwrap_or_default()
    }

    /// Creates a unix timestamp with the zero value.
    ///
    /// If a zero value is supplied to the API, expiration checks are skipped.
    pub fn zero() -> Self {
        Self(0)
    }

    /// Indicates if the timestamp is zero.
    pub fn is_zero(&self) -> bool {
        self.0 == 0
    }

    /// Indicates if time check should be disabled.
    pub fn checks_disabled(&self) -> bool {
        self.is_zero()
    }

    /// Returns the Unix timestamp as a u64.
    pub fn unix_seconds(&self) -> u64 {
        self.0
    }
}

impl Display for UnixTime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Timestamp> for UnixTime {
    fn from(value: Timestamp) -> Self {
        UnixTime::new(u64::from(value.as_secs()))
    }
}

impl From<UnixTime> for Timestamp {
    fn from(value: UnixTime) -> Self {
        // Ok to cast to u32 without checks.
        #[allow(clippy::cast_possible_truncation)]
        Timestamp::from_secs(value.0 as u32)
    }
}

/// `OpenPGP` liftime specification in seconds.
#[derive(Debug, Copy, Clone, PartialEq, Eq, PartialOrd, Default)]
pub struct Lifetime(pgp::types::Duration);

impl From<Lifetime> for std::time::Duration {
    fn from(value: Lifetime) -> Self {
        value.0.into()
    }
}

impl From<Lifetime> for pgp::types::Duration {
    fn from(value: Lifetime) -> Self {
        value.0
    }
}

impl Lifetime {
    /// Returns the number of seconds.
    pub fn as_secs(self) -> u32 {
        self.0.as_secs()
    }

    /// Creates a new [`Lifetime`] from seconds.
    pub fn from_secs(secs: u32) -> Self {
        Self(pgp::types::Duration::from_secs(secs))
    }
}

/// An optional Unix timestamp used for validating time against in `OpenPGP` operations.
///
/// If unset, time-based checks are disabled.
/// When constructing new `OpenPGP` packets with disabled checks, the current system time is used by default.
#[derive(PartialEq, Eq, Hash, Clone, Copy, Debug, Default)]
pub struct CheckUnixTime(Option<UnixTime>);

impl CheckUnixTime {
    pub fn new(unix_time: u64) -> Self {
        Self::enable(UnixTime::new(unix_time))
    }

    /// Enables time-based checks using the provided Unix timestamp.
    pub fn enable(unix_time: UnixTime) -> Self {
        Self(Some(unix_time))
    }

    /// Enables time-based checks using the current system time.
    pub fn enable_now() -> Self {
        Self(Some(UnixTime::now().unwrap_or_default()))
    }

    /// Disables time-based checks.
    pub fn disable() -> Self {
        Self(None)
    }

    pub fn is_enabled(&self) -> bool {
        self.0.is_some()
    }

    pub fn is_disabled(&self) -> bool {
        self.0.is_none()
    }

    pub fn at(&self) -> Option<UnixTime> {
        self.0
    }
}

impl From<UnixTime> for CheckUnixTime {
    fn from(value: UnixTime) -> Self {
        Self(Some(value))
    }
}

/// A sha256 fingerprint.
#[derive(Debug, PartialEq, Eq, Hash, Clone, Copy)]
pub struct FingerprintSha256(pub(crate) [u8; 32]);

impl FingerprintSha256 {
    pub fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub fn from_hex(hex: &str) -> Result<Self, FingerprintError> {
        let bytes = hex::decode(hex)?;
        let len = bytes.len();
        let raw_fp: [u8; 32] = bytes
            .try_into()
            .map_err(|_| FingerprintError::InvalidLength(len))?;
        Ok(Self(raw_fp))
    }

    pub fn to_hex(&self) -> String {
        self.to_string()
    }
}

impl AsRef<[u8]> for FingerprintSha256 {
    fn as_ref(&self) -> &[u8] {
        &self.0
    }
}

impl From<[u8; 32]> for FingerprintSha256 {
    fn from(value: [u8; 32]) -> Self {
        Self::new(value)
    }
}

impl Display for FingerprintSha256 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", hex::encode(self.0))
    }
}

#[derive(Default, Debug, PartialEq, Clone)]
pub struct GenericKeyIdentifierList(pub(crate) Vec<GenericKeyIdentifier>);

impl From<Vec<GenericKeyIdentifier>> for GenericKeyIdentifierList {
    fn from(value: Vec<GenericKeyIdentifier>) -> Self {
        Self(value)
    }
}

impl From<GenericKeyIdentifier> for GenericKeyIdentifierList {
    fn from(value: GenericKeyIdentifier) -> Self {
        Self(vec![value])
    }
}

impl Display for GenericKeyIdentifierList {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut key_ids = self.0.iter();

        write!(f, "[")?;
        if let Some(first) = key_ids.next() {
            write!(f, "{first}")?;
            for err in key_ids {
                write!(f, ", {err}")?;
            }
        }
        write!(f, "]")
    }
}

/// A generic key identifier, which can be a key id, a fingerprint, or both.
///
/// Can also encode a wildcard that matches any key id or fingerprint.
#[derive(Debug, Clone)]
pub enum GenericKeyIdentifier {
    /// A key id.   
    KeyId(KeyId),
    /// A fingerprint.
    Fingerprint(Fingerprint),
    /// A key id and a fingerprint.
    Both(KeyId, Fingerprint),
    /// A wildcard that matches any key id or fingerprint.
    Wildcard,
}

impl GenericKeyIdentifier {
    pub fn key_id(&self) -> Option<KeyId> {
        match self {
            Self::KeyId(key_id) | Self::Both(key_id, _) => Some(*key_id),
            Self::Fingerprint(fingerprint) => fingerprint.key_id(),
            Self::Wildcard => Some(KeyId::new([0_u8; 8])),
        }
    }

    pub fn is_wildcard(&self) -> bool {
        matches!(&self, GenericKeyIdentifier::Wildcard)
    }
}

impl PartialEq for GenericKeyIdentifier {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Fingerprint(l0), Self::Fingerprint(r0) | Self::Both(_, r0))
            | (Self::Both(_, l0), Self::Fingerprint(r0)) => l0 == r0,
            (Self::Both(l0, l1), Self::Both(r0, r1)) => l0 == r0 && l1 == r1,
            (Self::Both(l0, _), Self::KeyId(r0))
            | (Self::KeyId(l0), Self::KeyId(r0) | Self::Both(r0, _)) => l0 == r0,
            (Self::Wildcard, _) | (_, Self::Wildcard) => true,
            _ => false,
        }
    }
}

impl Display for GenericKeyIdentifier {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::KeyId(key_id) => write!(f, "{key_id}"),
            Self::Fingerprint(fingerprint) => write!(f, "{fingerprint}"),
            Self::Both(key_id, fingerprint) => write!(f, "{key_id} ({fingerprint})"),
            Self::Wildcard => write!(f, "Wildcard"),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone)]
pub struct PrettyKeyFlags(pub KeyFlags);

impl From<KeyFlags> for PrettyKeyFlags {
    fn from(value: KeyFlags) -> Self {
        Self(value)
    }
}

impl Display for PrettyKeyFlags {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Flags set:")?;

        if self.0.authentication() {
            write!(f, " authentication")?;
        }
        if self.0.sign() {
            write!(f, " sign")?;
        }
        if self.0.certify() {
            write!(f, " certify")?;
        }
        if self.0.encrypt_comms() {
            write!(f, " encrypt-communications")?;
        }
        if self.0.encrypt_storage() {
            write!(f, " encrypt-storage")?;
        }
        Ok(())
    }
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignatureMode {
    #[default]
    Binary,

    Text,
}

impl From<SignatureMode> for pgp::packet::SignatureType {
    fn from(value: SignatureMode) -> Self {
        match value {
            SignatureMode::Binary => pgp::packet::SignatureType::Binary,
            SignatureMode::Text => pgp::packet::SignatureType::Text,
        }
    }
}

#[derive(Debug, Default)]
pub(crate) struct CloneablePasswords(pub(crate) Vec<Password>);

impl Clone for CloneablePasswords {
    fn clone(&self) -> Self {
        let passwords: Vec<_> = self
            .0
            .iter()
            .map(|p| Password::from(p.read().as_slice()))
            .collect();
        Self(passwords)
    }
}

impl Deref for CloneablePasswords {
    type Target = Vec<Password>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl From<Vec<Password>> for CloneablePasswords {
    fn from(value: Vec<Password>) -> Self {
        Self(value)
    }
}

/// A detached signature to verify over the decrypted data.
#[derive(Debug, Clone)]
pub enum ExternalDetachedSignature<'a> {
    Unencrypted(Cow<'a, [u8]>, ResolvedDataEncoding),
    Encrypted(Cow<'a, [u8]>, ResolvedDataEncoding),
}

impl<'a> ExternalDetachedSignature<'a> {
    /// Creates a new unencrypted detached signature.
    pub fn new_unencrypted(
        detached_signature: impl Into<Cow<'a, [u8]>>,
        signature_data_encoding: DataEncoding,
    ) -> Self {
        let detached_signature = detached_signature.into();
        let resolved_data_encoding =
            signature_data_encoding.resolve_for_read(detached_signature.as_ref());
        Self::Unencrypted(detached_signature, resolved_data_encoding)
    }

    /// Creates a new encrypted detached signature.
    ///
    /// The signature is encrypted alongside its message.
    pub fn new_encrypted(
        detached_signature: impl Into<Cow<'a, [u8]>>,
        signature_data_encoding: DataEncoding,
    ) -> Self {
        let detached_signature = detached_signature.into();
        let resolved_data_encoding =
            signature_data_encoding.resolve_for_read(detached_signature.as_ref());
        Self::Encrypted(detached_signature, resolved_data_encoding)
    }

    pub fn armored(&self) -> crate::Result<Vec<u8>> {
        match self {
            Self::Unencrypted(signature, signature_data_encoding) => {
                match signature_data_encoding {
                    ResolvedDataEncoding::Armored => Ok(signature.to_vec()),
                    ResolvedDataEncoding::Unarmored => armor::armor_signature(signature),
                }
            }
            Self::Encrypted(signature, signature_data_encoding) => match signature_data_encoding {
                ResolvedDataEncoding::Armored => Ok(signature.to_vec()),
                ResolvedDataEncoding::Unarmored => armor::armor_message(signature),
            },
        }
    }

    pub fn unarmored(&self) -> crate::Result<Vec<u8>> {
        match self {
            Self::Unencrypted(signature, signature_data_encoding)
            | Self::Encrypted(signature, signature_data_encoding) => {
                match signature_data_encoding {
                    ResolvedDataEncoding::Armored => armor::unarmor(signature),
                    ResolvedDataEncoding::Unarmored => Ok(signature.to_vec()),
                }
            }
        }
    }
}

/// Fingerprint extension to extract the key id from a fingerprint.
pub trait FingerprintExt {
    /// Returns the key id of the fingerprint if extractable.
    fn key_id(&self) -> Option<KeyId>;
}

impl FingerprintExt for Fingerprint {
    fn key_id(&self) -> Option<KeyId> {
        match self {
            Fingerprint::V4(fp) => {
                // last 64 bits of fingerprint
                let key_id_bytes: Option<[u8; 8]> = fp[12..].try_into().ok();
                key_id_bytes.map(KeyId::new)
            }
            Fingerprint::V5(fp) | Fingerprint::V6(fp) => {
                // first 64 bits of fingerprint
                let key_id_bytes: Option<[u8; 8]> = fp[..8].try_into().ok();
                key_id_bytes.map(KeyId::new)
            }
            Fingerprint::V2(_) | Fingerprint::V3(_) | Fingerprint::Unknown(_) => None,
        }
    }
}

/// Defines an `OpenPGP` AEAD cipher suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AeadCiphersuite(pub Ciphersuite);

impl AeadCiphersuite {
    pub fn new(
        symmetric_key_algorithm: SymmetricKeyAlgorithm,
        aead_algorithm: AeadAlgorithm,
    ) -> Self {
        Self((symmetric_key_algorithm, aead_algorithm))
    }
}

impl From<Ciphersuite> for AeadCiphersuite {
    fn from(value: Ciphersuite) -> Self {
        Self(value)
    }
}

impl From<AeadCiphersuite> for Ciphersuite {
    fn from(value: AeadCiphersuite) -> Self {
        value.0
    }
}

impl Default for AeadCiphersuite {
    fn default() -> Self {
        Self((SymmetricKeyAlgorithm::AES256, AeadAlgorithm::Gcm))
    }
}
