// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { createElement } from 'react';
import { act } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { useSettings } from './useSettings';

// The real utils/appSettings, only the backend mocked: what reaches the vault
// and what stays in WebView storage is the point of these tests.
const backend = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: backend.invoke }));

let root: Root;
let host: HTMLDivElement;
let latest: ReturnType<typeof useSettings>;
function Harness() {
    latest = useSettings();
    return null;
}
const stores = () => backend.invoke.mock.calls.filter(([command]) => command === 'store_credential');

beforeEach(() => {
    Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
    localStorage.clear();
    backend.invoke.mockReset();
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

describe('useSettings legacy migration against the vault', () => {
    it('writes nothing to a vault that already holds the settings, and drops the plaintext copy', async () => {
        localStorage.setItem('aeroftp_settings', JSON.stringify({ fontSize: 15 }));
        backend.invoke.mockImplementation(async (command: string) => {
            if (command === 'get_credential') return JSON.stringify({ fontSize: 18 });
            return null;
        });
        await act(async () => root.render(createElement(Harness)));
        await vi.waitFor(() => expect(latest.fontSize).toBe(18));
        await act(async () => window.dispatchEvent(new Event('aeroftp-vault-ready')));
        await vi.waitFor(() => expect(backend.invoke.mock.calls.filter(([c]) => c === 'get_credential').length).toBeGreaterThanOrEqual(3));
        expect(stores()).toEqual([]);
        expect(localStorage.getItem('aeroftp_settings')).toBeNull();
    });

    it('moves settings found only in the plaintext copy into the vault once', async () => {
        localStorage.setItem('aeroftp_settings', JSON.stringify({ fontSize: 15 }));
        let vault: string | null = null;
        backend.invoke.mockImplementation(async (command: string, args: { password?: string }) => {
            if (command === 'get_credential') {
                if (vault === null) throw new Error('Credential not found: config_app_settings');
                return vault;
            }
            if (command === 'store_credential') vault = args.password ?? null;
            return null;
        });
        await act(async () => root.render(createElement(Harness)));
        await vi.waitFor(() => expect(stores()).toHaveLength(1));
        expect(JSON.parse(vault!)).toEqual({ fontSize: 15 });
        expect(localStorage.getItem('aeroftp_settings')).toBeNull();
        await act(async () => window.dispatchEvent(new Event('aeroftp-vault-ready')));
        expect(stores()).toHaveLength(1);
    });
});
