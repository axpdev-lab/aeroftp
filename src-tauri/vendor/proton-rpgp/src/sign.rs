use std::{
    borrow::Cow,
    io::{Read, Write},
};

use pgp::{
    armor::{self, BlockType},
    composed::{
        ArmorOptions, CleartextSignedMessage, DetachedSignature, Encryption, MessageBuilder,
        SubpacketConfig,
    },
    line_writer::LineBreak,
    normalize_lines::NormalizedReader,
    packet::{DataMode, SignatureVersion},
    ser::Serialize,
    types::{CompressionAlgorithm, KeyDetails, KeyVersion, Password},
};
use rand::{CryptoRng, Rng};

use crate::{
    core::message_signature_subpackets,
    preferences::{self, RecipientsAlgorithms},
    CheckUnixTime, DataEncoding, KeyValidationError, PrivateComponentKey, PrivateKey,
    PrivateKeySelectionExt, Profile, ResolvedDataEncoding, SignatureContext, SignatureMode,
    SignatureUsage, SigningError, UnixTime, Utf8CheckReader, DEFAULT_PROFILE,
};

mod reader;
pub use reader::*;

/// A signer that can create `OpenPGP` signatures over data.
#[derive(Debug, Clone)]
pub struct Signer<'a> {
    /// The profile to use for the signer.
    pub(crate) profile: Profile,

    /// The signing keys to create signatures with.
    pub(crate) signing_keys: Vec<&'a PrivateKey>,

    /// The date to use for the signatures.
    pub(crate) date: CheckUnixTime,

    /// Message compression preference.
    pub(crate) inline_message_compression: CompressionAlgorithm,

    /// The signature type to use for the signatures.
    ///
    /// Either binary or text.
    pub(crate) signature_type: SignatureMode,

    /// The literal packet type to use for the data.
    pub(crate) data_mode: DataMode,

    /// The signature context to use for the message signatures.
    pub(crate) signature_context: Option<Cow<'a, SignatureContext>>,
}

impl<'a> Signer<'a> {
    /// Create a new verifier with the given profile.
    pub fn new(profile: Profile) -> Self {
        let message_compression = profile.message_compression();
        Self {
            profile,
            signing_keys: Vec::new(),
            date: CheckUnixTime::enable_now(),
            signature_type: SignatureMode::default(),
            signature_context: None,
            data_mode: DataMode::Binary,
            inline_message_compression: message_compression,
        }
    }

    /// Adds a signing key to the signer.
    pub fn with_signing_key(mut self, key: &'a PrivateKey) -> Self {
        self.signing_keys.push(key);
        self
    }

    /// Adds multiple signing keys to the signer.
    ///
    /// For each key, a signature will be created.
    pub fn with_signing_keys(mut self, keys: impl IntoIterator<Item = &'a PrivateKey>) -> Self {
        self.signing_keys.extend(keys);
        self
    }

    /// Sets the date to use for the signatures.
    pub fn at_date(mut self, date: CheckUnixTime) -> Self {
        self.date = date;
        self
    }

    /// Sets the application signature context to use for the message signatures.
    pub fn with_signature_context(mut self, context: impl Into<Cow<'a, SignatureContext>>) -> Self {
        self.signature_context = Some(context.into());
        self
    }

    /// Enables compression for inline signing.
    ///
    /// The default is determined by the profile (most likely uncompressed).
    pub fn compress(mut self) -> Self {
        self.inline_message_compression = CompressionAlgorithm::ZLIB;
        self
    }

    /// Sets the signature type to text.
    ///
    /// If this is set, `OpenPGP` will canonicalize the line endings in the signature data before signing.
    pub fn as_utf8(mut self) -> Self {
        self.signature_type = SignatureMode::Text;
        self.data_mode = DataMode::Utf8;
        self
    }

