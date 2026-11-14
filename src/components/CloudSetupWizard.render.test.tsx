// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import React from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, expect, it, vi } from 'vitest';
import { CloudSetupWizard } from './CloudSetupWizard';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), profiles: [] as unknown[] }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string, params?: Record<string, unknown>) => params ? `${key} ${JSON.stringify(params)}` : key }));
vi.mock('../utils/serverProfileStore', () => ({ loadSavedServerProfilesStrict: async () => mocks.profiles }));
vi.mock('./AeroCloudProtocols', () => ({ AeroCloudMaturityBadge: () => null, isAeroCloudProtocol: () => true, useAeroCloudCaveat: () => () => null }));

let root: Root | undefined;
let host: HTMLDivElement;
afterEach(async () => { if (root) await React.act(async () => root!.unmount()); host?.remove(); root = undefined; mocks.invoke.mockReset(); });

it('refuses a starting folder with `..` under the login folder with a clear error, and enables nothing', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 22, username: 'u', protocol: 'sftp', initialPath: 'docs/../shared' }];
    mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
    const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
    await React.act(async () => button('common.next')!.click());
    await React.act(async () => button('nas')!.click());
    const alert = host.querySelector('[role="alert"]');
    // The folder is named as it would be saved, `..` included: never collapsed.
    expect(alert?.textContent).toContain('cloud.remoteFolderNotPlain');
    expect(alert?.textContent).toContain('~/docs/../shared/cloud/');
    expect(button('common.next')!.disabled).toBe(true);
    expect(mocks.invoke).not.toHaveBeenCalledWith('setup_aerocloud', expect.anything());
    // A sub-folder cannot step out either; a plain one clears the error.
    mocks.profiles = [];
    const input = host.querySelector<HTMLInputElement>('input[aria-label="cloud.remoteFolder"]')!;
    const type = (value: string) => React.act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await type('../escape');
    expect(host.querySelector('[role="alert"]')?.textContent).toContain('cloud.remoteFolderNotPlain');
});

it('refuses a sub-folder that is not plain on a plain starting folder, and lets a plain one through', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    // A plain starting folder: only the sub-folder can raise the alert, so
    // the assertions pin the sub-folder guard, not the base.
    mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 22, username: 'u', protocol: 'sftp', initialPath: 'docs' }];
    mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
    const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
    await React.act(async () => button('common.next')!.click());
    await React.act(async () => button('nas')!.click());
    const input = host.querySelector<HTMLInputElement>('input[aria-label="cloud.remoteFolder"]')!;
    const type = (value: string) => React.act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    for (const sub of ['../escape', './cloud', 'a//cloud']) {
        await type(sub);
        expect(host.querySelector('[role="alert"]')?.textContent, sub).toContain('cloud.remoteFolderNotPlain');
        expect(button('common.next')!.disabled, sub).toBe(true);
    }
    await type('cloud');
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(button('common.next')!.disabled).toBe(false);
});

it('lets a starting folder without `..` through', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 22, username: 'u', protocol: 'sftp', initialPath: 'docs' }];
    mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
    const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
    await React.act(async () => button('common.next')!.click());
    await React.act(async () => button('nas')!.click());
    expect(host.querySelector('[role="alert"]')).toBeNull();
    expect(button('common.next')!.disabled).toBe(false);
});
