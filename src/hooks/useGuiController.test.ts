// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { act, createElement as h, StrictMode } from 'react';
import { createRoot, type Root } from 'react-dom/client';
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { useGuiController } from './useGuiController';
import { GuiControllerBanner } from '../components/GuiControllerBanner';
import type { GuiHandlers, GuiSource } from '../gui/controller';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';

const bridge = vi.hoisted(() => ({
    callbacks: new Map<string, (event: { payload: unknown }) => void>(),
    invoke: vi.fn<(name: string, args: Record<string, unknown>) => Promise<unknown>>(),
}));
vi.mock('@tauri-apps/api/core', () => ({ invoke: bridge.invoke }));
vi.mock('@tauri-apps/api/event', () => ({ listen: vi.fn(async (name: string, handler: (event: { payload: unknown }) => void) => {
    bridge.callbacks.set(name, handler);
    return () => { if (bridge.callbacks.get(name) === handler) bridge.callbacks.delete(name); };
}) }));
vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));

let root: Root;
let host: HTMLDivElement;
let source: GuiSource;
let handlers: GuiHandlers;
const audit = vi.fn();
function Harness() {
    const controller = useGuiController(source, handlers, audit);
    return h(GuiControllerBanner, { lease: controller.lease, onStop: () => { void controller.stop(); } });
}
beforeEach(() => {
    (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
    source = { version: 'test', locked: false, blocked: false, view: 'files', connected: true,
        activeSessionId: 's1', sessions: [], panels: { local: { path: '/local', loading: false, selection: [], entriesCount: 0 } },
        queue: { active: 0, pending: 0, failed: 0 } };
    handlers = { showView: vi.fn(), navigate: vi.fn(async (_panel, path: string) => path), refresh: vi.fn(async () => {}),
        select: vi.fn(), disconnect: vi.fn(async () => {}), stop: vi.fn(async () => {}) };
    bridge.invoke.mockReset().mockImplementation(async name => name === 'gui_intent_claim' ? 2000 : undefined);
    host = document.createElement('div'); document.body.append(host); root = createRoot(host);
});
afterEach(async () => { await act(async () => root.unmount()); host.remove(); vi.restoreAllMocks(); audit.mockClear(); });
const mount = async () => { await act(async () => root.render(h(StrictMode, {}, h(Harness)))); };
async function until(predicate: () => boolean) {
    const deadline = Date.now() + 3000;
    while (!predicate()) {
        if (Date.now() >= deadline) throw new Error('hook test timeout');
        await act(async () => { await new Promise(resolve => setTimeout(resolve, 25)); });
    }
}

it('claims backend requests before executing and replies only with the structured result', async () => {
    await mount();
    await act(async () => bridge.callbacks.get('gui-intent')!({ payload: {
        id: 'backend-id', expires_at: Date.now() + 2000, request: { name: 'refresh', args: { panel: 'local' } },
    } }));
    await until(() => bridge.invoke.mock.calls.some(([name]) => name === 'gui_intent_result'));
    expect(bridge.invoke.mock.calls[0]).toEqual(['gui_intent_claim', { id: 'backend-id' }]);
    expect(handlers.refresh).toHaveBeenCalledExactlyOnceWith('local');
    const reply = bridge.invoke.mock.calls.find(([name]) => name === 'gui_intent_result')![1];
    expect(reply.id).toBe('backend-id'); expect(reply.payload).toMatchObject({ ok: true, error: null, snapshot: { schema_version: 1 } });
    expect(host.querySelector('[role="status"]')).not.toBeNull();
});

it('never acts when a request expired, its claim was refused, or a modal is open', async () => {
    await mount();
    const event = (id: string, expires_at: number) => ({ payload: { id, expires_at, request: { name: 'disconnect' } } });
    bridge.callbacks.get('gui-intent')!(event('expired', Date.now() - 1));
    expect(bridge.invoke).not.toHaveBeenCalled();
    bridge.invoke.mockRejectedValueOnce(new Error('gui_scope_changed'));
    await act(async () => bridge.callbacks.get('gui-intent')!(event('refused', Date.now() + 2000)));
    expect(handlers.disconnect).not.toHaveBeenCalled();
    const modal = document.createElement('div'); modal.setAttribute('aria-modal', 'true'); document.body.append(modal);
    let result;
    await act(async () => { result = await window.__aeroftpController!.run({ name: 'disconnect', pace: 'fast' }); });
    expect(result).toMatchObject({ ok: false, error: 'blocked' }); modal.remove();
});

it('cancels the active watch pause on broker cancellation and account changes', async () => {
    await mount();
    await act(async () => bridge.callbacks.get('gui-intent')!({ payload: {
        id: 'cancelled', expires_at: Date.now() + 2000, request: { name: 'disconnect' },
    } }));
    await act(async () => bridge.callbacks.get('gui-intent-cancel')!({ payload: { id: 'cancelled' } }));
    await until(() => bridge.invoke.mock.calls.some(([name]) => name === 'gui_intent_result'));
    expect(handlers.disconnect).not.toHaveBeenCalled();
    let pending!: ReturnType<NonNullable<typeof window.__aeroftpController>['run']>;
    await act(async () => { pending = window.__aeroftpController!.run({ name: 'disconnect' }); });
    await act(async () => window.dispatchEvent(new Event(PROFILES_CHANGED_EVENT)));
    expect((await pending).error).toBe('lease_interrupted');
});

it('preserves Stop through the trusted pointer capture and reaches the original Stop handler', async () => {
    const listen = vi.spyOn(window, 'addEventListener'); await mount();
    await act(async () => { await window.__aeroftpController!.run({ name: 'refresh', args: { panel: 'local' }, pace: 'fast' }); });
    const stop = host.querySelector('[data-gui-controller-stop]')!;
    const capture = listen.mock.calls.filter(([name]) => name === 'pointerdown').slice(-1)[0][1] as EventListener;
    await act(async () => capture({ isTrusted: true, target: stop } as unknown as Event));
    expect(host.querySelector('[role="status"]')).not.toBeNull();
    await act(async () => (stop as HTMLButtonElement).click());
    expect(handlers.stop).toHaveBeenCalledOnce(); expect(host.querySelector('[role="status"]')).toBeNull();
});

it('human trusted input wins; synthetic programmatic events cannot masquerade as a person', async () => {
    const listen = vi.spyOn(window, 'addEventListener'); await mount();
    const capture = listen.mock.calls.filter(([name]) => name === 'pointerdown').slice(-1)[0][1] as EventListener;
    await act(async () => { await window.__aeroftpController!.run({ name: 'refresh', args: { panel: 'local' }, pace: 'fast' }); });
    await act(async () => capture({ isTrusted: false, target: host } as unknown as Event));
    expect(host.querySelector('[role="status"]')).not.toBeNull();
    await act(async () => capture({ isTrusted: true, target: host } as unknown as Event));
    expect(host.querySelector('[role="status"]')).toBeNull();
});

it('uses the latest committed state and removes the dev global and listeners on unmount', async () => {
    await mount(); source = { ...source, locked: true };
    await act(async () => root.render(h(StrictMode, {}, h(Harness))));
    expect(window.__aeroftpController!.state().locked).toBe(true);
    await act(async () => root.unmount());
    expect(window.__aeroftpController).toBeUndefined(); expect(bridge.callbacks.size).toBe(0);
    // afterEach can safely unmount an already unmounted root.
});


it('records committed intermediate state even when a path changes back before the next request', async () => {
    await mount();
    const revision = window.__aeroftpController!.state().state_revision;
    const original = source.panels.local!.path;
    source = { ...source, panels: { ...source.panels, local: { ...source.panels.local!, path: '/intermediate' } } };
    await act(async () => root.render(h(StrictMode, {}, h(Harness))));
    source = { ...source, panels: { ...source.panels, local: { ...source.panels.local!, path: original } } };
    await act(async () => root.render(h(StrictMode, {}, h(Harness))));
    let result;
    await act(async () => { result = await window.__aeroftpController!.run({ name: 'disconnect', if_revision: revision, pace: 'fast' }); });
    expect(result).toMatchObject({ ok: false, error: 'stale_state' });
    expect(handlers.disconnect).not.toHaveBeenCalled();
});


it('accepts a satisfied minimum-timeout wait without rewriting it to invalid arguments', async () => {
    await mount();
    bridge.invoke.mockResolvedValueOnce(90);
    await act(async () => bridge.callbacks.get('gui-intent')!({ payload: {
        id: 'short-wait', expires_at: Date.now() + 100, request: { name: 'wait', args: { condition: 'connected' }, timeout_ms: 100 },
    } }));
    await until(() => bridge.invoke.mock.calls.some(([name]) => name === 'gui_intent_result'));
    expect(bridge.invoke.mock.calls.find(([name]) => name === 'gui_intent_result')![1].payload)
        .toMatchObject({ ok: true, error: null, snapshot: { connected: true } });
});

it('does not act after a delayed claim outlives the absolute broker expiry', async () => {
    await mount();
    let claim!: (remaining: number) => void;
    bridge.invoke.mockImplementationOnce(() => new Promise<number>(resolve => { claim = resolve; }));
    const expiresAt = Date.now() + 80;
    await act(async () => bridge.callbacks.get('gui-intent')!({ payload: {
        id: 'delayed-expired', expires_at: expiresAt, request: { name: 'disconnect', pace: 'fast' },
    } }));
    await until(() => Date.now() > expiresAt);
    await act(async () => claim(2000));
    await until(() => bridge.invoke.mock.calls.some(([name]) => name === 'gui_intent_result'));
    expect(handlers.disconnect).not.toHaveBeenCalled();
    const result = bridge.invoke.mock.calls.find(([name]) => name === 'gui_intent_result')![1];
    expect(result.payload).toMatchObject({ ok: false, error: 'gui_timeout' });
});
