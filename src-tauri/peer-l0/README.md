# aeroftp-peer-l0 — L0 Spike Crate (isolated)

**This is the active working tree for the T-PEER L0 connectivity spike.**

It exists as a completely separate Cargo package (`src-tauri/peer-l0/`) so that we can use modern iroh + iroh-blobs without poisoning the main `aeroftp` crate's dependency graph (russh 0.60.3 pins `aead = "=0.6.0-rc.10"` while iroh 0.95 pulls rc.2).

When the L0 gate is passed and we are ready to promote the capability, we will:
1. Resolve the dep conflict in the main tree (most likely by updating russh or accepting a controlled upgrade).
2. Move / merge the proven code under `src-tauri/src/peer/` behind a proper `peer` feature.
3. Delete or archive this directory.

## Quick start (local sanity)

```bash
cd src-tauri/peer-l0
cargo run --bin peer-l0-dial -- --help
cargo run --bin peer-l0-dial -- --mode listen   # terminal 1
cargo run --bin peer-l0-dial -- --mode dial --node <NODE_ID_FROM_LISTEN>   # terminal 2 (same machine)
```

The local dial always succeeds (no NAT). It proves the plumbing.

## Real L0 measurement (the actual gate)

The Go/No-Go decision is **not** "does it compile and dial on localhost".

It is:

> Does dial-by-NodeID + hole-punch + (optional relay) actually produce acceptable success rates on **real hostile consumer networks** (double NAT, CGNAT, mobile hotspots, strict office firewalls, one side on Starlink, etc.)?

### How to generate gate data

1. Build the binary on two different machines / networks:
   ```bash
   cargo build --release --bin peer-l0-dial
   ```

2. On machine A (the "receiver"):
   ```bash
   ./peer-l0-dial --mode listen --report /tmp/a-recv.json --note "home behind CGNAT + IPv4 only"
   ```
   It will print its NodeID. Leave it running.

3. On machine B (the "sender"), from a completely different network:
   ```bash
   ./peer-l0-dial --mode dial --node <THE_NODE_ID> --report /tmp/b-send.json --note "mobile hotspot"
   ```

4. After a successful (or failed) transfer of a tiny blob, both sides write a `ConnectivitySample` JSON.

5. Collect 20–50 samples across different combinations. The bar for greenlighting L1 is roughly "direct or hole-punched works the majority of the time for real users, or we are happy to document 'use a self-hosted relay' as a first-class supported path".

See the parent `docs/dev/roadmap/APPENDIX-PEER/PEER-P2P-Transfer.md` §7 and §8 for the exact Go/No-Go criteria.

## Current implemented surface (L0 slice)

- Ephemeral NodeId per run (stable key persistence comes after we decide the identity story with MU-VAULT).
- `listen` mode that accepts one connection and receives a small offer + blob.
- `dial` mode that connects by NodeId and sends a small blob (currently random bytes + blake3; vault encryption comes in the next slice).
- Explicit "offer" with size + name hint + optional human note (the only allowed human channel).
- Receiver prints the offer and asks for confirmation on stdin (the future GUI will be a nice modal).
- Structured JSON report for the measurement harness.

## Next immediate slices (after this compiles and basic dial works)

- Real end-to-end encrypted blob using the existing `user_crypto` primitives (or a fresh session key + long-term peer public key wrapping).
- Small "peer inbox" area on disk for received blobs (never auto-extract into the user's main filesystem).
- Size and count quotas on the receive side.
- Proper ALPN + length-prefixed protocol (or switch to iroh-blobs `iroh_blobs::protocol` for the wire format — it already gives us progress + verification for free).

## License / research note

Same as the rest of AeroFTP (GPL-3.0-or-later). This directory is explicitly **not** shipped in releases until the track graduates from RESEARCH ONLY.
