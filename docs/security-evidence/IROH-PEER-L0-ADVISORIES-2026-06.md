# iroh / peer-l0 transitive advisories - security evidence (2026-06)

Companion to the five `RUSTSEC-2026-0118 / 0119 / 2026-0002 / 2023-0089 / 2024-0436` entries in
`src-tauri/.cargo/audit.toml`. Records WHY they are ignored rather than fixed, and the
reachability analysis behind each. Investigated 2026-06-10 (T-PEER, branch
feat/peer-l0-spike-iroh); every cargo experiment restored the tree clean.

## Context
The user-to-user P2P drive links the isolated `aeroftp-peer-l0` crate as a **[dev-dependency]**
(`path = "peer-l0"`), exercised by `src-tauri/tests/peer_link.rs`. As a dev-dependency it builds
only for tests/benches and is **not in the shipped binary**, but its transitive graph (iroh 0.92,
iroh-blobs 0.94, iroh-docs 0.92, iroh-gossip 0.92) DOES enter `src-tauri/Cargo.lock`, so
`cargo audit` (CI gate, checks.yml) sees it. All five advisories originate exclusively in iroh's
relay / discovery / networking dependencies; none touch the app's own crypto, the russh SSH path,
or the sigstore path.

## The five advisories (exact versions + origin)
| ID | Crate | Class | Origin chain (all via iroh 0.92) |
|----|-------|-------|----------------------------------|
| RUSTSEC-2026-0118 | hickory-proto 0.25.2 | DoS (no 0.25-line fix) | hickory-resolver <- iroh-relay <- iroh |
| RUSTSEC-2026-0119 | hickory-proto 0.25.2 | DoS (fixed 0.26.1) | same chain |
| RUSTSEC-2026-0002 | lru 0.13.0 | unsound | pkarr 3.10 <- iroh-relay <- iroh |
| RUSTSEC-2023-0089 | atomic-polyfill 1.0.3 | unmaintained | heapless 0.7 <- postcard <- irpc <- iroh |
| RUSTSEC-2024-0436 | paste 1.0.15 | unmaintained | stun-rs <- iroh ; netlink <- netwatch <- iroh |

## Why ignored, not fixed: removal is blocked on every path (verified by cargo resolution)
1. **Targeted `cargo update` is pinned.** `cargo update -p hickory-proto --precise 0.26.1` fails:
   hickory-resolver 0.25.2 requires `^0.25`, and iroh 0.92 requires hickory-resolver `^0.25.1`.
   The 0.25 line is pinned the whole way up. Same shape for lru/paste/atomic-polyfill.
2. **Bumping iroh reintroduces the russh conflict WI-3a proved absent at 0.92.** Latest is iroh
   0.98.2 / blobs 0.102 / docs 0.100 / gossip 0.100; adding them fails to resolve because
   iroh-docs 0.100 pins `ed25519-dalek =3.0.0-pre.7` while russh 0.60.3 pins `=3.0.0-pre.6`
   (two exact pins on adjacent pre-releases of the same major). Intermediate versions hit the
   wall progressively: iroh-docs 0.93-0.95 -> `aead`; 0.96-0.97 -> `curve25519-dalek`; 0.100 ->
   `ed25519-dalek`. iroh 0.92 is the unique clean-resolution point with the app's current russh.
3. **Feature-gating cannot drop them.** hickory-resolver, pkarr and stun-rs are non-optional
   native deps of iroh 0.92. `iroh` with `default-features = false` still locks hickory-proto AND
   pkarr (verified). No feature flag removes any advisory crate.

## Reachability (the security substance)
- **RUSTSEC-2026-0118 (NSEC3 closest-encloser unbounded loop) = NOT REACHABLE.** The vulnerable
  code is behind hickory's `dnssec-ring`/`dnssec-aws-lc-rs` features (the `__dnssec` gate).
  hickory-resolver 0.25.2 `default` does not include dnssec, and `cargo tree -e features` over the
  full locked graph shows no crate enabling any dnssec feature. The NSEC3 proof path is not
  compiled. (This is the advisory with "no fixed upgrade available" for the 0.25 line - and it
  does not apply to us because DNSSEC validation is off.)
- **RUSTSEC-2026-0119 (O(n^2) name compression on encoding) = LOW reachability.** The quadratic
  blowup needs a message with many records to encode; iroh's discovery client encodes tiny
  single-name queries. The pathological case is large multi-record messages (server-side / large
  responses). Upstream-fixed in 0.26.1, gated behind the iroh-bump wall. DoS-class only.
- **RUSTSEC-2026-0002 (lru IterMut unsound)** = latent UB if `IterMut` is misused inside pkarr's
  discovery cache; no known exploit, soundness-class, no untrusted input in our use.
- **RUSTSEC-2023-0089 (atomic-polyfill) / RUSTSEC-2024-0436 (paste)** = "unmaintained", not
  vulnerabilities. paste is a compile-time proc-macro (no runtime surface). atomic-polyfill is a
  transitive atomics shim under postcard/heapless. Neither is a runtime risk.

## Exit path
Drop all five ignores when iroh ships a release that both (a) resolves with russh 0.60+ and
(b) uses hickory-proto >= 0.26.1. Re-review at each iroh bump attempt. Raw cargo-audit output and
the resolution experiments are archived on the NAS at
`linux-station/reports/wi3c-rustsec-investigation-2026-06-10.md` and
`linux-station/auto/wi3/wi3c-audit-full.txt`.

## WI-3d update (2026-06-10): iroh is now a SHIPPED dependency

As of WI-3d the L1 drive engine was moved into the `aeroftp-peer-l0` crate library and
`aeroftp-peer-l0` was promoted from `[dev-dependencies]` to a NORMAL `[dependencies]` of the app
(`src-tauri/Cargo.toml`), called via the in-app facade `src-tauri/src/peer/`. **Consequence: the
iroh P2P stack (~100 transitive crates) and therefore these five advisories now ship in the release
binary, not only in the test build (as they did at WI-3c).**

Nothing else changed about the risk assessment:
- The set of advisories is **UNCHANGED** (the same five IDs: RUSTSEC-2026-0118, -2026-0119,
  -2026-0002, -2023-0089, -2024-0436); `cargo audit` reports 0 vulnerabilities / 0 denied with
  exactly these five ignored, identical count to WI-3c.
- The **per-ID reachability arguments above are UNCHANGED** (DNSSEC/NSEC3 path not compiled;
  quadratic-encode only on large server-side messages; lru/pkarr cache fed no untrusted input;
  paste/atomic-polyfill are build-time/unmaintained, not vulnerabilities).
- The **exit path is UNCHANGED**: drop all five ignores when iroh ships a release that resolves with
  russh 0.60+ AND uses hickory-proto >= 0.26.1.

The promotion was an owner-approved, deliberate step (the P2P drive must run inside the app for the
WI-4d CLI). iroh stays confined behind the `aeroftp-peer-l0` crate boundary; the app reaches it only
through `src/peer/` and `tests/peer_link.rs`. `cargo tree -i iroh -e normal` from `src-tauri/` shows
the normal-edge path `aeroftp -> aeroftp-peer-l0 -> iroh`, confirming iroh is a shipped (non-dev)
dependency as of WI-3d.
