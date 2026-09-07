import { describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../../types';
import { appendImportedProfiles, bridgeProfileKey, commitImportedServers } from './bridgeImportCommit';
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

describe('the import callback and the commit helper together', () => {
    // A fake vault, so the two halves are composed for real rather than asserted
    // against each other's mocks: the callback reads back what the commit wrote.
    const fakeVault = (initial: ServerProfile[]) => {
        const state = { profiles: [...initial], writes: 0 };
        vi.mocked(loadSavedServerProfiles).mockImplementation(async () => [...state.profiles]);
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => {
            state.writes += 1;
            state.profiles = [...profiles];
        });
        return state;
    };

    it('does not resurrect the profile it just replaced when that leaves the vault empty', async () => {
        const vault = fakeVault([base]);
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        // The component's own snapshot still holds the old profile. Reading the
        // vault as empty and falling back to it is what put the two side by side.
        const staleSnapshot = [base];
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]),
            async servers => { expect(staleSnapshot).toHaveLength(1); await appendImportedProfiles(servers); });
        expect(outcome).toEqual({ added: 0, updated: 1 });
        expect(vault.profiles.map(s => s.id)).toEqual(['replacement']);
    });

    it('reports a failed vault write as a failed import and restores the replaced profile', async () => {
        const vault = fakeVault([base]);
        const write = vi.mocked(storeSavedServerProfiles).getMockImplementation()!;
        // Write 1 removes the profile being replaced, write 2 is the callback
        // persisting the merged list, and that is the one whose failure used to
        // be swallowed. Write 3 is the rollback, which must still go through.
        let calls = 0;
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => {
            calls += 1;
            if (calls === 2) throw new Error('vault write failed');
            await write(profiles);
        });
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]),
            async servers => { await appendImportedProfiles(servers); });
        expect(outcome.error).toBeDefined();
        expect(outcome.updated).toBe(0);
        expect(vault.profiles.map(s => s.id)).toEqual(['base']);
    });

    it('does not claim that nothing changed when the rollback itself failed', async () => {
        const vault = fakeVault([base]);
        const write = vi.mocked(storeSavedServerProfiles).getMockImplementation()!;
        // Write 1 removes the replaced profile, write 2 is the callback, write 3
        // is the rollback. Everything after the first one fails, which is the
        // case where the vault is left without the profile it started with.
        let calls = 0;
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => {
            calls += 1;
            if (calls >= 2) throw new Error('vault write failed');
            await write(profiles);
        });
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]),
            async servers => { await appendImportedProfiles(servers); });
        expect(vault.profiles).toEqual([]);
        expect(outcome.error).toContain('could not be restored');
    });
});
