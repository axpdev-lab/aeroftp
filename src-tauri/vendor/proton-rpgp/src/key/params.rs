use pgp::{
    crypto::ecdh::SecretKey as EcdhSecretKey,
    crypto::ecdsa::SecretKey as EcdsaSecretKey,
    crypto::eddsa_legacy::SecretKey as EddsaLegacySecretKey,
    types::{
        EcdhPublicParams, EcdsaPublicParams, EddsaLegacyPublicParams, PlainSecretParams,
        PublicParams,
    },
};

use crate::KeySecretParamValidationError;

pub trait PublicParamsExt {
    fn equal_raw_key_params(&self, other: &PublicParams) -> bool;

    fn can_perform_operation(&self) -> bool;
}

impl PublicParamsExt for PublicParams {
    fn equal_raw_key_params(&self, other: &PublicParams) -> bool {
        match self {
            PublicParams::ECDH(a) => {
                if let PublicParams::ECDH(b) = other {
                    ecdh_public_params_equal_raw_p(a, b)
                } else {
                    false
                }
            }
            PublicParams::RSA(_)
            | PublicParams::DSA(_)
            | PublicParams::ECDSA(_)
            | PublicParams::Elgamal(_)
            | PublicParams::EdDSALegacy(_)
            | PublicParams::Ed25519(_)
            | PublicParams::X25519(_)
            | PublicParams::X448(_)
            | PublicParams::Ed448(_)
            | PublicParams::Unknown { .. } => self == other,
            // AeroFTP patch: post-quantum variants exist only with `pqc`.
            #[cfg(feature = "pqc")]
            PublicParams::MlKem768X25519(_)
            | PublicParams::MlKem1024X448(_)
            | PublicParams::MlDsa65Ed25519(_)
            | PublicParams::MlDsa87Ed448(_)
            | PublicParams::SlhDsaShake128s(_)
            | PublicParams::SlhDsaShake128f(_)
            | PublicParams::SlhDsaShake256s(_) => self == other,
        }
    }

    fn can_perform_operation(&self) -> bool {
        match self {
            PublicParams::RSA(_)
            | PublicParams::Ed25519(_)
            | PublicParams::X25519(_)
            | PublicParams::X448(_)
            | PublicParams::Ed448(_) => true,
            // AeroFTP patch: post-quantum variants exist only with `pqc`.
            #[cfg(feature = "pqc")]
            PublicParams::MlKem768X25519(_)
            | PublicParams::MlKem1024X448(_)
            | PublicParams::MlDsa65Ed25519(_)
            | PublicParams::MlDsa87Ed448(_)
            | PublicParams::SlhDsaShake128s(_)
            | PublicParams::SlhDsaShake128f(_)
            | PublicParams::SlhDsaShake256s(_) => true,
            PublicParams::ECDSA(p) => !matches!(p, EcdsaPublicParams::Unsupported { .. }),
            PublicParams::ECDH(p) => !matches!(
                p,
                EcdhPublicParams::Brainpool256 { .. }
                    | EcdhPublicParams::Brainpool384 { .. }
                    | EcdhPublicParams::Brainpool512 { .. }
                    | EcdhPublicParams::Unsupported { .. }
            ),
            PublicParams::EdDSALegacy(p) => {
                !matches!(p, EddsaLegacyPublicParams::Unsupported { .. })
            }
            PublicParams::DSA(_) | PublicParams::Elgamal(_) | PublicParams::Unknown { .. } => false,
        }
    }
}

