# Security Evidence

Audit remediation evidence files for AeroFTP, organized by version.

## Reports

- **v2.9.5** (2026-03-13): Claude Opus 4.6 (8 area auditors) + GPT-5.4 counter-audit: all v2.9.4 findings tracked and resolved
  - [SECURITY-EVIDENCE-v2.9.5.md](SECURITY-EVIDENCE-v2.9.5.md)
- **v2.8.7** (2026-03-07): Claude Opus 4.6 8-area audit + GPT-5.4 counter-audit: Grade B+ → A-, 45+ findings resolved
  - [SECURITY-EVIDENCE-v2.8.7.md](SECURITY-EVIDENCE-v2.8.7.md)
- **v2.6.4** (2026-02-24): 8x Claude Opus 4.6 + GPT-5.3-Codex: 148 findings, 145 fixed
  - [SECURITY-EVIDENCE-v2.6.4.md](SECURITY-EVIDENCE-v2.6.4.md)
- **v2.4.0** (2026-02-15): 6x Claude Opus 4.6 + GPT-5.3-Codex: 12-auditor 4-phase provider audit, Grade A-
  - [SECURITY-EVIDENCE-v2.4.0.md](SECURITY-EVIDENCE-v2.4.0.md)
- **v2.3.0** (2026-02-10): 4x Claude Opus 4.6 + GPT-5.3-Codex: 55+ findings, SQL injection, XSS, WAL hardening
  - [SECURITY-EVIDENCE-v2.3.0.md](SECURITY-EVIDENCE-v2.3.0.md)
- **v2.2.4** (2026-02-06): 4x Claude Opus 4.6 + GPT-5.3-Codex: 13 findings, TOTP, Remote Vault, modals
  - [SECURITY-EVIDENCE-v2.2.4.md](SECURITY-EVIDENCE-v2.2.4.md)
- **v2.2.3** (2026-02-04): Shell execute backend migration, i18n structural audit
  - [SECURITY-EVIDENCE-v2.2.3.md](SECURITY-EVIDENCE-v2.2.3.md)
- **v2.0.8** (2026-01-25): Settings confidentiality, SFTP trust model, plugin safety, terminal guardrails
  - [SECURITY-EVIDENCE-v2.0.8.md](SECURITY-EVIDENCE-v2.0.8.md)

## CLI Audits

- **2026-05-06** (v3.7.2 surface): Codex three-track audit (security, CLI behavior, code quality): findings fixed, residual risks documented
  - [AEROFTP-CLI-AUDIT-2026-05-06.md](AEROFTP-CLI-AUDIT-2026-05-06.md)
  - [AEROFTP-CLI-AUDIT-EXECUTIVE-2026-05-06.md](AEROFTP-CLI-AUDIT-EXECUTIVE-2026-05-06.md)

## Dependency Evaluations

- **2026-06-08**: `reed-solomon-erasure` evaluation for AeroVault v4 Error Correction (transitive RUSTSEC-2024-0384 accepted with rationale)
  - [REED-SOLOMON-ERASURE-EVALUATION-2026-06.md](REED-SOLOMON-ERASURE-EVALUATION-2026-06.md)

## External Audit

- **Aikido Security: February 2026**: Top 5%, OWASP Top 10 coverage, 0 open issues
  - [Security Audit Report axpnet - February 2026.pdf](../Security%20Audit%20Report%20axpnet%20-%20February%202026.pdf)
