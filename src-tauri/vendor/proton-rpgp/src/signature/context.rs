use pgp::packet::{Notation, Subpacket, SubpacketData};

use std::{borrow::Cow, fmt};

use crate::{CheckUnixTime, MessageSignatureError, SignatureContextError, UnixTime};

pub const PROTON_CONTEXT_NOTATION_NAME: &str = "context@proton.ch";

/// Added to a signature to bind it to an application context.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SignatureContext {
    /// The value of the signature context.
    ///
    /// e.g., `attachment.mail.proton.me`
    pub value: String,

    /// Indicates if the signature context is critical.
    ///
    /// If critical, the signature willa also be rejected by non-Proton `OpenPGP` libraries
    /// due to an unknown critical notation.
    pub is_critical: bool,
}

impl SignatureContext {
    pub fn new(value: String, is_critical: bool) -> Self {
        Self { value, is_critical }
    }
}

impl From<SignatureContext> for Notation {
    fn from(context: SignatureContext) -> Self {
        Notation {
            readable: true,
            name: PROTON_CONTEXT_NOTATION_NAME.into(),
            value: context.value.into(),
        }
    }
}

impl<'a> From<&'a SignatureContext> for Cow<'a, SignatureContext> {
    fn from(context: &'a SignatureContext) -> Self {
        Cow::Borrowed(context)
    }
}

impl From<SignatureContext> for Cow<'_, SignatureContext> {
    fn from(context: SignatureContext) -> Self {
        Cow::Owned(context)
    }
}

impl fmt::Display for SignatureContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.is_critical {
            write!(f, "SignatureContext: [CRITICAL] {}", self.value)
        } else {
            write!(f, "SignatureContext: {}", self.value)
        }
    }
}

/// Allows to specify the expected application context of a signature.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct VerificationContext {
    /// The value of the verification context.
    pub value: String,

    /// Indicates if the context is required.
    pub is_required: bool,

    /// The unix timestamp after which the context is required.
    pub required_after: Option<UnixTime>,
}

impl VerificationContext {
    pub fn new(value: String, is_required: bool, required_after: Option<UnixTime>) -> Self {
        Self {
            value,
            is_required,
            required_after,
        }
    }

    /// Creates a new verification context that is always required.
    pub fn new_required(value: String) -> Self {
        Self::new(value, true, None)
    }

    fn is_required_at_time(&self, signature_time: CheckUnixTime) -> bool {
        if !self.is_required {
            return false;
        }
        let Some(signature_time) = signature_time.at() else {
            return true;
        };
        self.required_after
            .map_or_else(|| true, |time| signature_time >= time)
    }

    pub(crate) fn check_subpackets<'a>(
        &'a self,
        subpackets: impl IntoIterator<Item = &'a Subpacket>,
        date: CheckUnixTime,
    ) -> Result<(), MessageSignatureError> {
        // Collect all context notations matching the Proton context name
        let mut context_notations = Self::filter_context(subpackets)
            .filter_map(|subpacket| match subpacket {
                Subpacket {
                    is_critical: _,
                    data: SubpacketData::Notation(notation),
                    ..
                } => Some(notation),
                _ => None,
            })
            .collect::<Vec<_>>();

        // If there are multiple context notations, return an error.
        if context_notations.len() > 1 {
            let values = context_notations
                .iter()
                .map(|notation| String::from_utf8_lossy(notation.value.as_ref()).to_string())
                .collect();
            return Err(MessageSignatureError::Context(
                SignatureContextError::MultipleContexts(values),
            ));
        }

        // If required, there must be exactly one context notation
        let Some(context) = context_notations.pop() else {
            if self.is_required_at_time(date) {
                return Err(MessageSignatureError::Context(
                    SignatureContextError::MissingContext(self.clone()),
                ));
            }
            return Ok(());
        };

        // Does the context value match the verification context?
        if context.value.as_ref() != self.value.as_bytes() {
            return Err(MessageSignatureError::Context(
                SignatureContextError::WrongContext(
                    String::from_utf8_lossy(context.value.as_ref()).to_string(),
                    self.clone(),
                ),
            ));
        }

        Ok(())
    }

    pub(crate) fn filter_context<'a>(
        subpackets: impl IntoIterator<Item = &'a Subpacket>,
    ) -> impl Iterator<Item = &'a Subpacket> {
        subpackets.into_iter().filter(|subpacket| {
            matches!(
                subpacket,
                Subpacket {
                    data: SubpacketData::Notation(notation),
                    ..
                } if notation.name.as_ref() == PROTON_CONTEXT_NOTATION_NAME.as_bytes()
            )
        })
    }
}

impl<'a> From<&'a VerificationContext> for Cow<'a, VerificationContext> {
    fn from(context: &'a VerificationContext) -> Self {
        Cow::Borrowed(context)
    }
}

impl From<VerificationContext> for Cow<'_, VerificationContext> {
    fn from(context: VerificationContext) -> Self {
        Cow::Owned(context)
    }
}

impl fmt::Display for VerificationContext {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "VerificationContext: {}{}{}",
            self.value,
            if self.is_required { " [REQUIRED]" } else { "" },
            self.required_after
                .filter(|_| self.is_required)
                .map_or(String::new(), |time| format!(" (required after: {time})"))
        )
    }
}
