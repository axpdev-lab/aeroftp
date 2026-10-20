// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
// @vitest-environment jsdom
import { act, createElement } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';

const mocks = vi.hoisted(() => ({ invoke: vi.fn(), account: 0 }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));
vi.mock('../../utils/userPartitions', () => ({ accountChangeGeneration: () => mocks.account }));
vi.mock('../../hooks/useDraggableModal', () => ({ useDraggableModal: () => ({ panelProps: {}, dragHandleProps: {} }) }));
vi.mock('../../utils/serverProfileStore', () => ({
    loadSavedServerProfiles: async () => ['profile-a', 'profile-b'].map(id => ({
        id, siteTemplates: [{ id: 'template', name: 'Change password', line: 'CHPASS alice {password:secret}' }],
    })),
    mergeSavedServerProfile: vi.fn(),
}));
import { SiteCommandDialog } from './SiteCommandDialog';

function deferred<T>() {
    let resolve!: (value: T) => void;
    const promise = new Promise<T>(yes => { resolve = yes; });
    return { promise, resolve };
}
const rendered = { line: 'CHPASS alice synthetic-secret', echo: 'SITE CHPASS alice masked', carries_secret: true };
const report = { outcome: 'replied', command: 'SITE CHPASS', verb_kind: 'other', code: 200, lines: ['200 ok'],
    encoding: 'utf8', elapsed_ms: 1, session_reset: false, session_reconnected: null, reason: null };
let host: HTMLDivElement;
let root: Root;
const onClose = vi.fn();
const onActivity = vi.fn();
const scrollDescriptor = Object.getOwnPropertyDescriptor(HTMLElement.prototype, 'scrollIntoView');
type Props = Parameters<typeof SiteCommandDialog>[0];
async function render(over: Partial<Props> = {}) {
    await act(async () => root.render(createElement(SiteCommandDialog, {
        isOpen: true, sessionId: 'session-a', sessionLabel: 'Synthetic FTP', profileId: 'profile-a',
        onClose, onActivity, onFilesChanged: vi.fn(), ...over,
    })));
}
function button(label: string) {
    return Array.from(host.querySelectorAll('button')).find(node => node.textContent === label) as HTMLButtonElement;
}
async function selectTemplate() {
    await act(async () => {
        const select = host.querySelector('#site-template') as HTMLSelectElement;
        select.value = 'template';
        select.dispatchEvent(new Event('change', { bubbles: true }));
    });
}
function siteCalls() { return mocks.invoke.mock.calls.filter(([name]) => name === 'provider_site_command'); }
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    mocks.account = 0;
    onClose.mockReset();
    onActivity.mockReset();
    mocks.invoke.mockReset().mockImplementation((name: string) => {
        if (name === 'provider_site_session') return Promise.resolve({ supported: true, encrypted: false, generation: 7 });
        if (name === 'site_template_slots') return Promise.resolve([{ name: 'password', kind: 'secret' }]);
        if (name === 'site_template_render') return Promise.resolve(rendered);
        if (name === 'provider_site_command') return Promise.resolve(report);
        return Promise.resolve();
    });
    vi.stubGlobal('ResizeObserver', class { observe() {} disconnect() {} });
    Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', { configurable: true, value: vi.fn() });
    host = document.createElement('div');
    document.body.append(host);
    root = createRoot(host);
});
afterEach(async () => {
    await act(async () => root.unmount());
    host.remove();
    if (scrollDescriptor) Object.defineProperty(HTMLElement.prototype, 'scrollIntoView', scrollDescriptor);
    else Reflect.deleteProperty(HTMLElement.prototype, 'scrollIntoView');
    vi.restoreAllMocks();
    vi.unstubAllGlobals();
});

it.each([
    ['close', { isOpen: false }], ['session', { sessionId: 'session-b' }], ['profile', { profileId: 'profile-b' }],
] as const)('never sends a template completed after a %s boundary', async (_label, change) => {
    const pending = deferred<typeof rendered>();
    mocks.invoke.mockImplementation((name: string) => {
        if (name === 'provider_site_session') return Promise.resolve({ supported: true, encrypted: true, generation: 7 });
        if (name === 'site_template_render') return pending.promise;
        if (name === 'site_template_slots') return Promise.resolve([]);
        return Promise.resolve(report);
    });
    await render();
    await selectTemplate();
    await act(async () => { button('siteCommand.runTemplate').click(); });
    await render(change);
    await act(async () => { pending.resolve(rendered); });
    expect(siteCalls()).toHaveLength(0);
    expect(onActivity).not.toHaveBeenCalled();
});

