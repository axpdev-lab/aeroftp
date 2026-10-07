// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { GuiController, GuiError, buildGuiSnapshot, validateGuiRequest, type GuiSource, type GuiHandlers, type GuiRequest, type GuiPanel, type GuiLease, type GuiIntent } from './controller';

let source: GuiSource;
let handlers: GuiHandlers;
let controller: GuiController;
let changed: ReturnType<typeof vi.fn<(lease: GuiLease | null) => void>>;
let audit: ReturnType<typeof vi.fn<(intent: GuiIntent, ok: boolean, owner: string) => void>>;
beforeEach(() => {
    source = { version: 'test', locked: false, blocked: false, view: 'files', connected: true,
        activeSessionId: 's1', sessions: [{ id: 's1', name: 'Test', protocol: 'sftp', status: 'connected' }],
        panels: { local: { path: '/local', loading: false, selection: new Set(['file']), entriesCount: 1 },
            remote: { path: '/remote', loading: false, selection: new Set(), entriesCount: 1 } },
        queue: { active: 0, pending: 0, failed: 0 } };
    handlers = { showView: vi.fn(view => { source.view = view; }),
        navigate: vi.fn(async (panel: GuiPanel, path: string) => { source.panels[panel]!.path = path; return path; }),
        refresh: vi.fn(async () => {}), select: vi.fn(),
        disconnect: vi.fn(async () => { source.connected = false; }), stop: vi.fn(async () => {}) };
    changed = vi.fn(); audit = vi.fn(); controller = new GuiController(() => source, () => handlers, changed, audit);
});
afterEach(() => controller.dispose());

it('waits for same-path navigation and refresh to commit an idle panel', async () => {
    for (const name of ['navigate', 'refresh'] as const) {
        const handler = async () => { source.panels.local!.loading = true; return '/local'; };
        if (name === 'navigate') handlers.navigate = handler;
        else handlers.refresh = async () => { await handler(); };
        let settled = false;
        const request = controller.run({ name, args: name === 'navigate' ? { panel: 'local', path: '/local' } : { panel: 'local' }, pace: 'fast' })
            .finally(() => { settled = true; });
        await new Promise(resolve => setTimeout(resolve, 60));
        expect(settled).toBe(false);
        source.panels.local!.loading = false;
        expect(await request).toMatchObject({ ok: true, snapshot: { panels: { local: { loading: false } } } });
    }
});
const run = (name: string, args = {}, extra: Partial<GuiRequest> = {}) => controller.run({ name, args, pace: 'fast', ...extra });

it('projects snapshots without spreading secrets or unknown future fields', () => {
    const dirty = { ...source, password: 'SENTINEL_ROOT', unknown: { apiKey: 'SENTINEL_FUTURE' },
        sessions: source.sessions.map(s => ({ ...s, connectionParams: { password: 'SENTINEL_SESSION' } })),
        panels: { local: { ...source.panels.local!, token: 'SENTINEL_PANEL' } } };
    const serialized = JSON.stringify(buildGuiSnapshot(dirty));
    expect(serialized).not.toContain('SENTINEL');
    expect(JSON.parse(serialized).panels.local.selection).toEqual(['file']);
});

it('removes all session/path/queue data while the app or account is locked', () => {
    source.locked = true;
    const s = controller.state();
    expect(s.connected).toBe(false); expect(s.active_session_id).toBeNull();
    expect(s.sessions).toEqual([]); expect(s.panels).toEqual({}); expect(s.queue).toEqual({ active: 0, pending: 0, failed: 0 });
});

it('caps session and selection output but preserves the real selection count', () => {
    source.sessions = Array.from({ length: 100 }, (_, i) => ({ ...source.sessions[0], id: String(i) }));
    source.panels.local!.selection = Array.from({ length: 200 }, (_, i) => String(i));
    const s = controller.state(); expect(s.sessions).toHaveLength(32);
    expect(s.panels.local?.selection).toHaveLength(100); expect(s.panels.local?.selection_count).toBe(200);
});

it('uses a separate state revision and pins the schema version', () => {
    const first = controller.state(); expect(controller.state().state_revision).toBe(first.state_revision);
    source.panels.local!.path = '/changed'; expect(controller.state().state_revision).toBeGreaterThan(first.state_revision);
    expect(controller.state().schema_version).toBe(1);
});

