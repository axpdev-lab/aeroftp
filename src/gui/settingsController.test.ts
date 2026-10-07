// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Controller-level integration of the safe Settings surface: the REAL
// createSettingsHandlers (the same factory App.tsx wires) run against a real
// GuiController, with invoke backed by an in-memory credential store that
// records every account written. The central assertion is not "no
// store_credential calls" but "the ONLY accounts written are the public
// config blobs, and their payloads carry no secret field".

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { GuiController, type GuiSource } from './controller';
import { createSettingsHandlers } from './settingsHandlers';
import { buildAiProjection, buildGeneralProjection } from './settingsSchema';
import { AI_SETTINGS_EVENT } from '../utils/aiSettingsStore';
import { updateAiSettingsBlob } from '../utils/aiSettingsStore';
import { getDefaultAISettings, type AISettings } from '../types/ai';

const store = vi.hoisted(() => ({
    credentials: new Map<string, string>(),
    writes: [] as { account: string; password: string }[],
    readGate: undefined as (() => Promise<void>) | undefined,
    readError: undefined as Error | undefined,
    user: 1,
}));
vi.mock('@tauri-apps/api/core', () => ({
    invoke: vi.fn(async (name: string, args?: Record<string, unknown>) => {
        const account = String(args?.account ?? '');
        if (name === 'user_partitions_unlock_status') return { activeUserId: store.user, unlockedUserId: store.user, isUnlocked: true };
        if (name === 'store_credential') {
            store.writes.push({ account, password: String(args?.password ?? '') });
            store.credentials.set(account, String(args?.password ?? ''));
            return undefined;
        }
        if (name === 'get_credential') {
            await store.readGate?.();
            if (store.readError) throw store.readError;
            const value = store.credentials.get(account);
            if (value === undefined) throw new Error(`Credential not found: ${account}`);
            return value;
        }
        if (name === 'delete_credential') {
            store.credentials.delete(account);
            return undefined;
        }
        return undefined;
    }),
}));

let source: GuiSource;
let controller: GuiController;
let removeListeners: (() => void) | undefined;

afterEach(() => {
    controller.dispose();
    removeListeners?.();
    removeListeners = undefined;
});

const AI_BLOB: AISettings = {
    ...getDefaultAISettings(),
    providers: [{
        id: 'p1', name: 'OpenAI', type: 'openai', baseUrl: 'https://api.openai.com/v1',
        isEnabled: true, isDefault: true, createdAt: new Date(0), updatedAt: new Date(0),
    }],
    models: [
        {
            id: 'm1', providerId: 'p1', name: 'gpt-fixture-a', displayName: 'A', maxTokens: 4096,
            supportsStreaming: true, supportsTools: true, supportsVision: false, isEnabled: true, isDefault: true,
        },
        {
            id: 'm2', providerId: 'p1', name: 'gpt-fixture-b', displayName: 'B', maxTokens: 4096,
            supportsStreaming: true, supportsTools: true, supportsVision: false, isEnabled: true, isDefault: false,
        },
    ],
    advancedSettings: { ...getDefaultAISettings().advancedSettings, temperature: 0.7 },
};

