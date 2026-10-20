# AeroFTP — threat model for the OSS scanner

AeroFTP is a desktop file-transfer client (Tauri 2: Rust backend in `src-tauri/`,
React frontend in `src/`), plus `aeroftp-cli` and an MCP server in
`src-tauri/src/mcp/`. It handles remote storage, local files, encrypted archives,
credentials and AI-assisted operations with the operating-system user's authority.
A hostile server or document must not acquire that authority merely because the
user connects to it or opens it.

Read `docs/THREAT-MODEL.md` for the full STRIDE analysis and residual-risk register,
and `SECURITY.md` for disclosure policy and known issues. These are context, not
proof that a mitigation works: check the scanned revision's implementation.

## Untrusted inputs and security boundaries

- **Remote servers and P2P peers**: protocol responses, listings, paths, symlinks,
  metadata, redirects and file bytes from every provider, AeroRsync and AeroShare
  (iroh). Encrypted or authenticated transport does not make a peer trustworthy.
- **Archives and encrypted containers**: member names, link targets, compression
  metadata, KDF parameters, ciphertext, signatures and recovery data. Check bounded
  parsing, authentication before publication, and extraction-root confinement.
- **AI input and output**: file content, remote names and tool results can carry
  prompt injection; model-generated tool names and arguments are untrusted. The
  backend must enforce approval, scope and credential isolation independently of
  the model and WebView, subject to the explicitly selected agent mode.
- **Delegated workers**: child identity, granted local root or remote profile,
  credential handles, cancellation, expiry and shared budgets are boundaries.
  A child must not inherit unrelated parent authority or another child's secrets.
- **WebView and browser traffic**: remote text rendered by React must not become
  script or privileged Tauri IPC. Loopback OAuth and local HTTP handlers must
  authenticate the intended flow and reject unrelated origins/requests.
- **CLI/MCP and imports**: local MCP clients and external MCP tool results,
  configuration/profile imports, batch scripts and plugin manifests/scripts.
  A local caller normally has the same OS user's authority; report an actual
  violation of an application promise, not that the user can access their files.
- **Updates and installation**: downloaded metadata, packages and plugin inputs
  must not bypass authenticity, integrity or privileged-helper checks.

## Code to prioritize

- `src-tauri/src/credential_store.rs`, profile authentication/import/export and
  vault modules: master password, OAuth/SSH/TOTP secrets, sealing and zeroization.
- `src-tauri/src/ai_tools.rs`, `src-tauri/src/ai_core/runner/` and
  `src-tauri/src/mcp/`: grants, request binding, path confinement, delegated
  credentials, subprocess handling and rate/budget enforcement.
- `src-tauri/src/providers/`, including `proton_native/`: hostile response
  parsing, key/signature verification, redirects, transport validation and secret
  handling. Include the local-provider path and cross-profile transfers.
- Archive/vault extraction, sync/journal logic, transfer publication, AeroRsync,
  AeroShare, OAuth/localhost handlers, plugins and update helpers.
- `src/`, Tauri capabilities/configuration and IPC command registration: the
  frontend is fully in scope, especially rendering and privileged command access.
- **AeroFTP-maintained dependency patches are in scope**: inspect
  `src-tauri/Cargo.toml`'s patch section and the `AEROFTP-PATCH.md` provenance files
  in `src-tauri/vendor/`. In particular, include our `unrar` FFI/header changes and
  `proton-rpgp` feature changes, plus our pinned dependency-fork deltas.

Generated build outputs and unmodified third-party code are not primary targets.
Still report unsafe AeroFTP integration or a vulnerable AeroFTP-owned delta; do
not exclude the entire vendor directory. Documentation is reference material.

## Exercising the checkout offline

The image contains the checkout at `/src`, frontend dependencies and built assets,
the locked Cargo cache, debug binaries in `/src/src-tauri/target/debug/`, and the
compiled library test harness. Default Cargo features are enabled, including
`aerorsync`, `local-stt` and `proton-native`.

```sh
cd /src
./src-tauri/target/debug/aeroftp-cli --version
./src-tauri/target/debug/aeroftp-cli agent-info --json
cargo test --frozen --manifest-path src-tauri/Cargo.toml --lib TEST_FILTER
npm run test:unit
```

Substitute `TEST_FILTER` with an actual test name filter (e.g.
`ai_tools::session_grant_tests`), or omit it to run the whole suite.

Setup compiles all library tests and runs small credential/grant regressions. It
does **not** certify that the full Rust suite passes. To run that suite, omit the
filter; do not mistake a timeout, an ignored test or missing service for a pass.
CLI/integration targets can be compiled from the cached graph with `--frozen` as
needed. Use synthetic local fixtures and isolated loopback peers for network
tests. Live cloud credentials, the Proton CLI, an OS keyring session, a graphical
session and physical MTP devices are not supplied; the optional Linux libmtp
backend is not linked in this image. Code for other platforms remains in scope
for source review, but distinguish that from a reproduced Linux finding.

## Severity guidance

State attacker control, prerequisites, affected mode and demonstrated impact.
Severity depends on a reachable boundary violation, not the presence of a
dangerous API or a hypothetical consequence.

- **Critical**: a hostile peer/document/model achieves arbitrary code execution,
  vault-secret extraction, or broad arbitrary file destruction without the
  required authorization, under a supported configuration.
- **High**: a demonstrated approval/scope bypass with sensitive read/write impact;
  credential leakage; WebView script injection reaching privileged IPC; silent
  TLS/SSH/signature/authentication bypass; extraction or publication outside the
  authorized root. A missing CSP or a denylist gap alone is not this impact.
- **Medium**: reproducible app/CLI denial of service from hostile input, bounded
  unauthorized effects, or non-secret metadata disclosure across a real boundary.
- **Low/informational**: limited-impact observations. Same-user access already
  allowed by the OS is not itself a new vulnerability. Local attackers are still
  relevant where a privileged helper or an explicit protection is defeated.

## Known behavior and deduplication

Use RR-01 through RR-12 in `docs/THREAT-MODEL.md` and the current known-issue table
in `SECURITY.md` to identify exact duplicates. Cite the ID and explain whether the
same prerequisites and impact apply. Check the locked dependency version before
assuming a documented advisory is still present.

Do not report only the documented fact that Extreme mode auto-approves tools,
the home-directory MCP scope is broad, user-installed plugins execute as the
user, a user explicitly disabled TLS/host-key verification, or `.aerozip` is
plaintext. **These are not blanket exclusions**: new bypasses, failure to honor
the selected scope/mode, unauthorized activation, or stronger impact remain in
scope. Expert mode uses an in-app approval panel; Safe/Normal mode's native-dialog
guarantee must not be assumed for Expert mode.

The CLI's `--strict` / `AEROFTP_STRICT=1` rejects documented safety-relaxing CLI
flags. It does not govern every GUI setting or all environment escape hatches;
RR-12 explicitly documents AeroRsync knobs it does not yet reject. Report a
failure of an actual strict-mode guarantee, and distinguish it from those known
limits. Accepted CSP/denylist risks do not exclude an exploitable XSS or a grant
bypass. Benign alignment/performance diagnostics in unmodified dependencies are
not findings; an exploitable FFI defect in our wrapper is in scope.

## Useful reports and patches

Give the scanned commit, file/line evidence, attacker-controlled input, affected
principal/resource, required configuration and minimal offline reproducer using
dummy data. Separate observed effects from hypotheses and note platform/service
prerequisites that prevented reproduction. Prefer a small source fix at the
enforcement point and a focused regression test. Keep reports private according
to `SECURITY.md`; never use real credentials or contact production endpoints.
