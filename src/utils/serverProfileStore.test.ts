// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Tests for the MU-FE partition-aware server-profile choke point. Covers:
//   - happy-path partition write -> PROFILES_CHANGED_EVENT dispatch +
//     legacy localStorage cleanup
//   - STORE_NOT_READY fallback that routes the read/write through the
//     legacy `config_server_profiles` blob (the only path the rest of
//     the frontend may now safely take outside of this module).

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

import {
    PROFILES_CHANGED_EVENT,
    loadSavedServerProfiles,
    storeSavedServerProfiles,
} from './serverProfileStore';
import type { ServerProfile } from '../types';

const sampleProfile = (overrides: Partial<ServerProfile> = {}): ServerProfile => ({
    id: 'srv_1',
    name: 'default-sftp',
    host: 'sftp.example.com',
    port: 22,
    username: 'demo',
    protocol: 'sftp',
    initialPath: '/',
    ...overrides,
});

let eventTarget: EventTarget;
let localStoreMap: Map<string, string>;

beforeEach(() => {
    mockInvoke.mockReset();
    eventTarget = new EventTarget();
    localStoreMap = new Map();

    vi.stubGlobal('localStorage', {
        getItem: (key: string) => (localStoreMap.has(key) ? localStoreMap.get(key)! : null),
        setItem: (key: string, value: string) => { localStoreMap.set(key, value); },
        removeItem: (key: string) => { localStoreMap.delete(key); },
        clear: () => { localStoreMap.clear(); },
        key: (i: number) => Array.from(localStoreMap.keys())[i] ?? null,
        get length() { return localStoreMap.size; },
    });

    vi.stubGlobal('window', {
        dispatchEvent: (e: Event) => eventTarget.dispatchEvent(e),
        addEventListener: (t: string, l: EventListener) => eventTarget.addEventListener(t, l),
        removeEventListener: (t: string, l: EventListener) => eventTarget.removeEventListener(t, l),
    });
});

afterEach(() => { vi.unstubAllGlobals(); });

describe('storeSavedServerProfiles', () => {
    it('writes through the user_partitions save command on the happy path', async () => {
        mockInvoke.mockResolvedValueOnce(undefined); // user_partitions_save_active_server_profiles
        const profiles = [sampleProfile()];

        await storeSavedServerProfiles(profiles);

        expect(mockInvoke).toHaveBeenCalledWith(
            'user_partitions_save_active_server_profiles',
            { profiles },
        );
    });

    it('dispatches PROFILES_CHANGED_EVENT after a successful write', async () => {
        mockInvoke.mockResolvedValueOnce(undefined);
        const handler = vi.fn();
        eventTarget.addEventListener(PROFILES_CHANGED_EVENT, handler);

        await storeSavedServerProfiles([sampleProfile()]);

        expect(handler).toHaveBeenCalledTimes(1);
    });

    it('removes the legacy localStorage mirror after a successful write', async () => {
        mockInvoke.mockResolvedValueOnce(undefined);
        localStoreMap.set('aeroftp-saved-servers', JSON.stringify([sampleProfile()]));

        await storeSavedServerProfiles([sampleProfile({ name: 'renamed' })]);

        expect(localStoreMap.has('aeroftp-saved-servers')).toBe(false);
    });

    it('falls back to the legacy vault key on STORE_NOT_READY and still cleans up + dispatches', async () => {
        const handler = vi.fn();
        eventTarget.addEventListener(PROFILES_CHANGED_EVENT, handler);
        localStoreMap.set('aeroftp-saved-servers', '[]');

        mockInvoke
            .mockRejectedValueOnce(new Error('STORE_NOT_READY')) // partition save rejects
            .mockResolvedValueOnce(undefined); // legacy store_credential succeeds

        await storeSavedServerProfiles([sampleProfile()]);

        expect(mockInvoke.mock.calls[0][0]).toBe('user_partitions_save_active_server_profiles');
        expect(mockInvoke.mock.calls[1][0]).toBe('store_credential');
        expect(mockInvoke.mock.calls[1][1]).toMatchObject({
            account: 'config_server_profiles',
        });
        expect(handler).toHaveBeenCalledTimes(1);
        expect(localStoreMap.has('aeroftp-saved-servers')).toBe(false);
    });

    it('does not swallow a non-fallback error from the partition write', async () => {
        const boom = new Error('disk full');
        mockInvoke.mockRejectedValueOnce(boom);

        await expect(storeSavedServerProfiles([sampleProfile()])).rejects.toBe(boom);
    });
});

