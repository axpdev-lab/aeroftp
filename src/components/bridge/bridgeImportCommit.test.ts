// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import type { ServerProfile } from '../../types';
import { appendImportedProfiles, bridgeProfileKey, commitImportedServers, commitImportedServersForScan, importedServerProfile, type ImportedServer } from './bridgeImportCommit';
import { readSavedServerProfilesForWrite, storeSavedServerProfiles } from '../../utils/serverProfileStore';

vi.mock('../../utils/serverProfileStore', () => ({
    readSavedServerProfilesForWrite: vi.fn(), storeSavedServerProfiles: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));
vi.mock('../../i18n', () => ({
    useTranslation: () => (key: string, params?: Record<string, unknown>) =>
        params ? `${key} ${JSON.stringify(params)}` : key,
}));
vi.mock('../../utils/pickPath', () => ({ pickFile: vi.fn(), pickSave: vi.fn() }));
vi.mock('../../hooks/useDetectedBridgeConfigs', () => ({
    detectBridgeConfigBounded: vi.fn(async () => null),
}));

import { invoke } from '@tauri-apps/api/core';

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
        expect(onImport).toHaveBeenCalledWith([crypt], 3, undefined);
    });

    it('a failed commit write is reported as failed with the vault untouched (one atomic write, no rollback)', async () => {
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base, crypt], userId: 3 });
        vi.mocked(storeSavedServerProfiles).mockRejectedValueOnce(new Error('vault write failed'));
        const outcome = await commitImportedServers(
            [{ ...crypt, id: 'replacement' }],
            new Set([bridgeProfileKey(crypt)]),
            async (servers, expected, removeKeys) => { await appendImportedProfiles(servers, expected, removeKeys); },
        );
        expect(outcome.error).toContain('No changes were made');
        expect(outcome.updated).toBe(0);
        // The single atomic write failed: nothing was removed, so there is
        // nothing to restore and no second write runs.
        expect(storeSavedServerProfiles).toHaveBeenCalledTimes(1);
    });
});

