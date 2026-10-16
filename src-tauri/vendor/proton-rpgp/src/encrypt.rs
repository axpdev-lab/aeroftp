use std::{
    borrow::Cow,
    io::{Read, Write},
    mem,
    sync::Arc,
};

use pgp::{
    composed::{
        ArmorOptions, Encryption, EncryptionSeipdV1, EncryptionSeipdV2, MessageBuilder,
        NoEncryption, RawSessionKey,
    },
    crypto::{
        aead::{AeadAlgorithm, ChunkSize},
        sym::SymmetricKeyAlgorithm,
    },
    packet::{DataMode, PacketTrait, PublicKeyEncryptedSessionKey, SymKeyEncryptedSessionKey},
    types::{CompressionAlgorithm, KeyDetails, KeyVersion, Password},
};
use rand::{CryptoRng, Rng};

use crate::{
    preferences::{EncryptionMechanism, RecipientsAlgorithms},
    AeadCiphersuite, CheckUnixTime, Ciphersuite, CloneablePasswords, DataEncoding, EncryptionError,
    ExternalDetachedSignature, ExternalHashTracker, KeyValidationError, PrivateComponentKey,
    PrivateKey, Profile, PublicComponentKey, PublicKey, PublicKeySelectionExt,
    ResolvedDataEncoding, SessionKey, SignatureContext, Signer, SigningError, DEFAULT_PROFILE,
};

mod message;
pub use message::*;
mod observe;
pub use observe::*;

/// The encryptor to use to perform `OpenPGP` encryption/signcryption operations.
#[derive(Debug, Clone)]
pub struct Encryptor<'a> {
    /// The encryption keys to use.
    encryption_keys: Vec<&'a PublicKey>,

    /// The passphrases to encrypt the message with.
    passphrases: CloneablePasswords,

    /// The session keys to use.
    session_key: Option<Cow<'a, SessionKey>>,

    /// Message compression preference.
    message_compression: CompressionAlgorithm,

    /// Message symmetric algorithm preference.
    message_symmetric_algorithm: SymmetricKeyAlgorithm,

    /// Message AEAD cipher suite preference.
    message_aead_cipher_suite: Option<Ciphersuite>,

    /// Message AEAD chunk size preference.
    message_aead_chunk_size: ChunkSize,

    /// Determines if a detached signature should be created.
    detached_signature_op: SignDetachedOperation,

    /// The observer to use for the encryption if any.
    observer: Option<Arc<dyn EncryptionObserver>>,

    /// The internal signer to use for the signing part.
    pub(crate) signer: Signer<'a>,
}

impl<'a> Encryptor<'a> {
    /// Creates a new encryptor with the given profile.
    pub fn new(profile: Profile) -> Self {
        Self {
            encryption_keys: Vec::new(),
            passphrases: CloneablePasswords::default(),
            session_key: None,
            message_compression: profile.message_compression(),
            message_symmetric_algorithm: profile.message_symmetric_algorithm(),
            message_aead_cipher_suite: profile.message_aead_cipher_suite(),
            message_aead_chunk_size: profile.message_aead_chunk_size(),
            detached_signature_op: SignDetachedOperation::default(),
            observer: None,
            signer: Signer::new(profile),
        }
    }

    /// Adds a encryption key to the encryptor.
    pub fn with_encryption_key(mut self, key: &'a PublicKey) -> Self {
        self.encryption_keys.push(key);
        self
    }

    /// Adds multiple encryption keys to the encryptor.
    pub fn with_encryption_keys(mut self, keys: impl IntoIterator<Item = &'a PublicKey>) -> Self {
        self.encryption_keys.extend(keys);
        self
    }

    /// Adds a signing key to the signer.
    pub fn with_signing_key(mut self, key: &'a PrivateKey) -> Self {
        self.signer = self.signer.with_signing_key(key);
        self
    }

    /// Adds multiple signing keys to the signer.
    ///
    /// For each key, a signature will be created.
    pub fn with_signing_keys(mut self, keys: impl IntoIterator<Item = &'a PrivateKey>) -> Self {
        self.signer = self.signer.with_signing_keys(keys);
        self
    }

    /// Adds a passphrase to the encryptor.
    ///
    /// If a password is set, the output message will contain a key packet encrypted
    /// with the password.
    pub fn with_passphrase(mut self, passphrase: impl AsRef<[u8]>) -> Self {
        self.passphrases.0.push(Password::from(passphrase.as_ref()));
        self
    }

