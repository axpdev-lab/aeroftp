import { describe, expect, it } from 'vitest';
import {
    getProtocolClass,
    getProfileProtocolClass,
    NATIVE_PROVIDER_PROTOCOLS,
    connectionSupportsStorageQuota,
    providerServesQuota,
    effectiveManualCap,
} from './types';

describe('Proton Drive protocol class (pre-release review of 4.2.1, L21)', () => {
    // The catalog, the mode strip and the connection label all present Proton
    // Drive as a CLI connection; the My Servers class said "E2E 256-bit".
    it('is the CLI class, as the catalog presents it', () => {
        expect(getProtocolClass('proton')).toBe('CLI');
        expect(getProfileProtocolClass({ protocol: 'proton' })).toBe('CLI');
    });

    // The native client encrypts on this machine, as Filen and MEGA do, and
    // reports the account quota, which the CLI cannot.
    it('a native Proton profile is E2E and serves its own quota', () => {
        const native = { protocol: 'proton' as const, options: { proton_mode: 'native' as const } };
        expect(getProfileProtocolClass(native)).toBe('E2E');
        expect(getProfileProtocolClass({ protocol: 'proton', options: { proton_mode: 'cli' } })).toBe('CLI');
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
