import { describe, it, expect } from 'vitest';
import { classifyUpdateVerification } from './updateVerification';

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
