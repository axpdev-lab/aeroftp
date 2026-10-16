use std::io::Read;

use pgp::{
    composed::{Message, PlainSessionKey},
    crypto::{ecc_curve::ECCCurve, hash::HashAlgorithm, public_key::PublicKeyAlgorithm},
    packet::{self, PublicKeyEncryptedSessionKey, Signature, SignatureHasher},
    ser::Serialize,
    types::{
        DecryptionKey, EncryptionKey, Fingerprint, KeyDetails, KeyId, KeyVersion, Password,
        PkeskVersion, PublicParams, SecretParams, SigningKey, Timestamp, VerifyingKey,
    },
};

use crate::{
    core, signature::check_message_signature_details, CheckUnixTime, MessageSignatureError,
    PkeskDecryptionError, Profile, SignatureContext, SignatureError, SignatureMode, SigningError,
    UnixTime, VerificationContext,
};

/// Represents a view on a selected public component key in an `OpenPGP` key.
///
/// Since an `OpenPGP` key can contain multiple actual keys, an operation must
/// select one. A public component key represents such a selected key.
#[derive(Debug)]
pub struct PublicComponentKey<'a> {
    /// The public key part of the component key (either a primary or subkey).
    pub public_key: AnyPublicKey<'a>,

    /// The primary self-certification of the component key.
    pub primary_self_certification: &'a Signature,

    /// The self-certification of the component key.
    ///
    /// If the component key is a primary key, it points to the same signature
    /// as `primary_self_certification`
    pub self_certification: &'a Signature,
}

impl<'a> PublicComponentKey<'a> {
    pub fn new(
        public_key: AnyPublicKey<'a>,
        primary_self_certification: &'a Signature,
        self_certification: &'a Signature,
    ) -> Self {
        Self {
            public_key,
            primary_self_certification,
            self_certification,
        }
    }

    /// Verify a message signature using the public component key.
    pub fn verify_message_signature_with_data<R: Read>(
        &self,
        date: CheckUnixTime,
        signature: &Signature,
        data_to_verify: R,
        context: Option<&VerificationContext>,
        profile: &Profile,
    ) -> Result<(), MessageSignatureError> {
        signature
            .verify(&self.public_key, data_to_verify)
            .map_err(|err| MessageSignatureError::Failed(SignatureError::Verification(err)))?;
        check_message_signature_details(date, signature, self, context, profile)
    }

    pub fn verify_message_signature_with_message(
        &self,
        date: CheckUnixTime,
        message: &Message<'_>,
        signature_index: usize,
        context: Option<&VerificationContext>,
        profile: &Profile,
    ) -> Result<(), MessageSignatureError> {
        let signature = message
            .verify_nested_explicit(signature_index, &self.public_key)
            .map_err(|err| MessageSignatureError::Failed(SignatureError::Verification(err)))?;
        check_message_signature_details(date, signature, self, context, profile)
    }

    #[allow(clippy::indexing_slicing)]
    pub fn verify_signature_with_hash(
        &self,
        date: CheckUnixTime,
        signature: &Signature,
        hash: &[u8],
        context: Option<&VerificationContext>,
        profile: &Profile,
    ) -> Result<(), MessageSignatureError> {
        let Some(config) = signature.config() else {
            return Err(MessageSignatureError::Failed(SignatureError::ConfigAccess));
        };
        let Some(signed_hash_value) = signature.signed_hash_value() else {
            return Err(MessageSignatureError::Failed(SignatureError::ConfigAccess));
        };
        let Some(signature_bytes) = signature.signature() else {
            return Err(MessageSignatureError::Failed(SignatureError::ConfigAccess));
        };
        if hash.len() < 2 {
            return Err(MessageSignatureError::Failed(SignatureError::NoHash));
        }

        if signed_hash_value[0] != hash[0] || signed_hash_value[1] != hash[1] {
            return Err(MessageSignatureError::Failed(SignatureError::Verification(
                pgp::errors::Error::Message {
                    message: format!(
                        "assertion failed: `(left == right)`\\n  left: `{:?}`,\\n right: `{:?}`: signature: invalid signed hash value",
                        &signed_hash_value[..2],
                        &hash[..2]
                    ),
                    backtrace: None,
                },
            )));
        }
        self.public_key
            .verify(config.hash_alg, hash, signature_bytes)
            .map_err(|err| MessageSignatureError::Failed(SignatureError::Verification(err)))?;
        check_message_signature_details(date, signature, self, context, profile)
    }

