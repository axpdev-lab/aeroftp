// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { createElement, useEffect } from 'react';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useSettings } from './useSettings';

const mocks = vi.hoisted(() => ({
    invoke: vi.fn(),
    updateAppSettings: vi.fn(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../utils/appSettings', () => ({ updateAppSettings: mocks.updateAppSettings }));

let root: Root;
let host: HTMLDivElement;
let latest: ReturnType<typeof useSettings>;
let mounts: number;

function Harness() {
    useEffect(() => {
        mounts++;
    }, []);
    latest = useSettings();
    return null;
}

beforeEach(() => {
    vi.useFakeTimers();
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    localStorage.clear();
    mounts = 0;
    mocks.updateAppSettings.mockReset().mockResolvedValue({});
    mocks.invoke.mockReset().mockImplementation(async (command: string) => {
        if (command === 'get_credential') throw new Error('STORE_NOT_READY');
        return null;
    });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});

afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    vi.useRealTimers();
});

describe('useSettings vault readiness', () => {
    it('applies vault-only settings after readiness without a remount or polling', async () => {
        await act(async () => root.render(createElement(Harness)));
        expect(latest.fontSize).toBe(16);
        expect(latest.dateFormat).toBe('localized');
        const mounted = mounts;

        mocks.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') return JSON.stringify({ fontSize: 18, dateFormat: 'iso' });
            return null;
        });
        await act(async () => window.dispatchEvent(new Event('aeroftp-vault-ready')));

        expect(latest.fontSize).toBe(18);
        expect(latest.dateFormat).toBe('iso');
        expect(mounts).toBe(mounted);
        expect(mocks.invoke.mock.calls.filter(([command]) => command === 'get_credential')).toHaveLength(2);
        await act(async () => vi.advanceTimersByTimeAsync(10000));
        expect(mocks.invoke.mock.calls.filter(([command]) => command === 'get_credential')).toHaveLength(2);
    });

    it('does not let an older mount read overwrite the readiness reconcile', async () => {
        let rejectMount!: (error: Error) => void;
        let resolveReady!: (value: string) => void;
        mocks.invoke.mockImplementation((command: string) => {
            if (command === 'get_credential') {
                if (mocks.invoke.mock.calls.filter(([name]) => name === 'get_credential').length === 1) {
                    return new Promise<string>((_resolve, reject) => { rejectMount = reject; });
                }
                return new Promise<string>(resolve => { resolveReady = resolve; });
            }
            return Promise.resolve(null);
        });
        await act(async () => root.render(createElement(Harness)));
        await act(async () => window.dispatchEvent(new Event('aeroftp-vault-ready')));
        await act(async () => resolveReady(JSON.stringify({ fontSize: 20, dateFormat: 'dmy' })));
        expect(latest.fontSize).toBe(20);
        expect(latest.dateFormat).toBe('dmy');

        await act(async () => rejectMount(new Error('STORE_NOT_READY')));

        expect(latest.fontSize).toBe(20);
        expect(latest.dateFormat).toBe('dmy');
        expect(mocks.invoke.mock.calls.filter(([command]) => command === 'get_credential')).toHaveLength(2);
    });
});
