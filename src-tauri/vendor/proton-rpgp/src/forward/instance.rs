use pgp::{
    crypto::{hash::HashAlgorithm, public_key::PublicKeyAlgorithm, sym::SymmetricKeyAlgorithm},
    packet::PublicKeyEncryptedSessionKey,
    types::{
        EcdhKdfType, EcdhPublicParams, Fingerprint, ForwardingProxyParameter, KeyDetails, KeyId,
        KeyVersion, PublicParams, Timestamp,
    },
};

use crate::{FingerprintExt, ForwardingInstanceError, ForwardingTransformError, PkeskExt};

/// Everything a forwarding proxy needs to re-target a message from one
/// forwarder subkey to the matching forwardee subkey.
pub struct ForwardingInstance {
    key_details: ForwardingKeyDetails,
    proxy_parameter: ForwardingProxyParameter,
}

impl ForwardingInstance {
    /// Rebuilds a forwarding instance from its persisted parts.
    ///
    /// Both fingerprints must belong to v4 keys; forwarding is not defined for
    /// other key versions.
    pub fn new(
        forwarder_fingerprint: Fingerprint,
        forwardee_fingerprint: Fingerprint,
        proxy_parameter: ForwardingProxyParameter,
    ) -> crate::Result<Self> {
        Self::new_inner(
            forwarder_fingerprint,
            forwardee_fingerprint,
            proxy_parameter,
        )
        .map_err(Into::into)
    }

    pub(crate) fn new_inner(
        forwarder_fingerprint: Fingerprint,
        forwardee_fingerprint: Fingerprint,
        proxy_parameter: ForwardingProxyParameter,
    ) -> Result<Self, ForwardingInstanceError> {
        Ok(Self {
            key_details: ForwardingKeyDetails::new(forwarder_fingerprint, forwardee_fingerprint)?,
            proxy_parameter,
        })
    }

    /// Fingerprint of the key the message was originally encrypted to.
    pub fn forwarder_fingerprint(&self) -> &Fingerprint {
        &self.key_details.forwarder_fingerprint
    }

    /// Fingerprint of the key the message is being forwarded to.
    pub fn forwardee_fingerprint(&self) -> &Fingerprint {
        &self.key_details.forwardee_fingerprint
    }

    pub fn proxy_parameter(&self) -> &ForwardingProxyParameter {
        &self.proxy_parameter
    }

    pub(crate) fn transform(
        &self,
        pkesk: &PublicKeyEncryptedSessionKey,
    ) -> Result<PublicKeyEncryptedSessionKey, ForwardingTransformError> {
        let proxy_parameter = ForwardingProxyParameter::from(*self.proxy_parameter.as_ref());
        pkesk
            .forwarding_transform(&self.key_details, proxy_parameter)
            .map(|forwarded| self.override_wildcard_key_id(forwarded))
            .map_err(ForwardingTransformError::ProxyTransform)
    }

    fn override_wildcard_key_id(
        &self,
        pkesk: PublicKeyEncryptedSessionKey,
    ) -> PublicKeyEncryptedSessionKey {
        if !pkesk
            .generic_identifier()
            .is_some_and(|ident| ident.is_wildcard())
        {
            return pkesk;
        }
        match (pkesk, self.forwardee_fingerprint().key_id()) {
            (
                PublicKeyEncryptedSessionKey::V3 {
                    packet_header,
                    pk_algo,
                    values,
                    ..
                },
                Some(id),
            ) => PublicKeyEncryptedSessionKey::V3 {
                packet_header,
                id,
                pk_algo,
                values,
            },
            (pkesk, _) => pkesk,
        }
    }
}

impl Clone for ForwardingInstance {
    fn clone(&self) -> Self {
        Self {
            key_details: self.key_details.clone(),
            proxy_parameter: ForwardingProxyParameter::from(*self.proxy_parameter.as_ref()),
        }
    }
}

/// Redacts the proxy parameter, which is secret key material.
impl std::fmt::Debug for ForwardingInstance {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ForwardingInstance")
            .field("forwarder_fingerprint", self.forwarder_fingerprint())
            .field("forwardee_fingerprint", self.forwardee_fingerprint())
            .field("proxy_parameter", &"<redacted>")
            .finish()
    }
}

#[derive(Debug, Clone)]
struct ForwardingKeyDetails {
    forwarder_fingerprint: Fingerprint,
    forwardee_fingerprint: Fingerprint,
    forwardee_params: PublicParams,
}

impl ForwardingKeyDetails {
    /// Both fingerprints must be v4; forwarding is not defined for other key
    /// versions.
    fn new(
        forwarder_fingerprint: Fingerprint,
        forwardee_fingerprint: Fingerprint,
    ) -> Result<Self, ForwardingInstanceError> {
        let Fingerprint::V4(forwarder_bytes) = forwarder_fingerprint else {
            return Err(ForwardingInstanceError::UnsupportedForwarderKeyVersion(
                forwarder_fingerprint.version(),
            ));
        };
        if !matches!(forwardee_fingerprint, Fingerprint::V4(_)) {
            return Err(ForwardingInstanceError::UnsupportedForwardeeKeyVersion(
                forwardee_fingerprint.version(),
            ));
        }
        Ok(Self {
            forwarder_fingerprint,
            forwardee_fingerprint,
            forwardee_params: forwardee_public_params(&forwarder_bytes),
        })
    }
}

impl KeyDetails for ForwardingKeyDetails {
    fn version(&self) -> KeyVersion {
        self.forwardee_fingerprint
            .version()
            .unwrap_or(KeyVersion::Other(u8::MAX))
    }

    fn legacy_key_id(&self) -> KeyId {
        self.forwardee_fingerprint
            .key_id()
            .unwrap_or(KeyId::new([0_u8; 8]))
    }

    fn fingerprint(&self) -> Fingerprint {
        self.forwardee_fingerprint.clone()
    }

    fn algorithm(&self) -> PublicKeyAlgorithm {
        PublicKeyAlgorithm::ECDH
    }

    fn created_at(&self) -> Timestamp {
        Timestamp::from_secs(0)
    }

    fn legacy_v3_expiration_days(&self) -> Option<u16> {
        None
    }

    fn public_params(&self) -> &PublicParams {
        &self.forwardee_params
    }
}

/// Mocking public params for forwarding in rpgp.
/// [`PublicKeyEncryptedSessionKey::forwarding_transform`] requires a type that implements [`KeyDetails`].
fn forwardee_public_params(forwarder_fingerprint: &[u8; 20]) -> PublicParams {
    PublicParams::ECDH(EcdhPublicParams::Curve25519Legacy {
        p: x25519_dalek::PublicKey::from([0_u8; 32]),
        hash: HashAlgorithm::Sha256,
        alg_sym: SymmetricKeyAlgorithm::AES128,
        ecdh_kdf_type: EcdhKdfType::Replaced {
            replacement_fingerprint: *forwarder_fingerprint,
        },
    })
}
