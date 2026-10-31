// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { beforeEach, describe, expect, it, vi } from 'vitest';
import { invoke } from '@tauri-apps/api/core';
import { upsertFriendProfile } from './aeroShare';

vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn() }));

const other = { id: 'srv_other', name: 'Saved elsewhere', host: 'h', port: 21, username: 'u' };
const binding = { afid: 'friend-afid', alias: 'Friend' };

/** The backend: each read answers the next of `reads`, each save the next of `saves`. */
const backend = (reads: unknown[], saves: unknown[]) => {
    vi.mocked(invoke).mockImplementation(async (command: string) => {
        if (command === 'user_partitions_load_active_server_profiles_scoped') return reads.shift();
        if (command === 'user_partitions_save_active_server_profiles') {
            const answer = saves.shift();
            if (answer instanceof Error) throw answer;
            return answer;
        }
        if (command === 'get_credential') return JSON.stringify({ aeroShareEnabled: true });
        return undefined;
    });
};

const saves = () => vi.mocked(invoke).mock.calls.filter(call => call[0] === 'user_partitions_save_active_server_profiles');

describe('upsertFriendProfile after another writer saved', () => {
    beforeEach(() => {
        vi.mocked(invoke).mockReset();
    });

    it('merges once more into the new list of the same account', async () => {
        backend(
            [{ userId: 4, profiles: [], revision: 'r1' }, { userId: 4, profiles: [other], revision: 'r2' }],
            [new Error('PROFILES_CHANGED: nothing was saved'), [other, { id: 'peer', name: 'Friend' }]],
        );
        await upsertFriendProfile(binding);
        expect(saves()).toHaveLength(2);
        const last = saves()[1][1] as { profiles: { id: string }[]; expectedUserId: number; expectedRevision: string };
        expect(last.profiles[0]).toEqual(other);
        expect(last.profiles).toHaveLength(2);
        expect(last.expectedUserId).toBe(4);
        expect(last.expectedRevision).toBe('r2');
    });

    it('stops when the account changed, so the friend never lands in the other account', async () => {
        backend(
            [{ userId: 4, profiles: [], revision: 'r1' }, { userId: 9, profiles: [other], revision: 'r9' }],
            [new Error('PROFILES_CHANGED: nothing was saved')],
        );
        await expect(upsertFriendProfile(binding)).rejects.toThrow('ACCOUNT_CHANGED');
        expect(saves()).toHaveLength(1);
    });
});
