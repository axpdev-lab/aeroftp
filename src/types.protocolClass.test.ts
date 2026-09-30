import { describe, expect, it } from 'vitest';
import { getProtocolClass, getProfileProtocolClass, NATIVE_PROVIDER_PROTOCOLS } from './types';

describe('Proton Drive protocol class (pre-release review of 4.2.1, L21)', () => {
    // The catalog, the mode strip and the connection label all present Proton
    // Drive as a CLI connection; the My Servers class said "E2E 256-bit".
    it('is the CLI class, as the catalog presents it', () => {
        expect(getProtocolClass('proton')).toBe('CLI');
        expect(getProfileProtocolClass({ protocol: 'proton' })).toBe('CLI');
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
