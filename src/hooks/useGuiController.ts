// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { GuiController, GuiError, type GuiActor, type GuiHandlers, type GuiIntent, type GuiLease, type GuiRequest, type GuiSource } from '../gui/controller';
import { createTauriListener, useTauriListener } from './useTauriListener';
import { PROFILES_CHANGED_EVENT } from '../utils/serverProfileStore';
import { TID } from '../utils/testIds';

declare global {
    interface Window { __aeroftpController?: Pick<GuiController, 'run' | 'state' | 'interrupt'>; }
}
interface IntentEvent { id: string; expires_at: number; request: GuiRequest; }
interface IntentClaim { remaining_ms: number; actor: GuiActor; }
export function useGuiController(source: GuiSource, handlers: GuiHandlers, audit: (intent: GuiIntent, ok: boolean, owner: string) => void) {
    const current = useRef({ source, handlers, audit });
    const controller = useRef<GuiController | null>(null);
    const mutationId = useRef<string | null>(null);
    const ownedSettingsArea = useRef<string | null>(null);
    const requestedSettingsArea = useRef<string | null>(null);
    const commitSequence = useRef(0);
    const commitWaiters = useRef(new Map<number, () => void>());
    const [commitTicket, setCommitTicket] = useState(0);
    const [lease, setLease] = useState<GuiLease | null>(null);
    // Event callbacks and asynchronous waits must read the latest committed UI state.
    useEffect(() => { current.current = { source, handlers, audit }; controller.current?.state(); });
    // A same-path refresh can settle before React commits its setters. A fresh
    // render receipt makes the source above authoritative even when its path,
    // count and loading fields happen to equal the previous snapshot.
    useEffect(() => {
        for (const [ticket, resolve] of commitWaiters.current) {
            if (ticket <= commitTicket) { commitWaiters.current.delete(ticket); resolve(); }
        }
    }, [commitTicket]);
    useEffect(() => {
        let mounted = true;
        const committed = () => new Promise<void>(resolve => {
            if (!mounted) { resolve(); return; }
            const ticket = ++commitSequence.current;
            commitWaiters.current.set(ticket, resolve);
            setCommitTicket(ticket);
        });
        const service = new GuiController(() => ({ ...current.current.source,
            // The owned Settings dialog is a first-class controller surface, so
            // it does not self-block; any OTHER modal still interrupts control.
            blocked: current.current.source.blocked || Array.from(document.querySelectorAll('[aria-modal="true"]')).some(modal =>
                modal.getAttribute('data-gui-owned') !== 'settings' || modal.getAttribute('data-gui-safe') !== 'true' ||
                ![ownedSettingsArea.current, requestedSettingsArea.current].some(area => area && modal.getAttribute('data-gui-area') === area)) }), () => ({ ...current.current.handlers,
                stop: async () => { ownedSettingsArea.current = null; requestedSettingsArea.current = null; await current.current.handlers.stop(); },
                connect: async (id, scope) => {
                    if (!current.current.handlers.connect) throw new GuiError('blocked');
                    const result = await current.current.handlers.connect(id, scope);
                    await committed(); return result;
                },
                navigate: async (panel, path) => {
                    const result = await current.current.handlers.navigate(panel, path);
                    await committed(); return result;
                },
                refresh: async panel => { await current.current.handlers.refresh(panel); await committed(); },
                settingsOpen: async (area, scope) => {
                    if (!current.current.handlers.settingsOpen) throw new GuiError('blocked');
                    requestedSettingsArea.current = area;
                    try { await current.current.handlers.settingsOpen(area, scope); await committed(); ownedSettingsArea.current = area; }
                    catch (error) { ownedSettingsArea.current = null; throw error; }
                    finally { requestedSettingsArea.current = null; }
                },
                settingsClose: async scope => {
                    if (!current.current.handlers.settingsClose) throw new GuiError('blocked');
                    await current.current.handlers.settingsClose(scope); await committed();
                    ownedSettingsArea.current = null;
                },
                settingsRead: async (area, scope) => {
                    if (!current.current.handlers.settingsRead) throw new GuiError('blocked');
                    await current.current.handlers.settingsRead(area, scope); await committed();
                },
                settingsUpdate: async (area, set, scope) => {
                    if (!current.current.handlers.settingsUpdate) throw new GuiError('blocked');
                    await current.current.handlers.settingsUpdate(area, set, scope); await committed();
                },
            }),
            value => { if (mounted) setLease(value); }, (intent, ok, owner) => current.current.audit(intent, ok, owner));
        controller.current = service;
        const interrupt = (event: Event) => {
            if (event.isTrusted && !(event.target instanceof Element && event.target.closest('[data-gui-controller-stop]'))) {
                ownedSettingsArea.current = null; requestedSettingsArea.current = null; service.interrupt();
            }
        };
        const partitionChanged = (event: Event) => {
            // The connect flow writes timestamps and failure markers itself.
            // Account/profile edits still emit the ordinary interrupting event.
            if (service.connecting && (event as CustomEvent).detail?.connectionMetadata === true) return;
            ownedSettingsArea.current = null;
            requestedSettingsArea.current = null;
            service.interrupt();
        };
        window.addEventListener('pointerdown', interrupt, true);
        window.addEventListener('keydown', interrupt, true);
        window.addEventListener(PROFILES_CHANGED_EVENT, partitionChanged);
        // This symbol and its API are removed by Vite's production branch elimination.
        let finishListener = () => {};
        if (import.meta.env.DEV) {
            finishListener = createTauriListener<{ actor_id: string }>('gui-actor-ended', event => service.releaseActor(event.payload.actor_id));
            const actor: GuiActor = { id: `dev:${crypto.randomUUID()}`, kind: 'dev', label: 'Dev harness' };
            window.__aeroftpController = {
                run: (request, owner = actor) => service.run(request, owner), state: () => service.state(), interrupt: () => service.interrupt(),
            };
        }
        return () => {
            finishListener(); mounted = false; service.dispose(); controller.current = null;
            for (const resolve of commitWaiters.current.values()) resolve();
            commitWaiters.current.clear();
            window.removeEventListener('pointerdown', interrupt, true);
            window.removeEventListener('keydown', interrupt, true);
            window.removeEventListener(PROFILES_CHANGED_EVENT, partitionChanged);
            if (import.meta.env.DEV) delete window.__aeroftpController;
        };
    }, []);
    useEffect(() => { if (source.locked) { ownedSettingsArea.current = null; requestedSettingsArea.current = null; controller.current?.interrupt(); } }, [source.locked]);
    useEffect(() => {
        if (!lease?.panel || !lease.intent) return;
        const target = Array.from(document.querySelectorAll<HTMLElement>(`[data-testid="${TID.panel}"]`))
            .find(element => element.dataset.panel === lease.panel);
        target?.classList.add('ring-2', 'ring-amber-400', 'ring-inset');
        return () => target?.classList.remove('ring-2', 'ring-amber-400', 'ring-inset');
    }, [lease]);
    useTauriListener<IntentEvent>('gui-intent', event => {
        const { id, expires_at, request } = event.payload;
        const service = controller.current;
        if (!service || typeof id !== 'string' || !Number.isFinite(expires_at) || Date.now() >= expires_at) return;
        void (async () => {
            try {
                const claim = await invoke<IntentClaim>('gui_intent_claim', { id });
                const remaining = claim.remaining_ms;
                if (!Number.isFinite(remaining) || remaining <= 25 || controller.current !== service) return;
                const mutating = !['state', 'wait'].includes(request.name);
                // Busy requests must not overwrite the identity of an in-flight mutation.
                const ownsId = mutating && mutationId.current === null;
                if (ownsId) mutationId.current = id;
                try {
                    const reply = await service.run(request, claim.actor, Math.min(Date.now() + remaining - 25, expires_at - 25), id);
                    await invoke('gui_intent_result', { id, payload: reply });
                } finally { if (ownsId && mutationId.current === id) mutationId.current = null; }
            } catch { /* Broker expiry/window/account refusal has no raw error to expose. */ }
        })();
    });
    useTauriListener<{ id: string }>('gui-intent-cancel', event => {
        if (mutationId.current === event.payload.id) controller.current?.interrupt();
    });
    return { lease,
        interrupt: () => { ownedSettingsArea.current = null; requestedSettingsArea.current = null; controller.current?.interrupt(); },
        stop: () => controller.current?.run({ name: 'stop' }) };
}
