import { describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../../types';
import { bridgeProfileKey, commitImportedServers } from './bridgeImportCommit';
import { loadSavedServerProfiles, storeSavedServerProfiles } from '../../utils/serverProfileStore';

vi.mock('../../utils/serverProfileStore', () => ({
    loadSavedServerProfiles: vi.fn(), storeSavedServerProfiles: vi.fn().mockResolvedValue(undefined),
}));

const base: ServerProfile = { id: 'base', name: 'Koofr', host: 'app.koofr.net', port: 443, username: 'test', protocol: 'koofr' };
const crypt: ServerProfile = { ...base, id: 'crypt', name: 'Encrypted', initialPath: 'encrypted',
    aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt', remoteScope: 'encrypted' },
    hasStoredAeroCryptPassword: true, hasStoredAeroCryptSalt: false };

describe('Crypt bridge import', () => {
    it('keeps the plain account when adding its Crypt view', async () => {
        vi.mocked(loadSavedServerProfiles).mockResolvedValue([base]);
        const onImport = vi.fn();
        expect(bridgeProfileKey(base)).not.toBe(bridgeProfileKey(crypt));
        const outcome = await commitImportedServers([crypt], new Set([bridgeProfileKey(base)]), onImport);
        expect(outcome).toEqual({ added: 1, updated: 0 });
        expect(onImport).toHaveBeenCalledWith([crypt]);
    });

    it('waits for a failed async import before restoring the profile list', async () => {
        vi.mocked(loadSavedServerProfiles).mockResolvedValue([base, crypt]);
        const outcome = await commitImportedServers([{ ...crypt, id: 'replacement' }], new Set([bridgeProfileKey(crypt)]),
            async () => { throw new Error('vault write failed'); });
        expect(outcome.error).toBeDefined();
        expect(storeSavedServerProfiles).toHaveBeenLastCalledWith([base, crypt]);
    });
});
