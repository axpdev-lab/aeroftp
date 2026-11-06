import { describe, expect, it } from 'vitest';
import {
    getProtocolClass,
    getProfileProtocolClass,
    getE2EBits,
    NATIVE_PROVIDER_PROTOCOLS,
    connectionSupportsStorageQuota,
    s3PresetReportsUsage,
    profileHasQuota,
    providerServesQuota,
    effectiveManualCap,
} from './types';
import { getProtonConnectionBadge } from './utils/providerConnectionMeta';

describe('Proton Drive protocol class and mode badge', () => {
    // Both modes encrypt on this machine, so the class is E2E 256-bit as for
    // Filen and Internxt. What the 4.2.1 review (L21) asked for still holds:
    // a profile on the official CLI says CLI on its card, now through the
    // mode badge next to the class, as MEGA shows CMD.
    it('is E2E 256-bit in both modes, the mode badge names the client', () => {
        const native = { protocol: 'proton' as const, options: { proton_mode: 'native' as const } };
        for (const server of [{ protocol: 'proton' as const }, { protocol: 'proton' as const, options: { proton_mode: 'cli' as const } }, native]) {
            expect(getProfileProtocolClass(server)).toBe('E2E');
        }
        expect(getE2EBits('proton')).toBe(256);
        expect(getProtonConnectionBadge(native.options).label).toBe('API');
        expect(getProtonConnectionBadge({ proton_mode: 'cli' }).label).toBe('CLI');
        expect(getProtonConnectionBadge({}).label).toBe('CLI');
        expect(getProtonConnectionBadge({}).className).toContain('purple');
    });

    // Only the native client reports the account quota; the CLI cannot.
    it('a native Proton profile serves its own quota', () => {
        const native = { protocol: 'proton' as const, options: { proton_mode: 'native' as const } };
        expect(connectionSupportsStorageQuota('proton', native.options)).toBe(true);
        expect(connectionSupportsStorageQuota('proton', {})).toBe(false);
        expect(providerServesQuota('proton', null, null, native.options)).toBe(true);
        expect(providerServesQuota('proton', null, null, {})).toBe(false);
        expect(effectiveManualCap(1000, 'proton', null, null, native.options)).toBeUndefined();
        expect(effectiveManualCap(1000, 'proton', null, null, {})).toBe(1000);
    });

    it('keeps the E2E class of the providers that encrypt in AeroFTP', () => {
        for (const type of ['filen', 'internxt', 'mega'] as const) {
            expect(getProtocolClass(type)).toBe('E2E');
        }
    });

    it('back-fills the provider id of a saved Proton profile', () => {
        expect(NATIVE_PROVIDER_PROTOCOLS.has('proton')).toBe(true);
    });
});

describe('S3 presets that report usage on request', () => {
    it('is Tigris only, and never a bare S3 profile', () => {
        expect(s3PresetReportsUsage('s3', 'tigris')).toBe(true);
        expect(s3PresetReportsUsage('s3', 'filebase')).toBe(false);
        expect(s3PresetReportsUsage('s3', 'custom-s3')).toBe(false);
        expect(s3PresetReportsUsage('s3', undefined)).toBe(false);
        expect(s3PresetReportsUsage('webdav', 'tigris')).toBe(false);
    });

    it('stays out of the after-transfer refresh gate, which would bill a request per upload', () => {
        expect(connectionSupportsStorageQuota('s3', {})).toBe(false);
    });

    it('shows the fetched usage of a Tigris profile in My Servers, with no manual cap', () => {
        expect(profileHasQuota({ protocol: 's3', providerId: 'tigris', options: {} })).toBe(true);
        expect(profileHasQuota({ protocol: 's3', providerId: 'filebase', options: {} })).toBe(false);
        expect(profileHasQuota({ protocol: 's3', options: {} })).toBe(false);
        expect(profileHasQuota({ protocol: 's3', options: { manualTotalBytes: 1 } })).toBe(true);
    });
});
