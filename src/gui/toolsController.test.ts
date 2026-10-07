// @vitest-environment jsdom
// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { GuiController, buildGuiSnapshot, validateGuiRequest, type GuiSource } from './controller';
import { createToolsHandlers } from './toolsHandlers';
import { panelsWithAgentTool, toolsPanelCapacity } from './toolsWorkspace';
import type { ToolPanel } from './toolsSchema';
const backend = vi.hoisted(() => ({ user: 1, locked: false, gate: undefined as Promise<void> | undefined, checks: 0, rejectBroker: false }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: vi.fn(async (name: string) => {
    if (name === 'gui_intent_check') { if (backend.rejectBroker) throw Error('gui_scope_changed'); return; }
    if (name !== 'user_partitions_unlock_status') throw Error('Unexpected IPC');
    if (++backend.checks > 1) await backend.gate;
    return { activeUserId: backend.user, unlockedUserId: backend.user, isUnlocked: !backend.locked };
}) }));
let source: GuiSource, controller: GuiController;
let open: ReturnType<typeof vi.fn<(tool?: ToolPanel) => void>>, close: ReturnType<typeof vi.fn<() => void>>;
beforeEach(() => {
    backend.user = 1; backend.locked = false; backend.gate = undefined; backend.checks = 0; backend.rejectBroker = false;
    source = { version: 'test', locked: false, blocked: false, view: 'files', connected: false, activeSessionId: null,
        sessions: [], panels: {}, queue: { active: 0, pending: 0, failed: 0 },
        tools: { open: false, visible_panels: [], protected: false } };
    open = vi.fn((tool?: ToolPanel) => { source.tools = { open: true, visible_panels: tool ? [tool] : ['editor'], protected: false }; });
    close = vi.fn(() => { source.tools = { open: false, visible_panels: [], protected: false }; });
    controller = new GuiController(() => source, () => ({ showView: () => {}, navigate: async () => {}, refresh: async () => {},
        select: () => {}, disconnect: async () => {}, stop: async () => {}, ...createToolsHandlers({ open, close }) }), () => {});
});
afterEach(() => controller.dispose());
const run = (name: string, args = {}, timeout_ms = 1000) => controller.run({ name, args, pace: 'fast', timeout_ms }, 'test', Infinity, 'claimed');
it('opens, observes and closes committed workspace metadata through the real handler factory', async () => {
    expect((await run('tools_open', { tool: 'terminal' })).snapshot.tools?.visible_panels).toEqual(['terminal']);
    expect((await run('tools_read')).ok).toBe(true);
    expect((await run('tools_close')).snapshot.tools?.open).toBe(false);
    expect(open).toHaveBeenCalledTimes(1); expect(close).toHaveBeenCalledTimes(1);
});
it.each(['security', 'shell', 'vault', 'chat'])('refuses unsupported panel %s', tool => {
    expect(() => validateGuiRequest({ name: 'tools_open', args: { tool } })).toThrow();
});
it('refuses contents, commands and extra fields before dispatch', () => {
    for (const name of ['tools_open', 'tools_read', 'tools_close']) {
        expect(() => validateGuiRequest({ name, args: { command: 'sentinel' } })).toThrow();
    }
});
it('projects only panel enums/booleans and removes all tools state while locked', () => {
    source.tools = { open: true, visible_panels: ['editor', 'editor', 'SECRET' as ToolPanel], protected: false, content: 'SECRET' } as NonNullable<GuiSource['tools']>;
    expect(buildGuiSnapshot(source).tools).toEqual({ open: true, visible_panels: ['editor'], protected: false });
    source.locked = true; expect(buildGuiSnapshot(source).tools).toBeUndefined();
});
it('never alters a protected Security workspace but allows safe metadata observation', async () => {
    source.tools = { open: true, visible_panels: ['security'], protected: true };
    expect((await run('tools_open')).error).toBe('blocked');
    expect((await run('tools_close')).error).toBe('blocked');
    expect((await run('tools_read')).ok).toBe(true);
    expect(open).not.toHaveBeenCalled(); expect(close).not.toHaveBeenCalled();
});
it('native stale claim prevents any workspace change', async () => {
    backend.rejectBroker = true; expect((await run('tools_open')).error).toBe('lease_interrupted');
    expect(open).not.toHaveBeenCalled();
});
it.each(['stop', 'account', 'timeout'])('a deferred account check cannot open after %s', async reason => {
    let release!: () => void; backend.gate = new Promise<void>(resolve => { release = resolve; });
    const pending = run('tools_open', {}, reason === 'timeout' ? 100 : 1000);
    await vi.waitFor(() => expect(backend.checks).toBeGreaterThan(1));
    if (reason === 'stop') controller.interrupt();
    if (reason === 'account') backend.user = 2;
    if (reason === 'timeout') await new Promise(resolve => setTimeout(resolve, 120));
    release(); expect((await pending).ok).toBe(false); expect(open).not.toHaveBeenCalled();
});
it('cannot claim opening before the requested panel becomes visible', async () => {
    open.mockImplementation(() => {});
    expect((await run('tools_open', { tool: 'agent' }, 100)).error).toBe('gui_timeout');
});
it('adds a panel without disabling any existing panel and refuses full responsive capacity', () => {
    const panels = { editor: true, terminal: true, chat: false, security: false };
    expect(panelsWithAgentTool(panels, 'agent', 1024)).toEqual({ ...panels, chat: true });
    expect(() => panelsWithAgentTool(panels, 'agent', 800)).toThrow();
    expect(panelsWithAgentTool(panels, 'terminal', 800)).toBe(panels);
    expect(() => panelsWithAgentTool({ ...panels, security: true }, undefined, 1024)).toThrow();
    expect([599, 600, 899, 900, 999, 1000].map(toolsPanelCapacity)).toEqual([1, 2, 2, 3, 3, 4]);
});
