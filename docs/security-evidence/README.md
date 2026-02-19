# Security Evidence Index

Public security evidence documents for AeroFTP releases.

## Documents

- [SECURITY-EVIDENCE-v2.4.0.md](./SECURITY-EVIDENCE-v2.4.0.md) — Security evidence pack for v2.4.0 (current)
- [SECURITY-EVIDENCE-v2.3.0.md](./SECURITY-EVIDENCE-v2.3.0.md) — Security evidence pack for v2.3.0
- [SECURITY-EVIDENCE-v2.2.4.md](./SECURITY-EVIDENCE-v2.2.4.md) — Security evidence pack for v2.2.4
- [SECURITY-EVIDENCE-v2.2.3.md](./SECURITY-EVIDENCE-v2.2.3.md) — Security evidence pack for v2.2.3
- [SECURITY-EVIDENCE-v2.0.8.md](./SECURITY-EVIDENCE-v2.0.8.md) — Security evidence pack for v2.0.8
- [SECURITY-EVIDENCE-TEMPLATE.md](./SECURITY-EVIDENCE-TEMPLATE.md) — Template for future releases

## Usage

1. Copy the template for the target release:
   - `SECURITY-EVIDENCE-TEMPLATE.md` → `SECURITY-EVIDENCE-vX.Y.Z.md`
2. Fill release metadata, findings, fixes, and test evidence.
3. Add CI artifacts and reviewer sign-off before release.

## Policy

- Security score claims must be labeled as **Estimated** or **Externally Verified**.
- Reopened High/Critical findings require score downgrade until fixed.
- Keep this folder public and release-oriented (no internal-only notes).