fn ecdh_public_params_equal_raw_p(a: &EcdhPublicParams, b: &EcdhPublicParams) -> bool {
    match a {
        EcdhPublicParams::Curve25519Legacy { p, .. } => {
            matches!(b, EcdhPublicParams::Curve25519Legacy { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::P256 { p, .. } => {
            matches!(b, EcdhPublicParams::P256 { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::P384 { p, .. } => {
            matches!(b, EcdhPublicParams::P384 { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::P521 { p, .. } => {
            matches!(b, EcdhPublicParams::P521 { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::Brainpool256 { p, .. } => {
            matches!(b, EcdhPublicParams::Brainpool256 { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::Brainpool384 { p, .. } => {
            matches!(b, EcdhPublicParams::Brainpool384 { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::Brainpool512 { p, .. } => {
            matches!(b, EcdhPublicParams::Brainpool512 { p: p_b, .. } if p == p_b)
        }
        EcdhPublicParams::Unsupported { .. } => a == b,
    }
}

pub trait PlainSecretParamsExt {
    fn validate_public_params(
        &self,
        public_params: &PublicParams,
    ) -> Result<(), KeySecretParamValidationError>;
}

enum ValidationOp {
    ComputePublicParams,
    Unsupported,
}

impl PlainSecretParamsExt for PlainSecretParams {
    fn validate_public_params(
        &self,
        public_params: &PublicParams,
    ) -> Result<(), KeySecretParamValidationError> {
        let validation_op = match self {
            PlainSecretParams::RSA(_) // Ok the rsa library validates the public params on import
            | PlainSecretParams::Ed25519(_)
            | PlainSecretParams::X25519(_)
            | PlainSecretParams::X448(_)
            | PlainSecretParams::Ed448(_) => {
                ValidationOp::ComputePublicParams
            },
            // AeroFTP patch: post-quantum variants exist only with `pqc`.
            #[cfg(feature = "pqc")]
            PlainSecretParams::MlKem768X25519(_)
            | PlainSecretParams::MlKem1024X448(_)
            | PlainSecretParams::MlDsa65Ed25519(_)
            | PlainSecretParams::MlDsa87Ed448(_)
            | PlainSecretParams::SlhDsaShake128s(_)
            | PlainSecretParams::SlhDsaShake128f(_)
            | PlainSecretParams::SlhDsaShake256s(_) => {
                ValidationOp::ComputePublicParams
            },
            PlainSecretParams::EdDSALegacy(secret_key) => {
                match secret_key {
                    EddsaLegacySecretKey::Ed25519(_) => ValidationOp::ComputePublicParams,
                    EddsaLegacySecretKey::Unsupported { .. } => ValidationOp::Unsupported,
                }
            },
            PlainSecretParams::ECDH(secret_key) => {
                match secret_key {
                    EcdhSecretKey::Curve25519Legacy(_)
                    | EcdhSecretKey::P256 { .. }
                    | EcdhSecretKey::P384 { .. }
                    | EcdhSecretKey::P521 { .. } => {
                        ValidationOp::ComputePublicParams
                    },
                    EcdhSecretKey::Unsupported { .. } => ValidationOp::Unsupported,
                }
            },
            PlainSecretParams::ECDSA(secret_key) => {
                match secret_key {
                    EcdsaSecretKey::P256(_)
                    | EcdsaSecretKey::P384(_)
                    | EcdsaSecretKey::P521(_)
                    | EcdsaSecretKey::Secp256k1(_) => {
                        ValidationOp::ComputePublicParams
                    },
                    EcdsaSecretKey::Unsupported { .. } => ValidationOp::Unsupported,
                }
            }
            PlainSecretParams::DSA(_) | PlainSecretParams::Elgamal(_) | PlainSecretParams::Unknown {
                ..
            } => {
                ValidationOp::Unsupported
            }
        };

        match validation_op {
            ValidationOp::ComputePublicParams => {
                let computed_public_params = PublicParams::try_from(self)?;
                if !computed_public_params.equal_raw_key_params(public_params) {
                    return Err(KeySecretParamValidationError::ValidatePublicPartsFailed);
                }
                Ok(())
            }
            ValidationOp::Unsupported => {
                if public_params.can_perform_operation() {
                    Err(KeySecretParamValidationError::ValidatePublicPartsFailed)
                } else {
                    Ok(())
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use pgp::{
        composed::{DsaKeySize, KeyType},
        crypto::ecc_curve::ECCCurve,
        types::SecretParams,
    };
    use rand::{rngs::StdRng, SeedableRng};

    use super::*;

    fn test_rng() -> StdRng {
        StdRng::seed_from_u64(0xC0FF_EE42)
    }

    fn plain_secret(secret_params: &SecretParams) -> &PlainSecretParams {
        let SecretParams::Plain(plain) = secret_params else {
            panic!("expected plain secret params");
        };
        plain
    }

    fn assert_validates_supported(key_type: &KeyType) {
        let mut rng = test_rng();
        let (generated_public, secret_params) = key_type
            .generate(&mut rng)
            .expect("key generation should succeed");
        let plain_secret = plain_secret(&secret_params);

        plain_secret
            .validate_public_params(&generated_public)
            .unwrap_or_else(|err| {
                panic!(
                    "secret must validate against its own public params for {key_type:?}: {err:?}"
                )
            });

        // Public params from an independent key of the same type must be rejected.
        let (other_public, _) = key_type
            .generate(&mut rng)
            .expect("second key generation should succeed");
        assert!(
            matches!(
                plain_secret.validate_public_params(&other_public),
                Err(KeySecretParamValidationError::ValidatePublicPartsFailed)
            ),
            "public params from an independent key must not validate for {key_type:?}",
        );
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "slow in debug mode")]
    fn validate_public_params_rsa() {
        assert_validates_supported(&KeyType::Rsa(2048));
    }

    #[test]
    #[cfg_attr(debug_assertions, ignore = "slow in debug mode")]
    fn validate_public_params_rsa_rejects_mismatched_exponent() {
        use pgp::types::RsaPublicParams;
        use rsa::{traits::PublicKeyParts, BigUint, RsaPublicKey};

        let mut rng = test_rng();
        let (generated_public, secret_params) = KeyType::Rsa(2048)
            .generate(&mut rng)
            .expect("rsa generation should succeed");
        let plain_secret = plain_secret(&secret_params);

        let PublicParams::RSA(rsa_public) = &generated_public else {
            panic!("expected RSA public params");
        };

        let tampered_e = BigUint::from(65539_u32);
        assert_ne!(rsa_public.key.e(), &tampered_e,);
        let tampered_key = RsaPublicKey::new(rsa_public.key.n().clone(), tampered_e)
            .expect("constructing the tampered RSA public key should succeed");
        let tampered_public = PublicParams::RSA(RsaPublicParams { key: tampered_key });

        assert!(matches!(
            plain_secret.validate_public_params(&tampered_public),
            Err(KeySecretParamValidationError::ValidatePublicPartsFailed)
        ));
    }

    #[test]
    fn validate_public_params_ecdh_curve25519() {
        assert_validates_supported(&KeyType::ECDH(ECCCurve::Curve25519Legacy));
    }

    #[test]
    fn validate_public_params_ecdh_p256() {
        assert_validates_supported(&KeyType::ECDH(ECCCurve::P256));
    }

    #[test]
    fn validate_public_params_ecdh_p384() {
        assert_validates_supported(&KeyType::ECDH(ECCCurve::P384));
    }

    #[test]
    fn validate_public_params_ecdh_p521() {
        assert_validates_supported(&KeyType::ECDH(ECCCurve::P521));
    }

    #[test]
    fn validate_public_params_ecdsa_p256() {
        assert_validates_supported(&KeyType::ECDSA(ECCCurve::P256));
    }

    #[test]
    fn validate_public_params_ecdsa_p384() {
        assert_validates_supported(&KeyType::ECDSA(ECCCurve::P384));
    }

    #[test]
    fn validate_public_params_ecdsa_p521() {
        assert_validates_supported(&KeyType::ECDSA(ECCCurve::P521));
    }

    #[test]
    fn validate_public_params_ed25519_legacy() {
        assert_validates_supported(&KeyType::Ed25519Legacy);
    }

    #[test]
    fn validate_public_params_ed25519() {
        assert_validates_supported(&KeyType::Ed25519);
    }

    #[test]
    fn validate_public_params_ed448() {
        assert_validates_supported(&KeyType::Ed448);
    }

    #[test]
    fn validate_public_params_x25519() {
        assert_validates_supported(&KeyType::X25519);
    }

    #[test]
    fn validate_public_params_x448() {
        assert_validates_supported(&KeyType::X448);
    }

    #[test]
    #[cfg(feature = "pqc")]
    fn validate_public_params_ml_kem_768_x25519() {
        assert_validates_supported(&KeyType::MlKem768X25519);
    }

    #[test]
    #[cfg(feature = "pqc")]
    fn validate_public_params_ml_kem_1024_x448() {
        assert_validates_supported(&KeyType::MlKem1024X448);
    }

    #[test]
    #[cfg(feature = "pqc")]
    fn validate_public_params_ml_dsa_65_ed25519() {
        assert_validates_supported(&KeyType::MlDsa65Ed25519);
    }

    #[test]
    #[cfg(feature = "pqc")]
    fn validate_public_params_ml_dsa_87_ed448() {
        assert_validates_supported(&KeyType::MlDsa87Ed448);
    }

    #[test]
    #[cfg(feature = "pqc")]
    #[cfg_attr(debug_assertions, ignore = "slow in debug mode")]
    fn validate_public_params_slh_dsa_shake128s() {
        assert_validates_supported(&KeyType::SlhDsaShake128s);
    }

    #[test]
    #[cfg(feature = "pqc")]
    fn validate_public_params_slh_dsa_shake128f() {
        assert_validates_supported(&KeyType::SlhDsaShake128f);
    }

    #[test]
    #[cfg(feature = "pqc")]
    #[cfg_attr(debug_assertions, ignore = "slow in debug mode")]
    fn validate_public_params_slh_dsa_shake256s() {
        assert_validates_supported(&KeyType::SlhDsaShake256s);
    }

    #[test]
    fn validate_public_params_rejects_mismatched_algorithms() {
        let mut rng = test_rng();
        let (ed25519_public, ed25519_secret) = KeyType::Ed25519
            .generate(&mut rng)
            .expect("ed25519 generation should succeed");
        let (x25519_public, x25519_secret) = KeyType::X25519
            .generate(&mut rng)
            .expect("x25519 generation should succeed");
        let ed25519_plain = plain_secret(&ed25519_secret);
        let x25519_plain = plain_secret(&x25519_secret);

        assert!(matches!(
            ed25519_plain.validate_public_params(&x25519_public),
            Err(KeySecretParamValidationError::ValidatePublicPartsFailed)
        ));
        assert!(matches!(
            x25519_plain.validate_public_params(&ed25519_public),
            Err(KeySecretParamValidationError::ValidatePublicPartsFailed)
        ));
    }

    #[test]
    fn validate_public_params_unsupported_secret_accepts_its_own_public_params() {
        let mut rng = test_rng();
        let (dsa_public, dsa_secret) = KeyType::Dsa(DsaKeySize::B2048)
            .generate(&mut rng)
            .expect("dsa generation should succeed");
        let dsa_plain = plain_secret(&dsa_secret);

        assert!(!dsa_public.can_perform_operation());
        dsa_plain
            .validate_public_params(&dsa_public)
            .expect("unsupported secret must validate against its own unusable public params");
    }

    #[test]
    fn validate_public_params_unsupported_secret_accepts_unknown_public_params() {
        let mut rng = test_rng();
        let (_, dsa_secret) = KeyType::Dsa(DsaKeySize::B2048)
            .generate(&mut rng)
            .expect("dsa generation should succeed");
        let dsa_plain = plain_secret(&dsa_secret);

        let unknown = PublicParams::Unknown {
            data: pgp::bytes::Bytes::from_static(b"raw"),
        };
        dsa_plain
            .validate_public_params(&unknown)
            .expect("unsupported secret must validate against unusable unknown public params");
    }

    #[test]
    fn validate_public_params_unsupported_secret_rejects_usable_public_params() {
        let mut rng = test_rng();
        let (_, dsa_secret) = KeyType::Dsa(DsaKeySize::B2048)
            .generate(&mut rng)
            .expect("dsa generation should succeed");
        let (ed25519_public, _) = KeyType::Ed25519
            .generate(&mut rng)
            .expect("ed25519 generation should succeed");
        let dsa_plain = plain_secret(&dsa_secret);

        assert!(matches!(
            dsa_plain.validate_public_params(&ed25519_public),
            Err(KeySecretParamValidationError::ValidatePublicPartsFailed)
        ));
    }
}
