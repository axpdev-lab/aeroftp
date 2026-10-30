import { describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../../types';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { appendImportedProfiles, bridgeProfileKey, commitImportedServers, overwriteProfile, STORED_SECRET_FLAGS } from './bridgeImportCommit';
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from '../../utils/serverProfileStore';

vi.mock('../../utils/serverProfileStore', () => ({
    readSavedServerProfilesForWrite: vi.fn(), storeSavedServerProfiles: vi.fn().mockResolvedValue(undefined),
}));

const base: ServerProfile = { id: 'base', name: 'Koofr', host: 'app.koofr.net', port: 443, username: 'test', protocol: 'koofr' };
const crypt: ServerProfile = { ...base, id: 'crypt', name: 'Encrypted', initialPath: 'encrypted',
    aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt', remoteScope: 'encrypted' },
    hasStoredAeroCryptPassword: true, hasStoredAeroCryptSalt: false };

describe('Crypt bridge import', () => {
    it('keeps the plain account when adding its Crypt view', async () => {
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 3 });
        const onImport = vi.fn();
        expect(bridgeProfileKey(base)).not.toBe(bridgeProfileKey(crypt));
        const outcome = await commitImportedServers([crypt], new Set([bridgeProfileKey(base)]), onImport);
        expect(outcome).toEqual({ added: 1, updated: 0 });
        expect(onImport).toHaveBeenCalledWith([crypt]);
    });

    it('waits for a failed async import before restoring the profile list', async () => {
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base, crypt], userId: 3 });
        const outcome = await commitImportedServers([{ ...crypt, id: 'replacement' }], new Set([bridgeProfileKey(crypt)]),
            async () => { throw new Error('vault write failed'); });
        expect(outcome.error).toBeDefined();
        // The rollback goes back to the account the backup was read from.
        expect(storeSavedServerProfiles).toHaveBeenLastCalledWith([base, crypt], false, 3);
    });
});

describe('the import callback and the commit helper together', () => {
    // A fake vault, so the two halves are composed for real rather than asserted
    // against each other's mocks: the callback reads back what the commit wrote.
    const fakeVault = (initial: ServerProfile[]) => {
        const state = { profiles: [...initial], writes: 0 };
        vi.mocked(readSavedServerProfilesForWrite).mockImplementation(async () => ({ profiles: [...state.profiles], userId: 3 }));
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => {
            state.writes += 1;
            state.profiles = [...profiles];
            return state.profiles;
        });
        return state;
    };

    // The vault, not the import, settles the names: one already held is
    // stored with " (1)", and the list handed back is the one stored.
    it('hands back the list as the vault stored it, names included', async () => {
        const vault = fakeVault([base]);
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => {
            vault.profiles = profiles.map((p, i) => (i > 0 && p.name === base.name ? { ...p, name: `${p.name} (1)` } : p));
            return vault.profiles;
        });
        const imported: ServerProfile = { ...base, id: 'imported' };
        const merged = await appendImportedProfiles([imported]);
        expect(merged.map(s => s.name)).toEqual([base.name, `${base.name} (1)`]);
        expect(merged).toEqual(vault.profiles);
    });

    // An overwrite re-import carries the id of the profile it replaces: one
    // profile per id, in its place, and favourites keyed by that id still hold it.
    it('lays the profiles named as overwritten over the record read at write time, and appends the others', async () => {
        const second: ServerProfile = { ...base, id: 'second', name: 'Second' };
        // Edited on this device after the import read the list: a field the
        // file does not carry, and a stored credential the file did not bring.
        const editedSince: ServerProfile = { ...base, color: '#ff0000', hasStoredCredential: true };
        const vault = fakeVault([editedSince, second]);
        const overwritten: ServerProfile = { ...base, name: 'Koofr restored', host: 'restored.example', hasStoredCredential: false };
        const added: ServerProfile = { ...base, id: 'added', name: 'Added' };
        const merged = await appendImportedProfiles([added, overwritten], new Set([base.id]));
        expect(merged.map(s => s.id)).toEqual([base.id, 'second', 'added']);
        expect(merged[0]).toEqual({ ...overwritten, color: '#ff0000', hasStoredCredential: true });
        expect(vault.profiles).toEqual(merged);
    });

    // The profile was deleted elsewhere between the backend's read and this
    // write: there is nothing to replace, so it is stored rather than lost.
    it('an overwritten profile whose id is gone is appended, not dropped', async () => {
        const second: ServerProfile = { ...base, id: 'second', name: 'Second' };
        const vault = fakeVault([second]);
        const overwritten: ServerProfile = { ...base, name: 'Koofr restored' };
        const merged = await appendImportedProfiles([overwritten], new Set([base.id]));
        expect(merged).toEqual([second, overwritten]);
        expect(vault.profiles).toEqual(merged);
    });

    // `undefined` is no value: a field only this device has survives it,
    // while an explicit null from the file still clears the stored value.
    it('an overwrite keeps a stored field the import leaves undefined, and takes an explicit null', () => {
        const stored: ServerProfile = { ...base, color: '#ff0000', providerId: 'koofr' };
        const imported = { ...base, color: undefined, providerId: null } as unknown as ServerProfile;
        const merged = overwriteProfile(stored, imported);
        expect(merged.color).toBe('#ff0000');
        expect(merged.providerId).toBeNull();
    });

    // The twin must list the same flags as the Rust planner, or an overwrite
    // would drop a stored-secret flag on one side only.
    it('lists the same secret-presence flags as profile_import_plan.rs', () => {
        const rust = readFileSync(resolve(__dirname, '../../../src-tauri/src/profile_import_plan.rs'), 'utf8');
        const block = rust.match(/pub const STORED_SECRET_FLAGS: \[&str; \d+\] = \[([^\]]*)\]/);
        expect(block).not.toBeNull();
        const flags = [...block![1].matchAll(/"([^"]+)"/g)].map(m => m[1]);
        expect(flags.length).toBeGreaterThan(0);
        expect([...STORED_SECRET_FLAGS]).toEqual(flags);
    });

    // A profile to append whose id the vault already holds was saved by
    // another window after the import was planned: refuse, rather than store
    // two profiles of one id or replace one nobody asked to replace.
    it('refuses to append a profile whose id the vault already holds, and writes nothing', async () => {
        const vault = fakeVault([base]);
        const sameId: ServerProfile = { ...base, name: 'From a bridge' };
        await expect(appendImportedProfiles([sameId])).rejects.toThrow('saved by another window');
        expect(vault.writes).toBe(0);
        expect(vault.profiles).toEqual([base]);
    });

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
            return write(profiles);
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
            return write(profiles);
        });
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]),
            async servers => { await appendImportedProfiles(servers); });
        expect(vault.profiles).toEqual([]);
        expect(outcome.error).toContain('could not be restored');
    });

    it('refuses the import when the saved profiles cannot be read, without writing or calling back', async () => {
        const vault = fakeVault([base]);
        vi.mocked(readSavedServerProfilesForWrite).mockRejectedValueOnce(new Error('vault read failed'));
        const onImport = vi.fn();
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]), onImport);
        expect(outcome).toEqual({ added: 0, updated: 0, error: expect.stringContaining('could not be read') });
        // Nothing ran on the vault: no removal, no callback, so no second read
        // could disagree with the first and leave the old profile next to the new one.
        expect(onImport).not.toHaveBeenCalled();
        expect(vault.writes).toBe(0);
        expect(vault.profiles.map(s => s.id)).toEqual(['base']);
    });
});