    /// Creates an inline-signed `OpenPGP` message.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Signer, DataEncoding, UnixTime, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let message = Signer::default()
    ///     .with_signing_key(&key)
    ///     .sign("hello world", DataEncoding::Armored)
    ///     .expect("Failed to sign");
    /// ```
    pub fn sign(
        self,
        data: impl AsRef<[u8]>,
        message_encoding: DataEncoding,
    ) -> crate::Result<Vec<u8>> {
        let mut utf8_buffer = String::new();
        let message_data_ref = self.handle_data_mode(data.as_ref(), &mut utf8_buffer)?;

        let message_builder = MessageBuilder::from_reader("", message_data_ref);
        let mut buffer = Vec::new();
        self.sign_and_write(message_builder, message_encoding, &mut buffer)?;
        Ok(buffer)
    }

    /// Reads data from source and writes an inline-signed `OpenPGP` message to dest.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Signer, DataEncoding, UnixTime, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    /// let mut buffer = Vec::new();
    /// let message = Signer::default()
    ///     .with_signing_key(&key)
    ///     .sign_stream(&b"hello world"[..], DataEncoding::Armored, &mut buffer)
    ///     .expect("Failed to sign");
    /// ```
    pub fn sign_stream<R, W>(
        self,
        source: R,
        message_encoding: DataEncoding,
        mut dest: W,
    ) -> crate::Result<()>
    where
        R: Read,
        W: Write,
    {
        let message_data_ref = self.handle_data_mode_reader(source);
        let message_builder = MessageBuilder::from_reader("", message_data_ref);
        self.sign_and_write(message_builder, message_encoding, &mut dest)
    }

    /// Signs the given data and returns the signature.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Signer, DataEncoding, UnixTime, PrivateKey};
    /// let key_data = include_str!("../test-data/keys/private_key_v4.asc");
    /// let key = PrivateKey::import_unlocked(key_data.as_bytes(), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let signature_bytes = Signer::default()
    ///     .with_signing_key(&key)
    ///     .sign_detached(b"hello world", DataEncoding::Armored)
    ///     .unwrap();
    /// ```
    pub fn sign_detached(
        self,
        data: impl AsRef<[u8]>,
        signature_encoding: DataEncoding,
    ) -> crate::Result<Vec<u8>> {
        self.check_input_data(data.as_ref())?;

        let signing_keys = self
            .select_signing_keys()
            .map_err(SigningError::KeySelection)?;

        // Determine which hash algorithm to use for each key.
        let hash_algorithms = preferences::select_hash_algorithm_from_keys(
            self.profile.message_hash_algorithm(),
            &signing_keys,
            None,
            &self.profile,
        );

        // Create a signature for each key.
        let signatures: Result<Vec<_>, SigningError> = signing_keys
            .iter()
            .zip(hash_algorithms)
            .map(|(signing_key, hash_algorithm)| {
                signing_key
                    .sign_data(
                        data.as_ref(),
                        self.signature_creation_time(),
                        self.signature_type,
                        hash_algorithm,
                        self.signature_context.as_deref(),
                        &self.profile,
                    )
                    .map(DetachedSignature::new)
            })
            .collect();

        let signature_bytes = handle_signature_encoding(
            signatures?.as_slice(),
            signature_encoding.resolve_for_write(),
        )?;

        Ok(signature_bytes)
    }

