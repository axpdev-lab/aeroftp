// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement as h } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { AISettingsPanel } from './AISettingsPanel';
import { getDefaultAISettings, type AISettings } from '../../types/ai';
import { applyAgentAiSettingsUpdate } from '../../utils/aiSettingsStore';

const backend = vi.hoisted(() => ({ blob: '', key: 'FIXTURE-OLD', writes: [] as { account: string; password: string }[],
    delayKey: undefined as Promise<void> | undefined }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async (command: string, args?: { account?: string; password?: string }) => {
    if (command === 'get_credential') return args?.account === 'config_ai_settings' ? backend.blob : backend.key;
    if (command === 'store_credential') {
        const write = { account: args!.account!, password: args!.password! }; backend.writes.push(write);
        if (write.account === 'ai_apikey_p') { await backend.delayKey; backend.key = write.password; }
        else if (write.account === 'config_ai_settings') backend.blob = write.password;
        return;
    }
    if (command === 'list_plugins' || command === 'ai_list_models') return [];
    return undefined;
}) }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async () => () => {}) }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('./McpServersPanel', () => ({ McpServersPanel: () => null }));
vi.mock('./ProviderMarketplace', () => ({ ProviderMarketplace: () => null }));
vi.mock('./PluginBrowser', () => ({ PluginBrowser: () => null }));

let root: Root;
let host: HTMLDivElement;
beforeEach(async () => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    const settings: AISettings = getDefaultAISettings();
    settings.providers = [{ id: 'p', name: 'Public Fixture', type: 'custom', baseUrl: 'http://127.0.0.1:1',
        isEnabled: true, isDefault: true, createdAt: new Date(0), updatedAt: new Date(0) }];
    backend.blob = JSON.stringify(settings); backend.key = 'FIXTURE-OLD'; backend.writes = []; backend.delayKey = undefined;
    localStorage.clear();
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
    await act(async () => root.render(h(AISettingsPanel, { isOpen: true, onClose: () => {} })));
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); });
const button = (text: string) => [...host.querySelectorAll('button')].find(b => b.textContent?.trim() === text)!;
async function until(predicate: () => boolean) {
    const deadline = Date.now() + 3000;
    while (!predicate()) {
        if (Date.now() >= deadline) throw new Error('panel test timeout');
        await act(async () => { await new Promise(resolve => setTimeout(resolve, 20)); });
    }
}
async function expand() {
    await until(() => host.textContent!.includes('Public Fixture'));
    const label = [...host.querySelectorAll('*')].find(e => e.children.length === 0 && e.textContent === 'Public Fixture')!;
    await act(async () => label.parentElement!.parentElement!.querySelector<HTMLButtonElement>('button')!.click());
    return host.querySelector<HTMLInputElement>('input[type="password"]')!;
}
async function typeKey(input: HTMLInputElement, value: string) {
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, value);
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
}

it('an unrelated human preference save never rewrites a hydrated API key', async () => {
    await expand();
    await act(async () => button('ai.settings.enabled').click());
    await until(() => backend.writes.some(w => w.account === 'config_ai_settings'));
    expect(backend.writes.map(w => w.account)).toEqual(['config_ai_settings']);
    expect(backend.blob).not.toMatch(/FIXTURE-OLD|apiKey/);
    expect(backend.key).toBe('FIXTURE-OLD');
});

it('a new human key edit during a delayed old key write survives the public save and persists once', async () => {
    const input = await expand();
    let release!: () => void;
    backend.delayKey = new Promise<void>(resolve => { release = resolve; });
    await typeKey(input, 'FIXTURE-A');
    await until(() => backend.writes.some(w => w.password === 'FIXTURE-A'));
    await typeKey(input, 'FIXTURE-B');
    await act(async () => { await applyAgentAiSettingsUpdate({ advanced: { temperature: 0.2 } }); });
    expect(input.value).toBe('FIXTURE-B');
    await act(async () => { backend.delayKey = undefined; release(); });
    await until(() => backend.key === 'FIXTURE-B');
    expect(backend.writes.filter(w => w.account === 'ai_apikey_p').map(w => w.password)).toEqual(['FIXTURE-A', 'FIXTURE-B']);
    expect(backend.blob).not.toMatch(/FIXTURE-A|FIXTURE-B|FIXTURE-OLD|apiKey/);
    expect(JSON.parse(backend.blob).advancedSettings.temperature).toBe(0.2);
});

it('a pending human provider toggle and agent generation update both survive debounce', async () => {
    await expand();
    await act(async () => button('ai.settings.enabled').click());
    await act(async () => { await applyAgentAiSettingsUpdate({ advanced: { temperature: 0.2 } }); });
    await until(() => backend.writes.filter(w => w.account === 'config_ai_settings').length === 2);
    expect(JSON.parse(backend.blob)).toMatchObject({ providers: [{ isEnabled: false }], advancedSettings: { temperature: 0.2 } });
    expect(backend.writes.every(w => w.account === 'config_ai_settings')).toBe(true);
});
