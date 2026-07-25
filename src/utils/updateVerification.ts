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
 * What `download_update` reports back about the artifact it just fetched.
 *
 * Mirrors `UpdateVerificationInfo` in `src-tauri/src/lib.rs`. The `bundle_*` and
 * `rekor_*` fields are provenance read out of the Sigstore bundle before the
 * verifier consumes it; every one of them is nullable because an unrecognised
 * bundle shape must degrade to "not shown" rather than to a wrong claim.
 */
export interface UpdateVerificationInfo {
    mode: 'SigstoreVerified' | 'VerificationUnavailable' | 'VerificationFailed';
    workflow_identity: string | null;
    oidc_issuer: string | null;
    artifact_sha256: string;
    bundle_present: boolean;
    bundle_parsed: boolean;
    bundle_fetch_failed: boolean;
    message: string;
    bundle_version: string | null;
    rekor_log_index: number | null;
    rekor_integrated_time: number | null;
    rekor_entry_kind: string | null;
    attested_sha256: string | null;
    /**
     * `true`/`false` only when a signed digest existed to compare against.
     * `null` means nothing was published to compare with, which is a different
     * statement from "they differ" and stays distinguishable in the UI.
     */
    digest_match: boolean | null;
    verifier_version: string;
}

/** Public Rekor search entry for a transparency-log index. */
export function rekorSearchUrl(logIndex: number): string {
    return `https://search.sigstore.dev/?logIndex=${logIndex}`;
}

/**
 * Shorten a hex digest for display: first 8 and last 8 characters.
 * Returns the input unchanged when it is too short to be worth eliding.
 */
export function shortenDigest(hex: string): string {
    return hex.length > 20 ? `${hex.slice(0, 8)}…${hex.slice(-8)}` : hex;
}

/**
 * Format a Rekor `integratedTime` (Unix seconds) as a short local date.
 * Returns null for a missing or nonsensical timestamp rather than "Invalid Date".
 */
export function formatIntegratedTime(unixSeconds: number | null, locale?: string): string | null {
    if (unixSeconds === null || !Number.isFinite(unixSeconds) || unixSeconds <= 0) return null;
    const date = new Date(unixSeconds * 1000);
    if (Number.isNaN(date.getTime())) return null;
    return date.toLocaleDateString(locale, { year: 'numeric', month: 'short', day: 'numeric' });
}

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