    /// Adds multiple passphrases to the encryptor.
    pub fn with_passphrases(
        mut self,
        passphrases: impl IntoIterator<Item = impl AsRef<[u8]>>,
    ) -> Self {
        self.passphrases
            .0
            .extend(passphrases.into_iter().map(|p| Password::from(p.as_ref())));
        self
    }

    /// Adds a session key to the encryptor.
    ///
    /// The data packet will be encrypted with the provided session key.
    ///
    /// # Warning
    ///
    /// Use this function only if you fully understand the implications.
    /// Providing a custom session key can compromise security if not handled correctly.
    /// Prefer letting the library generate a secure session key unless you have a specific, well-understood use case.
    pub fn with_session_key(mut self, key: impl Into<Cow<'a, SessionKey>>) -> Self {
        self.session_key = Some(key.into());
        self
    }

    /// Sets the application signature context to use for the message signatures.
    pub fn with_signature_context(mut self, context: impl Into<Cow<'a, SignatureContext>>) -> Self {
        self.signer = self.signer.with_signature_context(context);
        self
    }

    /// Sets the observer to use for the encryption.
    pub fn with_observer(mut self, observer: Arc<dyn EncryptionObserver>) -> Self {
        self.observer = Some(observer);
        self
    }

    /// Sets the detached signature operation to use for the message.
    ///
    /// If `encrypt` is true, the detached signature will be encrypted.
    /// If `encrypt` is false, the detached signature will be plain.
    pub fn using_detached_signature(mut self, encrypt: bool) -> Self {
        self.detached_signature_op = if encrypt {
            SignDetachedOperation::Encrypted
        } else {
            SignDetachedOperation::Unencrypted
        };
        self
    }

    /// Sets the signature type to text.
    pub fn as_utf8(mut self) -> Self {
        self.signer = self.signer.as_utf8();
        self
    }

    /// Sets the date to use for the signatures and keys selection.
    pub fn at_date(mut self, date: CheckUnixTime) -> Self {
        self.signer = self.signer.at_date(date);
        self
    }

    /// Enables compression for the encryption.
    ///
    /// The default is determined by the profile (most likely uncompressed).
    /// Compression affects security and should be used with care.
    pub fn compress(mut self) -> Self {
        self.message_compression = CompressionAlgorithm::ZLIB;
        self
    }

    /// Enables AEAD (`SEIPDv2`, RFC 9580) encryption using the specified ciphersuite if provided.
    ///
    /// If `None` is provided, the encryptor will not consider AEAD encryption even if the recipients encryption keys support it.
    /// If a ciphersuite is provided, the encryptor will use the specified ciphersuite for AEAD encryption.
    /// In this case, if all the recipients encryption keys indicated `SEIPDv2` support,
    /// the encryptor encrypts with AEAD (`SEIPDv2` RFC9580) or generates an AEAD (`SEIPDv2` RFC9580) session key.
    /// If a session key is provided, the encryptor will use the specified type in the session key and ignore the ciphersuite provided.
    /// For session key generation, if no recipient encryption keys are provided, the encryptor will generate an AEAD
    /// session key using the ciphersuite provided
    /// or a default non-AEAD session key if no ciphersuite is provided.
    pub fn with_aead_allowed(mut self, ciphersuite: Option<AeadCiphersuite>) -> Self {
        self.message_aead_cipher_suite = ciphersuite.map(Into::into);
        self
    }

    /// Allows to override the AEAD chunk size for AEAD-encrypted (`SEIPDv2`, RFC 9580) messages.
    ///
    /// Per default the profile is used to determine the AEAD chunk size if used.
    pub fn with_aead_chunk_size(mut self, chunk_size: ChunkSize) -> Self {
        self.message_aead_chunk_size = chunk_size;
        self
    }

