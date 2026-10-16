use core::fmt;
use std::{
    borrow::Cow,
    io::{BufRead, BufReader, Read},
};

use pgp::{
    armor::BlockType,
    composed::{CleartextSignedMessage, Message},
    line_writer::LineBreak,
    normalize_lines::NormalizedReader,
    packet::{Packet, PacketParser, Signature},
};

use crate::{
    armor, check_and_sanitize_text,
    signature::{
        SignatureVerificationResult, VerificationError, VerificationResult,
        VerificationResultCreator,
    },
    ArmorError, CheckUnixTime, DataEncoding, Error, MessageProcessingError,
    MessageVerificationError, MessageVerificationExt, Profile, PublicKey, ReferencedReader,
    ResolvedDataEncoding, VerificationContext, VerificationInput, DEFAULT_PROFILE,
};

mod reader;
pub use reader::*;

/// Verifier type to verify `OpenPGP` signatures.
#[derive(Debug, Clone)]
pub struct Verifier<'a> {
    /// The profile to use for verification.
    pub(crate) profile: Profile,

    /// The verification keys that are used to verify the signatures.
    pub(crate) verification_keys: Vec<&'a PublicKey>,

    /// The date to verify the signature against.
    pub(crate) date: CheckUnixTime,

    /// Whether to sanitize the output plaintext from canonicalized line endings
    /// and check that the output is utf-8 encoded.
    pub(crate) native_newlines_utf8: bool,

    /// The verification context to use for verifying message signatures.
    pub(crate) verification_context: Option<Cow<'a, VerificationContext>>,

    /// Max message size to read.
    pub(crate) max_message_reading_size: Option<usize>,
}

impl<'a> Verifier<'a> {
    /// Create a new verifier with the given profile.
    pub fn new(profile: Profile) -> Self {
        let max_message_reading_size = profile.max_reading_size();
        Self {
            profile,
            verification_keys: Vec::new(),
            date: CheckUnixTime::enable_now(),
            verification_context: None,
            native_newlines_utf8: false,
            max_message_reading_size,
        }
    }

    /// Set the verification key to use.
    pub fn with_verification_key(mut self, key: &'a PublicKey) -> Self {
        self.verification_keys.push(key);
        self
    }

    /// Set the verification keys to use.
    pub fn with_verification_keys(mut self, keys: impl IntoIterator<Item = &'a PublicKey>) -> Self {
        self.verification_keys.extend(keys);
        self
    }

    /// Allows to specify the expected application context of a signature.
    ///
    /// The [`VerificationContext`] encodes how the signature context should be checked.
    pub fn with_verification_context(
        mut self,
        context: impl Into<Cow<'a, VerificationContext>>,
    ) -> Self {
        self.verification_context = Some(context.into());
        self
    }

    /// Set the date to verify the signature against.
    ///
    /// In default mode, the system clock is used.
    pub fn at_date(mut self, date: CheckUnixTime) -> Self {
        self.date = date;
        self
    }

    /// Setting output Utf8 indicates if the output plaintext is Utf8 encoded and
    /// should be sanitized from canonicalised line endings.
    ///
    /// If this setting is enabled, the decryptor throws an error if the output is
    /// not Utf-8 encoded.
    /// Further, the decryptor replaces canonical newlines (`\r\n`) with native newlines (`\n`).
    pub fn output_utf8(mut self) -> Self {
        self.native_newlines_utf8 = true;
        self
    }

    /// Allows to override the max message reading size.
    ///
    /// The verifier does not allow to read more data than the max message reading size.
    /// None means no limit.
    pub fn with_max_message_reading_size(mut self, size: Option<usize>) -> Self {
        self.max_message_reading_size = size;
        self
    }

    /// Verifies an inline-signed message with the verifier.
    ///
    /// Returns the verified data and result of its verification.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Verifier, PublicKey, DataEncoding, CheckUnixTime};
    ///
    /// const INPUT_DATA: &str = include_str!("../test-data/messages/signed_message_v4.asc");
    /// const KEY: &str = include_str!("../test-data/keys/public_key_v4.asc");
    /// let date = CheckUnixTime::new(1_753_088_183);
    ///
    /// let key = PublicKey::import(KEY.as_bytes(), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let verified_data = Verifier::default()
    ///     .with_verification_key(&key)
    ///     .at_date(date.into())
    ///     .verify(INPUT_DATA, DataEncoding::Armored)
    ///     .expect("Failed to verify");
    ///
    /// assert_eq!(verified_data.data, b"hello world");
    /// assert!(verified_data.verification_result.is_ok());
    /// ```
    pub fn verify(
        self,
        data: impl AsRef<[u8]>,
        data_encoding: DataEncoding,
    ) -> crate::Result<DataVerificationResult> {
        let resolved_data_encoding = data_encoding.resolve_for_read(data.as_ref());
        let message = armor::decode_to_message(data.as_ref(), resolved_data_encoding)
            .map_err(MessageVerificationError::MessageProcessing)?;

        let verified_data = self
            .verify_message(message)
            .map_err(MessageVerificationError::MessageProcessing)?;

        Ok(verified_data)
    }