    /// Get the unix creation time of the public component key.
    pub fn unix_created_at(&self) -> UnixTime {
        self.public_key.created_at().into()
    }
}

/// Represents a view on a selected secret component key in an `OpenPGP` key.
///
/// Since an `OpenPGP` key can contain multiple actual keys, an operation must
/// select one. A secret component key represents such a selected key.
#[derive(Debug, Clone)]
pub struct PrivateComponentKey<'a> {
    /// The secret key part of the component key (either a primary or subkey).
    ///
    /// We use a custom enum type because the secret key trait [`SecretKeyTrait`]
    /// does not include any decryption methods.
    pub private_key: AnySecretKey<'a>,

    /// The primary self-certification of the component key.
    pub primary_self_certification: &'a Signature,

    /// The self-certification of the component key.
    ///
    pub self_certification: &'a Signature,
}

impl<'a> PrivateComponentKey<'a> {
    pub(crate) fn new(
        private_key: AnySecretKey<'a>,
        primary_self_certification: &'a Signature,
        self_certification: &'a Signature,
    ) -> Self {
        Self {
            private_key,
            primary_self_certification,
            self_certification,
        }
    }

    pub(crate) fn sign_data(
        &self,
        data: &[u8],
        at_date: UnixTime,
        signature_mode: SignatureMode,
        hash_algorithm: HashAlgorithm,
        signature_context: Option<&SignatureContext>,
        profile: &Profile,
    ) -> Result<Signature, SigningError> {
        let config = core::configure_message_signature(
            &self.private_key,
            at_date,
            signature_mode,
            hash_algorithm,
            signature_context,
            profile.rng(),
        )?;

        config
            .sign(&self.private_key, &Password::default(), data.as_ref())
            .map_err(SigningError::Sign)
    }

    pub fn is_subkey(&self) -> bool {
        matches!(self.private_key, AnySecretKey::SecretSubKey(_))
    }

    pub fn is_forwarding_key(&self) -> bool {
        let has_forward_flag = self
            .self_certification
            .key_flags()
            .draft_decrypt_forwarded();
        let key_check = self.private_key.version() == KeyVersion::V4 && self.is_subkey();
        let curve_check = matches!(
            self.private_key.public_params(),
            PublicParams::ECDH(params) if params.curve() == ECCCurve::Curve25519Legacy
        );
        key_check && curve_check && has_forward_flag
    }

    pub(crate) fn sign_for_reader(
        &self,
        at_date: UnixTime,
        signature_mode: SignatureMode,
        hash_algorithm: HashAlgorithm,
        signature_context: Option<&SignatureContext>,
        profile: &Profile,
    ) -> Result<SignatureHasher, SigningError> {
        core::configure_message_signature(
            &self.private_key,
            at_date,
            signature_mode,
            hash_algorithm,
            signature_context,
            profile.rng(),
        )?
        .into_hasher()
        .map_err(SigningError::Sign)
    }

    /// Get a public view on the private component key.
    pub fn public_view(&self) -> PrivateComponentKeyPublicView<'a> {
        PrivateComponentKeyPublicView {
            key_details: Box::new(self.private_key.clone()),
            primary_self_certification: self.primary_self_certification,
            self_certification: self.self_certification,
        }
    }
}

/// Represents a public view on a selected private component key in an `OpenPGP` key.
pub struct PrivateComponentKeyPublicView<'a> {
    /// The private key part of the component key (either a primary or subkey).
    pub key_details: Box<dyn KeyDetails + 'a>,

    /// The primary self-certification of the component key.
    pub primary_self_certification: &'a Signature,

    /// The self-certification of the component key.
    pub self_certification: &'a Signature,
}