    /// Encrypts and optionally signs the given data and returns an `OpenPGP` message type.
    ///
    /// If no signing key is provided, the data will be encrypted without a signature.
    /// If a signing key is provided, the data will be signed before encryption.
    ///
    /// # Example
    ///
    /// ```rust
    /// use proton_rpgp::{AsPublicKeyRef, DataEncoding, Decryptor, Encryptor, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let encrypted_message = Encryptor::default()
    ///     .with_encryption_key(key.as_public_key())
    ///     .with_signing_key(&key)
    ///     .encrypt(b"Hello world!")
    ///     .expect("Failed to encrypt");
    /// ```
    pub fn encrypt(mut self, data: &[u8]) -> crate::Result<EncryptedMessage> {
        let detached_signature = self.handle_detached_signature_operation(data)?;

        let mut utf8_buffer = String::new();
        let message_data_ref = self
            .signer
            .handle_data_mode(data.as_ref(), &mut utf8_buffer)?;

        let mut output = Vec::new();
        let revealed_session_key =
            self.write_and_signcrypt(message_data_ref, DataEncoding::Unarmored, true, &mut output)?;

        let mut message = EncryptedMessage::new(output, revealed_session_key);
        message.info.detached_signature = detached_signature;

        Ok(message)
    }

    /// Encrypts and optionally signs the given data and writes an `OpenPGP` message type.
    ///
    /// Reads all data from the source and writes the encrypted message to the destination.
    ///
    /// # Example
    ///
    /// ```rust
    /// use proton_rpgp::{AsPublicKeyRef, DataEncoding, Decryptor, Encryptor, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let mut buffer = Vec::new();
    /// let encrypted_message = Encryptor::default()
    ///     .with_encryption_key(key.as_public_key())
    ///     .with_signing_key(&key)
    ///     .encrypt_stream(&b"Hello world!"[..], DataEncoding::Armored, &mut buffer)
    ///     .expect("Failed to encrypt");
    /// ```
    pub fn encrypt_stream<R: Read, W: Write>(
        mut self,
        source: R,
        data_encoding: DataEncoding,
        dest: W,
    ) -> crate::Result<EncryptedMessageInfo> {
        let (detached_signature_handle, input_source) =
            self.handle_detached_signature_hash_stream(source)?;

        let message_data_ref = self.signer.handle_data_mode_reader(input_source);

        let sk = self
            .clone()
            .write_and_signcrypt(message_data_ref, data_encoding, true, dest)?;

        let detached_signature =
            self.handle_detached_signature_sign_stream(detached_signature_handle)?;

        Ok(EncryptedMessageInfo::new(detached_signature, sk))
    }

    /// Encrypts and optionally signs the given data and writes an `OpenPGP` message type in split mode.
    ///
    /// Reads all data from the source and writes ONLY the data packet to the destination.
    /// The serialized key packets are returned in the first element of the return tuple.
    ///
    /// # Example
    ///
    /// ```rust
    /// use proton_rpgp::{AsPublicKeyRef, DataEncoding, Decryptor, Encryptor, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let mut data_packet = Vec::new();
    /// let (key_packets, _) = Encryptor::default()
    ///     .with_encryption_key(key.as_public_key())
    ///     .with_signing_key(&key)
    ///     .encrypt_stream_split(&b"Hello world!"[..], &mut data_packet)
    ///     .expect("Failed to encrypt");
    pub fn encrypt_stream_split<R: Read, W: Write>(
        mut self,
        source: R,
        dest: W,
    ) -> crate::Result<(Vec<u8>, EncryptedMessageInfo)> {
        let (detached_signature_handle, input_source) =
            self.handle_detached_signature_hash_stream(source)?;

        let message_data_ref = self.signer.handle_data_mode_reader(input_source);

        let num_key_packets = self.encryption_keys.len() + self.passphrases.len();
        let key_packet_tracker = SharedKeyPacketBuffer::new();
        let packet_split_writer =
            PacketSplitWriter::new(dest, key_packet_tracker.clone(), num_key_packets);

        let sk = self.clone().write_and_signcrypt(
            message_data_ref,
            DataEncoding::Unarmored,
            true,
            packet_split_writer,
        )?;

        let detached_signature =
            self.handle_detached_signature_sign_stream(detached_signature_handle)?;

        Ok((
            key_packet_tracker.into_key_packets(),
            EncryptedMessageInfo::new(detached_signature, sk),
        ))
    }

