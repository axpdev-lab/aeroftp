import { describe, expect, it } from 'vitest';
import { PROVIDERS } from './registry';

describe('MEGAcmd TLS setup step', () => {
    // `webdav -d --all` stops every location MEGAcmd serves, not only the
    // plain one, so the step has to say what to do before it (see what is
    // served) and after it (serve the other locations again and repoint
    // their clients), in that order.
    it('lists the served locations before stopping them, then restores them and their clients', () => {
        const preset = PROVIDERS.find((provider) => provider.id === 'megacmd-webdav');
        const step = preset?.setupInstructions?.find((line) => line.includes('--tls'));
        expect(step).toBeDefined();
        const text = step ?? '';
        const order = [
            'run webdav in MEGAcmd to list the locations it serves',
            'webdav -d --all',
            'webdav / --tls --certificate=<file> --key=<file>',
            'Serve again, with the same --tls options, each other location you still need',
            'point its other clients at the new https:// address',
        ].map((phrase) => {
            const at = text.indexOf(phrase);
            expect(at, phrase).toBeGreaterThanOrEqual(0);
            return at;
        });
        expect(order).toEqual([...order].sort((a, b) => a - b));
    });
});