    pub fn sign_detached_stream<R: Read + 'a>(
        self,
        data: R,
        signature_encoding: DataEncoding,
    ) -> crate::Result<DetachedSignatureGenerator<'a>> {
        let inner = self.sign_detached_inner_stream(data)?;

        Ok(DetachedSignatureGenerator::new(
            self,
            inner,
            signature_encoding,
        ))
    }

    /// Creates a cleartext signed message.
    ///
    /// A cleartext message has the following format:
    /// ```skip
    /// -----BEGIN PGP SIGNED MESSAGE-----
    ///
    /// Cleatext text comes here.
    ///
    /// -----BEGIN PGP SIGNATURE-----
    /// ...
    /// -----END PGP SIGNATURE-----
    /// ```
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Signer, DataEncoding, PrivateKey};
    /// let input_data = "hello world\n hello\n";
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let message = Signer::default()
    ///     .with_signing_key(&key)
    ///     .sign_cleartext(input_data.as_bytes())
    ///     .expect("Failed to sign");
    /// ```
    pub fn sign_cleartext(self, data: impl AsRef<[u8]>) -> crate::Result<Vec<u8>> {
        let str_data =
            std::str::from_utf8(data.as_ref()).map_err(SigningError::InvalidInputData)?;

        let signing_keys = self
            .select_signing_keys()
            .map_err(SigningError::KeySelection)?;
        // Determine which hash algorithm to use for each key.
        let hash_algorithms = preferences::select_hash_algorithm_from_keys(
            self.profile.message_hash_algorithm(),
            &signing_keys,
            None,
            &self.profile,
        );

        // Closure to create the signatures in rPGP.
        let create_signatures = |text: &str| {
            signing_keys
                .iter()
                .zip(hash_algorithms.iter())
                .map(|(signing_key, hash_algorithm)| {
                    signing_key
                        .sign_data(
                            text.as_bytes(),
                            self.signature_creation_time(),
                            SignatureMode::Text,
                            *hash_algorithm,
                            self.signature_context.as_deref(),
                            &self.profile,
                        )
                        .map_err(|err| pgp::errors::Error::Message {
                            message: err.to_string(),
                            backtrace: None,
                        })
                })
                .collect()
        };

        let cleartext_message = CleartextSignedMessage::new_many(str_data, create_signatures)
            .map_err(SigningError::Sign)?;

        let all_v6 = signing_keys
            .iter()
            .all(|key| key.private_key.version() == KeyVersion::V6);

        let cleartext_bytes = cleartext_message
            .to_armored_bytes(ArmorOptions {
                headers: None,
                include_checksum: !all_v6,
            })
            .map_err(SigningError::Serialize)?;

        Ok(cleartext_bytes)
    }

    fn sign_and_write<W>(
        &self,
        mut message_builder: MessageBuilder<impl Read>,
        message_encoding: DataEncoding,
        dest: &mut W,
    ) -> crate::Result<()>
    where
        W: Write,
    {
        let signing_keys = self
            .select_signing_keys()
            .map_err(SigningError::KeySelection)?;

        if self.inline_message_compression != CompressionAlgorithm::Uncompressed {
            message_builder.compression(self.inline_message_compression);
        }

        let signed_builder = self.sign_message(message_builder, &signing_keys, None)?;
        let rng = self.profile.rng();

        to_writer(
            &signing_keys,
            signed_builder,
            message_encoding.resolve_for_write(),
            rng,
            dest,
        )?;
        Ok(())
    }

    pub(crate) fn check_input_data(&self, data: &[u8]) -> Result<(), SigningError> {
        match self.signature_type {
            SignatureMode::Text => std::str::from_utf8(data)
                .map(|_| ())
                .map_err(SigningError::InvalidInputData),
            SignatureMode::Binary => Ok(()),
        }
    }

    pub(crate) fn handle_data_mode<'b>(
        &self,
        data: &'b [u8],
        buffer: &'b mut String,
    ) -> Result<&'b [u8], SigningError> {
        match self.data_mode {
            DataMode::Utf8 => {
                let mut reader = NormalizedReader::new(data, LineBreak::Crlf);
                reader.read_to_string(buffer)?;
                Ok(buffer.as_bytes())
            }
            _ => Ok(data),
        }
    }

    pub(crate) fn handle_data_mode_reader<'b, R: Read + 'b>(&self, data: R) -> Box<dyn Read + 'b> {
        match self.data_mode {
            DataMode::Utf8 => Box::new(NormalizedReader::new(
                Utf8CheckReader::new(data),
                LineBreak::Crlf,
            )),
            _ => Box::new(data),
        }
    }

    pub(crate) fn select_signing_keys(
        &self,
    ) -> Result<Vec<PrivateComponentKey<'_>>, KeyValidationError> {
        self.signing_keys
            .iter()
            .map(|key| {
                key.secret
                    .signing_key(self.date, None, SignatureUsage::Sign, &self.profile)
            })
            .collect()
    }

    pub(crate) fn sign_detached_inner_stream<R: Read + 'a>(
        &self,
        data: R,
    ) -> crate::Result<InnerDetachedHashingReader<'a>> {
        let signing_keys = self
            .select_signing_keys()
            .map_err(SigningError::KeySelection)?;

        // Determine which hash algorithm to use for each key.
        let hash_algorithms = preferences::select_hash_algorithm_from_keys(
            self.profile.message_hash_algorithm(),
            &signing_keys,
            None,
            &self.profile,
        );

        let keys_and_hashers: Result<Vec<_>, SigningError> = signing_keys
            .into_iter()
            .zip(hash_algorithms)
            .map(|(signing_key, hash_algorithm)| {
                signing_key.sign_for_reader(
                    self.signature_creation_time(),
                    self.signature_type,
                    hash_algorithm,
                    self.signature_context.as_deref(),
                    &self.profile,
                )
            })
            .collect();

        Ok(InnerDetachedHashingReader::new(
            Box::new(data),
            keys_and_hashers?,
        ))
    }

    /// Prepares the message builder to sign the message with the given signing keys.
    pub(crate) fn sign_message<R: Read, E: Encryption>(
        &self,
        mut message_builder: MessageBuilder<'a, R, E>,
        signing_keys: &'a [PrivateComponentKey<'a>],
        recipient_preferences_opt: Option<&RecipientsAlgorithms>,
    ) -> Result<MessageBuilder<'a, R, E>, SigningError> {
        let hash_algorithms = if let Some(recipient_preferences) = recipient_preferences_opt {
            recipient_preferences.select_hash_algorithm(
                self.profile.message_hash_algorithm(),
                signing_keys,
                &self.profile,
            )
        } else {
            preferences::select_hash_algorithm_from_keys(
                self.profile.message_hash_algorithm(),
                signing_keys,
                None,
                &self.profile,
            )
        };

        match self.signature_type {
            SignatureMode::Binary => message_builder.sign_binary(),
            SignatureMode::Text => message_builder.sign_text(),
        };

        message_builder
            .data_mode(self.data_mode)
            .map_err(SigningError::DataMode)?;

        let mut rng = self.profile.rng();
        for (signing_key, hash_algorithm) in signing_keys.iter().zip(hash_algorithms) {
            let (hashed, unhashed) = message_signature_subpackets(
                &signing_key.private_key,
                self.signature_creation_time(),
                hash_algorithm,
                self.signature_context.as_deref(),
                &mut rng,
            )?;
            message_builder.sign_with_subpackets(
                &signing_key.private_key,
                Password::default(),
                hash_algorithm,
                SubpacketConfig::UserDefined { hashed, unhashed },
            );
        }

        Ok(message_builder)
    }

    pub(crate) fn profile(&self) -> &Profile {
        &self.profile
    }

    fn signature_creation_time(&self) -> UnixTime {
        self.date.at().unwrap_or(UnixTime::now_unchecked()) // If there is no date specified, use local time.
    }
}