    /// Encrypts the session key and returns the encrypted session key packets (Key packets).
    ///
    /// # Example
    ///
    /// ```
    /// use proton_rpgp::{Encryptor, SessionKey};
    /// use proton_rpgp::pgp::crypto::sym::SymmetricKeyAlgorithm;
    ///
    /// let session_key = SessionKey::new(b"0000000000000000", SymmetricKeyAlgorithm::AES128);
    ///
    /// let key_packets = Encryptor::default()
    ///     .with_passphrase("password")
    ///     .encrypt_session_key(&session_key)
    ///     .expect("Failed to encrypt");
    /// ```
    pub fn encrypt_session_key(self, session_key: &SessionKey) -> crate::Result<Vec<u8>> {
        let encryption_keys = self.select_encryption_keys()?;

        if let Some(observer) = &self.observer {
            observer.observe_encryption_keys(&encryption_keys);
        }

        let recipients_algo = RecipientsAlgorithms::select(
            self.message_symmetric_algorithm,
            self.message_aead_cipher_suite,
            self.message_compression,
            &encryption_keys,
            self.profile(),
        );

        let encryption_mechanism =
            session_key.encryption_mechanism(&recipients_algo, self.profile())?;

        let session_key_bytes = session_key.as_raw_session_key();
        let mut rng = self.profile().rng();

        // PKESKs
        let pkesks = create_pkesk_packets(
            &encryption_keys,
            encryption_mechanism,
            &mut rng,
            session_key_bytes,
        )?;

        // SKESKs
        let skesks = create_skesk_packets(
            &self.passphrases,
            encryption_mechanism,
            &mut rng,
            session_key_bytes,
            self.profile(),
        )?;

        if pkesks.is_empty() && skesks.is_empty() {
            return Err(EncryptionError::MissingEncryptionTools.into());
        }

        key_packets_to_bytes(&pkesks, &skesks).map_err(Into::into)
    }

    /// Encrypts and optionally signs the given data and returns a serialized `OpenPGP` message.
    ///
    /// If no signing key is provided, the data will be encrypted without a signature.
    /// If a signing key is provided, the data will be signed before encryption.
    /// In detached signature mode, the signature is not included in the message, use [`Self::encrypt`] instead.
    ///
    /// # Example
    ///
    /// ```rust
    /// use proton_rpgp::{AsPublicKeyRef, DataEncoding, Decryptor, Encryptor, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let encrypted_data = Encryptor::default()
    ///     .with_encryption_key(key.as_public_key())
    ///     .with_signing_key(&key)
    ///     .encrypt_raw(b"Hello world!", DataEncoding::Armored)
    ///     .expect("Failed to encrypt");
    /// ```
    pub fn encrypt_raw(
        self,
        data: &'a [u8],
        data_encoding: DataEncoding,
    ) -> crate::Result<Vec<u8>> {
        let mut utf8_buffer = String::new();
        let message_data_ref = self
            .signer
            .handle_data_mode(data.as_ref(), &mut utf8_buffer)?;

        let mut output = Vec::with_capacity(data.len());
        self.write_and_signcrypt(message_data_ref, data_encoding, false, &mut output)?;
        Ok(output)
    }

    /// Generates a session key that is used for the encryption.
    ///
    /// Considers the recipient preferences and internal profile for algorithm selection.
    ///
    /// # Example
    ///
    /// ```rust
    /// use proton_rpgp::{AsPublicKeyRef, DataEncoding, Encryptor, PrivateKey};
    ///
    /// let key = PrivateKey::import_unlocked(include_bytes!("../test-data/keys/private_key_v4.asc"), DataEncoding::Armored)
    ///     .expect("Failed to import key");
    ///
    /// let session_key = Encryptor::default()
    ///     .with_encryption_key(key.as_public_key())
    ///     .generate_session_key()
    ///     .expect("Failed generate session key");
    /// ```
    pub fn generate_session_key(self) -> crate::Result<SessionKey> {
        self.check_encryption_tools()?;
        let encryption_keys = self.select_encryption_keys()?;

        let recipients_algorithm_selection = RecipientsAlgorithms::select(
            self.message_symmetric_algorithm,
            self.message_aead_cipher_suite,
            self.message_compression,
            &encryption_keys,
            self.profile(),
        );

        let session_key = match recipients_algorithm_selection.encryption_mechanism() {
            EncryptionMechanism::SeipdV1(symmetric_key_algorithm) => {
                SessionKey::generate_for_seipdv1(symmetric_key_algorithm, self.profile())
            }
            EncryptionMechanism::SeipdV2(symmetric_key_algorithm, _) => {
                SessionKey::generate_for_seipdv2(symmetric_key_algorithm, self.profile())
            }
        };

        Ok(session_key)
    }