beforeEach(() => {
    store.credentials.clear();
    store.writes.length = 0;
    store.readGate = undefined;
    store.readError = undefined;
    store.user = 1;
    localStorage.clear();
    source = {
        version: 'test', locked: false, blocked: false, view: 'servers', connected: false,
        activeSessionId: null, sessions: [], panels: {}, queue: { active: 0, pending: 0, failed: 0 },
        settings: { open: null, general: {}, ai: null },
    };
    const handlers = {
        // Unused base intents fail loudly if the settings tests reach them.
        showView: () => { throw new Error('unexpected'); },
        navigate: async () => { throw new Error('unexpected'); },
        refresh: async () => { throw new Error('unexpected'); },
        select: () => { throw new Error('unexpected'); },
        disconnect: async () => { throw new Error('unexpected'); },
        stop: async () => { throw new Error('unexpected'); },
        ...createSettingsHandlers({
            openGeneral: () => { source.settings = { ...source.settings!, open: 'general' }; },
            openAi: () => { source.settings = { ...source.settings!, open: 'ai' }; },
            closeAll: () => { source.settings = { ...source.settings!, open: null }; },
            onAiProjection: projection => { source.settings = { ...source.settings!, ai: projection }; },
        }),
    };
    controller = new GuiController(() => source, () => handlers, () => { });
    // Mirror useSettings: the public general blob change event refreshes the
    // live allowlisted values the projection is built from.
    const onGeneralSettings = (event: Event) => {
        const detail = (event as CustomEvent).detail as Record<string, unknown> | undefined;
        if (detail) source.settings = { ...source.settings!, general: buildGeneralProjection(detail) };
    };
    const onAiSettings = (event: Event) => {
        // Mirror App: the persisted blob detail refreshes the AI projection.
        const detail = (event as CustomEvent).detail as AISettings | undefined;
        if (detail && Array.isArray(detail.providers)) {
            source.settings = { ...source.settings!, ai: buildAiProjection(detail) };
        }
    };
    window.addEventListener('aeroftp-settings-changed', onGeneralSettings);
    window.addEventListener(AI_SETTINGS_EVENT, onAiSettings);
    removeListeners = () => {
        window.removeEventListener('aeroftp-settings-changed', onGeneralSettings);
        window.removeEventListener(AI_SETTINGS_EVENT, onAiSettings);
    };
});

const run = (name: string, args?: Record<string, unknown>, extra?: Record<string, unknown>) =>
    controller.run({ name, args, pace: 'fast', ...extra });

const SECRET_ACCOUNTS = /^(?!config_(app|ai)_settings$)/;

describe('general settings through the real handlers', () => {
    it('omits added scalar and nested secret fields even from a polluted source projection', async () => {
        source.settings = {
            open: 'general', general: { fontSize: 16, password: 'PROJECTION-SECRET' },
            ai: { providers: [{ id: 'p', name: 'Fixture', type: 'openai', enabled: true, apiKey: 'PROJECTION-SECRET' } as never],
                models: [], default_model_id: null, advanced: { temperature: 0.7, password: 'PROJECTION-SECRET' } },
            secret: 'PROJECTION-SECRET',
        } as never;
        const snapshot = (await run('state')).snapshot;
        expect(JSON.stringify(snapshot)).not.toContain('PROJECTION-SECRET');
        expect(snapshot.settings?.general).toEqual({ fontSize: 16 });
    });

    it('does not write unreadable stored preferences or use an old fallback', async () => {
        store.readError = new Error('STORE_NOT_READY');
        localStorage.setItem('aeroftp_settings', JSON.stringify({ fontSize: 14 }));
        expect(await run('settings_update', { area: 'general', set: { fontSize: 18 } }))
            .toMatchObject({ ok: false, error: 'action_failed' });
        expect(store.writes).toEqual([]);
    });

    it('Stop during a deferred config read prevents dispatch and retains the mutation lane until settlement', async () => {
        let release!: () => void;
        let reading!: () => void;
        const entered = new Promise<void>(resolve => { reading = resolve; });
        const gate = new Promise<void>(resolve => { release = resolve; });
        store.readGate = async () => { reading(); await gate; };
        store.credentials.set('config_app_settings', JSON.stringify({ fontSize: 14 }));
        const pending = run('settings_update', { area: 'general', set: { fontSize: 18 } });
        await entered;
        controller.interrupt();
        expect(await pending).toMatchObject({ ok: false, error: 'lease_interrupted' });
        expect(await run('settings_open', { area: 'general' })).toMatchObject({ ok: false, error: 'busy' });
        release();
        await new Promise(resolve => setTimeout(resolve, 30));
        expect(store.writes).toEqual([]);
        expect(JSON.parse(store.credentials.get('config_app_settings')!).fontSize).toBe(14);
    });
    it('opens, reads, updates and closes with only the public config blob written', async () => {
        store.credentials.set('config_app_settings', JSON.stringify({ showHiddenFiles: true, timeoutSeconds: 30, confirmBeforeDelete: true }));
        source.settings!.general = buildGeneralProjection({ showHiddenFiles: true });

        expect((await run('settings_open', { area: 'general' }))).toMatchObject({ ok: true, snapshot: { settings: { open: 'general' } } });
        expect((await run('settings_read', { area: 'general' }))).toMatchObject({ ok: true, snapshot: { settings: { general: { showHiddenFiles: true } } } });

        const reply = await run('settings_update', { area: 'general', set: { showHiddenFiles: false, dateFormat: 'iso' } });
        expect(reply).toMatchObject({ ok: true, error: null });
        // Only the public blob account was written; no profile/OAuth/key record.
        expect(store.writes.map(w => w.account)).toEqual(['config_app_settings']);
        const blob = JSON.parse(store.credentials.get('config_app_settings')!);
        expect(blob).toMatchObject({ showHiddenFiles: false, dateFormat: 'iso', timeoutSeconds: 30, confirmBeforeDelete: true });
        expect((await run('settings_close'))).toMatchObject({ ok: true, snapshot: { settings: { open: null } } });
    });

    it('refuses unknown or excluded keys before any write happens', async () => {
        for (const set of [{ confirmBeforeDelete: false }, { apiKey: 'x' }, { fontSize: 99 }]) {
            expect((await run('settings_update', { area: 'general', set }))).toMatchObject({ ok: false, error: 'invalid_args' });
        }
        expect(store.writes).toEqual([]);
    });

    it('keeps other mutations blocked while the owned Settings surface is open', async () => {
        expect((await run('settings_open', { area: 'general' }))).toMatchObject({ ok: true });
        expect((await run('navigate', { panel: 'local', path: '/tmp' }))).toMatchObject({ ok: false, error: 'blocked' });
        expect((await run('disconnect'))).toMatchObject({ ok: false, error: 'blocked' });
    });

    it('refuses the whole surface while the app is locked and redacts the projection', async () => {
        source.locked = true;
        expect((await run('settings_update', { area: 'general', set: { showHiddenFiles: true } }))).toMatchObject({ ok: false, error: 'locked' });
        const state = await run('state');
        expect(state.snapshot.settings).toBeUndefined();
        expect(store.writes).toEqual([]);
    });

    it('rejects a stale revision before dispatching the mutation', async () => {
        const revision = (await run('state')).snapshot.state_revision;
        source.view = 'files'; // any intervening change bumps the revision
        expect((await run('settings_update', { area: 'general', set: { showHiddenFiles: true } }, { if_revision: revision })))
            .toMatchObject({ ok: false, error: 'stale_state' });
        expect(store.writes).toEqual([]);
    });
});

