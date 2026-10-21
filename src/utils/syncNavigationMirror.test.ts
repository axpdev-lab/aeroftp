// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { expect, it, vi } from 'vitest';
import {
    cryptSyncBoundaryActive, planProviderSyncMirror, plainSftpSyncMirror, syncMirrorConflictText, syncMirrorProposesCreate,
} from './syncNavigationMirror';

const nfd = 'εσωτερικο\u0301ς';
const nfc = nfd.normalize('NFC');
const directory = (name: string) => ({ name, is_dir: true });

it('enables canonical mirroring only for plain SFTP', () => {
    expect(plainSftpSyncMirror('sftp', false)).toBe(true);
    expect(plainSftpSyncMirror('sftp', true)).toBe(false);
    expect(plainSftpSyncMirror('ftp', false)).toBe(false);
    expect(plainSftpSyncMirror('ftps', false)).toBe(false);
    expect(plainSftpSyncMirror(undefined, false)).toBe(false);
    expect(cryptSyncBoundaryActive({
        aeroVaultSessionId: null, rcloneCryptVaultId: null, aeroCryptVaultId: null, providerOverlayBound: false,
    })).toBe(false);
    expect(cryptSyncBoundaryActive({
        aeroVaultSessionId: null, rcloneCryptVaultId: 'vault', aeroCryptVaultId: null, providerOverlayBound: false,
    })).toBe(true);
});

it('navigates to the destination spelling and lists hidden names on both sides', async () => {
    const invoke = vi.fn(async (command: string, args: Record<string, unknown>) => {
        if (command === 'provider_list_files' && args.path === '/remote') return { files: [directory(nfc)] };
        if (command === 'get_local_files' && args.path === '/local' && args.showHidden === true) return [directory(nfd)];
        throw new Error(`unexpected ${command} ${JSON.stringify(args)}`);
    });
    const action = await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: `/local/${nfd}`, sourceBase: '/local', destBase: '/remote', invoke,
    });
    expect(action).toEqual({ kind: 'navigate', path: `/remote/${nfc}` });
    expect(syncMirrorProposesCreate(action)).toBe(false);
    expect(invoke).toHaveBeenCalledWith('provider_list_files', { path: '/remote' });
    expect(invoke).toHaveBeenCalledWith('get_local_files', { path: '/local', showHidden: true });
    expect(invoke.mock.calls.map(call => call[0])).not.toContain('provider_mkdir');
    expect(invoke.mock.calls.map(call => call[0])).not.toContain('create_remote_folder');
});

it('mirrors remote to local with the local spelling', async () => {
    const invoke = vi.fn(async (command: string, args: Record<string, unknown>) => {
        if (command === 'get_local_files' && args.path === '/local') return [directory(nfd)];
        if (command === 'provider_list_files' && args.path === '/remote') return { files: [directory(nfc)] };
        throw new Error(`unexpected ${command} ${JSON.stringify(args)}`);
    });
    const action = await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'remote-to-local',
        sourcePath: `/remote/${nfc}`, sourceBase: '/remote', destBase: '/local', invoke,
    });
    expect(action).toEqual({ kind: 'navigate', path: `/local/${nfd}` });
    expect(syncMirrorProposesCreate(action)).toBe(false);
});

it('offers mkdir only for a real miss and names a conflict without creating it', async () => {
    const missing = vi.fn(async (command: string) => {
        if (command === 'provider_list_files') return { files: [] };
        if (command === 'get_local_files') return [directory('only')];
        throw new Error(command);
    });
    const missed = await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: '/local/only', sourceBase: '/local', destBase: '/remote', invoke: missing,
    });
    expect(missed).toEqual({ kind: 'missing', path: '/remote/only' });
    expect(syncMirrorProposesCreate(missed)).toBe(true);

    const requested = 'a\u0323\u0301';
    const alternatives = ['ạ\u0301', 'á\u0323'];
    const conflict = vi.fn(async (command: string) => {
        if (command === 'provider_list_files') return { files: alternatives.map(directory) };
        if (command === 'get_local_files') return [directory(requested)];
        throw new Error(command);
    });
    const ambiguous = await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: `/local/${requested}`, sourceBase: '/local', destBase: '/remote', invoke: conflict,
    });
    expect(ambiguous.kind).toBe('conflict');
    expect(syncMirrorProposesCreate(ambiguous)).toBe(false);
    if (ambiguous.kind === 'conflict') {
        expect(syncMirrorConflictText(ambiguous.path, ambiguous.candidates)).toContain(alternatives[0]);
        expect(ambiguous.candidates).toEqual(alternatives.map(name => `/remote/${name}`));
    }
    expect(conflict.mock.calls.map(call => call[0])).not.toContain('provider_mkdir');
});

it('propagates a listing failure instead of calling the directory missing', async () => {
    const invoke = vi.fn(async () => { throw new Error('permission denied on /remote'); });
    await expect(planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: '/local/only', sourceBase: '/local', destBase: '/remote', invoke,
    })).rejects.toThrow('permission denied');
    expect(invoke).toHaveBeenCalledTimes(1);
    expect(invoke).toHaveBeenCalledWith('provider_list_files', { path: '/remote' });
});

it('skips a path outside the sync base and leaves FTP on the legacy route', async () => {
    const invoke = vi.fn();
    expect(await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: '/other/child', sourceBase: '/local', destBase: '/remote', invoke,
    })).toEqual({ kind: 'skip' });
    expect(await planProviderSyncMirror({
        protocol: 'ftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: '/local/child', sourceBase: '/local', destBase: '/remote', invoke,
    })).toEqual({ kind: 'legacy' });
    expect(await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: true, direction: 'remote-to-local',
        sourcePath: '/remote/child', sourceBase: '/remote', destBase: '/local', invoke,
    })).toEqual({ kind: 'legacy' });
    expect(invoke).not.toHaveBeenCalled();
    expect(syncMirrorProposesCreate({ kind: 'skip' })).toBe(false);
    expect(syncMirrorProposesCreate({ kind: 'legacy' })).toBe(false);
    expect(syncMirrorProposesCreate({ kind: 'cancelled' })).toBe(false);
});

it('discards a stale resolution without a path', async () => {
    const invoke = vi.fn(async () => ({ files: [] }));
    expect(await planProviderSyncMirror({
        protocol: 'sftp', cryptActive: false, direction: 'local-to-remote',
        sourcePath: '/local/only', sourceBase: '/local', destBase: '/remote', invoke,
        isCurrent: () => false,
    })).toEqual({ kind: 'cancelled' });
});