impl Default for Signer<'_> {
    fn default() -> Self {
        Self::new(DEFAULT_PROFILE.clone())
    }
}

fn handle_signature_encoding(
    signatures: &[DetachedSignature],
    signature_encoding: ResolvedDataEncoding,
) -> Result<Vec<u8>, SigningError> {
    match signature_encoding {
        ResolvedDataEncoding::Armored => {
            let all_v6 = signatures
                .iter()
                .all(|s| s.signature.version() == SignatureVersion::V6);
            let mut buffer = Vec::with_capacity(signatures.write_len());
            armor::write(
                &signatures,
                BlockType::Signature,
                &mut buffer,
                None,
                !all_v6,
            )
            .map_err(SigningError::Serialize)?;
            Ok(buffer)
        }
        ResolvedDataEncoding::Unarmored => {
            let mut buffer = Vec::with_capacity(signatures.write_len());
            signatures
                .to_writer(&mut buffer)
                .map_err(SigningError::Serialize)?;
            Ok(buffer)
        }
    }
}

fn to_writer<'a, RAND, W, R, E>(
    signing_keys: &'a [PrivateComponentKey<'a>],
    message_builder: MessageBuilder<R, E>,
    data_encoding: ResolvedDataEncoding,
    rng: RAND,
    output: W,
) -> Result<(), SigningError>
where
    RAND: Rng + CryptoRng,
    W: Write,
    R: Read,
    E: Encryption,
{
    match data_encoding {
        ResolvedDataEncoding::Armored => {
            let all_v6 = signing_keys
                .iter()
                .all(|key| key.private_key.version() == KeyVersion::V6);
            message_builder
                .to_armored_writer(
                    rng,
                    ArmorOptions {
                        headers: None,
                        include_checksum: !all_v6,
                    },
                    output,
                )
                .map_err(SigningError::Serialize)?;
        }
        ResolvedDataEncoding::Unarmored => message_builder
            .to_writer(rng, output)
            .map_err(SigningError::Serialize)?,
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use pgp::{
        crypto::hash::HashAlgorithm,
        packet::{LiteralData, Packet, PacketParser, Signature, SignatureType, SignatureVersion},
    };

    use crate::{AccessKeyInfo, DataEncoding, PrivateKey, SignatureExt, Signer, UnixTime};

    pub const TEST_KEY: &str = include_str!("../test-data/keys/private_key_v4.asc");

    #[test]
    pub fn create_detached_signature_v4_binary() {
        let date = UnixTime::new(1_752_476_259);
        let input_data = b"hello world";

        let key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let signature_bytes = Signer::default()
            .with_signing_key(&key)
            .at_date(date.into())
            .sign_detached(input_data, DataEncoding::Unarmored)
            .expect("Failed to sign");

        let signature = load_signature(&signature_bytes);
        assert_eq!(signature.version(), SignatureVersion::V4);
        assert_eq!(signature.typ(), Some(SignatureType::Binary));
        assert_eq!(signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(
            signature.issuer_key_id().first().copied(),
            Some(&key.key_id())
        );
        assert_eq!(signature.unix_created_at().unwrap(), date);
        assert_eq!(signature.notations().len(), 1);
    }

    #[test]
    pub fn create_detached_signature_v4_text() {
        let date = UnixTime::new(1_752_476_259);
        let input_data = b"hello world\n";

        let key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let signature_bytes = Signer::default()
            .with_signing_key(&key)
            .at_date(date.into())
            .as_utf8()
            .sign_detached(input_data, DataEncoding::Unarmored)
            .expect("Failed to sign");

        let signature = load_signature(&signature_bytes);
        assert_eq!(signature.version(), SignatureVersion::V4);
        assert_eq!(signature.typ(), Some(SignatureType::Text));
        assert_eq!(signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(
            signature.issuer_key_id().first().copied(),
            Some(&key.key_id())
        );
        assert_eq!(signature.unix_created_at().unwrap(), date);
        assert_eq!(signature.notations().len(), 1);
    }

    #[test]
    pub fn create_detached_signature_v6() {
        const TEST_KEY_V6: &str = include_str!("../test-data/keys/private_key_v6.asc");

        let date = UnixTime::new(1_752_476_259);
        let input_data = b"hello world";

        let key = PrivateKey::import_unlocked(TEST_KEY_V6.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let signature_bytes = Signer::default()
            .with_signing_key(&key)
            .at_date(date.into())
            .sign_detached(input_data, DataEncoding::Unarmored)
            .expect("Failed to sign");

        let signature = load_signature(&signature_bytes);
        assert_eq!(signature.version(), SignatureVersion::V6);
        assert_eq!(signature.typ(), Some(SignatureType::Binary));
        assert_eq!(signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert!(signature.issuer_key_id().is_empty());
        assert!(signature.notations().is_empty());
    }

    #[test]
    pub fn create_inline_signature_v4_binary() {
        let date = UnixTime::new(1_752_476_259);
        let input_data = b"hello world";

        let key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let data_bytes = Signer::default()
            .with_signing_key(&key)
            .at_date(date.into())
            .sign(input_data, DataEncoding::Unarmored)
            .expect("Failed to sign");

        let signature = load_signatures_inline(&data_bytes)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(signature.version(), SignatureVersion::V4);
        assert_eq!(signature.typ(), Some(SignatureType::Binary));
        assert_eq!(signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(
            signature.issuer_key_id().first().copied(),
            Some(&key.key_id())
        );
        assert_eq!(signature.unix_created_at().unwrap(), date);
        assert_eq!(signature.notations().len(), 1);
    }

    #[test]
    pub fn create_inline_signature_v4_text() {
        let date = UnixTime::new(1_752_476_259);
        let input_data = b"hello world\n ds   \n";
        let expected = b"hello world\r\n ds   \r\n";

        let key = PrivateKey::import_unlocked(TEST_KEY.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let data_bytes = Signer::default()
            .with_signing_key(&key)
            .at_date(date.into())
            .as_utf8()
            .sign(input_data, DataEncoding::Unarmored)
            .expect("Failed to sign");

        let literal_packet = load_literal_packet(&data_bytes);
        assert!(!literal_packet.is_binary());
        assert_eq!(literal_packet.data(), expected);

        let signature = load_signatures_inline(&data_bytes)
            .into_iter()
            .next()
            .unwrap();

        assert_eq!(signature.version(), SignatureVersion::V4);
        assert_eq!(signature.typ(), Some(SignatureType::Text));
        assert_eq!(signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert_eq!(
            signature.issuer_key_id().first().copied(),
            Some(&key.key_id())
        );
        assert_eq!(signature.unix_created_at().unwrap(), date);
        assert_eq!(signature.notations().len(), 1);
    }

    #[test]
    pub fn create_inline_signature_v6() {
        const TEST_KEY_V6: &str = include_str!("../test-data/keys/private_key_v6.asc");

        let date = UnixTime::new(1_752_476_259);
        let input_data = b"hello world";

        let key = PrivateKey::import_unlocked(TEST_KEY_V6.as_bytes(), DataEncoding::Armored)
            .expect("Failed to import key");

        let data_bytes = Signer::default()
            .with_signing_key(&key)
            .at_date(date.into())
            .sign_detached(input_data, DataEncoding::Unarmored)
            .expect("Failed to sign");

        let signature = load_signatures_inline(&data_bytes)
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(signature.version(), SignatureVersion::V6);
        assert_eq!(signature.typ(), Some(SignatureType::Binary));
        assert_eq!(signature.hash_alg(), Some(HashAlgorithm::Sha512));
        assert_eq!(
            signature.issuer_fingerprint().first().copied(),
            Some(&key.fingerprint())
        );
        assert!(signature.issuer_key_id().is_empty());
        assert!(signature.notations().is_empty());
    }

    fn load_signatures_inline(inline_message: &[u8]) -> Vec<Signature> {
        PacketParser::new(inline_message)
            .filter_map(|parse_result| match parse_result {
                Ok(Packet::Signature(signature)) => Some(signature),
                _ => None,
            })
            .collect()
    }

    fn load_signature(signature: &[u8]) -> Signature {
        let mut parser = PacketParser::new(signature);
        let packet = parser.next().unwrap().unwrap();
        match packet {
            Packet::Signature(signature) => signature,
            _ => panic!("Expected a signature packet"),
        }
    }

    fn load_literal_packet(inline_message: &[u8]) -> LiteralData {
        PacketParser::new(inline_message)
            .find_map(|parse_result| match parse_result {
                Ok(Packet::LiteralData(literal_data)) => Some(literal_data),
                _ => None,
            })
            .unwrap()
    }
}
