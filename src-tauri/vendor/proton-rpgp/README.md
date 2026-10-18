# Crate proton-rpgp

Pure Rust Proton specific OpenPGP library built on [rpgp](https://github.com/rpgp/rpgp). Used as Rust backend in [proton-crypto](https://github.com/ProtonMail/proton-crypto-rs).

## Usage

Add to `Cargo.toml`:

```toml
[dependencies]
proton-rpgp = "0.1"
```

Optional features:

- **`asm`** — Assembly-based optimizations.
- **`wasm`** — Build for WebAssembly.

## License

MIT. See [LICENSE](LICENSE).

## Tracked Security Issues in Dependencies

| Advisory ID | Issue Description | Status |
|-------------|-------------------|--------------------------------|
| [RUSTSEC-2023-0071](https://github.com/RustCrypto/RSA/security/advisories/GHSA-c38w-74pg-36hr) | Timing side-channel in RSA crate | Waiting for fix in the RSA crate |
| [RUSTSEC-2025-0144](https://github.com/RustCrypto/signatures/security/advisories/GHSA-hcp2-x6j4-29j7) | Timing side-channel in ML-DSA crate | Waiting for rpgp to bump ml-dsa |
| [GHSA-5x2r-hc65-25f9](https://github.com/RustCrypto/signatures/security/advisories/GHSA-5x2r-hc65-25f9) | ML-DSA Signature Verification Accepts Signatures with Repeated Hint Indices | Waiting for rpgp to bump ml-dsa |
