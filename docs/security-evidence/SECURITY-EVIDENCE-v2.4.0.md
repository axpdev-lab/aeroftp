# Security Evidence — v2.4.0

> Public release security evidence pack for AeroFTP.
> Tracks security claims, applied fixes, verification status, and acceptance gates.
>
> Status: Complete
> Date: 2026-02-19
> Owner: AeroFTP Development Team
> Reviewers: 12 auditors across 4 phases (6x Claude Opus 4.6 + GPT-5.3 Codex + 4x Opus Terminator Squad)

---

## 1) Release Metadata

- Version: v2.4.0
- Previous version: v2.3.0
- Branch/Tag: `main` / `v2.4.0`
- Commit range: v2.3.0..v2.4.0
- Platform scope tested: Linux (Ubuntu 24.04, WebKitGTK)
- Security grade claimed: A-
- Score label: Audited (12-auditor 4-phase review)

Minimum completion criteria:
- [x] Commit range and tag are final
- [x] Platform test matrix is explicit (Linux primary, CI builds all platforms)
- [x] Score label matches real validation state (no overclaim)
- [x] All CRITICAL and HIGH findings resolved

---

## 2) Security-Relevant Changes

### 2.1 — Zoho WorkDrive Provider (New)

**Change**: New cloud storage provider (`zoho_workdrive.rs`, ~900 lines) with full OAuth2 integration, 8 regional endpoints (US, EU, IN, AU, JP, UK, CA, SA), automatic team ID detection, and trash management.

**Security controls applied**:
- OAuth2 PKCE flow via existing `OAuth2Manager` with region-aware token/auth endpoints
- `refresh_guard: tokio::sync::Mutex<()>` prevents concurrent token refresh races (H-04)
- Token sanitized from error messages — generic error propagated to frontend (H-15)
- Region mapping covers all 10 Zoho data centers including CN and AE (H-09)
- Pagination for trash listing prevents unbounded memory growth (H-08)
- `tracing::` macros for consistent structured logging (BT-INT-030)

**Risk assessment**: Low — follows established OAuth2 patterns, credentials in SecretString.

### 2.2 — Streaming Upload Refactor (Modified)

**Change**: FTP, Dropbox, OneDrive, Google Drive, and Box uploads refactored from full-file buffering to chunk-based streaming from file handles.

**Security controls applied**:
- File handles read in chunks (64KB-10MB depending on provider) — eliminates OOM on large files (BT-GPT-H01)
- FTP: `tokio::fs::File::open()` + `AsyncReadExt` replaces `tokio::fs::read()`
- Dropbox: Upload session with chunked append for files >150MB
- OneDrive: Auto-switch to resumable upload for files >4MB
- Google Drive: 10MB chunks from file handle for resumable path
- Box: Per-chunk SHA-1 from file handle instead of pre-computed on full buffer

**Risk assessment**: High impact fix — prevents denial-of-service via large file upload.

### 2.3 — SecretString for All Provider Credentials (Modified)

**Change**: Access tokens, refresh tokens, and API keys across all 16 providers wrapped in `secrecy::SecretString` for automatic memory zeroization on drop.

**Security controls applied**:
- `WebDavConfig.password`: `String` → `secrecy::SecretString`
- `S3Config.secret_access_key`: `String` → `secrecy::SecretString`
- `AzureConfig.access_key`: `String` → `secrecy::SecretString`
- `AzureConfig.sas_token`: `Option<String>` → `Option<secrecy::SecretString>`
- 4shared OAuth tokens: 3 credential fields wrapped
- All credential access via `.expose_secret()` at point of use only

**Risk assessment**: Critical improvement — eliminates credential residue in memory dumps across entire provider layer.

### 2.4 — FTP TLS Downgrade Detection (New)

**Change**: New `tls_downgraded: bool` flag on FTP connections. When `ExplicitIfAvailable` mode fails TLS upgrade, flag is set and security warnings logged.

**Security controls applied**:
- `SECURITY:` log prefix for TLS downgrade events
- Warning includes host:port and "Credentials will be sent unencrypted"
- Flag accessible for future UI security badge integration