    /// Verifies a detached signature against the data.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Verifier, PublicKey, DataEncoding, CheckUnixTime};
    ///
    /// // Assume `public_key` is a valid PublicKey, and `signature` is a detached signature.
    /// let public_key = include_str!("../test-data/keys/public_key_v4.asc");
    /// let signature = include_str!("../test-data/signatures/signature_v4.asc");
    /// let data = b"hello world";
    /// let date = CheckUnixTime::enable_now();
    ///
    /// let public_key = PublicKey::import(public_key.as_bytes(), DataEncoding::Armored).unwrap();
    ///
    /// let result = Verifier::default()
    ///     .with_verification_key(&public_key)
    ///     .at_date(date.into())
    ///     .verify_detached(data, signature, DataEncoding::Armored);
    /// assert!(result.is_ok());
    /// ```
    pub fn verify_detached(
        self,
        data: impl AsRef<[u8]>,
        signature: impl AsRef<[u8]>,
        signature_encoding: DataEncoding,
    ) -> VerificationResult {
        let resolved_signature_encoding = signature_encoding.resolve_for_read(signature.as_ref());
        // Check encoding.
        let parser: PacketParser<Box<dyn BufRead>> =
            handle_signature_decoding(signature.as_ref(), resolved_signature_encoding)
                .map_err(|err| VerificationError::RuntimeError(err.to_string()))?;

        // Verify signatures.
        let verified_signatures: Vec<_> = parser
            .filter_map(|packet_result| match packet_result {
                Ok(Packet::Signature(signature)) => Some(signature),
                _ => None,
            })
            .map(|signature| {
                SignatureVerificationResult::create_by_verifying(
                    self.date,
                    signature,
                    &self.verification_keys,
                    VerificationInput::Data(data.as_ref()),
                    self.verification_context.as_deref(),
                    &self.profile,
                )
            })
            .collect();

        // Select the result.
        VerificationResultCreator::with_signatures(verified_signatures)
    }