    fn write_and_signcrypt<R: Read, W: Write>(
        self,
        data: R,
        data_encoding: DataEncoding,
        extract_session_key: bool,
        mut dest: W,
    ) -> Result<Option<SessionKey>, EncryptionError> {
        self.check_encryption_tools()?;
        let encryption_keys = self.select_encryption_keys()?;

        if let Some(stats_collector) = &self.observer {
            stats_collector.observe_encryption_keys(&encryption_keys);
        }

        let recipients_algorithm_selection = RecipientsAlgorithms::select(
            self.message_symmetric_algorithm,
            self.message_aead_cipher_suite,
            self.message_compression,
            &encryption_keys,
            self.profile(),
        );

        let mut message_builder = MessageBuilder::from_reader("", data);

        // Set the compression algorithm if any.
        if recipients_algorithm_selection.compression_algorithm
            != CompressionAlgorithm::Uncompressed
        {
            message_builder.compression(recipients_algorithm_selection.compression_algorithm);
        }

        let mut rng = self.profile().rng();
        let encryption_mechanism = if let Some(session_key) = &self.session_key {
            session_key.encryption_mechanism(&recipients_algorithm_selection, self.profile())?
        } else {
            recipients_algorithm_selection.encryption_mechanism()
        };

        if let Some(observer) = &self.observer {
            observer.observe_encryption_mechanism(&encryption_mechanism);
        }

        let resolved_data_encoding = data_encoding.resolve_for_write();
        let revealed_session_key = match encryption_mechanism {
            EncryptionMechanism::SeipdV1(symmetric_key_algorithm) => {
                let (seipd_v1_builder, session_key) = create_seipd_v1_message_builder(
                    message_builder,
                    &encryption_keys,
                    &self.passphrases,
                    symmetric_key_algorithm,
                    extract_session_key,
                    self.session_key.as_deref(),
                    &mut rng,
                    self.profile(),
                )?;

                self.write_and_sign(
                    seipd_v1_builder,
                    &encryption_keys,
                    &recipients_algorithm_selection,
                    resolved_data_encoding,
                    rng,
                    &mut dest,
                )?;

                session_key
            }
            EncryptionMechanism::SeipdV2(symmetric_key_algorithm, aead_algorithm) => {
                let (seipd_v2_builder, session_key) = create_seipd_v2_message_builder(
                    message_builder,
                    &encryption_keys,
                    &self.passphrases,
                    symmetric_key_algorithm,
                    aead_algorithm,
                    self.message_aead_chunk_size,
                    extract_session_key,
                    self.session_key.as_deref(),
                    &mut rng,
                    self.profile(),
                )?;

                self.write_and_sign(
                    seipd_v2_builder,
                    &encryption_keys,
                    &recipients_algorithm_selection,
                    resolved_data_encoding,
                    rng,
                    &mut dest,
                )?;

                session_key
            }
        };

        Ok(revealed_session_key)
    }

    /// Helper function to optionally sign the message and write it to the output.
    fn write_and_sign<RAND, W, R, E>(
        &self,
        message_builder: MessageBuilder<R, E>,
        encryption_keys: &[PublicComponentKey<'_>],
        recipients_algorithm_selection: &RecipientsAlgorithms,
        data_encoding: ResolvedDataEncoding,
        rng: RAND,
        output: W,
    ) -> Result<(), EncryptionError>
    where
        RAND: Rng + CryptoRng,
        W: Write,
        R: Read,
        E: Encryption,
    {
        let signing_keys = self
            .signer
            .select_signing_keys()
            .map_err(EncryptionError::SigningKeySelection)?;

        if let Some(observer) = &self.observer {
            let key_views: Vec<_> = signing_keys
                .iter()
                .map(PrivateComponentKey::public_view)
                .collect();
            observer.observe_signing_keys(&key_views);
        }

        let signed_builder = self.signer.sign_message(
            message_builder,
            &signing_keys,
            Some(recipients_algorithm_selection),
        )?;

        message_to_writer(encryption_keys, signed_builder, data_encoding, rng, output)?;

        Ok(())
    }

    /// Helper function to select the encryption keys to use for the encryption.
    fn select_encryption_keys(&self) -> Result<Vec<PublicComponentKey<'_>>, KeyValidationError> {
        self.encryption_keys
            .iter()
            .map(|key| key.inner.encryption_key(self.signer.date, self.profile()))
            .collect()
    }

