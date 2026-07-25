import { describe, it, expect } from 'vitest';
import {
    classifyUpdateVerification,
    formatIntegratedTime,
    rekorSearchUrl,
    shortenDigest,
} from './updateVerification';

// Pins the update-verification display mapping. This path is only observable
// across a signed release pair (it ships in vN, first runs on the vN -> vN+1
// update), so the unit test is the safety net that keeps the amber "advisory"
// state (Gap A, v4.1.3) from silently regressing back to a green "verified".
describe('classifyUpdateVerification', () => {
    it('passing sigstore check -> verified (green)', () => {
        expect(classifyUpdateVerification({ mode: 'SigstoreVerified', bundle_present: true, bundle_parsed: true }))
            .toBe('verified');
    });

    it('no signature bundle at all -> sha-only (green, nothing to verify)', () => {
        expect(classifyUpdateVerification({ mode: 'VerificationUnavailable', bundle_present: false, bundle_parsed: false }))
            .toBe('sha-only');
    });

    it('bundle present but unparseable -> unverified (amber, never green)', () => {
        expect(classifyUpdateVerification({ mode: 'VerificationUnavailable', bundle_present: true, bundle_parsed: false }))
            .toBe('unverified');
    });

    it('bundle present + parsed but sigstore verification failed -> unverified (amber advisory, NOT green)', () => {
        expect(classifyUpdateVerification({ mode: 'VerificationUnavailable', bundle_present: true, bundle_parsed: true }))
            .toBe('unverified');
    });

    it('bundle fetch infrastructure failure -> unverified (amber, never green)', () => {
        expect(classifyUpdateVerification({
            mode: 'VerificationUnavailable',
            bundle_present: false,
            bundle_parsed: false,
            bundle_fetch_failed: true,
        })).toBe('unverified');
    });

    it('VerificationFailed -> failed (red, blocks install; reserved for the v4.1.4 hard gate)', () => {
        expect(classifyUpdateVerification({ mode: 'VerificationFailed', bundle_present: true, bundle_parsed: true }))
            .toBe('failed');
    });
});

// The panel puts the locally computed digest next to the signed one and claims
// they match. These helpers are what the reader actually sees, so they are
// pinned: an elision that hides the wrong half, or a date that renders as
// "Invalid Date", would undermine exactly the claim the panel exists to make.
describe('shortenDigest', () => {
    const sha256 = 'eb4a00f69f2f68169c607c84aae124662b61f69be8ec5dfe171e95ac2010d822';

    it('keeps the head and the tail, which is what a reader compares', () => {
        expect(shortenDigest(sha256)).toBe('eb4a00f6…2010d822');
    });

    it('leaves a short value alone rather than eliding it to nothing', () => {
        expect(shortenDigest('abc123')).toBe('abc123');
        expect(shortenDigest('')).toBe('');
    });
});

describe('formatIntegratedTime', () => {
    it('formats a real Rekor integratedTime', () => {
        // 1784953451 = the v4.1.6 entry, integrated 2026-07-25 04:24:11 UTC.
        const formatted = formatIntegratedTime(1784953451, 'en-US');
        expect(formatted).toMatch(/2026/);
    });

    it('returns null instead of "Invalid Date" for missing or absurd values', () => {
        expect(formatIntegratedTime(null)).toBeNull();
        expect(formatIntegratedTime(0)).toBeNull();
        expect(formatIntegratedTime(-1)).toBeNull();
        expect(formatIntegratedTime(Number.NaN)).toBeNull();
        expect(formatIntegratedTime(Number.POSITIVE_INFINITY)).toBeNull();
    });
});

describe('rekorSearchUrl', () => {
    it('points at the public Rekor search for that log index', () => {
        expect(rekorSearchUrl(2242869187))
            .toBe('https://search.sigstore.dev/?logIndex=2242869187');
    });
});