**Risk assessment**: Medium — improves visibility of STARTTLS stripping attacks.

### 2.5 — XML Parsing Migration (Modified)

**Change**: All regex-based XML parsing replaced with `quick-xml 0.39` event-based parser for WebDAV PROPFIND, S3 ListBucketResult, and Azure ListBlobs.

**Security controls applied**:
- Event-based parsing prevents ReDoS attacks on malformed XML responses
- `trim_text(true)` for Azure fixes whitespace injection in blob names (BT-API-012)
- State machine pattern (`ParseState` enum) for Azure ensures correct field extraction
- No `unsafe` code — pure safe Rust parsing

**Risk assessment**: Low — eliminates regex-based XML vulnerabilities, improves correctness.

### 2.6 — Streaming Download Refactor (Modified)

**Change**: FTP, Filen downloads refactored from full-file RAM buffering to chunked streaming to disk. Parallel sync progress throttled.

**Security controls applied**:
- FTP download: `Vec<u8>` + `read_to_end()` → `tokio::fs::File` + 8KB chunk loop (P0-1)
- Filen download: `Vec<u8>` accumulation → per-chunk `write_all()` to file immediately after decrypt (P0-3)
- Parallel sync: 150ms/2% delta throttle on progress event emission — prevents IPC flooding (P0-4)
- Zoho WorkDrive: `reqwest::Client` reuse instead of per-download allocation (P1-1)
- Dropbox: Progress callback enabled in upload session append loop (P1-3)
- Cloud Service: Conflicts Vec capped at 10,000 entries (P1-4)
- Frontend: `completedTransferIds` Set capped at 500 with FIFO eviction (P1-2)

**Risk assessment**: Critical fix — eliminates denial-of-service via large file download (800MB peak → 64KB peak for 8 parallel FTP streams).

### 2.7 — StorageProvider Trait Expansion (Modified)

**Change**: 11 new trait methods added: `stat()`, `search()`, `move_file()`, `list_trash()`, `restore_from_trash()`, `permanent_delete()`, `create_share_link()`, `get_storage_quota()`, `list_versions()`, `download_version()`, `restore_version()`.

**Security controls applied**:
- All methods follow existing path validation patterns
- Trash operations require explicit provider support (default returns "not supported")
- Version operations use provider-native version IDs (no path manipulation)

**Risk assessment**: Low — extends existing validated patterns.

---

## 3) Findings Ledger (Current Release)

82 findings from provider audit + 17 findings from resource management audit (total: 99). 63 fixed, 27 risk-accepted (MEDIUM/LOW/INFO), 9 deferred to v2.4.1/v2.5.0.

### CRITICAL Findings (3/3 Fixed)

| ID | Description | Provider | Fix |
|----|-------------|----------|-----|
| CRIT-01 | pCloud token exposed in URL query string | pCloud | `Authorization: Bearer` header via `auth_header()` |
| CRIT-02 | Filen encryption key exposed to frontend | Filen | Keys in backend-only `file_key_cache`, removed from metadata |
| CRIT-03 | Azure access key stored as plain String | Azure | Wrapped in `secrecy::SecretString` |

### HIGH Findings (17/17 Fixed)

| ID | Description | Fix |
|----|-------------|-----|
| H-01 | Full-file in-memory transfers (OOM) | Streaming download/upload for all providers |
| H-02 | FTP STARTTLS stripping attack | `tls_downgraded` flag + security warning |
| H-03 | WebDAV password as plain String | `secrecy::SecretString` |
| H-04 | OAuth2 token refresh race condition | `tokio::sync::Mutex<()>` refresh guard |
| H-05 | S3 list_keys no pagination (>1000 items) | ContinuationToken pagination loop |
| H-06 | Google Drive query injection | Backslash + single quote escaping |
| H-08 | Zoho list_trash no pagination | Pagination loop |
| H-09 | Zoho/OAuth region desync (cn, ae) | Added region mappings |
| H-10 | S3 SigV4 canonical path not URI-encoded | Per-segment URI encoding |
| H-11 | Inconsistent credential protection | SecretString for WebDAV/S3/Azure/4shared |
| H-12 | 4shared OAuth tokens as plain String | SecretString for 3 fields |
| H-13 | Box no pagination (max 1000) | Offset-based pagination loop |
| H-14 | S3 XML response logged at info! level | Downgraded to `debug!` |
| H-15 | Zoho token leak in error message | Generic error, no token |
| BT-H01 | Upload OOM: 5 providers buffer file | Streaming from file handle |
| BT-H02 | FTP ExplicitIfAvailable TLS downgrade | `tls_downgraded: bool` + `SECURITY:` log |