it('acquires one flight before rendering and cannot release a newer opening flight', async () => {
    const first = deferred<typeof rendered>();
    const next = deferred<typeof rendered>();
    let count = 0;
    const original = mocks.invoke.getMockImplementation()!;
    mocks.invoke.mockImplementation((name: string, args: unknown) => name === 'site_template_render'
        ? (++count === 1 ? first.promise : next.promise) : original(name, args));
    await render();
    await selectTemplate();
    await act(async () => { const run = button('siteCommand.runTemplate'); run.click(); run.click(); });
    expect(count).toBe(1);
    await render({ sessionId: 'session-b' });
    await selectTemplate();
    await act(async () => { button('siteCommand.runTemplate').click(); });
    await act(async () => { first.resolve(rendered); });
    expect(button('siteCommand.runTemplate').disabled).toBe(true);
    expect(siteCalls()).toHaveLength(0);
    await act(async () => { next.resolve(rendered); });
    expect(siteCalls()).toHaveLength(1);
    expect(siteCalls()[0][1]).toMatchObject({ generation: 7, args: rendered.line });
});

it('allows manual refresh after a busy status and disables again when the backend reports a swap', async () => {
    const original = mocks.invoke.getMockImplementation()!;
    let status = 0;
    mocks.invoke.mockImplementation((name: string, args: unknown) => {
        if (name === 'provider_site_session') return Promise.resolve({ supported: true, encrypted: null, generation: status++ ? 9 : null });
        if (name === 'provider_site_command') return Promise.resolve({ ...report, outcome: 'not_sent', reason: 'connection_changed' });
        return original(name, args);
    });
    await render();
    await selectTemplate();
    expect(button('siteCommand.runTemplate').disabled).toBe(true);
    await act(async () => { button('common.refresh').click(); });
    expect(button('siteCommand.runTemplate').disabled).toBe(false);
    await act(async () => { button('siteCommand.runTemplate').click(); });
    expect(siteCalls()[0][1]).toMatchObject({ generation: 9 });
    expect(button('siteCommand.runTemplate').disabled).toBe(true);
    expect(button('common.refresh')).toBeDefined();
});

it('requires an explicit generation even when status says SITE is supported', async () => {
    const original = mocks.invoke.getMockImplementation()!;
    mocks.invoke.mockImplementation((name: string, args: unknown) => name === 'provider_site_session'
        ? Promise.resolve({ supported: true, encrypted: false }) : original(name, args));
    await render();
    await selectTemplate();
    expect(button('siteCommand.runTemplate').disabled).toBe(true);
    const input = host.querySelector('[aria-label="siteCommand.inputLabel"]') as HTMLInputElement;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'HELP');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    await act(async () => { input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true })); });
    expect(siteCalls()).toHaveLength(0);
    expect(button('common.refresh')).toBeDefined();
});

it('drops a render completed after an account switch before React has rerendered', async () => {
    const pending = deferred<typeof rendered>();
    const original = mocks.invoke.getMockImplementation()!;
    mocks.invoke.mockImplementation((name: string, args: unknown) => name === 'site_template_render' ? pending.promise : original(name, args));
    await render();
    await selectTemplate();
    await act(async () => { button('siteCommand.runTemplate').click(); });
    mocks.account += 1;
    await act(async () => { pending.resolve(rendered); });
    expect(siteCalls()).toHaveLength(0);
});

it('retires a pending template synchronously when close is requested', async () => {
    const pending = deferred<typeof rendered>();
    const original = mocks.invoke.getMockImplementation()!;
    mocks.invoke.mockImplementation((name: string, args: unknown) => name === 'site_template_render' ? pending.promise : original(name, args));
    await render();
    await selectTemplate();
    await act(async () => { button('siteCommand.runTemplate').click(); });
    await act(async () => { (host.querySelector('[aria-label="common.close"]') as HTMLButtonElement).click(); });
    expect(onClose).toHaveBeenCalledTimes(1);
    // The parent has not rendered isOpen=false yet.
    await act(async () => { pending.resolve(rendered); });
    expect(siteCalls()).toHaveLength(0);
});

it('shares the synchronous flight between template rendering and free-form Enter', async () => {
    const pending = deferred<typeof rendered>();
    const original = mocks.invoke.getMockImplementation()!;
    mocks.invoke.mockImplementation((name: string, args: unknown) => name === 'site_template_render' ? pending.promise : original(name, args));
    await render();
    await selectTemplate();
    const input = host.querySelector('[aria-label="siteCommand.inputLabel"]') as HTMLInputElement;
    await act(async () => {
        Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, 'value')!.set!.call(input, 'HELP');
        input.dispatchEvent(new Event('input', { bubbles: true }));
    });
    expect(input.value).toBe('HELP');
    await act(async () => {
        button('siteCommand.runTemplate').click();
        input.dispatchEvent(new KeyboardEvent('keydown', { key: 'Enter', bubbles: true }));
    });
    expect(siteCalls()).toHaveLength(0);
    await act(async () => { pending.resolve(rendered); });
    expect(siteCalls()).toHaveLength(1);
    expect(siteCalls()[0][1]).toMatchObject({ args: rendered.line });
});