    /// Checks if the encryptor has any encryption tools to use.
    fn check_encryption_tools(&self) -> Result<(), EncryptionError> {
        if self.encryption_keys.is_empty()
            && self.passphrases.is_empty()
            && self.session_key.is_none()
        {
            return Err(EncryptionError::MissingEncryptionTools);
        }
        Ok(())
    }

    /// Handles the detached signature operation in encryption.
    fn handle_detached_signature_operation(
        &mut self,
        data: &[u8],
    ) -> Result<Option<ExternalDetachedSignature<'static>>, EncryptionError> {
        match self.detached_signature_op {
            SignDetachedOperation::None => Ok(None),
            SignDetachedOperation::Unencrypted | SignDetachedOperation::Encrypted => {
                let mut replaced_signer = Signer::new(self.signer.profile.clone());
                replaced_signer.data_mode = self.signer.data_mode;
                let signer = mem::replace(&mut self.signer, replaced_signer);
                let signature = signer
                    .sign_detached(data, DataEncoding::Unarmored)
                    .map_err(SigningError::from)?;
                if self.detached_signature_op == SignDetachedOperation::Unencrypted {
                    Ok(Some(ExternalDetachedSignature::new_unencrypted(
                        signature,
                        DataEncoding::Unarmored,
                    )))
                } else {
                    let mut encryptor = self.clone();
                    encryptor.signer.data_mode = DataMode::Binary;
                    let encrypted_signature = encryptor
                        .encrypt_raw(&signature, DataEncoding::Unarmored)
                        .map_err(EncryptionError::from)?;
                    Ok(Some(ExternalDetachedSignature::new_encrypted(
                        encrypted_signature,
                        DataEncoding::Unarmored,
                    )))
                }
            }
        }
    }

    /// Wraps a detached signature reader if necessary.
    fn handle_detached_signature_hash_stream<'b, R: Read + 'b>(
        &mut self,
        data: R,
    ) -> Result<(Option<DetachedSignatureHandle<'a>>, Box<dyn Read + 'b>), EncryptionError>
    where
        'a: 'b,
    {
        if self.detached_signature_op == SignDetachedOperation::None {
            Ok((None, Box::new(data)))
        } else {
            let mut replaced_signer = Signer::new(self.signer.profile.clone());
            replaced_signer.data_mode = self.signer.data_mode;
            let signer = mem::replace(&mut self.signer, replaced_signer);
            let inner = signer.sign_detached_inner_stream(data)?;
            Ok((
                Some(DetachedSignatureHandle {
                    tracker: inner.external_hash_tracker(),
                    signer,
                }),
                Box::new(inner),
            ))
        }
    }

    /// Handle the detached signature creation if required by the handle.
    fn handle_detached_signature_sign_stream(
        &mut self,
        detached_singer: Option<DetachedSignatureHandle<'a>>,
    ) -> Result<Option<ExternalDetachedSignature<'static>>, EncryptionError> {
        match self.detached_signature_op {
            SignDetachedOperation::None => Ok(None),
            SignDetachedOperation::Unencrypted | SignDetachedOperation::Encrypted => {
                let Some(handle) = detached_singer else {
                    return Err(EncryptionError::Signing(SigningError::NotAllDataRead));
                };
                let signature = handle
                    .tracker
                    .sign_with(&handle.signer, DataEncoding::Unarmored)?;
                if self.detached_signature_op == SignDetachedOperation::Unencrypted {
                    Ok(Some(ExternalDetachedSignature::new_unencrypted(
                        signature,
                        DataEncoding::Unarmored,
                    )))
                } else {
                    let mut encryptor = self.clone();
                    encryptor.signer.data_mode = DataMode::Binary;
                    let encrypted_signature = encryptor
                        .encrypt_raw(&signature, DataEncoding::Unarmored)
                        .map_err(EncryptionError::from)?;
                    Ok(Some(ExternalDetachedSignature::new_encrypted(
                        encrypted_signature,
                        DataEncoding::Unarmored,
                    )))
                }
            }
        }
    }

    /// Returns the internally set profile.
    fn profile(&self) -> &Profile {
        self.signer.profile()
    }
}

impl Default for Encryptor<'_> {
    fn default() -> Self {
        Self::new(DEFAULT_PROFILE.clone())
    }
}

