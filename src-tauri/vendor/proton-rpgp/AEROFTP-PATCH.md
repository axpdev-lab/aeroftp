# AeroFTP's proton-rpgp patch

Source: crates.io `proton-rpgp` 0.9.0, Proton AG, MIT
([ProtonMail/proton-crypto-rs](https://github.com/ProtonMail/proton-crypto-rs)).
The crate archive has SHA-256
`5cc147eb6645c4caeef154d247e6db44722c5be58568c2fcced21bc22d2bc528`.
The Rust sources, README, CHANGELOG and LICENSE are retained. The upstream
integration tests and test data are omitted. `Cargo.toml` is the published
manifest with the changes below and without the test targets.

Only the optional `proton-native` feature reaches this crate, through
`proton-crypto-account` (`rustpgp` backend). No default AeroFTP build compiles
it.

## Why

Upstream always enables rpgp's `pqc` feature. In rpgp 0.21 that pulls
`ml-kem` 0.2.3, which pins `kem = "=0.3.0-pre.0"`, and this crate pins the same
version. AeroFTP's `russh` 0.64 uses `ml-kem` 0.3.2, which needs `kem` 0.3.0.
Cargo admits one `kem` 0.3.x per graph, so the resolution of the whole
`aeroftp` package failed, even with `proton-crypto-account` optional and the
feature off.

## What changed

The post-quantum algorithms become an opt-in `pqc` feature, off by default:

- `Cargo.toml`: `pqc = ["pgp/pqc", "dep:kem"]`; `kem` is optional; `pqc` is
  removed from the always-on `pgp` features (both the default and the wasm32
  dependency entries).
- `src/key/params.rs`: in `PublicParamsExt` (two matches) and
  `PlainSecretParamsExt::validate_public_params`, the ML-KEM, ML-DSA and
  SLH-DSA arms move to separate arms under `#[cfg(feature = "pqc")]`. rpgp 0.21
  gates those enum variants the same way, so the arms are identical to
  upstream when `pqc` is on.
- `src/key/preferences.rs`: `MlDsa87Ed448` gets its own `#[cfg(feature = "pqc")]`
  arm next to `Ed448`, same result.
- `src/profile/keygen.rs`: the `KeyGenerationType::PQC` variant and its three
  match arms are under `#[cfg(feature = "pqc")]`. `proton-crypto` 0.14.1 never
  names that variant.
- `src/key/generation.rs` and `src/key/params.rs`: the inline tests that
  generate or validate post-quantum keys (one and seven tests) run only with
  `pqc`.

Nothing else differs. With `--features pqc` the crate is upstream 0.9.0.

## What AeroFTP loses

Without `pqc`, keys and messages that use ML-KEM, ML-DSA or SLH-DSA do not
parse. Proton Drive's file and folder keys are v4 ECC keys, and Proton's
post-quantum address keys are opt-in. An account whose only address keys are
post-quantum ones would fail to unlock with a crypto error, never with wrong
data.

## Verification

On 2026-10-16, in a standalone copy with the upstream integration tests and
test data and the package's own `Cargo.lock` (`cargo test --locked`):

- without `pqc`: every test passes except the two post-quantum integration
  tests, `sign_create_detached_signature_v6_pqc` and
  `verify_detached_signature_v6_pqc`, which fail because their v6 PQC key no
  longer parses. That failure is the positive control that the feature is off;
- with `--features pqc`: every test passes, as upstream.

In AeroFTP, `proton_native::account` unlocks user and address keys generated
with `proton-crypto-account` through this crate, and refuses a token whose
signature belongs to another key.

## Removal condition

A released `proton-rpgp` whose dependency graph resolves next to `ml-kem` 0.3,
which needs rpgp's move to `ml-kem` 0.3 (rpgp/rpgp#573, announced in
rpgp/rpgp#855). Then delete this directory and the `[patch.crates-io]` entry
and keep `proton-crypto-account` on the registry version. No upstream report
has been filed by this change.
