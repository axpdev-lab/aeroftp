use std::borrow::Cow;

use pgp::{
    composed::{DecryptionOptions, Message, PlainSessionKey, TheRing},
    packet::PublicKeyEncryptedSessionKey,
    types::{PkeskVersion, Seipdv1ReadMode},
};

use crate::{
    CheckUnixTime, DecryptionError, Decryptor, GenericKeyIdentifier, PrivateKey,
    PrivateKeySelectionExt, Profile, PublicKeyExt,
};

pub trait MessageDecryptionExt<'a> {
    /// Decrypts the message using the given decryptor.
    fn decrypt_with_decryptor(
        self,
        decryptor: &Decryptor,
        seipdv1_read_mode: Seipdv1ReadMode,
    ) -> Result<Message<'a>, DecryptionError>;
}

impl<'a> MessageDecryptionExt<'a> for Message<'a> {
    /// Decrypts the message using the given decryptor.
    ///
    /// This is a helper function for the decryptor.
    fn decrypt_with_decryptor(
        self,
        decryptor: &Decryptor,
        seipdv1_read_mode: Seipdv1ReadMode,
    ) -> Result<Message<'a>, DecryptionError> {
        let Message::Encrypted {
            esk,
            edata: _,
            is_nested: _,
        } = &self
        else {
            return Err(DecryptionError::UnexpectedPlaintext);
        };

        // Try to extract the session key to decrypt the message with.
        // The first decryptable session key is used.
        let mut session_keys = Vec::with_capacity(1);

        if let Some(session_key) = &decryptor.session_key {
            session_keys.push(session_key.clone().into_owned().into());
        }

        if session_keys.is_empty() {
            let session_key = decryptor.decrypt_session_key_inner(esk.iter().map(Cow::Borrowed))?;
            session_keys.push(session_key.into());
        }

        let decrypt_options = DecryptionOptions::new()
            .enable_gnupg_aead()
            .set_seipdv1_read_mode(seipdv1_read_mode);

        // Use the session keys to decrypt the message with
        // `the ring`
        let the_ring = TheRing {
            session_keys,
            decrypt_options,
            ..Default::default()
        };

        let (message, _) = self.decrypt_the_ring(the_ring, false)?;

        Ok(message)
    }
}

pub(crate) trait PkeskExt {
    /// Returns the generic identifier of the PKESK.
    fn generic_identifier(&self) -> Option<GenericKeyIdentifier>;
}

impl PkeskExt for PublicKeyEncryptedSessionKey {
    fn generic_identifier(&self) -> Option<GenericKeyIdentifier> {
        match self.version() {
            PkeskVersion::V3 => match self.id() {
                Ok(key_id) => {
                    if key_id.is_wildcard() {
                        Some(GenericKeyIdentifier::Wildcard)
                    } else {
                        Some(GenericKeyIdentifier::KeyId(*key_id))
                    }
                }
                Err(_) => None,
            },
            PkeskVersion::V6 => match self.fingerprint() {
                Ok(Some(fp)) => Some(GenericKeyIdentifier::Fingerprint(fp.clone())),
                Ok(None) => Some(GenericKeyIdentifier::Wildcard),
                Err(_) => None,
            },
            PkeskVersion::Other(_) => None,
        }
    }
}

// Helper function to handle PKESK session key decryption.
pub(crate) fn handle_pkesk_decryption<'a>(
    pkesk: &PublicKeyEncryptedSessionKey,
    decryption_keys: impl Iterator<Item = &'a PrivateKey>,
    allow_forwarding_decryption: bool,
    profile: &Profile,
) -> Result<PlainSessionKey, DecryptionError> {
    let Some(generic_identifier) = pkesk.generic_identifier() else {
        return Err(DecryptionError::PkeskNoIssuer);
    };
    let mut extracted_sks = None;
    let mut errors = Vec::new();

    // Try to decrypt the PKESK with decryption keys that match the PKESK.
    for decryption_key in decryption_keys.filter(|dk| check_pkesk_match(&generic_identifier, dk)) {
        // Only allow verified decryption keys.
        let decryption_keys_result = decryption_key.secret.decryption_keys(
            CheckUnixTime::disable(), // disable time checks for decryption keys.
            Some(generic_identifier.clone()),
            allow_forwarding_decryption,
            profile,
        );

        let decryption_keys = match decryption_keys_result {
            Ok(keys) => keys,
            Err(e) => {
                errors.push(DecryptionError::KeySelection(
                    Box::new(generic_identifier.clone()),
                    e,
                ));
                continue;
            }
        };

        // Try to decrypt with the valid component decryption keys.
        if let Some(sk) = decryption_keys.into_iter().find_map(|dk| {
            match dk.private_key.decrypt_session_key(pkesk) {
                Ok(sk) => Some(sk),
                Err(err) => {
                    errors.push(DecryptionError::SinglePkeskDecryption(err));
                    None
                }
            }
        }) {
            extracted_sks = Some(sk);
            break;
        }
    }

    if extracted_sks.is_none() && errors.is_empty() {
        return Err(DecryptionError::PkeskNoMatchingKey(Box::new(
            generic_identifier,
        )));
    }

    extracted_sks.ok_or(DecryptionError::PkeskDecryption(
        Box::new(generic_identifier),
        errors.into(),
    ))
}

/// helper function to check if a decryption key matches a PKESK.
fn check_pkesk_match(pkesk_identifier: &GenericKeyIdentifier, decryption_key: &PrivateKey) -> bool {
    decryption_key
        .secret
        .secret_subkeys
        .iter()
        .map(|k| k.generic_identifier())
        .any(|identifier| &identifier == pkesk_identifier)
        || &decryption_key.as_signed_public_key().generic_identifier() == pkesk_identifier
}