it('rejects reveal, unlock, script, settings and file-write requests structurally', async () => {
    for (const name of ['reveal_password', 'unlock', 'eval', 'click', 'delete', 'rename', 'mkdir', 'transfer', '__proto__']) {
        expect((await run(name)).error).toBe('unsupported_intent');
    }
    expect((await run('show_view', { view: 'settings-security' })).error).toBe('invalid_args');
    expect(Object.values(handlers).every(h => vi.mocked(h).mock.calls.length === 0)).toBe(true);
});

it('checks exact argument types and refuses hidden extra fields', () => {
    for (const request of [
        { name: 'state', args: { password: 'x' } }, { name: 'state', args: null },
        { name: 'state', timeout_ms: '1000' }, { name: 'state', if_revision: -1 },
        { name: 'navigate', args: { panel: 'remote', path: 'bad\0path' } },
        { name: 'select', args: { panel: 'remote', names: ['../secret'] } },
        { name: 'select', args: { panel: 'remote', names: ['file'], mode: 'all' } },
        { name: 'state', script: 'evil' },
        { name: 'wait', args: { condition: ['connected'] } },
        { name: 'refresh', args: { panel: ['local'] } },
        { name: 'select', args: { panel: 'local', names: ['file'], mode: ['names'] } },
    ]) expect(() => validateGuiRequest(request as GuiRequest)).toThrow('invalid_args');
});

it('calls the original navigation, refresh, selection, view and disconnect handlers', async () => {
    expect((await run('navigate', { panel: 'local', path: '/new' })).ok).toBe(true);
    expect(handlers.navigate).toHaveBeenCalledExactlyOnceWith('local', '/new');
    expect((await run('refresh', { panel: 'remote' })).ok).toBe(true); expect(handlers.refresh).toHaveBeenCalledWith('remote');
    expect((await run('select', { panel: 'local', names: ['file'] })).ok).toBe(true);
    expect(handlers.select).toHaveBeenCalledWith('local', ['file'], 'names');
    await run('show_view', { view: 'servers' }); expect(handlers.showView).toHaveBeenCalledWith('servers');
    await run('disconnect'); expect(handlers.disconnect).toHaveBeenCalledOnce();
});

it('refuses stale revisions, locked state, dialogs, unseen panels and remote disconnection', async () => {
    expect((await run('disconnect', {}, { if_revision: 0 })).error).toBe('stale_state');
    source.locked = true; expect((await run('disconnect')).error).toBe('locked');
    source.locked = false; source.blocked = true; expect((await run('disconnect')).error).toBe('blocked');
    source.blocked = false; expect((await run('refresh', { panel: 'local2' })).error).toBe('blocked');
    source.view = 'servers'; expect((await run('refresh', { panel: 'local' })).error).toBe('blocked');
    source.view = 'files'; source.connected = false; expect((await run('refresh', { panel: 'remote' })).error).toBe('not_connected');
});

it('does not call a handler after a human interrupts the watch pause', async () => {
    const result = controller.run({ name: 'disconnect', pace: 'watch' });
    controller.interrupt();
    expect((await result).error).toBe('lease_interrupted'); expect(handlers.disconnect).not.toHaveBeenCalled();
});

it('revalidates UI state after the visible watch pause', async () => {
    const result = controller.run({ name: 'disconnect', pace: 'watch' });
    source.activeSessionId = 'replacement';
    expect((await result).error).toBe('stale_state'); expect(handlers.disconnect).not.toHaveBeenCalled();
});

it('bounds waits and returns only a satisfied condition', async () => {
    source.connected = false;
    expect((await run('wait', { condition: 'connected' }, { timeout_ms: 100 })).error).toBe('gui_timeout');
    const result = run('wait', { condition: 'connected' }); source.connected = true;
    expect((await result).snapshot.connected).toBe(true);
    source.locked = true; const unlocked = run('wait', { condition: 'unlocked' }); source.locked = false;
    expect((await unlocked).ok).toBe(true);
});

it('keeps the mutation lane owned until a timed-out underlying handler settles', async () => {
    let resolve!: () => void;
    handlers.refresh = vi.fn(() => new Promise<void>(r => { resolve = r; }));
    const result = run('refresh', { panel: 'local' }, { timeout_ms: 100 });
    expect((await result).error).toBe('gui_timeout');
    expect((await run('disconnect')).error).toBe('busy');
    resolve(); await Promise.resolve(); await Promise.resolve(); await Promise.resolve();
    expect((await run('disconnect')).ok).toBe(true);
});

