// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { GuiController, validateGuiRequest, type GuiHandlers, type GuiSource } from './controller';
import { normalizeGuiPresentation } from './presentation';

let source: GuiSource;
let handlers: GuiHandlers;
let c: GuiController;
let preferences = normalizeGuiPresentation(undefined);
const tick = (ms = 1000) => vi.advanceTimersByTimeAsync(ms);
const show = (extra = {}) => c.run({ name: 'show_view', args: { view: 'servers' }, ...extra });
beforeEach(() => {
    vi.useFakeTimers(); preferences = normalizeGuiPresentation(undefined);
    source = { version: 'test', locked: false, blocked: false, view: 'files', connected: false,
        activeSessionId: null, sessions: [], panels: {}, queue: { active: 0, pending: 0, failed: 0 },
        tools: { open: false, visible_panels: [], protected: false } };
    handlers = { showView: vi.fn(view => { source.view = view; }), navigate: vi.fn(),
        refresh: vi.fn(), select: vi.fn(), disconnect: vi.fn(), stop: vi.fn(async () => {}), toolsRead: vi.fn(async () => {}) };
    c = new GuiController(() => source, () => handlers, () => {}, undefined, () => preferences);
});
afterEach(() => { c.dispose(); vi.useRealTimers(); });

it('preserves the first speed across expiry, Stop and failure, without leaking private bookkeeping', async () => {
    const first = show({ speed_percent: 200 }); await tick();
    expect((await first).snapshot.control).toMatchObject({ speed_percent: 200, speed_source: 'agent' });
    expect(c.state().control).not.toHaveProperty('started');
    await tick(60001);
    expect(c.state().control).toBeUndefined();
    expect((await show({ speed_percent: 300 })).error).toBe('speed_locked');
    const stop = c.run({ name: 'stop' }); await tick(50); await stop;
    expect((await show({ speed_percent: 300 })).error).toBe('speed_locked');
    handlers.showView = vi.fn(() => { throw new Error('owned failure'); });
    const failure = show(); await tick(); expect((await failure).error).toBe('action_failed');
    expect((await show({ speed_percent: 300 })).error).toBe('speed_locked');
    c.resetSessions();
    handlers.showView = view => { source.view = view; };
    const fresh = show({ speed_percent: 300 }); await tick(); expect((await fresh).ok).toBe(true);
});

it('observations leave the initial choice available and use the latest saved default', async () => {
    await c.run({ name: 'state' });
    const observation = c.run({ name: 'tools_read' }); await tick(); expect((await observation).ok).toBe(true);
    preferences = { defaultSpeed: 150, presets: [50, 100, 200] };
    const mutation = show(); await tick();
    expect((await mutation).snapshot.control).toMatchObject({ speed_percent: 150, speed_source: 'default' });
});

it('human speed overrides remain session-local and do not invalidate an application revision', async () => {
    const first = show({ speed_percent: 200 }); await tick(); await first;
    const revision = c.state().state_revision;
    c.setHumanSpeed(75);
    expect(c.state().control).toMatchObject({ speed_percent: 75, speed_source: 'human' });
    expect(c.state().state_revision).toBe(revision);
    expect(preferences.defaultSpeed).toBe(100);
    expect((await show({ speed_percent: 100 })).error).toBe('speed_locked');
});

it('pauses before dispatch and Resume never replays the rejected request', async () => {
    const action = show(); c.setPaused(true); await tick();
    expect((await action).error).toBe('paused'); expect(handlers.showView).not.toHaveBeenCalled();
    await tick(60001);
    expect((await show()).error).toBe('paused');
    c.setPaused(false); await tick(); expect(handlers.showView).not.toHaveBeenCalled();
    const fresh = show(); await tick(); expect((await fresh).ok).toBe(true);
    expect(handlers.showView).toHaveBeenCalledTimes(1);
});

it('returns committed success when paused during the in-flight handler', async () => {
    let resolve!: () => void;
    handlers.showView = vi.fn(async view => { await new Promise<void>(r => { resolve = r; }); source.view = view; });
    const action = show(); await tick(375);
    expect(handlers.showView).toHaveBeenCalledTimes(1);
    c.setPaused(true); resolve(); await tick(100);
    expect(await action).toMatchObject({ ok: true, snapshot: { control: { paused: true } } });
    expect((await show()).error).toBe('paused');
    c.setPaused(false); await tick(); expect(handlers.showView).toHaveBeenCalledTimes(1);
});

it('Stop during visual dwell cannot change a committed result or resurrect its lease', async () => {
    const action = show(); await tick(400);
    expect(c.state().control?.phase).toBe('dwell');
    const stop = c.run({ name: 'stop' }); await tick(50); await stop; await tick();
    expect((await action).ok).toBe(true); expect(c.state().control).toBeUndefined();
    expect(handlers.showView).toHaveBeenCalledTimes(1); expect(handlers.stop).toHaveBeenCalledTimes(1);
});

it('bounds dwell by the remaining budget, but times out before dispatch at a slow speed', async () => {
    const short = show({ timeout_ms: 450 }); await tick(450);
    expect((await short).ok).toBe(true);
    c.resetSessions(); vi.mocked(handlers.showView).mockClear();
    const slow = show({ speed_percent: 10, timeout_ms: 200 }); await tick(250);
    expect((await slow).error).toBe('gui_timeout'); expect(handlers.showView).not.toHaveBeenCalled();
});

it('rejects out-of-range, observation and Settings-default agent overrides', () => {
    for (const speed_percent of [9, 401, 0, 100.5, '100']) {
        expect(() => validateGuiRequest({ name: 'show_view', args: { view: 'servers' }, speed_percent } as never)).toThrow('invalid_args');
    }
    for (const name of ['state', 'stop', 'wait', 'tools_read', 'settings_read']) {
        expect(() => validateGuiRequest({ name, speed_percent: 200 })).toThrow('invalid_args');
    }
    expect(() => validateGuiRequest({ name: 'settings_update', args: { area: 'general', set: { guiPresentation: preferences } } })).toThrow('invalid_args');
});