describe('ai settings through the real handlers', () => {
    const seedAiBlob = () => store.credentials.set('config_ai_settings', JSON.stringify(AI_BLOB));

    it.each(['stop', 'account', 'timeout'])('prevents an AI write after %s during a deferred config read', async reason => {
        seedAiBlob();
        let release!: () => void;
        let reading!: () => void;
        const entered = new Promise<void>(resolve => { reading = resolve; });
        const gate = new Promise<void>(resolve => { release = resolve; });
        store.readGate = async () => { reading(); await gate; };
        const pending = run('settings_update', { area: 'ai', set: { advanced: { temperature: 0.3 } } },
            reason === 'timeout' ? { timeout_ms: 100 } : undefined);
        await entered;
        if (reason === 'stop') controller.interrupt();
        if (reason === 'account') store.user = 2;
        if (reason === 'timeout') {
            expect(await pending).toMatchObject({ ok: false, error: 'gui_timeout' });
        }
        release();
        if (reason !== 'timeout') expect(await pending).toMatchObject({ ok: false, error: 'lease_interrupted' });
        await new Promise(resolve => setTimeout(resolve, 30));
        expect(store.writes).toEqual([]);
        expect(localStorage.getItem('aeroftp_ai_settings')).toBeNull();
    });

    it('a queued AI update rechecks Stop after an earlier human save settles', async () => {
        seedAiBlob();
        let release!: () => void;
        let reading!: () => void;
        const entered = new Promise<void>(resolve => { reading = resolve; });
        const gate = new Promise<void>(resolve => { release = resolve; });
        store.readGate = async () => { reading(); await gate; };
        const human = updateAiSettingsBlob(existing => existing!);
        await entered;
        const pending = run('settings_update', { area: 'ai', set: { advanced: { temperature: 0.3 } } });
        await new Promise(resolve => setTimeout(resolve, 30));
        controller.interrupt();
        expect(await pending).toMatchObject({ ok: false, error: 'lease_interrupted' });
        release();
        await human;
        await new Promise(resolve => setTimeout(resolve, 30));
        expect(store.writes).toHaveLength(1);
        expect(JSON.parse(store.writes[0].password).advancedSettings.temperature).toBe(0.7);
    });

    it('updates provider/model identity and bounded parameters with zero secret writes', async () => {
        seedAiBlob();
        // A pre-existing key in the keyring must survive untouched.
        store.credentials.set('ai_apikey_p1', 'STORED-SECRET');

        expect((await run('settings_open', { area: 'ai' }))).toMatchObject({ ok: true });
        expect((await run('settings_read', { area: 'ai' }))).toMatchObject({ ok: true, snapshot: { settings: { ai: { providers: [{ id: 'p1', enabled: true }] } } } });

        const reply = await run('settings_update', { area: 'ai', set: {
            provider_enabled: { id: 'p1', enabled: false },
            model_default: { id: 'm2' },
            advanced: { temperature: 0.2, response_style: 'concise' },
        } });
        expect(reply).toMatchObject({ ok: true, error: null });
        expect(reply.snapshot.settings?.ai?.providers[0]?.enabled).toBe(false);
        expect(reply.snapshot.settings?.ai?.models.find(m => m.id === 'm2')?.is_default).toBe(true);
        expect(reply.snapshot.settings?.ai?.models.find(m => m.id === 'm1')?.is_default).toBe(false);
        expect(reply.snapshot.settings?.ai?.advanced).toMatchObject({ temperature: 0.2, response_style: 'concise' });

        // Exactly one account written: the public blob. No apiKey field or
        // value anywhere in the payload, and the stored key is untouched.
        expect(store.writes.map(w => w.account)).toEqual(['config_ai_settings']);
        expect(store.writes[0].password).not.toContain('apiKey');
        expect(store.writes[0].password).not.toContain('STORED-SECRET');
        expect(store.credentials.get('ai_apikey_p1')).toBe('STORED-SECRET');
    });

    it('refuses unknown ids, secret fields and a missing blob without writing', async () => {
        seedAiBlob();
        expect((await run('settings_update', { area: 'ai', set: { provider_enabled: { id: 'nope', enabled: true } } })))
            .toMatchObject({ ok: false, error: 'invalid_args' });
        expect((await run('settings_update', { area: 'ai', set: { apiKey: 'x' } as never })))
            .toMatchObject({ ok: false, error: 'invalid_args' });
        expect(store.writes).toEqual([]);

        store.credentials.clear();
        expect((await run('settings_update', { area: 'ai', set: { provider_enabled: { id: 'p1', enabled: true } } })))
            .toMatchObject({ ok: false, error: 'action_failed' });
        expect(store.writes).toEqual([]);
    });

    it('read reflects external blob changes and stays off the write lane', async () => {
        seedAiBlob();
        expect((await run('settings_read', { area: 'ai' }))).toMatchObject({ ok: true, snapshot: { settings: { ai: { advanced: { temperature: 0.7 } } } } });
        const changed = { ...AI_BLOB, advancedSettings: { ...AI_BLOB.advancedSettings, temperature: 1.4 } };
        store.credentials.set('config_ai_settings', JSON.stringify(changed));
        expect((await run('settings_read', { area: 'ai' }))).toMatchObject({ ok: true, snapshot: { settings: { ai: { advanced: { temperature: 1.4 } } } } });
        expect(store.writes).toEqual([]);
        expect(SECRET_ACCOUNTS.test('config_ai_settings')).toBe(false);
    });

    it('never includes the settings projection in a locked snapshot', async () => {
        seedAiBlob();
        source.locked = true;
        const reply = await run('settings_read', { area: 'ai' });
        expect(reply).toMatchObject({ ok: false, error: 'locked' });
        expect(reply.snapshot.settings).toBeUndefined();
    });
});
