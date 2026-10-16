use std::borrow::Cow;

use pgp::{
    composed::Message,
    packet::{PacketTrait, Signature, Subpacket, SubpacketData},
    types::KeyId,
};

use crate::{
    check_signature_details, types::CheckUnixTime, AsPublicKeyRef, KeyInfo, MessageProcessingError,
    MessageSignatureError, Profile, PublicComponentKey, PublicKeySelectionExt,
    SignatureContextError, SignatureError, SignatureExt, SignatureUsage, UnixTime,
    VerificationContext, VerificationResultUtilityError, FUTURE_SIGNATURE_ERROR_MESSAGE,
    LIB_ERROR_PREFIX,
};

/// The result of verifying signature in an `OpenPGP` message.
pub type VerificationResult = Result<VerificationInformation, VerificationError>;

/// Gives information about the verified signature.
#[derive(Debug, Clone)]
pub struct VerificationInformation {
    /// The `OpenPGP` key ID that the selected signature is signed with.
    pub key_id: KeyId,

    /// The creation time of the selected signature.
    pub signature_creation_time: UnixTime,

    /// The `OpenPGP` signature that has been verified.
    pub signature: Signature,

    /// The `OpenPGP` signature that have not been verified.
    pub unverified_signatures: Vec<Signature>,
}

impl From<Signature> for VerificationInformation {
    fn from(signature: Signature) -> Self {
        Self::new(signature, Vec::new(), None)
    }
}

impl VerificationInformation {
    pub fn new(
        signature: Signature,
        unverified_signatures: Vec<Signature>,
        info: Option<KeyInfo>,
    ) -> Self {
        let key_id = if let Some(info) = info {
            info.key_id
        } else {
            let generic_identifier = signature.issuer_generic_identifier().into_iter().next();
            // Fallback to the first issuer if no key info is provided.
            generic_identifier
                .and_then(|id| id.key_id())
                .unwrap_or(KeyId::new([0_u8; 8]))
        };

        Self {
            key_id,
            signature_creation_time: signature.unix_created_at().unwrap_or_default(),
            signature,
            unverified_signatures,
        }
    }

    pub fn signature_bytes(&self) -> crate::Result<Vec<u8>> {
        let mut bytes = Vec::with_capacity(self.signature.write_len_with_header());
        self.signature
            .to_writer_with_header(&mut bytes)
            .map_err(VerificationResultUtilityError::SignatureBytes)?;
        Ok(bytes)
    }

    pub fn all_signature_bytes(&self) -> crate::Result<Vec<u8>> {
        let mut signatures = Vec::new();

        let all_signatures =
            std::iter::once(&self.signature).chain(self.unverified_signatures.iter());

        for signature in all_signatures {
            signature
                .to_writer_with_header(&mut signatures)
                .map_err(VerificationResultUtilityError::SignatureBytes)?;
        }
        Ok(signatures)
    }
}

/// Errors that can occur when verifying a signature.
#[derive(Debug, Clone, thiserror::Error)]
pub enum VerificationError {
    #[error("{LIB_ERROR_PREFIX}: No signature found")]
    NotSigned,

    #[error("{LIB_ERROR_PREFIX}: No valid verification keys found for signature {}: {}", .0.key_id, .1)]
    NoVerifier(Box<VerificationInformation>, String),

    #[error("{LIB_ERROR_PREFIX}: Signature verification failed: {1}")]
    Failed(Box<VerificationInformation>, String),

    /// Signature context did not match verification context.
    #[error("{LIB_ERROR_PREFIX}: Signature context does not match the verification context: {1}")]
    BadContext(Box<VerificationInformation>, String),

    /// Unknown error occurred.
    #[error("{LIB_ERROR_PREFIX}: Runtime error: {0}")]
    RuntimeError(String),
}

impl VerificationError {
    pub fn verification_information(&self) -> Option<&VerificationInformation> {
        match self {
            VerificationError::NotSigned | VerificationError::RuntimeError(_) => None,
            VerificationError::NoVerifier(info, _)
            | VerificationError::Failed(info, _)
            | VerificationError::BadContext(info, _) => Some(info),
        }
    }

    pub fn into_verification_information(self) -> Option<VerificationInformation> {
        match self {
            VerificationError::NotSigned | VerificationError::RuntimeError(_) => None,
            VerificationError::NoVerifier(info, _)
            | VerificationError::Failed(info, _)
            | VerificationError::BadContext(info, _) => Some(*info),
        }
    }
}

/// Provides utility functions for a signature verification result.
#[derive(Debug, Clone)]
pub struct VerificationResultUtility<'a>(Cow<'a, VerificationResult>);

impl<'a> From<&'a VerificationResult> for VerificationResultUtility<'a> {
    fn from(result: &'a VerificationResult) -> Self {
        Self(Cow::Borrowed(result))
    }
}

impl From<VerificationResult> for VerificationResultUtility<'static> {
    fn from(result: VerificationResult) -> Self {
        Self(Cow::Owned(result))
    }
}

impl From<VerificationResultUtility<'_>> for VerificationResult {
    fn from(result: VerificationResultUtility<'_>) -> Self {
        result.0.into_owned()
    }
}