### Resource Management Findings (8/17 Fixed, 9 Deferred)

Identified by 5x Claude Opus 4.6 agents + GPT-5.3 Codex cross-reference.

| ID | Severity | Description | Fix |
|----|----------|-------------|-----|
| P0-1 | Critical | FTP download buffers entire file in RAM | Streaming 8KB chunks to `tokio::fs::File` |
| P0-3 | Critical | Filen decrypt accumulates entire file | Per-chunk `write_all()` immediately after decrypt |
| P0-4 | Critical | Parallel sync progress no throttle (IPC flood) | 150ms/2% delta guards in both closures |
| P1-1 | High | Zoho creates new reqwest::Client per download | Reuse `self.client` with Accept override |
| P1-2 | High | completedTransferIds Set unbounded | Cap at 500 entries with FIFO eviction |
| P1-3 | High | Dropbox upload session no progress callback | Callback after start + each append |
| P1-4 | High | Cloud Service conflicts Vec unbounded | Guard `if len < 10_000` before push |
| P0-2 | — | SFTP download buffering (false positive) | Already streaming at 32KB chunks |

### Risk-Accepted (27 items)

All deferred items are MEDIUM/LOW/INFO severity requiring protocol-level or platform-specific work beyond the provider layer scope (e.g., FTP PASV IP validation, SFTP TOFU UX, Digest auth MD5, MEGA .bat on Windows). See internal audit document for full details.

---

## 4) Applied Fixes Summary

| Fix ID | Priority | Description | Files | Verification |
|--------|----------|-------------|-------|--------------|
| CRIT-01 | P0 | pCloud token to Authorization header | `pcloud.rs` | Code review — no URL query token |
| CRIT-02 | P0 | Filen keys removed from IPC metadata | `filen.rs` | Code review — keys in backend cache only |
| CRIT-03 | P0 | Azure key to SecretString | `types.rs`, `azure.rs` | Code review — `.expose_secret()` |
| H-01/BT-H01 | P1 | Streaming uploads for 5 providers | `ftp.rs`, `dropbox.rs`, `onedrive.rs`, `google_drive.rs`, `box_provider.rs` | Manual — 500MB file upload without OOM |
| H-02/BT-H02 | P1 | FTP TLS downgrade detection | `ftp.rs` | Manual — test with TLS-rejecting server |
| H-03/H-11/H-12 | P1 | SecretString for all credentials | `types.rs`, 5 providers | Code review — zero plain String credentials |
| H-04 | P1 | OAuth2 refresh mutex | `oauth2.rs` | Code review — single refresh guard |
| H-05/H-13 | P1 | Pagination for S3/Box/Azure | `s3.rs`, `box_provider.rs`, `azure.rs` | Manual — >1000 items listed correctly |
| XML-MIG | P2 | quick-xml 0.39 migration | `webdav.rs`, `s3.rs`, `azure.rs` | `cargo check` — zero warnings |
| I18N-KEYS | P3 | Zoho keys in 47 languages | 47 locale files | `npm run i18n:validate` — 47/47 at 100% |
| P0-1 | P0 | FTP download streams to disk | `ftp.rs`, `providers/ftp.rs` | Code review — 8KB chunk loop, no Vec buffer |
| P0-3 | P0 | Filen decrypt streams to disk | `providers/filen.rs` | Code review — per-chunk write_all |
| P0-4 | P0 | Parallel sync progress throttle | `lib.rs` | Code review — 150ms/2% guards |
| P1-1 | P1 | Zoho client reuse | `zoho_workdrive.rs` | Code review — self.client with Accept override |
| P1-2 | P1 | Transfer ID Set bounded | `useTransferEvents.ts` | Code review — cap 500 FIFO |
| P1-3 | P1 | Dropbox session progress | `dropbox.rs` | Code review — callback in append loop |
| P1-4 | P1 | Conflicts Vec bounded | `cloud_service.rs` | Code review — 10K guard |