impl<'a> From<Encryptor<'a>> for Signer<'a> {
    fn from(encryptor: Encryptor<'a>) -> Self {
        encryptor.signer
    }
}

/// Helper function to create the message builder for SEIPD v1.
#[allow(clippy::too_many_arguments)]
fn create_seipd_v1_message_builder<'b, RAND, R>(
    message_builder: MessageBuilder<'b, R, NoEncryption>,
    encryption_keys: &[PublicComponentKey<'_>],
    passphrases: &[Password],
    symmetric_key_algorithm: SymmetricKeyAlgorithm,
    extract_session_key: bool,
    provided_session_key: Option<&SessionKey>,
    mut rng: RAND,
    profile: &Profile,
) -> Result<(MessageBuilder<'b, R, EncryptionSeipdV1>, Option<SessionKey>), EncryptionError>
where
    RAND: Rng + CryptoRng,
    R: Read,
{
    let mut seipd_v1_builder = message_builder.seipd_v1(&mut rng, symmetric_key_algorithm);

    if let Some(session_key) = provided_session_key {
        seipd_v1_builder
            .set_session_key(session_key.export_bytes())
            .map_err(EncryptionError::DataEncryption)?;
    }

    for encryption_key in encryption_keys {
        seipd_v1_builder
            .encrypt_to_key(&mut rng, &encryption_key.public_key)
            .map_err(EncryptionError::PkeskEncryption)?;
    }

    for passphrase in passphrases {
        let s2k = profile.message_s2k_params();
        seipd_v1_builder
            .encrypt_with_password(s2k, passphrase)
            .map_err(EncryptionError::SkeskEncryption)?;
    }

    let revealed_session_key = extract_session_key.then(|| {
        SessionKey::new_for_seipdv1(
            seipd_v1_builder.session_key().as_ref(),
            symmetric_key_algorithm,
        )
    });

    Ok((seipd_v1_builder, revealed_session_key))
}

/// Helper function to create the message builder for SEIPD v2.
#[allow(clippy::too_many_arguments)]
fn create_seipd_v2_message_builder<'b, RAND, R>(
    message_builder: MessageBuilder<'b, R, NoEncryption>,
    encryption_keys: &[PublicComponentKey<'_>],
    passphrases: &[Password],
    symmetric_key_algorithm: SymmetricKeyAlgorithm,
    aead_algorithm: AeadAlgorithm,
    aead_chunk_size: ChunkSize,
    extract_session_key: bool,
    provided_session_key: Option<&SessionKey>,
    mut rng: RAND,
    profile: &Profile,
) -> Result<(MessageBuilder<'b, R, EncryptionSeipdV2>, Option<SessionKey>), EncryptionError>
where
    RAND: Rng + CryptoRng,
    R: Read,
{
    let mut seipd_v2_builder = message_builder.seipd_v2(
        &mut rng,
        symmetric_key_algorithm,
        aead_algorithm,
        aead_chunk_size,
    );

    if let Some(session_key) = provided_session_key {
        seipd_v2_builder
            .set_session_key(session_key.export_bytes())
            .map_err(EncryptionError::DataEncryption)?;
    }

    for encryption_key in encryption_keys {
        seipd_v2_builder
            .encrypt_to_key(&mut rng, &encryption_key.public_key)
            .map_err(EncryptionError::PkeskEncryption)?;
    }

    for passphrase in passphrases {
        let s2k = profile.message_s2k_params();
        seipd_v2_builder
            .encrypt_with_password(&mut rng, s2k, passphrase)
            .map_err(EncryptionError::SkeskEncryption)?;
    }

    let revealed_session_key = extract_session_key
        .then(|| SessionKey::new_for_seipdv2(seipd_v2_builder.session_key().as_ref()));

    Ok((seipd_v2_builder, revealed_session_key))
}

/// Helper function to write the message to the output.
fn message_to_writer<'a, RAND, W, R, E>(
    encryption_keys: &'a [PublicComponentKey<'a>],
    message_builder: MessageBuilder<R, E>,
    data_encoding: ResolvedDataEncoding,
    rng: RAND,
    output: W,
) -> Result<(), EncryptionError>
where
    RAND: Rng + CryptoRng,
    W: Write,
    R: Read,
    E: Encryption,
{
    match data_encoding {
        ResolvedDataEncoding::Armored => {
            let all_v6 = encryption_keys
                .iter()
                .all(|key| key.public_key.version() == KeyVersion::V6);
            message_builder
                .to_armored_writer(
                    rng,
                    ArmorOptions {
                        headers: None,
                        include_checksum: !all_v6,
                    },
                    output,
                )
                .map_err(EncryptionError::DataEncryption)?;
        }
        ResolvedDataEncoding::Unarmored => message_builder
            .to_writer(rng, output)
            .map_err(EncryptionError::DataEncryption)?,
    }
    Ok(())
}

