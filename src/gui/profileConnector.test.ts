// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { beforeEach, expect, it, vi } from 'vitest';
import { ConnectScope } from './connectScope';
import { createProfileConnector } from './profileConnector';
import type { ServerProfile } from '../types';
const state = vi.hoisted(() => ({
    status: { isUnlocked: true, activeUserId: 1, unlockedUserId: 1 },
    load: vi.fn<() => Promise<ServerProfile[]>>(),
}));
vi.mock('../utils/userPartitions', () => ({ getUnlockStatus: async () => ({ ...state.status }) }));
vi.mock('../utils/serverProfileStore', () => ({ loadSavedServerProfilesStrict: state.load }));
const profile = (id: string) => ({ id, name: 'Same name', host: 'test.invalid', username: 'user', protocol: 'ftp', password: 'PRIVATE_SENTINEL' } as ServerProfile);
beforeEach(() => { state.status = { isUnlocked: true, activeUserId: 1, unlockedUserId: 1 }; state.load.mockReset().mockResolvedValue([profile('one'), profile('two')]); });
it('routes duplicate names by exact ID and returns only a safe outcome', async () => {
    const connect = vi.fn(async (_profile: ServerProfile, _scope: ConnectScope) => 'connected' as const);
    const reply = await createProfileConnector(connect, () => true)('two', new ConnectScope());
    expect(connect.mock.calls[0][0]).toMatchObject({ id: 'two' });
    expect(reply).toBe('connected'); expect(JSON.stringify(reply)).not.toContain('PRIVATE');
});
it('refuses missing, deleted, foreign and duplicate IDs without connecting', async () => {
    const connect = vi.fn(async (_profile: ServerProfile, _scope: ConnectScope) => 'connected' as const); const connector = createProfileConnector(connect, () => true);
    await expect(connector('foreign', new ConnectScope())).rejects.toThrow('invalid_args');
    state.load.mockResolvedValue([]); await expect(connector('one', new ConnectScope())).rejects.toThrow('invalid_args');
    state.load.mockResolvedValue([profile('one'), profile('one')]); await expect(connector('one', new ConnectScope())).rejects.toThrow('invalid_args');
    expect(connect).not.toHaveBeenCalled();
});
it('refuses account changes and stale registrations while the owned profile lookup is pending', async () => {
    for (const change of ['account', 'unmount'] as const) {
        state.status = { isUnlocked: true, activeUserId: 1, unlockedUserId: 1 };
        let mounted = true; let release!: (profiles: ServerProfile[]) => void; let began = false;
        state.load.mockImplementation(() => { began = true; return new Promise(resolve => { release = resolve; }); });
        const connect = vi.fn(async (_profile: ServerProfile, _scope: ConnectScope) => 'connected' as const);
        const pending = createProfileConnector(connect, () => mounted)('one', new ConnectScope());
        while (!began) await Promise.resolve();
        if (change === 'account') state.status.activeUserId = 2; else mounted = false;
        release([profile('one')]); await expect(pending).rejects.toThrow('lease_interrupted'); expect(connect).not.toHaveBeenCalled();
    }
});
it('blocks late credential dispatch after an account change even without a UI event', async () => {
    let release!: () => void; let began = false; const dispatch = vi.fn();
    const connector = createProfileConnector(async (_profile, scope) => {
        await scope.step(() => { began = true; return new Promise<void>(resolve => { release = resolve; }); });
        await scope.step(dispatch); return 'connected';
    }, () => true);
    const pending = connector('one', new ConnectScope());
    while (!began) await Promise.resolve(); state.status.unlockedUserId = 2; release();
    await expect(pending).rejects.toThrow('lease_interrupted'); expect(dispatch).not.toHaveBeenCalled();
});
