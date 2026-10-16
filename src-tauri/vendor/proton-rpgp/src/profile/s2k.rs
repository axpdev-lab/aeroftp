use pgp::{
    bytes::Bytes,
    crypto::{aead::AeadAlgorithm, hash::HashAlgorithm, sym::SymmetricKeyAlgorithm},
    types::{S2kParams, StringToKey},
};
use rand::{CryptoRng, Rng};

/// S2K option for key encryption and password based encryption.
///
/// Allows to generate the lower lewel params from `rPGP`.
#[derive(Debug, Clone)]
pub enum StringToKeyOption {
    IteratedAndSalted {
        /// The symmetric key algorithm to use for encryption.
        sym_alg: SymmetricKeyAlgorithm,

        /// The hash algorithm to use for key derivation.
        hash_alg: HashAlgorithm,

        /// The count of iterations for the key derivation.
        ///
        /// Computed `iterations = ((Int32)16 + (count & 15)) << ((count >> 4) + 6);`
        count: u8,
    },
    Argon2 {
        /// The symmetric key algorithm to use for encryption.
        sym_alg: SymmetricKeyAlgorithm,

        /// The AEAD algorithm to use for encryption.
        aead_mode: AeadAlgorithm,

        /// Argon2 param one-octet number of passes t
        t: u8,

        /// Argon2 param one-octet degree of parallelism p
        p: u8,

        /// Argon2 param one-octet `encoded_m`, specifying the exponent of the memory size
        m_enc: u8,
    },
}

impl StringToKeyOption {
    pub(crate) fn generate_s2k_params<R: Rng + CryptoRng>(&self, mut rng: R) -> StringToKey {
        match self {
            StringToKeyOption::IteratedAndSalted {
                hash_alg,
                count,
                sym_alg: _,
            } => StringToKey::new_iterated(&mut rng, *hash_alg, *count),
            StringToKeyOption::Argon2 { t, p, m_enc, .. } => {
                StringToKey::new_argon2(&mut rng, *t, *p, *m_enc)
            }
        }
    }

    pub(crate) fn generate_s2k_encryption_params<R: Rng + CryptoRng>(
        &self,
        mut rng: R,
    ) -> S2kParams {
        match self {
            StringToKeyOption::IteratedAndSalted { sym_alg, .. } => {
                let mut iv_vec = vec![0_u8; sym_alg.block_size()];
                rng.fill_bytes(&mut iv_vec);

                S2kParams::Cfb {
                    sym_alg: *sym_alg,
                    s2k: self.generate_s2k_params(&mut rng),
                    iv: Bytes::from(iv_vec),
                }
            }
            StringToKeyOption::Argon2 {
                sym_alg, aead_mode, ..
            } => {
                let mut nonce = vec![0_u8; aead_mode.nonce_size()];
                rng.fill_bytes(&mut nonce);

                S2kParams::Aead {
                    sym_alg: *sym_alg,
                    aead_mode: *aead_mode,
                    s2k: self.generate_s2k_params(&mut rng),
                    nonce: Bytes::from(nonce),
                }
            }
        }
    }
}

impl Default for StringToKeyOption {
    fn default() -> Self {
        StringToKeyOption::IteratedAndSalted {
            sym_alg: SymmetricKeyAlgorithm::AES256,
            hash_alg: HashAlgorithm::Sha256,
            count: 96,
        }
    }
}