it('does not silently call successful navigation when a swallowed failure kept the same path', async () => {
    handlers.navigate = vi.fn(async () => undefined as unknown as string);
    expect((await run('navigate', { panel: 'remote', path: '/different' })).error).toBe('action_failed');
});

it('Stop is available while locked or a mutation is in flight and uses the real Stop handler', async () => {
    let resolve!: () => void;
    handlers.refresh = vi.fn(() => new Promise<void>(r => { resolve = r; }));
    const active = run('refresh', { panel: 'local' });
    await new Promise(r => setTimeout(r, 10)); source.locked = true;
    expect((await run('stop')).ok).toBe(true); expect(handlers.stop).toHaveBeenCalledOnce();
    expect((await active).error).toBe('lease_interrupted'); resolve();
});

it('audits observations, waits, actions and failures without raw exception text', async () => {
    await run('state'); await run('wait', { condition: 'idle' });
    handlers.disconnect = vi.fn(async () => { throw new Error('SENTINEL_PASSWORD'); });
    const reply = await run('disconnect'); expect(reply.error).toBe('action_failed');
    expect(JSON.stringify(reply)).not.toContain('SENTINEL');
    expect(audit.mock.calls).toEqual([['state', true, 'AeroAgent'], ['wait', true, 'AeroAgent'], ['disconnect', false, 'AeroAgent']]);
});


it('bounds Stop even when its original handler does not settle', async () => {
    handlers.stop = vi.fn(() => new Promise<void>(() => {}));
    expect((await run('stop', {}, { timeout_ms: 100 })).error).toBe('gui_timeout');
    expect(handlers.stop).toHaveBeenCalledOnce();
});


it('honors a broker deadline shorter than the validated request timeout', async () => {
    expect((await controller.run({ name: 'disconnect', timeout_ms: 10000 }, 'AeroAgent', Date.now() + 40)).error).toBe('gui_timeout');
    expect(handlers.disconnect).not.toHaveBeenCalled();
});


it('waits for committed UI state after a setter callback has resolved', async () => {
    handlers.showView = vi.fn(() => { setTimeout(() => { source.view = 'servers'; }, 60); });
    const r = await run('show_view', { view: 'servers' });
    expect(r.ok).toBe(true); expect(r.snapshot.view).toBe('servers');
    source.view = 'files';
    handlers.select = vi.fn(() => { setTimeout(() => { source.panels.local!.selection = new Set(); }, 60); });
    const selected = await run('select', { panel: 'local', mode: 'none' });
    expect(selected.ok).toBe(true); expect(selected.snapshot.panels.local?.selection_count).toBe(0);
});


it('Stop remains successful when human input interrupts during its handler', async () => {
    let resolve!: () => void;
    handlers.stop = vi.fn(() => new Promise<void>(r => { resolve = r; }));
    const stop = run('stop'); controller.interrupt(); resolve();
    expect((await stop).ok).toBe(true);
});

it('uses the canonical navigation result and waits for its committed path', async () => {
    handlers.navigate = vi.fn(async () => { setTimeout(() => { source.panels.local!.path = '/canonical'; }, 60); return '/canonical'; });
    const r = await run('navigate', { panel: 'local', path: '/local/../canonical' });
    expect(r.ok).toBe(true); expect(r.snapshot.panels.local?.path).toBe('/canonical');
    handlers.navigate = vi.fn(async () => { throw new GuiError('action_failed'); });
    expect((await run('navigate', { panel: 'local', path: '/canonical' })).error).toBe('action_failed');
});

