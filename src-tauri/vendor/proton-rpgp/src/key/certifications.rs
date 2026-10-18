use pgp::{
    composed::{SignedPublicKey, SignedPublicSubKey, SignedSecretKey, SignedSecretSubKey},
    packet::{self},
    ser::Serialize,
    types::{SignedUser, Tag, VerifyingKey},
};

use crate::{
    check_signature_details, types::CheckUnixTime, KeyCertificationSelectionError, Profile,
    SignatureError, SignatureExt,
};

/// Trait for selecting the best self-certification for a key part.
///
/// This can be used on `UserIds`, Signed Subkeys, Signed Primary key.
pub trait CertificationSelectionExt {
    /// Iterates over the self-certifications of this key part ignoring revocations.
    fn iter_self_certifications(&self) -> impl DoubleEndedIterator<Item = &packet::Signature>;

    /// Iterates over the self-revocations of this key part.
    fn iter_self_revocations(&self) -> impl Iterator<Item = &packet::Signature>;

    /// Verifies that the given signature is a valid self-certification for this key part.
    fn verify_certification<K: VerifyingKey + Serialize>(
        &self,
        key: &K,
        key_signature: &packet::Signature,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(), SignatureError>;

    /// Selects the latest valid self-certification for this key part.
    ///
    /// If there are multiple valid self-certifications, it will select the one that is the most recent.
    /// If date is zero, the date checks are disabled.
    ///
    /// # Errors
    ///
    /// If there are no valid self-certifications, it will return [`KeyCertificationSelectionError::NoSelfCertification`].
    fn latest_valid_self_certification<K: VerifyingKey + Serialize>(
        &self,
        primary_key: &K,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<&packet::Signature, KeyCertificationSelectionError> {
        let mut selected_signature: Option<&packet::Signature> = None;
        let mut failures: Vec<SignatureError> = Vec::new();

        for signature in self
            .iter_self_certifications()
            .rev()
            .filter(|sig| sig.is_issued_by(&primary_key) && !sig.is_revocation())
        {
            let sig_creation_time = signature.unix_created_at()?;
            let selected_sig_creation_time = selected_signature
                .map(SignatureExt::unix_created_at)
                .transpose()?;

            // Check if the signature is not in the future with date as a reference.
            if let Some(date) = date.at() {
                if date < sig_creation_time {
                    failures.push(SignatureError::FutureSignature(sig_creation_time));
                    continue;
                }
            }

            // Skip if we already have a newer valid signature.
            if let Some(selected_time) = selected_sig_creation_time {
                if selected_time >= sig_creation_time {
                    continue;
                }
            }

            // Verify the certification signature.
            match self.verify_certification(primary_key, signature, date, profile) {
                Ok(()) => selected_signature = Some(signature),
                Err(err) => failures.push(err),
            }
        }

        selected_signature
            .ok_or_else(|| KeyCertificationSelectionError::NoSelfCertification(failures.into()))
    }

    /// Checks if there is a valid revocation for the given date and self-certification.
    ///
    /// If date is zero, the date checks are disabled.
    /// Note that third-party revocation signatures are not supported.
    fn revoked<K: VerifyingKey + Serialize>(
        &self,
        primary_key: &K,
        self_signature: Option<&packet::Signature>,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> bool {
        let self_signature_creation_time = if let Some(self_signature) = self_signature {
            self_signature
                .unix_created_at()
                .map_or(CheckUnixTime::disable(), CheckUnixTime::from)
        } else {
            CheckUnixTime::disable()
        };

        // Helper closure to verify signature + details
        let is_valid = |sig: &packet::Signature, date: CheckUnixTime| {
            self.verify_certification(primary_key, sig, date, profile)
                .is_ok()
        };

        // 1. Check hard revocations
        if self
            .iter_self_revocations()
            .filter(|sig| sig.is_issued_by(primary_key) && sig.is_hard_revocation())
            .any(|sig| is_valid(sig, CheckUnixTime::disable()))
        {
            return true;
        }

        // 2. Check soft revocations
        self.iter_self_revocations()
            .filter(|sig| sig.is_issued_by(primary_key) && sig.is_revocation())
            .filter_map(|sig| sig.unix_created_at().ok().map(|time| (sig, time)))
            .filter(|(_, time)| {
                if let Some(self_signature_creation_time) = self_signature_creation_time.at() {
                    *time >= self_signature_creation_time
                } else {
                    true
                }
            })
            .any(|(sig, _)| is_valid(sig, date))
    }

    /// Checks if the key part has a valid self-certification for the given date and profile and returns it.
    ///
    /// # Errors
    ///
    /// Returns an error if there is no valid self-certification or if the self-certification is revoked.
    fn check_validity<K: VerifyingKey + Serialize>(
        &self,
        primary_key: &K,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<&packet::Signature, KeyCertificationSelectionError> {
        let self_signature = self.latest_valid_self_certification(primary_key, date, profile)?;
        if self.revoked(primary_key, Some(self_signature), date, profile) {
            return Err(KeyCertificationSelectionError::Revoked(Box::new(
                self_signature.to_owned(),
            )));
        }
        Ok(self_signature)
    }
}

impl CertificationSelectionExt for SignedUser {
    fn iter_self_certifications(&self) -> impl DoubleEndedIterator<Item = &packet::Signature> {
        self.signatures
            .iter()
            .filter(|sig| sig.is_certification() && !sig.is_revocation())
    }

    fn iter_self_revocations(&self) -> impl Iterator<Item = &packet::Signature> {
        self.signatures
            .iter()
            .filter(|sig| sig.is_certification() && sig.is_revocation())
    }

    fn verify_certification<K: VerifyingKey + Serialize>(
        &self,
        key: &K,
        self_signature: &packet::Signature,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(), SignatureError> {
        self_signature
            .verify_certification(key, Tag::UserId, &self.id)
            .map_err(SignatureError::Verification)?;
        check_signature_details(self_signature, date, profile)
    }
}

impl CertificationSelectionExt for SignedPublicSubKey {
    fn iter_self_certifications(&self) -> impl DoubleEndedIterator<Item = &packet::Signature> {
        self.signatures.iter().filter(|sig| !sig.is_revocation())
    }

    fn iter_self_revocations(&self) -> impl Iterator<Item = &packet::Signature> {
        self.signatures.iter().filter(|sig| sig.is_revocation())
    }

    fn verify_certification<K: VerifyingKey + Serialize>(
        &self,
        signing_key: &K,
        self_signature: &packet::Signature,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(), SignatureError> {
        verify_subkey_signature(self_signature, signing_key, &self.key, date, profile)
    }
}

impl CertificationSelectionExt for SignedSecretSubKey {
    fn iter_self_certifications(&self) -> impl DoubleEndedIterator<Item = &packet::Signature> {
        self.signatures.iter().filter(|sig| !sig.is_revocation())
    }

    fn iter_self_revocations(&self) -> impl Iterator<Item = &packet::Signature> {
        self.signatures.iter().filter(|sig| sig.is_revocation())
    }

    fn verify_certification<K: VerifyingKey + Serialize>(
        &self,
        signing_key: &K,
        self_signature: &packet::Signature,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(), SignatureError> {
        verify_subkey_signature(
            self_signature,
            signing_key,
            self.key.public_key(),
            date,
            profile,
        )
    }
}

fn verify_subkey_signature<K, S>(
    self_signature: &packet::Signature,
    signing_key: &K,
    subkey: &S,
    date: CheckUnixTime,
    profile: &Profile,
) -> Result<(), SignatureError>
where
    K: VerifyingKey + Serialize,
    S: VerifyingKey + Serialize,
{
    self_signature
        .verify_subkey_binding(signing_key, subkey)
        .map_err(SignatureError::Verification)?;
    check_signature_details(self_signature, date, profile)?;

    if !self_signature.is_revocation() && self_signature.key_flags().sign() {
        match self_signature.embedded_signature() {
            Some(embedded) => {
                embedded
                    .verify_primary_key_binding(subkey, signing_key)
                    .map_err(SignatureError::Verification)?;
                check_signature_details(embedded, date, profile)?;
            }
            None => {
                return Err(SignatureError::MissingCrossSignature(
                    subkey.legacy_key_id(),
                ));
            }
        }
    }

    Ok(())
}

impl CertificationSelectionExt for SignedPublicKey {
    fn iter_self_certifications(&self) -> impl DoubleEndedIterator<Item = &packet::Signature> {
        self.details.direct_signatures.iter()
    }

    fn iter_self_revocations(&self) -> impl Iterator<Item = &packet::Signature> {
        self.details.revocation_signatures.iter()
    }

    fn verify_certification<K: VerifyingKey + Serialize>(
        &self,
        key: &K,
        key_signature: &packet::Signature,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(), SignatureError> {
        key_signature
            .verify_key(key)
            .map_err(SignatureError::Verification)?;
        check_signature_details(key_signature, date, profile)
    }
}

impl CertificationSelectionExt for SignedSecretKey {
    fn iter_self_certifications(&self) -> impl DoubleEndedIterator<Item = &packet::Signature> {
        self.details.direct_signatures.iter()
    }

    fn iter_self_revocations(&self) -> impl Iterator<Item = &packet::Signature> {
        self.details.revocation_signatures.iter()
    }

    fn verify_certification<K: VerifyingKey + Serialize>(
        &self,
        key: &K,
        key_signature: &packet::Signature,
        date: CheckUnixTime,
        profile: &Profile,
    ) -> Result<(), SignatureError> {
        key_signature
            .verify_key(key)
            .map_err(SignatureError::Verification)?;
        check_signature_details(key_signature, date, profile)
    }
}