/// Helper function to create the key packets encrypted with public keys.
fn create_pkesk_packets<R>(
    encryption_keys: &[PublicComponentKey<'_>],
    encryption_mechanism: EncryptionMechanism,
    mut rng: R,
    session_key_bytes: &RawSessionKey,
) -> Result<Vec<PublicKeyEncryptedSessionKey>, EncryptionError>
where
    R: Rng + CryptoRng,
{
    let mut pkesks = Vec::with_capacity(encryption_keys.len());
    // PKESKs
    for encryption_key in encryption_keys {
        let pkesk = match encryption_mechanism {
            EncryptionMechanism::SeipdV1(symmetric_key_algorithm) => {
                PublicKeyEncryptedSessionKey::from_session_key_v3(
                    &mut rng,
                    session_key_bytes,
                    symmetric_key_algorithm,
                    &encryption_key.public_key,
                )
                .map_err(EncryptionError::PkeskEncryption)?
            }
            EncryptionMechanism::SeipdV2(_, _) => {
                PublicKeyEncryptedSessionKey::from_session_key_v6(
                    &mut rng,
                    session_key_bytes,
                    &encryption_key.public_key,
                )
                .map_err(EncryptionError::PkeskEncryption)?
            }
        };
        pkesks.push(pkesk);
    }
    Ok(pkesks)
}

/// Helper function to create the key packets with passphrases.
fn create_skesk_packets<R>(
    passphrases: &[Password],
    encryption_mechanism: EncryptionMechanism,
    mut rng: R,
    session_key_bytes: &RawSessionKey,
    profile: &Profile,
) -> Result<Vec<SymKeyEncryptedSessionKey>, EncryptionError>
where
    R: Rng + CryptoRng,
{
    let mut skesks = Vec::with_capacity(passphrases.len());
    for password in passphrases {
        let skesk = match encryption_mechanism {
            EncryptionMechanism::SeipdV1(sym_alg) => {
                let s2k = profile.message_s2k_params();
                SymKeyEncryptedSessionKey::encrypt_v4(password, session_key_bytes, s2k, sym_alg)
                    .map_err(EncryptionError::SkeskEncryption)?
            }
            EncryptionMechanism::SeipdV2(sym_alg, aead_alg) => {
                let s2k = profile.message_s2k_params();
                SymKeyEncryptedSessionKey::encrypt_v6(
                    &mut rng,
                    password,
                    session_key_bytes,
                    s2k,
                    sym_alg,
                    aead_alg,
                )
                .map_err(EncryptionError::SkeskEncryption)?
            }
        };
        skesks.push(skesk);
    }
    Ok(skesks)
}

/// Helper function to write the key packets to a byte vector.
fn key_packets_to_bytes(
    pkesks: &[PublicKeyEncryptedSessionKey],
    skesks: &[SymKeyEncryptedSessionKey],
) -> Result<Vec<u8>, EncryptionError> {
    let output_len = pkesks
        .iter()
        .map(PacketTrait::write_len_with_header)
        .sum::<usize>()
        + skesks
            .iter()
            .map(PacketTrait::write_len_with_header)
            .sum::<usize>();

    let mut output = Vec::with_capacity(output_len);
    for pkesk in pkesks {
        pkesk
            .to_writer_with_header(&mut output)
            .map_err(EncryptionError::DataEncryption)?;
    }
    for skesk in skesks {
        skesk
            .to_writer_with_header(&mut output)
            .map_err(EncryptionError::DataEncryption)?;
    }
    Ok(output)
}

#[derive(Default, Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum SignDetachedOperation {
    #[default]
    None,
    // Insecure when signing low-entropy data.
    Unencrypted,
    Encrypted,
}

struct DetachedSignatureHandle<'a> {
    tracker: ExternalHashTracker,
    signer: Signer<'a>,
}