/// The [`SecretKeyTrait`] does not expose decryption methods. Thus, we
/// need an explicit enum type covering all secret key types.
/// [`AnySecretKey`] either represents a secret primary or secret subkey.
#[derive(Debug, Clone)]
pub enum AnySecretKey<'a> {
    /// A secret primary key.
    PrimarySecretKey(&'a packet::SecretKey),

    /// A secret subkey.
    SecretSubKey(&'a packet::SecretSubkey),
}

impl KeyDetails for AnySecretKey<'_> {
    fn version(&self) -> KeyVersion {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.version(),
            AnySecretKey::SecretSubKey(key) => key.version(),
        }
    }

    fn fingerprint(&self) -> Fingerprint {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.fingerprint(),
            AnySecretKey::SecretSubKey(key) => key.fingerprint(),
        }
    }

    fn legacy_key_id(&self) -> KeyId {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.legacy_key_id(),
            AnySecretKey::SecretSubKey(key) => key.legacy_key_id(),
        }
    }

    fn algorithm(&self) -> PublicKeyAlgorithm {
        match &self {
            AnySecretKey::PrimarySecretKey(key) => key.algorithm(),
            AnySecretKey::SecretSubKey(key) => key.algorithm(),
        }
    }

    fn created_at(&self) -> Timestamp {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.created_at(),
            AnySecretKey::SecretSubKey(key) => key.created_at(),
        }
    }

    fn legacy_v3_expiration_days(&self) -> Option<u16> {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.legacy_v3_expiration_days(),
            AnySecretKey::SecretSubKey(key) => key.legacy_v3_expiration_days(),
        }
    }

    fn public_params(&self) -> &PublicParams {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.public_params(),
            AnySecretKey::SecretSubKey(key) => key.public_params(),
        }
    }
}

impl SigningKey for AnySecretKey<'_> {
    fn sign(
        &self,
        key_pw: &Password,
        hash: HashAlgorithm,
        data: &[u8],
    ) -> pgp::errors::Result<pgp::types::SignatureBytes> {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.sign(key_pw, hash, data),
            AnySecretKey::SecretSubKey(key) => {
                <packet::SecretSubkey as SigningKey>::sign(key, key_pw, hash, data)
            }
        }
    }

    fn hash_alg(&self) -> HashAlgorithm {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.hash_alg(),
            AnySecretKey::SecretSubKey(key) => key.hash_alg(),
        }
    }
}

impl DecryptionKey for AnySecretKey<'_> {
    fn decrypt(
        &self,
        key_pw: &Password,
        values: &pgp::types::PkeskBytes,
        typ: pgp::types::EskType,
    ) -> pgp::errors::Result<pgp::errors::Result<PlainSessionKey>> {
        match self {
            AnySecretKey::PrimarySecretKey(key) => key.decrypt(key_pw, values, typ),
            AnySecretKey::SecretSubKey(key) => key.decrypt(key_pw, values, typ),
        }
    }
}

#[allow(clippy::match_wildcard_for_single_variants)]
impl AnySecretKey<'_> {
    pub(crate) fn decrypt_session_key(
        &self,
        pkesk: &PublicKeyEncryptedSessionKey,
    ) -> Result<PlainSessionKey, PkeskDecryptionError> {
        let esk_type = match pkesk.version() {
            PkeskVersion::V3 => pgp::types::EskType::V3_4,
            PkeskVersion::V6 => pgp::types::EskType::V6,
            v => return Err(PkeskDecryptionError::InvalidPkesk(v)),
        };
        match self {
            AnySecretKey::PrimarySecretKey(secret_key) => match secret_key.secret_params() {
                SecretParams::Plain(plain_secret_params) => {
                    let public_key = secret_key.public_key();
                    plain_secret_params
                        .decrypt(
                            public_key.public_params(),
                            pkesk.values()?,
                            esk_type,
                            public_key,
                        )
                        .map_err(PkeskDecryptionError::Pkesk)
                }
                SecretParams::Encrypted(_) => Err(PkeskDecryptionError::LockedKey),
            },
            AnySecretKey::SecretSubKey(secret_subkey) => match secret_subkey.secret_params() {
                SecretParams::Plain(plain_secret_params) => {
                    let public_key = secret_subkey.public_key();
                    plain_secret_params
                        .decrypt(
                            public_key.public_params(),
                            pkesk.values()?,
                            esk_type,
                            public_key,
                        )
                        .map_err(PkeskDecryptionError::Pkesk)
                }
                SecretParams::Encrypted(_) => Err(PkeskDecryptionError::LockedKey),
            },
        }
    }
}

