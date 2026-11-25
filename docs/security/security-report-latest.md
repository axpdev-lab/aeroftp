# AeroFTP Security Report

- Frontend version: **4.2.8**
- Backend (src-tauri) version: **4.2.8**
- Generated: 2026-11-25T14:56:44.947Z
- Platform: linux-x64

## Summary

**No open vulnerabilities** across cargo audit, npm audit, and osv-scanner.
- 35 additional finding(s) suppressed via documented rationale in [`src-tauri/.cargo/audit.toml`](../../src-tauri/.cargo/audit.toml)

| Tool | Version | Result |
|---|---|---|
| cargo audit | 0.22.2 | 0 vulnerabilities, 0 warnings |
| npm audit | 11.19.0 | 0 critical, 0 high, 0 moderate, 0 low, 0 info |
| osv-scanner | 2.3.8 | 0 open, 35 suppressed |

## cargo audit

No vulnerabilities found in the Rust dependency tree.

## npm audit

No vulnerabilities found in production npm dependencies.

## osv-scanner

No open findings. All known advisories in scope are documented in the suppression list with written rationales.

### Suppressed via [`src-tauri/.cargo/audit.toml`](../../src-tauri/.cargo/audit.toml): 35 findings (one per package version and advisory, counted once across lockfiles)