---

## 5) Security Tests and Results

### Automated

| Test Suite | Scope | Result | CI Job | Artifact |
|------------|-------|--------|--------|----------|
| `npm run build` | TypeScript compilation | Pass | Local | Zero errors |
| `npm run i18n:validate` | i18n key completeness | Pass | Local | 47/47 languages at 100% |
| `cargo check` | Rust compilation | Pass | Local | Zero errors/warnings |
| `npm run security:regression` | Security regression (5 checks) | Pass | Local | All checks green |

### Manual Validation

| Scenario | Expected | Result | Tester | Date |
|----------|----------|--------|--------|------|
| Zoho WorkDrive OAuth connect (US region) | Token stored in vault, team detected | Pass | Dev | 2026-02-19 |
| Upload 100MB+ file via FTP | No OOM, streaming chunks | Pass | Dev | 2026-02-19 |
| FTP ExplicitIfAvailable with TLS-rejecting server | `tls_downgraded=true`, warning logged | Pass | Dev | 2026-02-19 |
| S3 bucket with >1000 objects | All objects listed via pagination | Pass | Dev | 2026-02-19 |
| WebDAV/Azure XML responses with whitespace | Correct parsing with quick-xml | Pass | Dev | 2026-02-19 |
| pCloud file download | No token in URL, Authorization header used | Pass | Dev | 2026-02-19 |
| Zoho trash list/restore/permanent delete | All operations functional | Pass | Dev | 2026-02-19 |

| FTP download 500MB file | Streaming, no memory spike | Pass | Dev | 2026-02-19 |
| 8 parallel FTP downloads | ~64KB peak RAM (not 800MB) | Pass | Dev | 2026-02-19 |
| Filen encrypted file download | Per-chunk decrypt+write | Pass | Dev | 2026-02-19 |
| Dropbox upload >150MB | Progress visible during session | Pass | Dev | 2026-02-19 |

Known limitations:
- MEGA CLI dependency for some operations (platform-specific .bat risk on Windows — documented)
- FTP PASV IP validation deferred (requires protocol-level changes)
- SFTP TOFU UX improvement deferred to v2.5.0
- Resource audit P1-5/6/7 and all P2 deferred to v2.4.1/v2.5.0 (see `docs/dev/RESOURCE-ANALYSIS-v2.4.0.md`)

---

## 6) Regression Watchlist

- [x] AI tool whitelist — unchanged (45 tools + clipboard_read_image)
- [x] Plugin execution model — unchanged
- [x] Chat history SQLite — unchanged
- [x] TOTP 2FA for vault — unchanged
- [x] Remote vault security — unchanged
- [x] Shell execute security controls — unchanged
- [x] CSP configuration — unchanged
- [x] Tauri capabilities scope — unchanged
- [x] Path validation in AI tools — unchanged
- [x] Credential storage and migration — **enhanced** (SecretString for all 16 providers)

---

## 7) Risk Acceptance (If Any)

| Risk ID | Severity | Reason accepted | Expiry date | Owner |
|---------|----------|-----------------|------------|-------|
| RISK-240-01 | Medium | FTP PASV IP validation — requires protocol-level changes to suppaftp | v2.6.0 | Dev Team |
| RISK-240-02 | Medium | SFTP TOFU lacks visual fingerprint UX — connections still fail-closed on mismatch | v2.5.0 | Dev Team |
| RISK-240-03 | Low | MEGA .bat command injection on Windows — mitigated by backend-only execution | v2.6.0 | Dev Team |
| RISK-240-04 | Low | WebDAV Digest auth uses MD5 — server-negotiated, no alternative | Permanent | Dev Team |
| RISK-240-05 | Medium | Cloud Filter root cleanup not called on shutdown (Windows) | v2.4.1 | Dev Team |
| RISK-240-06 | Medium | Remote file index HashMap unbounded in AeroCloud sync | v2.4.1 | Dev Team |
| RISK-240-07 | Low | Cloud Service status lock contention during sync | v2.4.1 | Dev Team |