describe('the import callback and the commit helper together', () => {
    // A fake vault, so the two halves are composed for real rather than asserted
    // against each other's mocks: the callback reads back what the commit wrote.
    const fakeVault = (initial: ServerProfile[]) => {
        const state = { profiles: [...initial], writes: 0 };
        vi.mocked(readSavedServerProfilesForWrite).mockImplementation(async () => ({ profiles: [...state.profiles], userId: 3, revision: `r${state.writes}` }));
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

    // Two profiles of one batch with one id would both be stored.
    it('refuses a batch that holds two profiles of one id', async () => {
        const vault = fakeVault([base]);
        const one: ServerProfile = { ...base, id: 'twin', name: 'One' };
        const two: ServerProfile = { ...base, id: 'twin', name: 'Two' };
        await expect(appendImportedProfiles([one, two])).rejects.toThrow('share the id twin');
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
        await expect(appendImportedProfiles([imported], null)).rejects.toThrow('active account changed');
        expect(vault.writes).toBe(0);
    });

    // A vault that reads no account (the legacy store) is not the account the
    // import restored into either.
    it('refuses to write to a vault that reads no account when an account is expected', async () => {
        const vault = fakeVault([]);
        vi.mocked(readSavedServerProfilesForWrite).mockImplementation(async () => ({ profiles: [], userId: undefined }));
        const imported: ServerProfile = { ...base, id: 'imported' };
        await expect(appendImportedProfiles([imported], 3)).rejects.toThrow('active account changed');
        expect(vault.writes).toBe(0);
    });

    // The import restored its secrets into account 4; the vault now writes for
    // account 3 (switched from another window): nothing reaches account 3.
    it('refuses to write when the active account is not the one the import restored into', async () => {
        const vault = fakeVault([base]);
        const imported: ServerProfile = { ...base, id: 'imported', name: 'Imported' };
        await expect(appendImportedProfiles([imported], 4)).rejects.toThrow('active account changed');
        expect(vault.writes).toBe(0);
        const merged = await appendImportedProfiles([imported], 3);
        expect(merged.map(s => s.id)).toEqual([base.id, 'imported']);
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
            async (servers, expected, removeKeys) => { expect(staleSnapshot).toHaveLength(1); await appendImportedProfiles(servers, expected, removeKeys); });
        expect(outcome).toEqual({ added: 0, updated: 1 });
        // One atomic write: base removed and replacement appended together.
        expect(vault.writes).toBe(1);
        expect(vault.profiles.map(s => s.id)).toEqual(['replacement']);
    });

    it('removes the replaced profile and appends the replacement in ONE account-bound write', async () => {
        const vault = fakeVault([base]);
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const onImport = vi.fn(async (servers: ServerProfile[], expected?: number | null, removeKeys?: ReadonlySet<string>) => {
            await appendImportedProfiles(servers, expected, removeKeys);
        });
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]), onImport);
        expect(outcome).toEqual({ added: 0, updated: 1 });
        // Exactly one vault write, carrying removal + append, under account 3.
        expect(vault.writes).toBe(1);
        expect(storeSavedServerProfiles).toHaveBeenCalledWith([replacement], false, 3, 'r0');
        expect(vault.profiles.map(s => s.id)).toEqual(['replacement']);
        // The callback received the removal keys so ITS write is the atomic one.
        expect(onImport).toHaveBeenCalledWith([replacement], 3, new Set([bridgeProfileKey(base)]));
    });

    it('a failed atomic write leaves the replaced profile in place, so the error can honestly say no changes were made', async () => {
        const vault = fakeVault([base]);
        vi.mocked(storeSavedServerProfiles).mockRejectedValueOnce(new Error('vault write failed'));
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]),
            async (servers, expected, removeKeys) => { await appendImportedProfiles(servers, expected, removeKeys); });
        expect(outcome.error).toContain('No changes were made');
        expect(outcome.updated).toBe(0);
        expect(vault.profiles.map(s => s.id)).toEqual(['base']);
        expect(vault.writes).toBe(0);
    });

    // The write carries the revision of its own read, so a save from another
    // window in between is refused and not overwritten.
    it('binds the append to the revision of the list it read', async () => {
        fakeVault([base]);
        const imported: ServerProfile = { ...base, id: 'imported' };
        await appendImportedProfiles([imported]);
        expect(storeSavedServerProfiles).toHaveBeenLastCalledWith([base, imported], false, 3, 'r0');
    });

    // The write is one atomic operation: if it is refused (here: the revision
    // was taken by another window), nothing was removed and nothing is appended.
    it('a refused atomic write removes nothing and appends nothing', async () => {
        const vault = fakeVault([base]);
        // The single write is refused (another window saved in between).
        vi.mocked(storeSavedServerProfiles).mockRejectedValue(new Error('PROFILES_CHANGED'));
        const replacement: ServerProfile = { ...base, id: 'replacement', name: 'Koofr renamed' };
        const outcome = await commitImportedServers([replacement], new Set([bridgeProfileKey(base)]),
            async (servers, expected, removeKeys) => { await appendImportedProfiles(servers, expected, removeKeys); });
        expect(outcome.error).toContain('No changes were made');
        expect(outcome.updated).toBe(0);
        // The vault still holds exactly what it held before the import: the
        // removal and the append were one write, and it never landed.
        expect(vault.profiles.map(s => s.id)).toEqual(['base']);
        expect(vault.writes).toBe(0);
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

describe('an imported Filen API key (#230)', () => {
    // The two shapes `import_bridge_config` returns (pinned on the backend in
    // `an_imported_filen_key_moves_to_the_vault_and_leaves_the_options`): the
    // key stored in the vault, and a failed or locked write.
    const filen = (stored: boolean): ImportedServer => ({
        id: 'filen-1', name: 'f', host: 'filen.io', port: 443, username: 'a@example.test',
        protocol: 'filen', options: { filen_2fa_code: '' }, hasStoredCredential: true,
        hasStoredFilenApiKey: stored,
    });

    it.each([true, false])('commits the flag the vault reported (%s) and never a key', async (stored) => {
        const state = { profiles: [base] as ServerProfile[] };
        vi.mocked(readSavedServerProfilesForWrite).mockImplementation(async () => ({ profiles: [...state.profiles], userId: 3 }));
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => {
            state.profiles = [...profiles];
            return state.profiles;
        });
        const outcome = await commitImportedServers([importedServerProfile(filen(stored))], new Set(), async (servers) => {
            await appendImportedProfiles(servers);
        });
        expect(outcome).toEqual({ added: 1, updated: 0 });
        const saved = state.profiles.find(p => p.id === 'filen-1');
        expect(saved?.hasStoredFilenApiKey).toBe(stored);
        expect(saved?.options).not.toHaveProperty('filen_api_key');
    });

    it('carries no field the backend did not return', () => {
        const withExtra = { ...filen(true), options: { filen_2fa_code: '' }, filen_api_key: 'FILEN-KEY-1' } as ImportedServer;
        expect(JSON.stringify(importedServerProfile(withExtra))).not.toContain('FILEN-KEY-1');
    });
});

describe('an imported AWS STS session token', () => {
    it.each([true, false])('forwards hasStoredSessionToken (%s) without persisting a plaintext token', (stored) => {
        const imported: ImportedServer = {
            id: 'aws-sts-1',
            name: 'sts',
            host: '',
            port: 443,
            username: 'ASIATEST',
            protocol: 's3',
            options: { region: 'us-east-1' },
            hasStoredCredential: true,
            hasStoredSessionToken: stored,
        };
        const saved = importedServerProfile(imported);
        expect(saved.hasStoredSessionToken).toBe(stored);
        expect(saved.options).not.toHaveProperty('sessionToken');
    });
});

describe('BridgeSourcePanel export auto-close guard', () => {
    it('stays open when export returns warnings even with zero skipped profiles', async () => {
        const { shouldAutoCloseBridgeExport } = await import('../BridgeSourcePanel');
        expect(shouldAutoCloseBridgeExport({ skipped: [], warnings: [] })).toBe(true);
        expect(
            shouldAutoCloseBridgeExport({
                skipped: [],
                warnings: [{ name: 'AWS config', reason: 'merge config.aeroftp manually' }],
            }),
        ).toBe(false);
        expect(
            shouldAutoCloseBridgeExport({
                skipped: [{ name: 'ftp-1', reason: 'not exportable' }],
                warnings: [],
            }),
        ).toBe(false);
    });
});

describe('BridgeSourcePanel filtered security-review alignment (D1)', () => {
    it('keeps certificate-review entry visible and committable when row 0 is omitted', async () => {
        const { matchBridgeSecurityEntries, resolveBridgeRowFileIndex } = await import(
            '../BridgeSourcePanel'
        );
        // Row 0 (capped Crypt) was omitted by the backend, so the remaining
        // profile arrives at filtered position 0 with original `fileIndex: 1`.
        const filteredServers = [
            { id: 's3-row-1', fileIndex: 1 },
        ];
        const securityEntries = [
            {
                fileIndex: 1,
                name: 'Plain S3',
                reset: [{ key: 'verifyCert', label: 'TLS verification disabled', value: 'false' }],
                kept: [],
            },
        ];
        const selected = new Set(['s3-row-1']);

        const visible = matchBridgeSecurityEntries(filteredServers, securityEntries, selected);
        expect(visible).toHaveLength(1);
        expect(visible[0].fileIndex).toBe(1);
        expect(resolveBridgeRowFileIndex(filteredServers[0], 0)).toBe(1);
        // Backwards compatibility when `fileIndex` is omitted falls back to row position.
        expect(resolveBridgeRowFileIndex({}, 2)).toBe(2);
    });
});

describe('importedServerProfile security relaxations on bridge imports', () => {
    const winscpWebdavResetByBackend: ImportedServer = {
        id: 'winscp-webdav-1',
        name: 'Public WebDAV',
        host: 'dav.example.com',
        port: 8080,
        username: 'alice',
        protocol: 'webdav',
        options: { webdavScheme: 'https' },
        relaxedOptions: { webdavScheme: 'http' },
        hasStoredCredential: true,
    };

    const winscpWebdavRawHttp: ImportedServer = {
        id: 'winscp-webdav-2',
        name: 'Raw WebDAV',
        host: 'dav.example.com',
        port: 8080,
        username: 'alice',
        protocol: 'webdav',
        options: { webdavScheme: 'http' },
        hasStoredCredential: true,
    };

    it('defaults to resetting plain-HTTP WebDAV on a non-local host to https and strips relaxed metadata', () => {
        const fromBackend = importedServerProfile(winscpWebdavResetByBackend);
        expect(fromBackend.options?.webdavScheme).toBe('https');
        expect(fromBackend).not.toHaveProperty('relaxedOptions');
        expect(fromBackend).not.toHaveProperty('relaxedHost');

        const fromRaw = importedServerProfile(winscpWebdavRawHttp);
        expect(fromRaw.options?.webdavScheme).toBe('https');
        expect(fromRaw).not.toHaveProperty('relaxedOptions');
    });

    it('preserves plain-HTTP WebDAV only when webdavScheme is explicitly kept in the security review', () => {
        const keptFromBackend = importedServerProfile(winscpWebdavResetByBackend, ['webdavScheme']);
        expect(keptFromBackend.options?.webdavScheme).toBe('http');
        expect(keptFromBackend).not.toHaveProperty('relaxedOptions');
        expect(keptFromBackend).not.toHaveProperty('relaxedHost');

        const keptFromRaw = importedServerProfile(winscpWebdavRawHttp, ['webdavScheme']);
        expect(keptFromRaw.options?.webdavScheme).toBe('http');
    });
});

describe('a scan-bound commit', () => {
    it('binds the commit to the account the scan wrote into, not the one active at commit time', async () => {
        // The scan ran under account 7; the vault now reads account 3.
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 3, revision: 'r1' });
        const onImport = vi.fn();
        const outcome = await commitImportedServersForScan([crypt], new Set([bridgeProfileKey(base)]), 7, onImport);
        // The scan account is validated BEFORE anything is written: no removal,
        // no append, the commit reports failure.
        expect(outcome.error).toMatch(/active account changed since the scan/);
        expect(outcome.added).toBe(0);
        expect(outcome.updated).toBe(0);
        expect(onImport).not.toHaveBeenCalled();
        expect(storeSavedServerProfiles).not.toHaveBeenCalled();
    });

    it('refuses the replacement path before any removal when the scan account changed', async () => {
        // The scan ran under account 7; the vault now reads account 3, and the
        // import REPLACES base (its key is in the existing set), so the removal
        // write would run first if the scan account were not validated first.
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 3, revision: 'r1' });
        vi.mocked(storeSavedServerProfiles).mockClear();
        const onImport = vi.fn();
        const outcome = await commitImportedServersForScan(
            [{ ...base, id: 'replacement', name: 'New' }],
            new Set([bridgeProfileKey(base)]),
            7,
            onImport,
        );
        expect(outcome.error).toMatch(/active account changed since the scan/);
        // The removal write must not have run either.
        expect(storeSavedServerProfiles).not.toHaveBeenCalled();
        expect(onImport).not.toHaveBeenCalled();
    });

    it('hands the validated scan account and the removal keys to the ONE atomic write', async () => {
        // Scan and commit both under account 7; the import replaces base.
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 7, revision: 'r1' });
        vi.mocked(storeSavedServerProfiles).mockClear();
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => profiles);
        const onImport = vi.fn(async (_servers: ServerProfile[], _expected?: number | null, _removeKeys?: ReadonlySet<string>) => undefined);
        const outcome = await commitImportedServersForScan(
            [{ ...base, id: 'replacement', name: 'New' }],
            new Set([bridgeProfileKey(base)]),
            7,
            onImport,
        );
        expect(outcome).toEqual({ added: 0, updated: 1 });
        // No separate removal write: the one write happens inside onImport,
        // carrying the validated account and the removal keys together.
        expect(storeSavedServerProfiles).not.toHaveBeenCalled();
        expect(onImport).toHaveBeenCalledWith([expect.objectContaining({ id: 'replacement' })], 7, new Set([bridgeProfileKey(base)]));
    });

    it('commits normally when the account did not change since the scan', async () => {
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 7, revision: 'r1' });
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => profiles);
        const onImport = vi.fn(async (servers: ServerProfile[], expected?: number | null, removeKeys?: ReadonlySet<string>) => {
            await appendImportedProfiles(servers, expected, removeKeys);
        });
        const outcome = await commitImportedServersForScan([crypt], new Set([bridgeProfileKey(base)]), 7, onImport);
        expect(outcome).toEqual({ added: 1, updated: 0 });
        expect(onImport).toHaveBeenCalledWith([crypt], 7, undefined);
    });

    // A locked scan could not read an owner: scanUserId is UNDEFINED (unknown),
    // not null (confirmed legacy). After unlock + rescan the account reads as
    // 7 and the commit proceeds: no false account-changed refusal.
    it('an unknown scan owner (undefined, locked scan) does not trip a false account-changed refusal after unlock + rescan', async () => {
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 7, revision: 'r1' });
        vi.mocked(storeSavedServerProfiles).mockImplementation(async (profiles: ServerProfile[]) => profiles);
        const onImport = vi.fn(async (servers: ServerProfile[], expected?: number | null, removeKeys?: ReadonlySet<string>) => {
            await appendImportedProfiles(servers, expected, removeKeys);
        });
        const outcome = await commitImportedServersForScan([crypt], new Set([bridgeProfileKey(base)]), undefined, onImport);
        expect(outcome).toEqual({ added: 1, updated: 0 });
        expect(onImport).toHaveBeenCalledWith([crypt], 7, undefined);
    });

    // A CONFIRMED legacy scan (null: vault open, no account) is still refused
    // once an account is active: null is a real answer, not a missing one.
    it('a confirmed legacy scan (null) is refused once an account is active', async () => {
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [base], userId: 7, revision: 'r1' });
        vi.mocked(storeSavedServerProfiles).mockClear();
        const onImport = vi.fn();
        const outcome = await commitImportedServersForScan([crypt], new Set(), null, onImport);
        expect(outcome.error).toMatch(/active account changed since the scan/);
        expect(onImport).not.toHaveBeenCalled();
        expect(storeSavedServerProfiles).not.toHaveBeenCalled();
    });
});