| Ecosystem | Package | ID | CVSS | Fixed in | Summary |
|---|---|---|---|---|---|
| crates.io | atomic-polyfill 1.0.3 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2023-0089](https://osv.dev/vulnerability/RUSTSEC-2023-0089) | unknown |  | atomic-polyfill is unmaintained |
| crates.io | chrono 0.4.45 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0543](https://osv.dev/vulnerability/RUSTSEC-2026-0543) | unknown |  | chrono is unmaintained |
| crates.io | crypto-bigint 0.5.5 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0387](https://osv.dev/vulnerability/RUSTSEC-2026-0387) | unknown | 0.7.6 | Multiple timing variability issues in operations intended to be constant-time |
| crates.io | glib 0.18.5 (src-tauri/Cargo.lock) | [RUSTSEC-2024-0429](https://osv.dev/vulnerability/RUSTSEC-2024-0429) | unknown | 0.20.0 | Unsoundness in `Iterator` and `DoubleEndedIterator` impls for `glib::VariantStrIter` |
| crates.io | glib 0.18.5 (src-tauri/Cargo.lock) | [GHSA-wrw7-89jp-8q8g](https://osv.dev/vulnerability/GHSA-wrw7-89jp-8q8g) | CVSS:4.0/AV:N/AC:L/AT:N/PR:N/UI:N/VC:N/VI:L/VA:N/SC:N/SI:N/SA:N | 0.20.0 | Unsoundness in `Iterator` and `DoubleEndedIterator` impls for `glib::VariantStrIter` |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0574](https://osv.dev/vulnerability/RUSTSEC-2026-0574) | unknown | 0.9.4 | `FindMutView::pop` after mutating the value would lead to a use after free |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0575](https://osv.dev/vulnerability/RUSTSEC-2026-0575) | unknown | 0.9.4 | `Spsc::peek` used `Relaxed` ordering where it should have use `Acquire` |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0576](https://osv.dev/vulnerability/RUSTSEC-2026-0576) | unknown | 0.9.4 | `IndexMap::truncate` could lead to an inconsistent state |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0577](https://osv.dev/vulnerability/RUSTSEC-2026-0577) | unknown | 0.9.4 | Panicking `Drop` implementation could lead to UB |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0578](https://osv.dev/vulnerability/RUSTSEC-2026-0578) | unknown | 0.9.4 | `SortedLinkedList::zeroize` lead to an inconsistent state |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0579](https://osv.dev/vulnerability/RUSTSEC-2026-0579) | unknown | 0.9.4 | The `Deque` implementation from `TryFrom` was wrong when the `Deque` ended up full |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0580](https://osv.dev/vulnerability/RUSTSEC-2026-0580) | unknown | 0.9.4 | The treiber stack implementation in heapless had a race condition |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0581](https://osv.dev/vulnerability/RUSTSEC-2026-0581) | unknown | 0.9.4 | `Deque::make_contiguous` could lead to broken invariants |
| crates.io | heapless 0.7.17 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2026-0582](https://osv.dev/vulnerability/RUSTSEC-2026-0582) | unknown | 0.9.4 | On maps of `u16::MAX` elements, `IndexMap::insert` could lead to a broken invariant |
| crates.io | instant 0.1.13 (src-tauri/Cargo.lock) | [RUSTSEC-2024-0384](https://osv.dev/vulnerability/RUSTSEC-2024-0384) | unknown |  | `instant` is unmaintained |
| crates.io | libsqlite3-sys 0.30.1 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0459](https://osv.dev/vulnerability/RUSTSEC-2026-0459) | CVSS:4.0/AV:L/AC:L/AT:N/PR:N/UI:P/VC:H/VI:N/VA:N/SC:N/SI:N/SA:N | 0.39.0 | `libsqlite3-sys` via C SQLite CVE-2026-119375 |
| crates.io | lru 0.7.8 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0253](https://osv.dev/vulnerability/RUSTSEC-2026-0253) | unknown | 0.18.2 | Potential use-after-free due to lack of panic safety in `LruCache::pop()` |
| crates.io | paste 1.0.15 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2024-0436](https://osv.dev/vulnerability/RUSTSEC-2024-0436) | unknown |  | paste - no longer maintained |
| crates.io | proc-macro-error 0.4.12 (src-tauri/Cargo.lock, src-tauri/peer-l0/Cargo.lock) | [RUSTSEC-2024-0370](https://osv.dev/vulnerability/RUSTSEC-2024-0370) | unknown |  | proc-macro-error is unmaintained |
| crates.io | proc-macro-error 1.0.4 (src-tauri/Cargo.lock) | [RUSTSEC-2024-0370](https://osv.dev/vulnerability/RUSTSEC-2024-0370) | unknown |  | proc-macro-error is unmaintained |
| crates.io | rsa 0.9.10 (src-tauri/Cargo.lock) | [RUSTSEC-2023-0071](https://osv.dev/vulnerability/RUSTSEC-2023-0071) | CVSS:3.1/AV:N/AC:H/PR:N/UI:N/S:U/C:H/I:N/A:N | 0.10.0 | Marvin Attack: potential key recovery through timing sidechannels |
| crates.io | tauri 2.11.0 (src-tauri/Cargo.lock) | [GHSA-7gmj-67g7-phm9](https://osv.dev/vulnerability/GHSA-7gmj-67g7-phm9) | CVSS:3.1/AV:N/AC:L/PR:N/UI:R/S:U/C:H/I:H/A:H | 2.11.1 | Tauri has an Origin Confusion Issue that Allows Remote Pages to Invoke Local-Only IPC Commands |
| crates.io | unic-char-property 0.9.0 (src-tauri/Cargo.lock) | [RUSTSEC-2025-0081](https://osv.dev/vulnerability/RUSTSEC-2025-0081) | unknown |  | `unic-char-property` is unmaintained |
| crates.io | unic-char-range 0.9.0 (src-tauri/Cargo.lock) | [RUSTSEC-2025-0075](https://osv.dev/vulnerability/RUSTSEC-2025-0075) | unknown |  | `unic-char-range` is unmaintained |
| crates.io | unic-common 0.9.0 (src-tauri/Cargo.lock) | [RUSTSEC-2025-0080](https://osv.dev/vulnerability/RUSTSEC-2025-0080) | unknown |  | `unic-common` is unmaintained |
| crates.io | unic-ucd-ident 0.9.0 (src-tauri/Cargo.lock) | [RUSTSEC-2025-0100](https://osv.dev/vulnerability/RUSTSEC-2025-0100) | unknown |  | `unic-ucd-ident` is unmaintained |
| crates.io | unic-ucd-version 0.9.0 (src-tauri/Cargo.lock) | [RUSTSEC-2025-0098](https://osv.dev/vulnerability/RUSTSEC-2025-0098) | unknown |  | `unic-ucd-version` is unmaintained |
| crates.io | zbus_names 3.0.0 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0371](https://osv.dev/vulnerability/RUSTSEC-2026-0371) | unknown |  | `zbus_names` is unmaintained |
| crates.io | zbus_names 4.3.4 (src-tauri/Cargo.lock, tests/portal-chooser/fake-portal/Cargo.lock) | [RUSTSEC-2026-0371](https://osv.dev/vulnerability/RUSTSEC-2026-0371) | unknown |  | `zbus_names` is unmaintained |
| crates.io | zvariant 4.2.0 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0370](https://osv.dev/vulnerability/RUSTSEC-2026-0370) | unknown |  | `zvariant` is unmaintained |
| crates.io | zvariant 5.15.0 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0370](https://osv.dev/vulnerability/RUSTSEC-2026-0370) | unknown |  | `zvariant` is unmaintained |
| crates.io | zvariant_derive 4.2.0 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0372](https://osv.dev/vulnerability/RUSTSEC-2026-0372) | unknown |  | `zvariant_derive` is unmaintained |
| crates.io | zvariant_derive 5.15.0 (src-tauri/Cargo.lock) | [RUSTSEC-2026-0372](https://osv.dev/vulnerability/RUSTSEC-2026-0372) | unknown |  | `zvariant_derive` is unmaintained |
| crates.io | zvariant 5.15.1 (tests/portal-chooser/fake-portal/Cargo.lock) | [RUSTSEC-2026-0370](https://osv.dev/vulnerability/RUSTSEC-2026-0370) | unknown |  | `zvariant` is unmaintained |
| crates.io | zvariant_derive 5.15.1 (tests/portal-chooser/fake-portal/Cargo.lock) | [RUSTSEC-2026-0372](https://osv.dev/vulnerability/RUSTSEC-2026-0372) | unknown |  | `zvariant_derive` is unmaintained |

---

Generated by `scripts/security-report.cjs` - self-hosted, no third-party dependency. Data sources: [RustSec](https://rustsec.org/), [npm registry](https://docs.npmjs.com/cli/v10/commands/npm-audit), [OSV](https://osv.dev/).
