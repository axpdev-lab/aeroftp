// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { beforeEach, describe, expect, it, vi } from 'vitest';

const invoke = vi.hoisted(() => vi.fn());
const readForWrite = vi.hoisted(() => vi.fn());
const storeProfiles = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('./serverProfileStore', () => ({
    readSavedServerProfilesForWrite: readForWrite,
    storeSavedServerProfiles: storeProfiles,
}));

import { saveLaunchUrlProfile } from './connectUrlSave';
import type { OpenRequest } from '../types';

const baseRequest: OpenRequest = {
    kind: 'remoteUrl',
    path: 'sftp://bob@example.com/reports',
    folder: '/reports',
    name: '',
    isDir: true,
    protocol: 'sftp',
    server: 'example.com',
    username: 'bob',
};

beforeEach(() => {
    invoke.mockReset();
    readForWrite.mockReset();
    storeProfiles.mockReset();
});

describe('saveLaunchUrlProfile', () => {
    it('saves the protocol default port, never 0, when the URL omits it', async () => {
        readForWrite.mockResolvedValue({ profiles: [], userId: 1, revision: 'r1' });
        storeProfiles.mockImplementation(async (profiles) => profiles);
        const profile = await saveLaunchUrlProfile(baseRequest, '', undefined);
        expect(profile.port).toBe(22);
        expect(profile.protocol).toBe('sftp');
        expect(profile.hasStoredCredential).toBe(false);
        expect(invoke).not.toHaveBeenCalledWith('store_credential', expect.anything());
    });

    it('writes the secret to the vault only after the profile write lands', async () => {
        readForWrite.mockResolvedValue({ profiles: [], userId: 1, revision: 'r1' });
        storeProfiles.mockImplementation(async (profiles) => profiles);
        invoke.mockResolvedValue(undefined);
        const profile = await saveLaunchUrlProfile(baseRequest, 'My server', 'hunter2');
        const cred = invoke.mock.calls.find((c) => c[0] === 'store_credential');
        expect(cred).toBeDefined();
        expect(cred![1]).toEqual({ account: `server_${profile.id}`, password: 'hunter2' });
        expect(storeProfiles.mock.invocationCallOrder[0]).toBeLessThan(invoke.mock.invocationCallOrder[0]);
        expect(profile.name).toBe('My server');
        expect(profile.hasStoredCredential).toBe(true);
    });

    it('leaves no orphan credential when the profile write is refused', async () => {
        readForWrite.mockResolvedValue({ profiles: [], userId: 1, revision: 'r1' });
        storeProfiles.mockRejectedValue(new Error('PROFILES_CHANGED'));
        await expect(saveLaunchUrlProfile(baseRequest, '', 'hunter2')).rejects.toThrow('PROFILES_CHANGED');
        expect(invoke).not.toHaveBeenCalledWith('store_credential', expect.anything());
    });

    it('rolls the new profile back out when the vault credential write fails after the profile write landed', async () => {
        let storedList: unknown[] = [];
        storeProfiles.mockImplementation(async (profiles) => {
            storedList = profiles as unknown[];
            return profiles;
        });
        readForWrite
            .mockResolvedValueOnce({ profiles: [], userId: 1, revision: 'r1' })
            .mockImplementation(async () => ({ profiles: storedList as never, userId: 1, revision: 'r2' }));
        invoke.mockRejectedValue(new Error('VAULT_LOCKED'));
        await expect(saveLaunchUrlProfile(baseRequest, 'My server', 'hunter2')).rejects.toThrow('VAULT_LOCKED');
        expect(invoke).toHaveBeenCalledTimes(1);
        expect(storeProfiles).toHaveBeenCalledTimes(2);
        const firstList = storeProfiles.mock.calls[0][0] as Array<{ id: string }>;
        const newId = firstList[firstList.length - 1].id;
        expect(newId).toMatch(/^srv_/);
        const rollbackList = storeProfiles.mock.calls[1][0] as Array<{ id: string }>;
        expect(rollbackList.some((p) => p.id === newId)).toBe(false);
        expect(rollbackList).toEqual([]);
    });

    it('fails closed when the vault read refuses (locked or account switched)', async () => {
        readForWrite.mockRejectedValue(new Error('STORE_NOT_READY'));
        await expect(saveLaunchUrlProfile(baseRequest, '', 'hunter2')).rejects.toThrow('STORE_NOT_READY');
        expect(storeProfiles).not.toHaveBeenCalled();
        expect(invoke).not.toHaveBeenCalled();
    });
});