impl VerificationResultUtility<'_> {
    pub fn verification_success(&self) -> bool {
        self.0.is_ok()
    }

    pub fn verification_failed(&self) -> bool {
        self.0.is_err()
    }

    pub fn verification_information(&self) -> Option<&VerificationInformation> {
        match self.0.as_ref() {
            Ok(info) => Some(info),
            Err(err) => err.verification_information(),
        }
    }

    /// Serialize the selected signature bytes.
    ///
    /// If there were no signatures, an empty vector is returned.
    pub fn selected_signature_bytes(&self) -> crate::Result<Vec<u8>> {
        self.verification_information()
            .map_or(Ok(Vec::new()), VerificationInformation::signature_bytes)
    }

    /// Serialize all signature bytes.
    ///
    /// If there were no signatures, an empty vector is returned.
    pub fn all_signature_bytes(&self) -> crate::Result<Vec<u8>> {
        self.verification_information()
            .map_or(Ok(Vec::new()), VerificationInformation::all_signature_bytes)
    }
}

/// A creator for verification results.
pub(crate) struct VerificationResultCreator {}

impl VerificationResultCreator {
    /// Create a verification result from a list of verified signatures.
    ///
    /// Selects result for the first signature that is valid or the last one if no valid signature is found.
    pub fn with_signatures(verifications: Vec<SignatureVerificationResult>) -> VerificationResult {
        if verifications.is_empty() {
            return Err(VerificationError::NotSigned);
        }

        let selected_signature_index = verifications
            .iter()
            .position(|verification| verification.verification_result.is_ok())
            .unwrap_or(verifications.len() - 1);

        let mut selected_verification = None;
        let unverified_signatures = verifications
            .into_iter()
            .enumerate()
            .filter_map(|(id, verification)| {
                if id == selected_signature_index {
                    selected_verification = Some(verification);
                    None
                } else {
                    Some(verification.signature.clone())
                }
            })
            .collect();

        let selected_verification = selected_verification.ok_or(VerificationError::NotSigned)?;

        let verification_info = VerificationInformation::new(
            selected_verification.signature,
            unverified_signatures,
            selected_verification.verified_by,
        );

        match selected_verification.verification_result {
            Ok(()) => Ok(verification_info),
            Err(MessageSignatureError::Failed(err)) => Err(VerificationError::Failed(
                Box::new(verification_info),
                err.to_string(),
            )),
            Err(MessageSignatureError::NoMatchingKey(err)) => Err(VerificationError::NoVerifier(
                Box::new(verification_info),
                err.to_string(),
            )),
            Err(MessageSignatureError::Context(err)) => Err(VerificationError::BadContext(
                Box::new(verification_info),
                err.to_string(),
            )),
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub enum VerificationInput<'a> {
    Message(&'a Message<'a>, usize),
    Data(&'a [u8]),
    Hash(&'a [u8]),
}

/// Represents an internal verified signature.
#[derive(Debug)]
pub(crate) struct SignatureVerificationResult {
    /// The signature that has been verified.
    pub signature: Signature,

    /// The key information that has been used to verify the signature.
    pub verified_by: Option<KeyInfo>,

    /// The result of the verification.
    pub verification_result: Result<(), MessageSignatureError>,
}

impl SignatureVerificationResult {
    /// Create a verified signature by verifying the signature with the given public keys.
    pub fn create_by_verifying(
        date: CheckUnixTime,
        signature: Signature,
        with_public_keys: &[impl AsPublicKeyRef],
        data_to_verify: VerificationInput<'_>,
        context: Option<&VerificationContext>,
        profile: &Profile,
    ) -> Self {
        // Select the verification keys from the list of public keys.
        // The keys are selected based on the signature issuer's key ID list.
        let verification_candidates =
            match Self::select_verification_keys(&signature, with_public_keys, date, profile) {
                Ok(candidates) => candidates,
                Err(error) => {
                    return Self {
                        signature,
                        verified_by: None,
                        verification_result: Err(error),
                    };
                }
            };

        // Try to verify the signature with the selected keys.
        // Most of the time, there is only one key in the list, but there
        // might be collisions on the key id.
        let mut verification_result = Ok(());
        let mut verified_by = None;
        for candidate in verification_candidates {
            verification_result = match data_to_verify {
                VerificationInput::Message(message, signature_index) => candidate
                    .verify_message_signature_with_message(
                        date,
                        message,
                        signature_index,
                        context,
                        profile,
                    ),
                VerificationInput::Data(input_data) => candidate
                    .verify_message_signature_with_data(
                        date, &signature, input_data, context, profile,
                    ),
                VerificationInput::Hash(hash) => {
                    candidate.verify_signature_with_hash(date, &signature, hash, context, profile)
                }
            };
            if verification_result.is_ok() {
                verified_by = Some(candidate.into());
                break;
            }
        }

        Self {
            signature,
            verified_by,
            verification_result,
        }
    }

    /// Helper function to select verification keys for a signature.
    fn select_verification_keys<'a>(
        signature: &Signature,
        public_keys: &'a [impl AsPublicKeyRef],
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<Vec<PublicComponentKey<'a>>, MessageSignatureError> {
        let mut verification_candidates = Vec::new();
        let signature_creation_time = signature.unix_created_at()?;

        let mut key_selection_errors = Vec::new();
        for key in public_keys {
            let keys = match key
                .as_public_key()
                .as_signed_public_key()
                .verification_keys(
                    signature_creation_time.into(),
                    signature.issuer_generic_identifier(),
                    SignatureUsage::Sign,
                    profile,
                ) {
                Ok(keys) => keys,
                Err(error) => {
                    let result = if profile.allow_insecure_verification_with_reformatted_keys()
                        && error.to_string().contains(FUTURE_SIGNATURE_ERROR_MESSAGE)
                    {
                        key.as_public_key()
                            .as_signed_public_key()
                            .verification_keys(
                                date,
                                signature.issuer_generic_identifier(),
                                SignatureUsage::Sign,
                                profile,
                            )
                    } else {
                        Err(error)
                    };
                    match result {
                        Ok(keys) => keys,
                        Err(error) => {
                            key_selection_errors.push(error);
                            continue;
                        }
                    }
                }
            };
            verification_candidates.extend(keys);
        }
        if verification_candidates.is_empty() {
            return Err(MessageSignatureError::NoMatchingKey(
                key_selection_errors.into(),
            ));
        }
        Ok(verification_candidates)
    }
}

/// Additional checks for signatures that are verified in a message.
pub(crate) fn check_message_signature_details(
    date: CheckUnixTime,
    signature: &Signature,
    selected_key: &PublicComponentKey<'_>,
    context: Option<&VerificationContext>,
    profile: &Profile,
) -> Result<(), MessageSignatureError> {
    // Check the used message hash algorithm, might reject more than in the
    // default rejection.
    if profile.reject_message_hash_algorithm(signature.hash_alg()) {
        return Err(SignatureError::InvalidHash(signature.hash_alg()).into());
    }
    // Check the signature details of the signature.
    check_signature_details(signature, date, profile)?;

    // Check if the signature is older than the key.
    let signature_creation_time = signature.unix_created_at()?;
    let key_creation_time = selected_key.unix_created_at();
    if signature_creation_time < key_creation_time {
        return Err(SignatureError::SignatureOlderThanKey {
            signature_date: signature_creation_time,
            key_date: key_creation_time,
        }
        .into());
    }

    let Some(config) = signature.config() else {
        return Err(SignatureError::ConfigAccess.into());
    };

    // Check the Proton signature context.
    if let Some(verification_context) = context {
        // If there is a verification context, we check if signature notations match the verification context.
        verification_context.check_subpackets(&config.hashed_subpackets, date)?;
    } else if let Some(criticial_context) =
        // If there is no verification context, we check if there is a critical Proton context in the notations.
        VerificationContext::filter_context(
            &config.hashed_subpackets,
        )
        .find_map(|subpacket| match subpacket {
            Subpacket {
                is_critical: true,
                data: SubpacketData::Notation(notation),
                ..
            } => Some(String::from_utf8_lossy(notation.value.as_ref()).to_string()),
            _ => None,
        })
    {
        return Err(MessageSignatureError::Context(
            SignatureContextError::CriticialContext(criticial_context),
        ));
    }
    Ok(())
}

/// Extension trait for [`pgp::composed::Message`] to verify signatures with our logic.
pub(crate) trait MessageVerificationExt {
    /// Verifies the signatures of the message.
    ///
    /// The data has to be fully read before calling this function.
    fn verify_message_signatures(
        &self,
        date: CheckUnixTime,
        keys: &[impl AsPublicKeyRef],
        context: Option<&VerificationContext>,
        profile: &Profile,
    ) -> Result<Vec<SignatureVerificationResult>, MessageProcessingError>;
}

impl MessageVerificationExt for Message<'_> {
    fn verify_message_signatures(
        &self,
        date: CheckUnixTime,
        keys: &[impl AsPublicKeyRef],
        context: Option<&VerificationContext>,
        profile: &Profile,
    ) -> Result<Vec<SignatureVerificationResult>, MessageProcessingError> {
        match self {
            Message::Signed { reader, .. } => {
                let max_signatures = profile.max_number_of_message_signatures();
                let signature_count = reader.num_signatures().min(max_signatures);

                let verification_results: Result<Vec<_>, _> = (0..signature_count)
                    .map(|index| {
                        let signature = reader
                            .signature(index)
                            .ok_or(MessageProcessingError::NotFullyRead)?;
                        Ok(SignatureVerificationResult::create_by_verifying(
                            date,
                            signature.clone(),
                            keys,
                            VerificationInput::Message(self, index),
                            context,
                            profile,
                        ))
                    })
                    .collect();

                verification_results
            }
            Message::Literal { .. } => Ok(Vec::new()),
            Message::Compressed { .. } => Err(MessageProcessingError::Compression),
            Message::Encrypted { .. } => Err(MessageProcessingError::Encrypted),
        }
    }
}
