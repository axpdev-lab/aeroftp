//! The signed-in user's keys: user keys and address keys, unlocked.
//!
//! Follows `incubating/account/js/src/addresses.ts` of the Proton Drive SDK at
//! commit [`super::SDK_REFERENCE_COMMIT`] (MIT, Copyright (c) 2025-2026 Proton
//! AG, see `LICENSE-PROTON-DRIVE-SDK.md`):
//! - `GET /core/v4/users`: every user key is imported with the user key
//!   password from the sign-in; keys that fail are skipped, none at all is an
//!   error;
//! - `GET /core/v4/addresses` (page 0, 50 per page): every address key is
//!   unlocked through its token, decrypted with the user keys and verified
//!   against them, or with the user key password for legacy keys without a
//!   token; the first address is the primary one.
//!
//! The OpenPGP work is done by Proton's official `proton-crypto-account`
//! crate with its pure Rust backend, never by hand.

// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

use proton_crypto_account::keys::{
    AddressKeys, LockedKey, UnlockedAddressKeys, UnlockedUserKeys, UserKeys,
};
use proton_crypto_account::proton_crypto::crypto::PGPProviderSync;
use proton_crypto_account::salts::KeySecret;
use secrecy::{ExposeSecret, SecretString};
use serde::Deserialize;

use super::api::ProtonApi;
use super::error::ProtonError;

/// `GET /core/v4/users`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct UsersResponse {
    user: ApiUser,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiUser {
    #[serde(rename = "ID")]
    id: String,
    #[serde(default)]
    keys: Vec<LockedKey>,
}

/// `GET /core/v4/addresses`.
#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct AddressesResponse {
    #[serde(default)]
    addresses: Vec<ApiAddress>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct ApiAddress {
    #[serde(rename = "ID")]
    id: String,
    email: String,
    #[serde(default)]
    keys: Vec<LockedKey>,
}

/// One of the user's own addresses with its unlocked keys.
pub struct OwnAddress<P: PGPProviderSync> {
    pub id: String,
    pub email: String,
    pub keys: UnlockedAddressKeys<P>,
}

impl<P: PGPProviderSync> std::fmt::Debug for OwnAddress<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // No email: Debug output can reach logs.
        f.debug_struct("OwnAddress")
            .field("id", &self.id)
            .field("keys", &self.keys.len())
            .finish_non_exhaustive()
    }
}

/// The unlocked key material of the signed-in account.
pub struct ProtonAccount<P: PGPProviderSync> {
    provider: P,
    user_id: String,
    user_keys: UnlockedUserKeys<P>,
    /// Addresses whose keys unlocked, in API order.
    addresses: Vec<OwnAddress<P>>,
    /// ID of the first address the API returned: the primary one.
    primary_address_id: String,
    /// IDs of addresses none of whose keys unlocked.
    locked_address_ids: Vec<String>,
}

impl<P: PGPProviderSync> std::fmt::Debug for ProtonAccount<P> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProtonAccount")
            .field("user_keys", &self.user_keys.len())
            .field("addresses", &self.addresses.len())
            .field("locked_addresses", &self.locked_address_ids.len())
            .finish_non_exhaustive()
    }
}

impl<P: PGPProviderSync> ProtonAccount<P> {
    pub fn provider(&self) -> &P {
        &self.provider
    }

    pub fn user_id(&self) -> &str {
        &self.user_id
    }

    pub fn user_keys(&self) -> &UnlockedUserKeys<P> {
        &self.user_keys
    }

    /// The primary address, the one Drive uses by default. As in
    /// `addresses.ts`, it is the first address of the API answer, and it is
    /// an error (not a fallback to another address) when its keys are locked.
    pub fn primary_address(&self) -> Result<&OwnAddress<P>, ProtonError> {
        self.address(&self.primary_address_id)
    }

    pub fn addresses(&self) -> &[OwnAddress<P>] {
        &self.addresses
    }

    /// An own address by ID or email, as `addresses.ts::getOwnAddress`.
    pub fn address(&self, id_or_email: &str) -> Result<&OwnAddress<P>, ProtonError> {
        if let Some(address) = self
            .addresses
            .iter()
            .find(|a| a.id == id_or_email || a.email == id_or_email)
        {
            return Ok(address);
        }
        if self.locked_address_ids.iter().any(|id| id == id_or_email) {
            return Err(ProtonError::Crypto(
                "no key of this address could be unlocked".into(),
            ));
        }
        Err(ProtonError::Decode("address not found".into()))
    }
}