    /// Verifies a cleartext signed message with the verifier.
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
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Verifier, PublicKey, DataEncoding};
    ///
    /// const INPUT_DATA: &str = include_str!("../test-data/messages/signed_cleartext_message_v4.asc");
    ///
    /// let key = PublicKey::import(include_bytes!("../test-data/keys/public_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let verified_data = Verifier::default()
    ///     .with_verification_key(&key)
    ///     .verify_cleartext(INPUT_DATA)
    ///     .expect("Failed to verify");
    ///
    /// assert_eq!(verified_data.data, b"hello world\n    with multiple lines\n");
    /// assert!(verified_data.verification_result.is_ok());
    /// ```
    pub fn verify_cleartext(
        self,
        cleartext_message: impl AsRef<[u8]>,
    ) -> crate::Result<DataVerificationResult> {
        let (parsed_message, _) =
            CleartextSignedMessage::from_armor(cleartext_message.as_ref().trim_ascii_end())
                .map_err(|err| {
                    MessageVerificationError::MessageProcessing(
                        MessageProcessingError::MessageParsing(err),
                    )
                })?;

        let signed_data = parsed_message.signed_text();

        let verified_signatures: Vec<_> = parsed_message
            .signatures()
            .iter()
            .map(|signature| {
                SignatureVerificationResult::create_by_verifying(
                    self.date,
                    signature.clone(),
                    &self.verification_keys,
                    VerificationInput::Data(signed_data.as_ref()),
                    self.verification_context.as_deref(),
                    &self.profile,
                )
            })
            .collect();

        let output_sanitized = check_and_sanitize_text(parsed_message.signed_text().as_bytes())
            .map_err(MessageProcessingError::TextSanitization)
            .map_err(MessageVerificationError::MessageProcessing)?;

        let verification_result = VerificationResultCreator::with_signatures(verified_signatures);
        Ok(DataVerificationResult {
            data: output_sanitized,
            verification_result,
        })
    }

    /// Verifies a detached signature against the input data stream using the verifier.
    ///
    /// Instead directly returning the verification result,
    /// this method returns a reader that can be used to read the data to be verifed from the input source.
    /// Once all data has been read, the verification result can be
    /// obtained by calling the `verification_result` method on the reader.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Verifier, PublicKey, DataEncoding, UnixTime};
    /// use std::io;
    ///
    /// const INPUT_DATA: &str = include_str!("../test-data/messages/signed_message_v4.asc");
    /// const KEY: &str = include_str!("../test-data/keys/public_key_v4.asc");
    /// let date = UnixTime::new(1_753_088_183);
    ///
    /// let key = PublicKey::import(KEY.as_bytes(), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let mut reader = Verifier::default()
    ///     .with_verification_key(&key)
    ///     .at_date(date.into())
    ///     .verify_stream(INPUT_DATA.as_bytes(), DataEncoding::Armored)
    ///     .expect("Failed to decrypt");
    ///
    /// let mut buffer = Vec::new();
    /// io::copy(&mut reader, &mut buffer).expect("Failed to copy");
    /// let verification_result = reader.verification_result();
    ///
    /// assert_eq!(buffer, b"hello world");
    /// assert!(verification_result.is_ok());
    /// ```
    pub fn verify_detached_stream(
        self,
        data: impl Read + 'a,
        signature: impl AsRef<[u8]>,
        signature_encoding: DataEncoding,
    ) -> crate::Result<VerifyingReader<'a>> {
        let resolved_signature_encoding = signature_encoding.resolve_for_read(signature.as_ref());
        // Check encoding.
        let parser: PacketParser<Box<dyn BufRead>> =
            handle_signature_decoding(signature.as_ref(), resolved_signature_encoding).map_err(
                |err| {
                    Error::Verification(MessageVerificationError::MessageProcessing(
                        MessageProcessingError::Unarmor(err),
                    ))
                },
            )?;

        let signatures: Vec<Signature> = parser
            .filter_map(|packet_result| match packet_result {
                Ok(Packet::Signature(signature)) => Some(signature),
                _ => None,
            })
            .collect();

        let reader = DetachedVerifyingReader::new(self, signatures, Box::new(BufReader::new(data)));

        Ok(reader.into())
    }

    /// Verifies an inline-signed message with the verifier.
    ///
    /// Instead of directly returning the verified data,
    /// this method returns a reader that can be used to read the data to be verifed from the input source.
    /// Once all data has been read, the verification result can be obtained
    /// by calling the `verification_result` method on the reader.
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Verifier, PublicKey, DataEncoding, UnixTime};
    ///
    /// const INPUT_DATA: &str = include_str!("../test-data/messages/signed_message_v4.asc");
    /// const KEY: &str = include_str!("../test-data/keys/public_key_v4.asc");
    /// let date = UnixTime::new(1_753_088_183);
    ///
    /// let key = PublicKey::import(KEY.as_bytes(), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let verified_data = Verifier::default()
    ///     .with_verification_key(&key)
    ///     .at_date(date.into())
    ///     .verify(INPUT_DATA, DataEncoding::Armored)
    ///     .expect("Failed to verify");
    ///
    /// assert_eq!(verified_data.data, b"hello world");
    /// assert!(verified_data.verification_result.is_ok());
    /// ```
    pub fn verify_stream(
        self,
        data: impl Read + fmt::Debug + Send + 'a,
        data_encoding: DataEncoding,
    ) -> crate::Result<VerifyingReader<'a>> {
        let mut buffered_reader = BufReader::new(data);
        let resolved_data_encoding = data_encoding.resolve_for_read_stream(&mut buffered_reader);
        let message = armor::decode_to_message_reader(buffered_reader, resolved_data_encoding)
            .map_err(MessageVerificationError::MessageProcessing)?;

        self.verify_message_stream(message)
            .map_err(|err| MessageVerificationError::MessageProcessing(err).into())
    }

    /// Helper function to verify and process a decrypted `OpenPGP` message.
    pub(crate) fn verify_message(
        &self,
        mut message: Message<'_>,
    ) -> Result<DataVerificationResult, MessageProcessingError> {
        if message.is_encrypted() {
            return Err(MessageProcessingError::Encrypted);
        }

        if message.is_compressed() {
            message = message
                .decompress()
                .map_err(MessageProcessingError::Decompression)?;
            if message.is_compressed() {
                return Err(MessageProcessingError::Compression);
            }
        }

        let (mut cleartext, message) = if let Some(max_reading_size) = self.max_message_reading_size
        {
            let mut reader = LimitingReader::new(message, Some(max_reading_size));
            let mut cleartext = Vec::new();
            reader.read_to_end(&mut cleartext)?;
            (cleartext, reader.into_inner())
        } else {
            (message.as_data_vec()?, message)
        };

        let verified_signatures = message.verify_message_signatures(
            self.date,
            &self.verification_keys,
            self.verification_context.as_deref(),
            &self.profile,
        )?;

        if self.native_newlines_utf8 {
            cleartext = check_and_sanitize_text(cleartext.as_slice())?;
        }

        let verification_result = VerificationResultCreator::with_signatures(verified_signatures);

        Ok(DataVerificationResult {
            data: cleartext,
            verification_result,
        })
    }

    /// Helper function to verify and process a decrypted `OpenPGP` message with a reader.
    pub(crate) fn verify_message_stream(
        self,
        mut message: Message<'a>,
    ) -> Result<VerifyingReader<'a>, MessageProcessingError> {
        if message.is_encrypted() {
            return Err(MessageProcessingError::Encrypted);
        }

        if message.is_compressed() {
            message = message
                .decompress()
                .map_err(MessageProcessingError::Decompression)?;
            if message.is_compressed() {
                return Err(MessageProcessingError::Compression);
            }
        }

        let normalize = self.native_newlines_utf8;
        let max_reading_size = self.max_message_reading_size;
        let message_reader =
            LimitingReader::new(MessageVerifyingReader::new(self, message), max_reading_size);
        if normalize {
            let inner_reader = ReferencedReader::new(message_reader);
            let referenced_inner_reader = inner_reader.reference();
            let reader = NormalizedReader::new(inner_reader, LineBreak::Lf);

            return Ok(VerifyingReader::InlineNormalizedLineEndings {
                referenced_inner_reader,
                normalized_reader: Box::new(reader),
            });
        }
        Ok(message_reader.into())
    }
}