it('connect accepts only an exact bounded profile ID and never inline credentials', async () => {
    for (const args of [{}, { profile_id: 'one', password: 'PRIVATE' }, { profile_id: 'one', host: 'host' },
        { profile_id: '../one' }, { profile_id: 'one two' }, { profile_id: 'x'.repeat(257) }, { profile_id: ['one'] }]) {
        expect((await run('connect', args)).error).toBe('invalid_args');
    }
});
it('connect waits for the requested active profile and a committed idle remote panel', async () => {
    source.sessions = [{ ...source.sessions[0], savedProfileId: 'other' }];
    handlers.connect = vi.fn(async id => {
        source.panels.remote!.loading = true;
        setTimeout(() => {
            source.sessions = [{ ...source.sessions[0], savedProfileId: id }];
            source.panels.remote!.loading = false;
        }, 70);
        return 'connected' as const;
    });
    let settled = false; const pending = run('connect', { profile_id: 'requested' }).finally(() => { settled = true; });
    await new Promise(resolve => setTimeout(resolve, 35)); expect(settled).toBe(false);
    expect(await pending).toMatchObject({ ok: true, snapshot: { sessions: [{ saved_profile_id: 'requested' }] } });
});
it('another already connected profile cannot satisfy a failed or pending-human connect', async () => {
    source.sessions = [{ ...source.sessions[0], savedProfileId: 'other' }];
    handlers.connect = vi.fn(async () => 'failed' as const);
    expect((await run('connect', { profile_id: 'requested' })).error).toBe('action_failed');
    handlers.connect = vi.fn(async () => 'pending_human' as const);
    expect((await run('connect', { profile_id: 'requested' })).error).toBe('pending_human');
    expect((await run('state')).snapshot.sessions[0].saved_profile_id).toBe('other');
});
it('keeps an active tab beyond the snapshot cap and requires an actual remote panel', async () => {
    source.sessions = Array.from({ length: 33 }, (_, i) => ({ id: `s${i}`, name: 'Tab', protocol: 'ftp', status: 'connected', savedProfileId: `p${i}` }));
    source.activeSessionId = 's32';
    handlers.connect = vi.fn(async () => 'connected' as const);
    const reply = await run('connect', { profile_id: 'p32' });
    expect(reply.ok).toBe(true); expect(reply.snapshot.sessions).toHaveLength(32);
    expect(reply.snapshot.sessions.some(s => s.id === 's32')).toBe(true);
    delete source.panels.remote;
    expect((await run('connect', { profile_id: 'p32' }, { timeout_ms: 100 })).error).toBe('gui_timeout');
});
it('does not report success when a locked overlay blocks an otherwise connected idle session', async () => {
    handlers.connect = async id => {
        source.sessions = [{ ...source.sessions[0], savedProfileId: id }];
        source.blocked = true; return 'connected';
    };
    expect((await run('connect', { profile_id: 'requested' })).error).toBe('pending_human');
    expect(changed).toHaveBeenLastCalledWith(null);
});
it('yields to a human dialog without letting its late answer resume the agent connect', async () => {
    let answer!: () => void; const dispatch = vi.fn();
    handlers.connect = async (_id, scope) => {
        await scope.step(() => new Promise<void>(resolve => {
            answer = resolve; source.blocked = true;
            scope.onCancel(() => resolve());
        }));
        dispatch(); return 'connected';
    };
    expect((await run('connect', { profile_id: 'requested' })).error).toBe('pending_human');
    expect(source.blocked).toBe(true); answer();
    await new Promise(resolve => setTimeout(resolve, 35));
    expect(dispatch).not.toHaveBeenCalled(); source.blocked = false;
    expect((await run('refresh', { panel: 'local' })).ok).toBe(true);
});
it('Stop before a delayed credential result prevents dispatch and keeps the lane until it settles', async () => {
    let release!: () => void; const dispatch = vi.fn(); let began = false;
    handlers.connect = async (_id, scope) => {
        await scope.step(() => { began = true; return new Promise<void>(resolve => { release = resolve; }); });
        await scope.step(dispatch); return 'connected' as const;
    };
    const connect = run('connect', { profile_id: 'one' });
    while (!began) await new Promise(resolve => setTimeout(resolve, 5));
    expect((await run('stop')).ok).toBe(true); expect((await connect).error).toBe('lease_interrupted');
    expect((await run('refresh', { panel: 'local' })).error).toBe('busy');
    release(); await new Promise(resolve => setTimeout(resolve, 35)); expect(dispatch).not.toHaveBeenCalled();
    expect((await run('refresh', { panel: 'local' })).ok).toBe(true);
});
it('expiry cancels the bound connect and never reveals backend errors or secret sentinels', async () => {
    let release!: () => void; let began = false; const cancel = vi.fn();
    handlers.connect = async (_id, scope) => {
        await scope.cancellable(cancel, () => { began = true; return new Promise<void>(resolve => { release = resolve; }); });
        throw new Error('PRIVATE_CREDENTIAL_SENTINEL');
    };
    const pending = run('connect', { profile_id: 'one' }, { timeout_ms: 100 });
    while (!began) await new Promise(resolve => setTimeout(resolve, 5));
    const reply = await pending; expect(reply.error).toBe('gui_timeout'); expect(cancel).toHaveBeenCalledOnce();
    expect(JSON.stringify(reply)).not.toContain('PRIVATE');
    release(); await new Promise(resolve => setTimeout(resolve, 35));
});