/// Fetches the user and address keys and unlocks them with the user key
/// password of the session.
pub async fn load_account(
    api: &ProtonApi,
    key_password: &SecretString,
) -> Result<ProtonAccount<impl PGPProviderSync>, ProtonError> {
    // Independent reads, sent together: each connect waited for the user
    // before asking for the addresses. Concurrent 401s share one refresh.
    let address_page = [("Page", "0".to_string()), ("PageSize", "50".to_string())];
    let (users, addresses): (UsersResponse, AddressesResponse) = tokio::try_join!(
        api.get("/core/v4/users", &[]),
        api.get("/core/v4/addresses", &address_page),
    )?;
    let secret = KeySecret::new(key_password.expose_secret().as_bytes().to_vec());

    // Key import runs the S2K of every private key: CPU work that must not
    // stall the async runtime.
    tokio::task::spawn_blocking(move || {
        unlock_account(
            proton_crypto_account::proton_crypto::new_pgp_provider(),
            users.user,
            addresses.addresses,
            &secret,
        )
    })
    .await
    .map_err(|error| ProtonError::Crypto(format!("key unlock task failed: {error}")))?
}

fn unlock_account<P: PGPProviderSync>(
    provider: P,
    user: ApiUser,
    addresses: Vec<ApiAddress>,
    secret: &KeySecret,
) -> Result<ProtonAccount<P>, ProtonError> {
    let user_unlock = UserKeys::new(user.keys).unlock(&provider, secret);
    if !user_unlock.failed.is_empty() {
        log::warn!(
            "Proton: {} user key(s) could not be unlocked",
            user_unlock.failed.len()
        );
    }
    if user_unlock.unlocked_keys.is_empty() {
        return Err(ProtonError::Crypto("missing user primary keys".into()));
    }
    let user_keys = UnlockedUserKeys::from(user_unlock.unlocked_keys);

    let Some(primary_address_id) = addresses.first().map(|address| address.id.clone()) else {
        return Err(ProtonError::Decode("missing primary address".into()));
    };

    let mut unlocked = Vec::with_capacity(addresses.len());
    let mut locked_address_ids = Vec::new();
    for address in addresses {
        let result = AddressKeys::new(address.keys).unlock(&provider, &user_keys, Some(secret));
        if !result.failed.is_empty() {
            log::warn!(
                "Proton: {} key(s) of an address could not be unlocked",
                result.failed.len()
            );
        }
        if result.unlocked_keys.is_empty() {
            locked_address_ids.push(address.id);
            continue;
        }
        unlocked.push(OwnAddress {
            id: address.id,
            email: address.email,
            keys: UnlockedAddressKeys::from(result.unlocked_keys),
        });
    }

    Ok(ProtonAccount {
        provider,
        user_id: user.id,
        user_keys,
        addresses: unlocked,
        primary_address_id,
        locked_address_ids,
    })
}