describe('BridgeSourcePanel scan generation', () => {
    let root: Root;
    let host: HTMLDivElement;

    beforeEach(() => {
        (globalThis as { IS_REACT_ACT_ENVIRONMENT?: boolean }).IS_REACT_ACT_ENVIRONMENT = true;
        host = document.createElement('div');
        document.body.appendChild(host);
        root = createRoot(host);
    });

    afterEach(() => {
        act(() => root.unmount());
        host.remove();
        vi.mocked(invoke).mockReset();
    });

    /// Two dropped files resolving in REVERSE order: the newer scan's rows,
    /// account id and errors win, the older one is dropped entirely.
    it('a superseded scan cannot overwrite the newer scan\'s rows', async () => {
        const { BridgeSourcePanel } = await import('../BridgeSourcePanel');
        const { detectBridgeConfigBounded } = await import('../../hooks/useDetectedBridgeConfigs');
        vi.mocked(detectBridgeConfigBounded).mockResolvedValue(null as unknown as string);
        const meta = {
            id: 'aws', label: 'AWS', supportedProtocols: ['s3'], importExtensions: ['ini'],
            exportExt: 'ini', exportLabel: 'AWS', secretPolicy: 'full', importFilterName: 'AWS',
        };
        const deferred: Array<{ resolve: (v: unknown) => void }> = [];
        vi.mocked(invoke).mockImplementation((cmd: string) => {
            if (cmd === 'bridge_source_meta') return Promise.resolve(meta);
            if (cmd === 'import_bridge_config') {
                let resolve!: (v: unknown) => void;
                const promise = new Promise(r => { resolve = r; });
                deferred.push({ resolve });
                return promise;
            }
            return Promise.reject(new Error(`unexpected invoke ${cmd}`));
        });
        const source = { id: 'aws', label: 'AWS' } as never;
        const props = {
            source, direction: 'import' as const, servers: [] as ServerProfile[], accountId: 7,
            existingServerKeys: new Set<string>(), onImport: vi.fn(), onClose: vi.fn(), onBack: vi.fn(),
        };
        act(() => { root.render(createElement(BridgeSourcePanel, { ...props, presetFilePath: '/a.ini' })); });
        act(() => { root.render(createElement(BridgeSourcePanel, { ...props, presetFilePath: '/b.ini' })); });
        expect(deferred.length).toBe(2);

        const scanA = {
            servers: [{ id: 'a1', name: 'scan-A', host: '', port: 443, username: 'x', protocol: 's3' }],
            skipped: [], sourcePath: '/a.ini', totalRemotes: 1, scanUserId: 7,
        };
        const scanB = {
            servers: [{ id: 'b1', name: 'scan-B', host: '', port: 443, username: 'x', protocol: 's3' }],
            skipped: [], sourcePath: '/b.ini', totalRemotes: 1, scanUserId: 9,
        };
        // Reverse order: the NEWER scan (b) resolves first, then the older (a).
        await act(async () => { deferred[1].resolve(scanB); });
        await act(async () => { deferred[0].resolve(scanA); });
        expect(host.textContent).toContain('scan-B');
        expect(host.textContent).not.toContain('scan-A');

        // Commit the surviving scan: onImport must receive the NEWER scan's
        // owner (9), not the superseded scan's (7).
        vi.mocked(readSavedServerProfilesForWrite).mockResolvedValue({ profiles: [], userId: 9, revision: 'r9' });
        const importButton = Array.from(host.querySelectorAll('button'))
            .find(b => b.textContent?.includes('bridgeImportSelected'));
        expect(importButton).toBeDefined();
        await act(async () => { importButton!.dispatchEvent(new MouseEvent('click', { bubbles: true })); });
        expect(props.onImport).toHaveBeenCalledWith(
            [expect.objectContaining({ id: 'b1' })],
            9,
            undefined,
        );
    });
});