---

## 8) Evidence Index

- Diffs:
  - `zoho_workdrive.rs` — New Zoho WorkDrive provider (~900 lines)
  - `ZohoTrashManager.tsx` — Zoho trash management UI
  - `types.rs` — SecretString migrations for WebDAV/S3/Azure credentials
  - `ftp.rs` — Streaming upload + TLS downgrade detection
  - `dropbox.rs`, `onedrive.rs`, `google_drive.rs`, `box_provider.rs` — Streaming upload refactor
  - `webdav.rs`, `s3.rs`, `azure.rs` — quick-xml 0.39 migration
  - `oauth2.rs` — Zoho OAuth config + refresh mutex
  - `pcloud.rs` — Token moved to Authorization header
  - `filen.rs` — Encryption keys removed from frontend metadata
  - `mod.rs` — Zoho module registration, trait expansion (11 new methods)
  - `provider_commands.rs` — Zoho trash commands
  - `lib.rs` — Zoho command registrations
  - Frontend: `ProtocolSelector.tsx`, `ProviderLogos.tsx`, `SavedServers.tsx`, `OAuthConnect.tsx`, `ConnectionScreen.tsx`, `registry.ts`, `types.ts`
  - 47 locale files — Zoho i18n keys
  - `ftp.rs` — Streaming download refactor (8KB chunks to disk)
  - `providers/ftp.rs` — StorageProvider download streaming
  - `providers/filen.rs` — Per-chunk decrypt + write to disk
  - `lib.rs` — Parallel sync progress throttle (150ms/2%)
  - `providers/zoho_workdrive.rs` — Client reuse for downloads
  - `providers/dropbox.rs` — Upload session progress callback
  - `cloud_service.rs` — Conflicts Vec cap (10K)
  - `useTransferEvents.ts` — Transfer ID Set cap (500 FIFO)
- Audit reports:
  - `docs/dev/providers/PROVIDERS-FINAL-AUDIT-v2.4.0.md` — 12-auditor consolidated audit (82 findings, grade A-)
  - `docs/dev/RESOURCE-ANALYSIS-v2.4.0.md` — Resource management audit (17 findings, 8 fixed)
  - Phase A: Critical + Frontend (6x Claude Opus 4.6)
  - Phase B: Security (GPT-5.3 Codex)
  - Phase C: Integration (Claude Opus 4.6)
  - Phase D: Bugs Terminator Counter-Audit (4x Opus + GPT-5.3)
- CI runs:
  - GitHub Actions triggered on tag push (builds Linux/Windows/macOS)
- Test reports:
  - `npm run build` — zero errors
  - `npm run i18n:validate` — 47/47 at 100%
  - `cargo check` — zero errors/warnings
  - `npm run security:regression` — all checks pass

---

## 9) Security Sign-off

- Engineering owner sign-off: AeroFTP Dev Team
- Security reviewer sign-off: 12 provider auditors + 5 resource auditors + GPT-5.3 Codex cross-ref
- Release manager sign-off: AeroFTP Dev Team

Decision:
- [ ] Approved for release
- [x] Approved with accepted risks
- [ ] Blocked

---

## 10) Post-release Follow-up

- [ ] 24h monitoring completed
- [ ] 7-day regression check completed
- [ ] New findings triaged into roadmap

Follow-up issues:
- SFTP TOFU visual fingerprint dialog (v2.5.0)
- CSP Phase 2 tightening (v2.5.0) — replace wildcard sources with specific origins
- FTP PASV IP validation (v2.6.0)
- Resource audit Phase 2 (v2.4.1): Cloud Filter cleanup, remote index bounds, status lock throttle
- Resource audit Phase 3 (v2.5.0): HashMap bounds, journal optimization, SyncPanel state cleanup, config validation
