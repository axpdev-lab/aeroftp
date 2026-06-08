# Dependency Evaluation: reed-solomon-erasure (AeroVault v4 ECC) — 2026-06-08

> Scope: the `reed-solomon-erasure` crate added for the AeroVault v4 ECC
> (error-correction) wrapper on branch `feat/aerovault-v4-ecc`.
> Purpose: document why the dependency was chosen, its audit posture, and the
> one transitive advisory it introduces (RUSTSEC-2024-0384, `instant`).
> Status: accepted with a tracked, justified `cargo audit` ignore. No code-path
> exposure. Companion: `src-tauri/.cargo/audit.toml`.

---

## 1. What was added and why

| Field | Value |
| --- | --- |
| Crate | `reed-solomon-erasure` |
| Version | `6.0.0` (latest stable 6.x) |
| Manifest | `src-tauri/Cargo.toml` |
| Checksum | `7263373d500d4d4f505d43a2a662d475a894aa94503a1ee28e9188b5f3960d4f` |
| Used by | `src-tauri/src/aerovault_v3.rs` (`compute_ecc_shards`, `reconstruct_from_ecc`) |
| Reachable from | ECC-enabled vaults only (`create --ecc`); inert for v1/v2 and non-ECC v3 |

The ECC wrapper is the fourth wrapper in the AeroVault pipeline
(`compression -> chunking -> crypt -> ECC`), per discussions #272/#276. It
protects the concatenated live-block stream of an encrypted vault with a fixed
Reed-Solomon 10+2 grid (~20% overhead). `reed-solomon-erasure` was selected
because:

- Pure Rust, no C/FFI surface (consistent with the project's audit history of
  removing FFI/RSA exposure).
- Mature GF(2^8) implementation with explicit data/parity shard configuration
  and erasure-only reconstruction, which is exactly the model the ECC layer
  needs (checksum-driven erasure localization over already-encrypted blocks).
- Low direct footprint; widely used in archival/backup tooling.

Alternatives considered: `reed-solomon-simd` (faster, newer, but a different
API and SIMD-runtime assumptions) and `reed-solomon-novelpoly` (FFT-based,
larger). Both are viable future migration targets (see §4).

## 2. Audit posture

`cargo audit` is clean for `reed-solomon-erasure` itself (no advisory against
the crate). It does, however, pull one **transitive** unmaintained dependency:

```
instant 0.1.13 <- parking_lot 0.11.2 <- reed-solomon-erasure 6.0.0 <- aeroftp
```

`reed-solomon-erasure 6.0.0` pins `parking_lot = "^0.11.2"`. parking_lot 0.12
dropped `instant`, but the `^0.11.2` requirement cannot resolve to 0.12, so the
advisory is **not locally fixable** without forking the crate (verified:
`cargo update -p parking_lot --precise 0.12.4` fails the version requirement).

### RUSTSEC-2024-0384 (`instant` unmaintained) — accepted, ignored with rationale

- `instant` is a monotonic-clock shim, superseded by `web-time`. The only code
  path with any concern is wasm-specific.
- AeroFTP's native desktop targets (Linux/macOS/Windows, never wasm) compile
  `instant` down to a thin wrapper over `std::time::Instant`; the wasm path is
  never built.
- parking_lot uses it only for internal lock-elapsed bookkeeping, reached
  exclusively from `reed-solomon-erasure`'s in-memory parity compute/reconstruct
  on ECC-enabled vaults. No untrusted input, no parsing, and no cryptographic
  decision flows through `instant`.
- Decision: ignore `RUSTSEC-2024-0384` in `src-tauri/.cargo/audit.toml` with the
  justification above, consistent with the project's policy of ignoring
  transitive unmaintained advisories only with a written threat-model note.

The full transitive set introduced (`lru`, `ahash`, `libm`, `parking_lot`,
`lock_api`, `parking_lot_core`, `smallvec`, `instant`) is otherwise advisory-clean
at the time of writing.

## 3. Exposure analysis

- ECC code runs only on vaults created with `--ecc`. A user who never enables
  ECC never executes any `reed-solomon-erasure` code.
- Inputs to the RS layer are the vault's own already-encrypted blocks and the
  ECC payload, both produced locally; there is no network or attacker-controlled
  input to the parity math.
- Repair correctness is gated end-to-end by re-verifying every reconstructed
  block against its authenticated `cipher_hash` (all-or-nothing persist), so a
  faulty reconstruction can never silently corrupt a vault.

## 4. Exit path / future work

- Preferred: migrate to a maintained RS crate (`reed-solomon-simd` or
  `reed-solomon-novelpoly`) once the ECC API is stable, which would also drop the
  old parking_lot/`instant` chain.
- Or: adopt a newer `reed-solomon-erasure` release if upstream bumps
  `parking_lot` to >= 0.12.
- Until then, the `RUSTSEC-2024-0384` ignore stays and is reviewed on each
  `cargo audit` pass.

## 5. Gates at evaluation time

| Gate | Result |
| ---- | ------ |
| `cargo test --lib aerovault_v3` | Pass (22) |
| `cargo clippy --all-targets` | Pass |
| `cargo audit` (with the two justified ignores) | Pass |
| `cargo fmt --all -- --check` | Pass |

> Note: this evaluation also covers the unrelated `RUSTSEC-2026-0173`
> (`proc-macro-error2`, build-time, sigstore/oci path) that was freshly
> published on 2026-06-07 and surfaced in the same `cargo audit` run; it is
> ignored with its own rationale in `audit.toml` and is not part of the ECC
> dependency set.