/// An account with one address, `me0@example.com`, whose keys are generated
/// here (tests of the Drive client).
#[cfg(test)]
pub(crate) fn test_account() -> ProtonAccount<impl PGPProviderSync + Send + Sync + 'static> {
    let (user, addresses) = tests::generated_account(b"pw", 1, false);
    unlock_account(
        proton_crypto_account::proton_crypto::new_pgp_provider(),
        user,
        addresses,
        &KeySecret::new(b"pw".to_vec()),
    )
    .expect("the generated account unlocks")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The API shapes deserialize into the crate's `LockedKey` exactly as the
    /// SDK reads them (integers for booleans, PascalCase names).
    #[test]
    fn api_user_and_address_shapes_deserialize() {
        let users: UsersResponse = serde_json::from_str(
            r#"{"Code":1000,"User":{"ID":"u1","Name":"n","Keys":[
                {"ID":"k1","Version":3,"PrivateKey":"-----BEGIN PGP PRIVATE KEY BLOCK-----","Primary":1,"Active":1}
            ]}}"#,
        )
        .expect("users");
        assert_eq!(users.user.id, "u1");
        assert_eq!(users.user.keys.len(), 1);
        assert!(users.user.keys[0].primary);

        let addresses: AddressesResponse = serde_json::from_str(
            r#"{"Code":1000,"Total":1,"Addresses":[
                {"ID":"a1","Email":"me@example.com","Status":1,"Order":1,"Keys":[
                    {"ID":"ak1","Version":3,"PrivateKey":"-----BEGIN PGP PRIVATE KEY BLOCK-----",
                     "Token":"-----BEGIN PGP MESSAGE-----","Signature":"-----BEGIN PGP SIGNATURE-----",
                     "Primary":1,"Active":1,"Flags":3}
                ]}
            ]}"#,
        )
        .expect("addresses");
        assert_eq!(addresses.addresses.len(), 1);
        assert_eq!(addresses.addresses[0].email, "me@example.com");
        assert!(addresses.addresses[0].keys[0].token.is_some());
    }

    /// Builds the `/core/v4/users` and `/core/v4/addresses` JSON of an account
    /// whose keys are generated here with Proton's own key generation: one
    /// user key locked with `password`, and address keys locked through a
    /// token encrypted to and signed by that user key, as Proton stores them.
    /// `swap_signatures` gives each address key the token signature of the
    /// other one, which must make verification fail.
    pub(super) fn generated_account(
        password: &[u8],
        addresses: usize,
        swap_signatures: bool,
    ) -> (ApiUser, Vec<ApiAddress>) {
        use proton_crypto_account::keys::{LocalAddressKey, LocalUserKey};
        use proton_crypto_account::proton_crypto::crypto::KeyGeneratorAlgorithm;

        let provider = proton_crypto_account::proton_crypto::new_pgp_provider();
        let secret = KeySecret::new(password.to_vec());
        let user_key = LocalUserKey::generate(&provider, KeyGeneratorAlgorithm::ECC, &secret)
            .expect("user key generation");
        let user: ApiUser = serde_json::from_value(serde_json::json!({
            "ID": "u1",
            "Keys": [{
                "ID": "uk1", "Version": 3, "Primary": 1, "Active": 1,
                "PrivateKey": user_key.private_key,
            }],
        }))
        .unwrap();

        let mut unlocked = UserKeys::new(user.keys.clone()).unlock(&provider, &secret);
        assert!(
            unlocked.failed.is_empty(),
            "the generated user key must unlock"
        );
        let unlocked_user_key = unlocked.unlocked_keys.remove(0);

        let generated: Vec<LocalAddressKey> = (0..addresses)
            .map(|i| {
                LocalAddressKey::generate(
                    &provider,
                    &format!("me{i}@example.com"),
                    KeyGeneratorAlgorithm::ECC,
                    Default::default(),
                    true,
                    &unlocked_user_key,
                )
                .expect("address key generation")
            })
            .collect();
        let api_addresses = generated
            .iter()
            .enumerate()
            .map(|(i, key)| {
                let signature = if swap_signatures {
                    &generated[(i + 1) % generated.len()].signature
                } else {
                    &key.signature
                };
                serde_json::from_value(serde_json::json!({
                    "ID": format!("a{i}"),
                    "Email": format!("me{i}@example.com"),
                    "Keys": [{
                        "ID": format!("ak{i}"), "Version": 3, "Primary": 1, "Active": 1,
                        "Flags": key.flags.to_u32(),
                        "PrivateKey": key.private_key,
                        "Token": key.token,
                        "Signature": signature,
                    }],
                }))
                .unwrap()
            })
            .collect();
        (user, api_addresses)
    }

    /// The whole unlock path on real keys: user key with the password, then
    /// each address key through its decrypted and verified token.
    #[test]
    fn generated_keys_unlock_through_the_signed_token() {
        let (user, addresses) = generated_account(b"fork-key-password", 2, false);
        let account = unlock_account(
            proton_crypto_account::proton_crypto::new_pgp_provider(),
            user,
            addresses,
            &KeySecret::new(b"fork-key-password".to_vec()),
        )
        .expect("account unlocks");
        assert_eq!(account.user_keys().len(), 1);
        assert_eq!(account.addresses().len(), 2);
        let primary = account.primary_address().expect("primary unlocked");
        assert_eq!(primary.id, "a0");
        assert_eq!(primary.keys.len(), 1);
        assert_eq!(
            account.address("me1@example.com").expect("by email").id,
            "a1"
        );
    }

    /// The wrong key password unlocks nothing.
    #[test]
    fn the_wrong_key_password_unlocks_nothing() {
        let (user, addresses) = generated_account(b"right", 1, false);
        let error = unlock_account(
            proton_crypto_account::proton_crypto::new_pgp_provider(),
            user,
            addresses,
            &KeySecret::new(b"wrong".to_vec()),
        )
        .unwrap_err();
        assert!(matches!(error, ProtonError::Crypto(_)), "{error:?}");
    }

    /// A token whose signature does not match is refused: the address stays
    /// locked and the primary address is an error, never another address.
    #[test]
    fn a_token_with_a_foreign_signature_keeps_the_address_locked() {
        let (user, addresses) = generated_account(b"pw", 2, true);
        let account = unlock_account(
            proton_crypto_account::proton_crypto::new_pgp_provider(),
            user,
            addresses,
            &KeySecret::new(b"pw".to_vec()),
        )
        .expect("user keys still unlock");
        assert!(account.addresses().is_empty(), "{account:?}");
        let error = account.primary_address().unwrap_err();
        assert!(matches!(error, ProtonError::Crypto(_)), "{error:?}");
    }

    /// Garbage keys never unlock, and an account with no usable user key is
    /// an error rather than an empty keyring.
    #[test]
    fn an_account_without_usable_user_keys_is_refused() {
        let user: ApiUser = serde_json::from_str(
            r#"{"ID":"u1","Keys":[{"ID":"k1","Version":3,"PrivateKey":"not a key","Primary":1,"Active":1}]}"#,
        )
        .unwrap();
        let address: ApiAddress =
            serde_json::from_str(r#"{"ID":"a1","Email":"me@example.com","Keys":[]}"#).unwrap();
        let error = unlock_account(
            proton_crypto_account::proton_crypto::new_pgp_provider(),
            user,
            vec![address],
            &KeySecret::new(b"pw".to_vec()),
        )
        .unwrap_err();
        assert!(matches!(error, ProtonError::Crypto(_)), "{error:?}");
    }
}
