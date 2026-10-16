# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased] - 2026-00-00

## [0.9.0] - 2026-10-05

### Added

- `KeyGenerationType::RsaCustom(bits)` to generate RSA keys with a custom modulus size between `MIN_RSA_KEY_GEN_BITS` (1023) and `MAX_RSA_KEY_GEN_BITS` (8192). Out-of-range sizes fail with `KeyGenerationError::InvalidRsaBits`. Requires the new `hazmat-key-gen` feature (intended for testing only).

### Changed

- Bump `pgp` to `0.21.0`.

## [0.8.0] - 2026-09-28

### Added

- Support recursive forwarding: a forwarder key that is itself a forwardee is now chained correctly by `PrivateKey::forwarding_key_generator`/`forwarding_key_generator_with_profile`. `PublicKeySelectionExt::forwardable_key` selects a key valid for generating a forwarding key.
- `ForwardingPkesk::proxy_forward_single` to transform a single PKESK against one `ForwardingInstance`, returning `None` if it doesn't match.

### Changed

- BREAKING: `ForwardingPkesk::proxy_forward` now returns `Vec<ForwardedPkesk>`, forwarding to every matching instance instead of only the first one, and matches PKESKs with a wildcard key id against any instance.
- Forwarded PKESKs now always carry the forwardee's key id, even when the original PKESK used a wildcard key id, matching go-encryption-service and enabling chained forwarding.

## [0.7.0] - 2026-09-22

### Added

- `forward` module implementing `draft-wussler-openpgp-forwarding` write support: `PrivateKey::forwarding_key_generator`/`forwarding_key_generator_with_profile` to derive a forwardee key and its `ForwardingInstance`s from a forwarder key, `ForwardingPkesk`/`ForwardedPkesk` to transform a single PKESK, and `EncryptedMessage::proxy_forward` to transform all eligible PKESKs of a message.

## [0.6.0] - 2026-09-15

### Added

- `certify_with_external`/`verify_with_external` on `PublicKey`/`PrivateKey`, with `ExternalCertifier`/`ExternalVerifier` builders, to certify a key with an external (e.g. Proton CA) key and verify existing certifications, including certification expiration.
- `GenericKeyIdentifier::key_id` to derive the key ID from a generic key identifier.
- `KeyGenerator::generate_with_rng` to generate a key with a custom CSPRNG.
- `import_single_enforce`/`import_unlocked_single_enforce` on `PublicKey`, `LockedPrivateKey`, and `PrivateKey` to import a key while rejecting inputs containing more than one key.

### Changed

- When decrypting with passphrases, prefer a PKESK-derived session key over an SKESK-derived one.

## [0.5.0] - 2026-07-02

### Added

- Adds the AEAD Seipdv2 API for proton-rpgp.
- `HAZARD_AEAD_PROFILE`: profile with AEAD enabled by default
- New `with_aead` and `with_aead_chunk_size` method on the Encryptor for AEAD encryption.
- `is_seipdv2_aead` on the `SessionKey` type

### Changed

- Bump `pgp` to `0.20.0`: Fixes an issue where decrypting or verifying empty messages triggerd an error.

## [0.4.0] - 2026-06-05

### Changed

- Allow streaming verification of an empty message with a detached signature.

- BREAKING: `PrivateKey::import` now throws an error if the key is not locked. Unifies the behaviour with gopenpgp-sys and improves security.

- On unlocking a locked key, validate that the primary key’s public parameters match the just-decrypted secret values. This thwarts tampering with unauthenticated public material and hardens key security.

- BREAKING: Removed the `asm` feature flag due to dangerous instability on apple arm (Apple clang 21 (CLT 26.5)). 

## [0.3.3] - 2026-04-17

### Fixed

- Fixed `VerificationResultUtility::selected_signature_bytes` to return only the selected signature bytes rather than all signatures bytes.

## [0.3.2] - 2026-04-16

### Fixed

- Compression while writing messages is now applied only when explicitly enabled through the API or user profile independant of the key preferences.

## [0.3.1] - 2026-03-23

### Added

- Add API for importing multiple private keys from a blob.

## [0.3.0] - 2026-03-13

### Changed

- Removed the `wasm` feature flag
- Enable wasm32 target build without adding any external dependencies.

## [0.2.0] - 2026-03-03

### Added

- Support for modifying keys.
- Allow to collect information about the encryption process in the encryptor.

## [0.1.0] - 2026-02-10

### Added

- Initial release of the `proton-rpgp` crate.
