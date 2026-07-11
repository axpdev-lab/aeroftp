// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Display tone for the in-app update's signature / checksum verification result.
 *  - verified   : sigstore signature matched the project CI identity (green).
 *  - sha-only   : no signature bundle was published; the artifact is still
 *                 SHA-256 verified (green).
 *  - unverified : a signature bundle WAS published but could not be parsed or
 *                 verified -> advisory (amber). Installs on SHA-256, but never
 *                 masquerades as a green "verified" badge.
 *  - failed     : hard failure that blocks the install (red). Reserved for the
 *                 v4.1.4 enforcement gate; the backend does not emit it yet.
 */
export type UpdateVerifyTone = 'verified' | 'sha-only' | 'unverified' | 'failed';

/**
 * Classify a backend UpdateVerificationInfo into a display tone.
 *
 * The backend keeps the Rust verify path untouched and reports a genuine sigstore
 * verification failure as `VerificationUnavailable`. Any published bundle that
 * does not yield `SigstoreVerified` is surfaced distinctly (amber), whether it
 * failed parsing or cryptographic verification. Only a genuinely absent bundle
 * gets the green SHA-only fallback. Pure and side-effect free so it can be unit
 * tested without driving the (2-releases-to-observe) real update flow.
 */
export function classifyUpdateVerification(v: {
    mode: string;
    bundle_present: boolean;
    bundle_parsed: boolean;
    bundle_fetch_failed?: boolean;
}): UpdateVerifyTone {
    if (v.mode === 'VerificationFailed') return 'failed';
    if (v.mode === 'SigstoreVerified') return 'verified';
    if (v.bundle_present || v.bundle_fetch_failed) return 'unverified';
    return 'sha-only';
}
