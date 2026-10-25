// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { PortableIsolationBanner } from './PortableIsolationBanner';

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

const portableInfo = {
    is_portable: true,
    data_root: 'D:\\Programs\\AeroFTP\\data',
    webview_data_dir: null,
    credential_store_dir: 'D:\\Programs\\AeroFTP\\data\\aeroftp',
    shared_legacy_present: false,
};

let root: Root;
let host: HTMLDivElement;

const render = async (vaultReady: boolean) => {
    await React.act(async () => {
        root.render(React.createElement(PortableIsolationBanner, { vaultReady }));
        await Promise.resolve();
    });
};

beforeEach(() => {
    vi.useFakeTimers();
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
    invoke.mockReset();
});

afterEach(async () => {
    await React.act(async () => root.unmount());
    host.remove();
    vi.useRealTimers();
});

describe('PortableIsolationBanner vault readiness', () => {
    it('keeps a dismissed banner hidden when the first read hits STORE_NOT_READY', async () => {
        invoke.mockImplementation(async (command: string) => {
            if (command === 'portable_info') return portableInfo;
            if (command === 'get_credential') {
                if (invoke.mock.calls.filter(([name]) => name === 'get_credential').length === 1) {
                    throw new Error('STORE_NOT_READY');
                }
                return 'true';
            }
            return null;
        });

        await render(true);
        await React.act(async () => vi.advanceTimersByTimeAsync(200));

        expect(invoke).toHaveBeenCalledTimes(3);
        expect(host.querySelector('[role="status"]')).toBeNull();
    });

    it('never treats an exhausted STORE_NOT_READY retry sequence as an absent dismissal', async () => {
        invoke.mockImplementation(async (command: string) => {
            if (command === 'portable_info') return portableInfo;
            if (command === 'get_credential') throw new Error('STORE_NOT_READY');
            return null;
        });

        await render(true);
        await React.act(async () => vi.advanceTimersByTimeAsync(3000));

        expect(invoke.mock.calls.filter(([name]) => name === 'get_credential')).toHaveLength(6);
        expect(host.querySelector('[role="status"]')).toBeNull();
    });

    it('does not read before unlock and reads the dismissal when the vault becomes ready', async () => {
        invoke.mockImplementation(async (command: string) => {
            if (command === 'portable_info') return portableInfo;
            if (command === 'get_credential') return 'true';
            return null;
        });

        await render(false);
        expect(invoke).not.toHaveBeenCalled();

        await render(true);
        expect(invoke).toHaveBeenCalledWith('get_credential', {
            account: 'config_portable_isolation_notice_v3_7_8',
        });
        expect(host.querySelector('[role="status"]')).toBeNull();
    });

    it('retries a dismissal write that starts before the vault is ready', async () => {
        invoke.mockImplementation(async (command: string) => {
            if (command === 'portable_info') return portableInfo;
            if (command === 'get_credential') return null;
            if (command === 'store_credential') {
                if (invoke.mock.calls.filter(([name]) => name === 'store_credential').length === 1) {
                    throw new Error('STORE_NOT_READY');
                }
            }
            return null;
        });

        await render(true);
        const dismiss = Array.from(host.querySelectorAll('button'))
            .find(button => button.textContent === 'portable.banner.dismiss');
        expect(dismiss).toBeTruthy();
        await React.act(async () => dismiss!.click());
        await React.act(async () => vi.advanceTimersByTimeAsync(200));

        expect(invoke.mock.calls.filter(([name]) => name === 'store_credential')).toHaveLength(2);
        expect(invoke).toHaveBeenLastCalledWith('store_credential', {
            account: 'config_portable_isolation_notice_v3_7_8',
            password: 'true',
        });
    });
});