impl Default for Verifier<'_> {
    fn default() -> Self {
        Self::new(DEFAULT_PROFILE.clone())
    }
}

/// The result of verifying signed data in an `OpenPGP` message.
#[derive(Debug, Clone)]
pub struct DataVerificationResult {
    /// The data against which the signature was verified.
    ///
    /// WARNING: Accessing this data directly ignores the result of the verification.
    /// Thus, it could have been maliciously modified.
    /// Check [`Self::verification_result`] for the result.
    pub data: Vec<u8>,

    /// The verification result of verifying the underlying signature against the data.
    pub verification_result: VerificationResult,
}

impl DataVerificationResult {
    pub fn as_unverified_data(&self) -> &[u8] {
        &self.data
    }

    pub fn try_as_verified_data(&self) -> Result<&[u8], VerificationError> {
        if let Err(err) = &self.verification_result {
            return Err(err.clone());
        }
        Ok(&self.data)
    }

    pub fn into_unverified_data(self) -> Vec<u8> {
        self.data
    }

    pub fn into_verified_data(self) -> Result<Vec<u8>, VerificationError> {
        self.verification_result?;
        Ok(self.data)
    }

    pub fn verification_succeeded(&self) -> bool {
        self.verification_result.is_ok()
    }

    pub fn verification_failed(&self) -> bool {
        self.verification_result.is_err()
    }
}

impl TryFrom<DataVerificationResult> for Vec<u8> {
    type Error = VerificationError;

    fn try_from(value: DataVerificationResult) -> Result<Self, Self::Error> {
        value.into_verified_data()
    }
}

fn handle_signature_decoding<'a>(
    signature: &'a [u8],
    signature_encoding: ResolvedDataEncoding,
) -> Result<PacketParser<Box<dyn BufRead + 'a>>, ArmorError> {
    match signature_encoding {
        ResolvedDataEncoding::Unarmored => Ok(PacketParser::new(Box::new(signature))),
        ResolvedDataEncoding::Armored => {
            let reader = armor::decode_to_reader(signature, Some(BlockType::Signature))?;
            Ok(PacketParser::new(Box::new(BufReader::new(reader))))
        }
    }
}
