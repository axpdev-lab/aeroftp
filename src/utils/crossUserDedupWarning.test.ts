// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// MU-7: a profile saved from the connection form that another user account on
// this device already stores gets a soft warning. The backend probe existed
// since the multi-user work, but no save path ever called it, so the warning
// never appeared.

import { beforeEach, describe, expect, it, vi, type Mock } from 'vitest';
import connectionScreenSource from '../components/ConnectionScreen.tsx?raw';
import type { ServerProfile } from '../types';
import type { OperationStatus, OperationType } from '../hooks/useActivityLog';

const mockInvoke = vi.fn();
vi.mock('@tauri-apps/api/core', () => ({
    invoke: (cmd: string, args?: unknown) => mockInvoke(cmd, args),
}));

import { warnIfSavedByOtherAccount } from './crossUserDedupWarning';

// Echo the key and its parameters, so the assertions read what was asked for.
const t = (key: string, params?: Record<string, string | number>) =>
    params ? `${key} ${JSON.stringify(params)}` : key;

const profile = (over: Partial<ServerProfile> = {}): ServerProfile => ({
    id: 'srv_1',
    name: 'Office SFTP',
    host: 'sftp.example.com',
    port: 22,
    username: 'alice',
    protocol: 'sftp',
    ...over,
} as ServerProfile);

let toasts: Array<{ type?: string; title?: string; message?: string }>;
type LogActivity = (operation: OperationType, message: string, status?: OperationStatus, details?: string) => void;
let logActivity: Mock<LogActivity>;

beforeEach(() => {
    mockInvoke.mockReset();
    toasts = [];
    const target = new EventTarget();
    target.addEventListener('aeroftp-toast', (e) => toasts.push((e as CustomEvent).detail));
    vi.stubGlobal('window', target);
    logActivity = vi.fn<LogActivity>();
});

describe('warnIfSavedByOtherAccount', () => {
    it('asks the backend about the saved profile and warns with the other account names', async () => {
        mockInvoke.mockResolvedValue([
            { userId: 2, userName: 'Bob' },
            { userId: 3, userName: 'Carol' },
        ]);
        const saved = profile();

        const names = await warnIfSavedByOtherAccount(saved, undefined, t, logActivity);

        expect(names).toEqual(['Bob', 'Carol']);
        expect(mockInvoke).toHaveBeenCalledWith('user_partitions_find_cross_user_dedup', { profile: saved });
        expect(toasts).toHaveLength(1);
        expect(toasts[0].type).toBe('warning');
        expect(toasts[0].title).toBe('manageUsers.crossUserDedupTitle');
        expect(toasts[0].message).toContain('"accounts":"Bob, Carol"');
        expect(toasts[0].message).toContain('"name":"Office SFTP"');
        // The record that survives toasts being turned off.
        expect(logActivity).toHaveBeenCalledWith(
            'PROFILE_DUPLICATE',
            expect.stringContaining('Bob, Carol'),
            'success',
            expect.stringMatching(/^dedupKey=/),
        );
    });

    it('says nothing when no other account has the profile', async () => {
        mockInvoke.mockResolvedValue([]);
        expect(await warnIfSavedByOtherAccount(profile(), undefined, t, logActivity)).toEqual([]);
        expect(toasts).toHaveLength(0);
        expect(logActivity).not.toHaveBeenCalled();
    });

    it('never throws when the probe fails, so the save it follows is unaffected', async () => {
        mockInvoke.mockRejectedValue('NO_ACTIVE_USER');
        const warn = vi.spyOn(console, 'warn').mockImplementation(() => {});
        await expect(warnIfSavedByOtherAccount(profile(), undefined, t, logActivity)).resolves.toEqual([]);
        expect(toasts).toHaveLength(0);
        expect(warn).toHaveBeenCalled();
        warn.mockRestore();
    });

    it('skips an edit that keeps the same server account, and probes one that changes it', async () => {
        mockInvoke.mockResolvedValue([{ userId: 2, userName: 'Bob' }]);
        const before = profile();

        await warnIfSavedByOtherAccount({ ...before, name: 'Renamed', initialPath: '/www' }, before, t, logActivity);
        expect(mockInvoke).not.toHaveBeenCalled();
        expect(toasts).toHaveLength(0);

        await warnIfSavedByOtherAccount({ ...before, host: 'other.example.com' }, before, t, logActivity);
        expect(mockInvoke).toHaveBeenCalledTimes(1);
        expect(toasts).toHaveLength(1);
    });
});

/** The body of `const <name> = async (...) => { ... };`, by brace depth. */
function bodyOf(name: string): string {
    const start = connectionScreenSource.indexOf(`const ${name} = `);
    expect(start, `${name} is defined`).toBeGreaterThan(-1);
    const open = connectionScreenSource.indexOf('{', connectionScreenSource.indexOf('=>', start));
    let depth = 0;
    for (let i = open; i < connectionScreenSource.length; i++) {
        if (connectionScreenSource[i] === '{') depth++;
        else if (connectionScreenSource[i] === '}' && --depth === 0) return connectionScreenSource.slice(open, i + 1);
    }
    throw new Error(`unbalanced body for ${name}`);
}

describe('connection form save paths', () => {
    // ConnectionScreen has no render harness, so this reads its source: the
    // property is that every form path that stores a profile with a possibly
    // new server account runs the probe after the store.
    it('runs the cross-user probe after every save that can change the account', () => {
        const save = bodyOf('saveToServers');
        // Edit branch (compared with the profile before the edit) and add branch.
        expect(save).toContain('warnIfSavedByOtherAccount(savedServer, prevProfile, t, logActivity)');
        expect(save).toContain('warnIfSavedByOtherAccount(newServer, undefined, t, logActivity)');
        expect(bodyOf('handleSaveAsNew')).toContain('warnIfSavedByOtherAccount(newServer, originalServer, t, logActivity)');
        expect(bodyOf('handleConvertMode')).toContain('warnIfSavedByOtherAccount(newServer, originalServer, t, logActivity)');
        // Not awaited: a slow or failing probe never holds up the save.
        expect(connectionScreenSource).not.toMatch(/await\s+warnIfSavedByOtherAccount/);
        for (const fn of ['saveToServers', 'handleSaveAsNew', 'handleConvertMode']) {
            const body = bodyOf(fn);
            expect(body.indexOf('storeSavedServerProfiles('), fn)
                .toBeLessThan(body.indexOf('warnIfSavedByOtherAccount('));
        }
    });
});