describe('loadSavedServerProfiles', () => {
    it('returns the partition payload on the happy path', async () => {
        const profiles = [sampleProfile()];
        // seedLegacyLocalProfilesForPartitionMigration short-circuits because
        // localStorage is empty, so the only invoke is the partition load.
        mockInvoke.mockResolvedValueOnce(profiles);

        const result = await loadSavedServerProfiles();

        expect(result).toEqual(profiles);
        expect(mockInvoke).toHaveBeenCalledWith(
            'user_partitions_load_active_server_profiles',
            undefined,
        );
    });

    it('roundtrips an MTP device profile with deviceFingerprint', async () => {
        // APPENDIX-DEVICE-PROFILES Phase 1: vault load/store must preserve the
        // structured fingerprint (and path fields) for saved MTP devices.
        const mtpProfile = sampleProfile({
            id: 'dev_xperia',
            name: 'Sony Xperia backup',
            host: 'XQ-DQ54',
            port: 0,
            username: '',
            protocol: 'mtp',
            providerId: 'mtp-portable',
            initialPath: '/Internal shared storage/DCIM',
            localInitialPath: '/home/user/PhoneBackup',
            deviceFingerprint: {
                kind: 'mtp',
                serial: 'QV770LUNJD',
                vid: '0FCE',
                pid: '020D',
                model: 'XQ-DQ54',
                canonical: 'mtp:serial=QV770LUNJD',
            },
        });

        mockInvoke.mockResolvedValueOnce(undefined); // partition save
        await storeSavedServerProfiles([mtpProfile]);
        expect(mockInvoke).toHaveBeenCalledWith(
            'user_partitions_save_active_server_profiles',
            { profiles: [mtpProfile] },
        );

        mockInvoke.mockReset();
        mockInvoke.mockResolvedValueOnce([mtpProfile]); // partition load
        const loaded = await loadSavedServerProfiles();

        expect(loaded).toHaveLength(1);
        const row = loaded[0];
        expect(row.protocol).toBe('mtp');
        expect(row.initialPath).toBe('/Internal shared storage/DCIM');
        expect(row.localInitialPath).toBe('/home/user/PhoneBackup');
        expect(row.deviceFingerprint).toEqual({
            kind: 'mtp',
            serial: 'QV770LUNJD',
            vid: '0FCE',
            pid: '020D',
            model: 'XQ-DQ54',
            canonical: 'mtp:serial=QV770LUNJD',
        });
        // Deep equal on the whole profile (JSON vault path is identity for optional fields).
        expect(row).toEqual(mtpProfile);
    });

    it('seeds the legacy vault when local data exists and the vault is empty', async () => {
        const legacy = [sampleProfile({ id: 'srv_legacy', name: 'legacy-row' })];
        localStoreMap.set('aeroftp-saved-servers', JSON.stringify(legacy));

        // seedLegacyLocalProfilesForPartitionMigration probes the legacy
        // vault key first; returning null lets it adopt the localStorage
        // snapshot before the partition load runs.
        mockInvoke
            .mockRejectedValueOnce(new Error('not found')) // get_credential probe
            .mockResolvedValueOnce(undefined)              // store_credential seed
            .mockResolvedValueOnce(legacy);                // partition load

        const result = await loadSavedServerProfiles();

        expect(result).toEqual(legacy);
        expect(localStoreMap.has('aeroftp-saved-servers')).toBe(false);
    });

    it('falls back to the legacy vault read on STORE_NOT_READY', async () => {
        const legacy = [sampleProfile({ id: 'srv_legacy_2' })];

        mockInvoke
            .mockRejectedValueOnce(new Error('STORE_NOT_READY')) // partition load rejects
            .mockResolvedValueOnce(JSON.stringify(legacy));      // legacy get_credential resolves

        const result = await loadSavedServerProfiles();

        expect(result).toEqual(legacy);
        expect(mockInvoke.mock.calls[0][0]).toBe('user_partitions_load_active_server_profiles');
        expect(mockInvoke.mock.calls[1][0]).toBe('get_credential');
        expect(mockInvoke.mock.calls[1][1]).toMatchObject({
            account: 'config_server_profiles',
        });
    });
});