/// [`AnyPublicKey`] either represents a public primary or public subkey.
///
/// The [`Signature::verify`] method does not allow to pass dyn reference to a public key implementing [`VerifyingKey`].
/// Thus, we need an explicit enum type covering all public key types.
#[derive(Debug, Clone)]
pub enum AnyPublicKey<'a> {
    /// A secret primary key.
    PrimaryPublicKey(&'a packet::PublicKey),

    /// A secret subkey.
    PublicSubKey(&'a packet::PublicSubkey),
}

impl KeyDetails for AnyPublicKey<'_> {
    fn version(&self) -> KeyVersion {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.version(),
            AnyPublicKey::PublicSubKey(key) => key.version(),
        }
    }

    fn fingerprint(&self) -> Fingerprint {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.fingerprint(),
            AnyPublicKey::PublicSubKey(key) => key.fingerprint(),
        }
    }

    fn legacy_key_id(&self) -> KeyId {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.legacy_key_id(),
            AnyPublicKey::PublicSubKey(key) => key.legacy_key_id(),
        }
    }

    fn algorithm(&self) -> PublicKeyAlgorithm {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.algorithm(),
            AnyPublicKey::PublicSubKey(key) => key.algorithm(),
        }
    }

    fn created_at(&self) -> Timestamp {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.created_at(),
            AnyPublicKey::PublicSubKey(key) => key.created_at(),
        }
    }

    fn legacy_v3_expiration_days(&self) -> Option<u16> {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.legacy_v3_expiration_days(),
            AnyPublicKey::PublicSubKey(key) => key.legacy_v3_expiration_days(),
        }
    }

    fn public_params(&self) -> &PublicParams {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.public_params(),
            AnyPublicKey::PublicSubKey(key) => key.public_params(),
        }
    }
}

impl VerifyingKey for AnyPublicKey<'_> {
    fn verify(
        &self,
        hash: HashAlgorithm,
        data: &[u8],
        signature: &pgp::types::SignatureBytes,
    ) -> pgp::errors::Result<()> {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.verify(hash, data, signature),
            AnyPublicKey::PublicSubKey(key) => key.verify(hash, data, signature),
        }
    }
}

impl EncryptionKey for AnyPublicKey<'_> {
    fn encrypt<R: rand::CryptoRng + rand::Rng>(
        &self,
        rng: R,
        plain: &[u8],
        typ: pgp::types::EskType,
    ) -> pgp::errors::Result<pgp::types::PkeskBytes> {
        match self {
            AnyPublicKey::PrimaryPublicKey(key) => key.encrypt(rng, plain, typ),
            AnyPublicKey::PublicSubKey(key) => key.encrypt(rng, plain, typ),
        }
    }
}

impl Serialize for AnyPublicKey<'_> {
    fn to_writer<W: std::io::Write>(&self, w: &mut W) -> pgp::errors::Result<()> {
        match self {
            AnyPublicKey::PrimaryPublicKey(public_key) => public_key.to_writer(w),
            AnyPublicKey::PublicSubKey(public_subkey) => public_subkey.to_writer(w),
        }
    }

    fn write_len(&self) -> usize {
        match self {
            AnyPublicKey::PrimaryPublicKey(public_key) => public_key.write_len(),
            AnyPublicKey::PublicSubKey(public_subkey) => public_subkey.write_len(),
        }
    }
}
