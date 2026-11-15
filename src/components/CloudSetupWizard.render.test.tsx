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
    // The alert says why and names no folder: a profile path may carry a
    // password or a token.
    expect(alert?.textContent?.trim()).toBe('cloud.remoteFolderNotPlain');
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
    // Repeated outer separators are empty steps too: refused, not stripped
    // into a plain `~/docs/a/`.
    for (const sub of ['../escape', './cloud', 'a//cloud', 'a//', '//a', '/a//']) {
        await type(sub);
        expect(host.querySelector('[role="alert"]')?.textContent?.trim(), sub).toBe('cloud.remoteFolderNotPlain');
        expect(button('common.next')!.disabled, sub).toBe(true);
    }
    // One leading or trailing separator is no step.
    for (const sub of ['cloud', '/cloud', 'cloud/', '/cloud/']) {
        await type(sub);
        expect(host.querySelector('[role="alert"]'), sub).toBeNull();
        expect(button('common.next')!.disabled, sub).toBe(false);
    }
});

it('refuses a sub-folder whose backslash joins an empty step into the folder it composes', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    // Each part is plain alone: `/data`, and `\\cloud` (a `\\` ends a step, so
    // its leading empty step is a leading separator). Joined, `/data/\\cloud/`
    // has an empty step, which the backend refuses on save.
    mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 22, username: 'u', protocol: 'sftp', initialPath: '/data' }];
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
    for (const sub of ['\\cloud', 'cloud\\', '/\\cloud']) {
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

it('names no folder in the not-plain alert, so a password in the profile path is never shown there', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    // An empty step after the scheme: not plain.
    mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 443, username: 'u', protocol: 'webdav', initialPath: 'https://alice:pa55word@example.com//data' }];
    mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
    const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
    await React.act(async () => button('common.next')!.click());
    await React.act(async () => button('nas')!.click());
    const alert = host.querySelector('[role="alert"]');
    expect(alert?.textContent?.trim()).toBe('cloud.remoteFolderNotPlain');
    expect(alert?.textContent).not.toContain('pa55word');
    expect(button('common.next')!.disabled).toBe(true);
});

it('shows a profile folder written as a URL by its scheme only, anywhere in the wizard', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    for (const initialPath of ['https://alice:pa55word@example.com//data', 'https://example.com/share/T0KEN-opaque/data?sig=s3cret#fr4g']) {
        mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 443, username: 'u', protocol: 'webdav', initialPath }];
        mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
        const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
        await React.act(async () => button('common.next')!.click());
        await React.act(async () => button('nas')!.click());
        expect(host.innerHTML).toContain('https://…');
        // The host is not shown either: no part of the folder after its scheme.
        for (const secret of ['example.com', 'pa55word', 'T0KEN-opaque', 's3cret', 'fr4g']) expect(host.innerHTML).not.toContain(secret);
        await React.act(async () => root!.unmount()); host.remove(); root = undefined;
    }
});

it('shows a URL folder of a login-relative profile by its scheme only, saved with or without ~/, in the list, the root label and its title', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    // Written as a URL, and saved already in `~/` form, with a password
    // that carries `@`, `/`, digits that read as a port, or a user part and
    // host before another `@`: the scheme only, whatever the shape.
    const written = [
        'https://alice:pa55word@example.com/T0KEN-opaque/data?sig=s3cret#fr4g',
        'https://alice:pa55@w0rd@example.com/T0KEN-opaque/data',
        'https://alice:pa55/w0rd@example.com/T0KEN-opaque/data',
        'https://alice:5512/w0rd@example.com/T0KEN-opaque/data',
        'https://user:fake@secretpart:5512/rest@example.com/data',
        'https://alice:pa55@w0rd/x@w0rdhost.example/T0KEN-opaque/data',
    ].flatMap(folder => [folder, `~/${folder}`]);
    // The combined shape of the review, as written there.
    expect(written).toContain('~/https://user:fake@secretpart:5512/rest@example.com/data');
    for (const [protocol, initialPath] of (['sftp', 'ftp'] as const).flatMap(p => written.map(w => [p, w] as const))) {
        mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 22, username: 'u', protocol, initialPath }];
        mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
        host = document.createElement('div'); document.body.append(host); root = createRoot(host);
        await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
        const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
        await React.act(async () => button('common.next')!.click());
        await React.act(async () => button('nas')!.click());
        // The folder opens under the login folder: the label carries `~/`.
        const label = [...host.querySelectorAll('span[title]')].find(s => s.getAttribute('title')?.startsWith('~/'));
        const name = `${protocol} ${initialPath}`;
        expect(label?.getAttribute('title'), name).toBe('~/https://…/');
        expect(label?.textContent?.trim(), name).toBe('~/https://…/');
        // The profile list, the label and its title alike.
        for (const secret of ['example.com', 'alice', 'pa55', 'fake', 'secretpart', '5512', 'rest@', 'w0rd', 'T0KEN-opaque', 's3cret', 'fr4g']) expect(host.innerHTML, name).not.toContain(secret);
        await React.act(async () => root!.unmount()); host.remove(); root = undefined;
    }
});

it('shows a plain profile folder as written, a ? in its name included, up to the summary', async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.profiles = [{ id: 'srv_1', name: 'nas', host: 'nas.example', port: 22, username: 'u', protocol: 'sftp', initialPath: '/srv/reports?2026' }];
    mocks.invoke.mockImplementation(async (command: string) => command === 'get_default_cloud_folder' ? '/home/u/AeroCloud' : null);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await React.act(async () => root!.render(<CloudSetupWizard onComplete={vi.fn()} onCancel={vi.fn()} />));
    const button = (label: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.includes(label));
    await React.act(async () => button('common.next')!.click());
    await React.act(async () => button('nas')!.click());
    expect(host.textContent).toContain('/srv/reports?2026/');
    expect(button('common.next')!.disabled).toBe(false);
    await React.act(async () => button('common.next')!.click());
    // The summary names the folder as written, with the default sub-folder.
    expect([...host.querySelectorAll('code')].map(c => c.textContent)).toContain('/srv/reports?2026/cloud/');
});
