import { describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../../types';
import { readFileSync } from 'node:fs';
import { resolve } from 'node:path';
import { appendImportedProfiles, bridgeProfileKey, commitImportedServers, OMITTED_WHEN_EMPTY, OPTIONS_SECRET_KEYS, overwriteProfile, STORED_SECRET_FLAGS } from './bridgeImportCommit';
import { invoke } from '@tauri-apps/api/core';
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from '../../utils/serverProfileStore';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn().mockResolvedValue(true) }));
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
        // Bound to the account the list was read from (3), so a switch in
        // between refuses the write instead of landing in another account.
        expect(onImport).toHaveBeenCalledWith([crypt], new Set(), 3);
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
        const overwritten: ServerProfile = { ...base, name: 'Koofr restored', initialPath: '/restored', hasStoredCredential: false };
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

    // Two profiles of one batch with one id would both be stored.
    it('refuses a batch that holds two profiles of one id, appended or replacing', async () => {
        const vault = fakeVault([base]);
        const one: ServerProfile = { ...base, id: 'twin', name: 'One' };
        const two: ServerProfile = { ...base, id: 'twin', name: 'Two' };
        await expect(appendImportedProfiles([one, two])).rejects.toThrow('share the id twin');
        const again: ServerProfile = { ...base, name: 'Again' };
        await expect(appendImportedProfiles([base, again], new Set([base.id]))).rejects.toThrow(`share the id ${base.id}`);
        expect(vault.writes).toBe(0);
    });

    // Every profile skipped: nothing to store, so the list is not rewritten.
    it('does not rewrite the list when there is nothing to store', async () => {
        const vault = fakeVault([base]);
        expect(await appendImportedProfiles([])).toEqual([base]);
        expect(vault.writes).toBe(0);
    });

    // A bridge that read the legacy store expects no account (null): one that
    // became active since must not receive the profiles.
    it('refuses to write to an account that became active after a legacy read', async () => {
        const vault = fakeVault([]);
        const imported: ServerProfile = { ...base, id: 'imported' };
        await expect(appendImportedProfiles([imported], new Set(), null)).rejects.toThrow('active account changed');
        expect(vault.writes).toBe(0);
    });

    // The record was pointed elsewhere after the backend planned this overwrite:
    // its secrets must not follow the file. The rule is asked of the backend.
    it('refuses to replace a profile pointed at another server since the plan', async () => {
        const vault = fakeVault([base]);
        vi.mocked(invoke).mockResolvedValueOnce(false);
        const overwritten: ServerProfile = { ...base, name: 'From the file' };
        await expect(appendImportedProfiles([overwritten], new Set([base.id]))).rejects.toThrow('pointed at another server');
        expect(invoke).toHaveBeenCalledWith('profile_same_target', { stored: base, imported: overwritten });
        expect(vault.writes).toBe(0);
    });

    // A vault that reads no account (the legacy store) is not the account the
    // import restored into either.
    it('refuses to write to a vault that reads no account when an account is expected', async () => {
        const vault = fakeVault([]);
        vi.mocked(readSavedServerProfilesForWrite).mockImplementation(async () => ({ profiles: [], userId: undefined }));
        const imported: ServerProfile = { ...base, id: 'imported' };
        await expect(appendImportedProfiles([imported], new Set(), 3)).rejects.toThrow('active account changed');
        expect(vault.writes).toBe(0);
    });

    // The import restored its secrets into account 4; the vault now writes for
    // account 3 (switched from another window): nothing reaches account 3.
    it('refuses to write when the active account is not the one the import restored into', async () => {
        const vault = fakeVault([base]);
        const imported: ServerProfile = { ...base, id: 'imported', name: 'Imported' };
        await expect(appendImportedProfiles([imported], new Set(), 4)).rejects.toThrow('active account changed');
        expect(vault.writes).toBe(0);
        const merged = await appendImportedProfiles([imported], new Set(), 3);
        expect(merged.map(s => s.id)).toEqual([base.id, 'imported']);
    });

    // Same cases as the Rust planner test: a field the format always writes
    // follows the file, null included; one it omits when empty says nothing when
    // null; a stored options secret the file does not carry stays.
    it('an overwrite follows the file except for what the file cannot speak to', () => {
        const stored = { ...base, color: '#ff0000', initialPath: '/old', localOnly: 'kept',
            aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt' },
            options: { verifyCert: false, timeout: 30, totp_secret: 'stored-seed', sasToken: 'stored-sas' } } as unknown as ServerProfile;
        const imported = { ...base, name: 'New', color: null, initialPath: undefined, aeroCryptOverlay: null,
            options: { timeout: 60, sasToken: 'file-sas' } } as unknown as ServerProfile;
        const merged = overwriteProfile(stored, imported) as unknown as Record<string, unknown>;
        expect(merged.name).toBe('New');
        expect(merged.color).toBeNull();
        expect(merged.initialPath).toBe('/old');
        expect(merged.localOnly).toBe('kept');
        expect(merged.aeroCryptOverlay).toEqual({ enabled: true, kind: 'rclone-crypt' });
        expect(merged.options).toEqual({ timeout: 60, sasToken: 'file-sas', totp_secret: 'stored-seed' });
        // A backup without credentials, its options stripped to nothing.
        const stripped = overwriteProfile(stored, { ...base, options: null } as unknown as ServerProfile) as unknown as Record<string, unknown>;
        expect(stripped.options).toEqual({ totp_secret: 'stored-seed', sasToken: 'stored-sas' });
    });

    // The twins must list what the Rust side lists, or an overwrite would treat
    // a flag, an omitted field or an options secret differently on one side.
    it('lists the same keys as the Rust overwrite', () => {
        const rustList = (file: string, name: string) => {
            const source = readFileSync(resolve(__dirname, '../../../src-tauri/src', file), 'utf8');
            const block = source.match(new RegExp(`pub const ${name}: [^=]*= &?\\[([\\s\\S]*?)\\];`));
            expect(block).not.toBeNull();
            const code = block![1].split('\n').filter(line => !line.trim().startsWith('//')).join('\n');
            const keys = [...code.matchAll(/"([^"]+)"/g)].map(m => m[1]);
            expect(keys.length).toBeGreaterThan(0);
            return keys;
        };
        expect([...STORED_SECRET_FLAGS]).toEqual(rustList('profile_import_plan.rs', 'STORED_SECRET_FLAGS'));
        expect([...OMITTED_WHEN_EMPTY]).toEqual(rustList('profile_import_plan.rs', 'OMITTED_WHEN_EMPTY'));
        expect([...OPTIONS_SECRET_KEYS]).toEqual(rustList('profile_export.rs', 'OPTIONS_SECRET_KEYS'));
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
