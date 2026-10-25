// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement as h } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { SSHTerminal } from './SSHTerminal';

const backend = vi.hoisted(() => ({ settings: '', readErrors: 0 }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async (command: string, args?: { account?: string }) => {
    if (command === 'get_credential' && args?.account === 'config_terminal_settings') {
        if (backend.readErrors > 0) {
            backend.readErrors--;
            throw new Error('STORE_NOT_READY');
        }
        return backend.settings;
    }
    return undefined;
}) }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    localStorage.clear();
    backend.readErrors = 0;
    backend.settings = '';
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
});

it('applies terminal vault settings after an initial STORE_NOT_READY fallback', async () => {
    localStorage.setItem('aeroftp-terminal-settings', JSON.stringify({ themeName: 'dracula', fontSize: 12 }));
    backend.settings = JSON.stringify({ themeName: 'nord', fontSize: 18 });
    backend.readErrors = 1;
    await act(async () => root.render(h(SSHTerminal)));

    expect(host.textContent).toContain('12');
    expect(host.querySelector('[style*="rgb(40, 42, 54)"]')).not.toBeNull();
    await act(async () => { window.dispatchEvent(new Event('aeroftp-vault-ready')); });

    expect(host.textContent).toContain('18');
    expect(host.textContent).not.toContain('12');
    expect(host.querySelector('[style*="rgb(46, 52, 64)"]')).not.toBeNull();
});
