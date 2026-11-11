// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { transformWithOxc } from 'vite';
import app from '../App.tsx?raw';
import { QueuedTargetChangedError } from './queuedTarget';

const start = app.indexOf('  const applyOpenDrivePrivacyToPaths =');
const end = app.indexOf('  const [fileLuRemoteUploadDialog', start);
if (start < 0 || end < 0) throw new Error('Properties handler not found');
const { code } = await transformWithOxc(app.slice(start, end), 'opendrive-properties.ts');

describe('Properties access target', () => {
    it('refuses all writes when the target changes while its ownership is acquired', async () => {
        let current = 'A';
        let release!: (value: { epoch: number }) => void;
        const prepared = new Promise<{ epoch: number }>(resolve => { release = resolve; });
        const target = { profileId: 'A' };
        const owner = { target, prepared };
        const invoke = vi.fn(async () => undefined);
        const notify = { error: vi.fn(), warning: vi.fn(), success: vi.fn() };
        const context = {
            propertiesOwnerRef: { current: owner },
            queuedTargetGuard: (wanted: typeof target) => () => {
                if (wanted.profileId !== current) throw new QueuedTargetChangedError('changed');
            },
            resolveGuiTarget: async (pending: Promise<{ epoch: number }>, check: () => void) => {
                const result = await pending; check(); return result.epoch;
            },
            invoke, notify, t: (key: string) => key,
            humanLog: { logRaw: () => 'log', updateEntry: vi.fn() },
            setPropertiesDialog: vi.fn(), setMultiPropertiesDialog: vi.fn(), loadRemoteFiles: vi.fn(),
        };
        const apply = new Function(...Object.keys(context), `${code}; return applyOpenDrivePrivacyToPaths;`)(...Object.values(context));
        const pending = apply('public', [{ path: '/a', isDir: false }, { path: '/b', isDir: false }]);
        current = 'B';
        release({ epoch: 9 });
        await pending;
        expect(invoke).not.toHaveBeenCalled();
        expect(context.loadRemoteFiles).not.toHaveBeenCalled();
        expect(notify.error).toHaveBeenCalled();
    });

    it('passes the same captured epoch to each access change', async () => {
        const target = { profileId: 'A' };
        const invoke = vi.fn(async () => undefined);
        const context = {
            propertiesOwnerRef: { current: { target, prepared: Promise.resolve({ epoch: 9 }) } },
            queuedTargetGuard: () => () => undefined,
            resolveGuiTarget: async (pending: Promise<{ epoch: number }>) => (await pending).epoch,
            invoke, notify: { error: vi.fn(), warning: vi.fn(), success: vi.fn() }, t: (key: string) => key,
            humanLog: { logRaw: () => 'log', updateEntry: vi.fn() },
            setPropertiesDialog: vi.fn(), setMultiPropertiesDialog: vi.fn(), loadRemoteFiles: vi.fn(),
        };
        const apply = new Function(...Object.keys(context), `${code}; return applyOpenDrivePrivacyToPaths;`)(...Object.values(context));
        await apply('private', [{ path: '/a', isDir: false }, { path: '/b', isDir: true }]);
        expect(invoke.mock.calls).toEqual([
            ['opendrive_set_path_access', { path: '/a', accessLevel: 'private', isDir: false, expectedGuiTarget: 9 }],
            ['opendrive_set_path_access', { path: '/b', accessLevel: 'private', isDir: true, expectedGuiTarget: 9 }],
        ]);
    });
});
