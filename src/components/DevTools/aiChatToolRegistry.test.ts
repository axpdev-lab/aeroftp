// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { AGENT_TOOLS, generateToolsPrompt } from '../../types/tools';
import type { PluginManifest } from '../../types/plugins';
import { DEFAULT_MACROS } from './aiChatToolMacros';
import { appendAssistantTurn, appendToolResult, assertToolResultsComplete } from './aiChatNativeTurn';
import { recordToolDispatch, assertToolExecutionCurrent, buildToolRegistry, resolveMacroStep, resolveRegisteredTool, ToolExposure } from './aiChatToolRegistry';

const STALE = { turnExpired: 'fixture: turn expired', identityChanged: 'fixture: identity changed' };

const plugin = (id: string, name = 'local_read'): PluginManifest => ({
    id, name: id, author: 'test', version: '1', enabled: true,
    tools: [{ name, description: 'Read sample plugin data', parameters: [], dangerLevel: 'safe', command: 'private executable --secret-path' }],
});

describe('namespaced tool registry and scoped exposure', () => {
    it('keeps builtin identity and distinguishes both same-name plugins', () => {
        const registry = buildToolRegistry([plugin('first'), plugin('second')], DEFAULT_MACROS);
        expect(resolveRegisteredTool(registry, 'local_read')?.source.kind).toBe('builtin');
        const plugins = registry.filter(e => e.source.kind === 'plugin');
        expect(new Set(plugins.map(e => e.tool.name)).size).toBe(2);
        for (const e of plugins) {
            expect(e.tool.name).toMatch(/^plugin_[a-zA-Z0-9_-]{1,57}$/);
            expect(e.tool.dangerLevel).toBe('medium');
            expect(e.source).toMatchObject({ toolName: 'local_read' });
        }
        expect(new Set(registry.map(e => e.tool.name)).size).toBe(registry.length);
    });

    it('rejects duplicate owners and duplicate identities regardless of ordering', () => {
        const p = plugin('duplicate');
        expect(buildToolRegistry([p, p], []).some(e => e.source.kind === 'plugin')).toBe(false);
        expect(buildToolRegistry([{ ...p, tools: [...p.tools, ...p.tools] }], []).some(e => e.source.kind === 'plugin')).toBe(false);
        expect(buildToolRegistry([], [DEFAULT_MACROS[0], DEFAULT_MACROS[0]]).some(e => e.source.kind === 'macro')).toBe(false);
        const disabled = [false, undefined].map(enabled => ({ ...plugin(String(enabled)), enabled }));
        expect(buildToolRegistry(disabled, []).some(e => e.source.kind === 'plugin')).toBe(false);
    });

    it('is deterministic, immutable and does not freeze editable settings', () => {
        const p = plugin('a'); const q = plugin('b');
        const registry = buildToolRegistry([p, q], DEFAULT_MACROS);
        expect(registry).toEqual(buildToolRegistry([q, p], [...DEFAULT_MACROS].reverse()));
        p.tools[0].description = 'changed';
        expect(registry.find(e => e.source.kind === 'plugin')?.tool.description).not.toContain('changed');
        expect(Object.isFrozen(registry[0].tool.parameters)).toBe(true);
    });

    it('does not reuse wire names or approval identities after a plugin implementation update', () => {
        const p = plugin('a');
        const before = buildToolRegistry([p], []);
        const entry = before.find(e => e.source.kind === 'plugin')!;
        const exposure = new ToolExposure('turn', before);
        exposure.search('local_read');
        for (const changed of [{ ...p, version: '2' }, { ...p, tools: [{ ...p.tools[0], command: 'replacement' }] }, { ...p, tools: [{ ...p.tools[0], integrity: 'new-script-hash' }] }]) {
            const after = buildToolRegistry([changed], []);
            expect(exposure.permits('turn', entry.tool.name, after)).toBe(false);
            expect(() => assertToolExecutionCurrent(entry, after, 'turn', 'turn', STALE)).toThrow(STALE.identityChanged);
        }
    });

    it('keeps macros distinct from plugins and resolves only unambiguous legacy steps', () => {
        const registry = buildToolRegistry([plugin('one', 'macro_safe_edit'), plugin('two', 'macro_safe_edit')], DEFAULT_MACROS);
        expect(registry.filter(e => e.source.kind === 'macro')).toHaveLength(2);
        expect(resolveMacroStep(registry, 'macro_safe_edit')).toBeUndefined();
        expect(resolveMacroStep(registry, 'local_read')?.source.kind).toBe('builtin');
        const single = buildToolRegistry([plugin('one', 'custom_read')], DEFAULT_MACROS);
        expect(resolveMacroStep(single, 'custom_read')?.source).toMatchObject({ kind: 'plugin', ownerId: 'one' });
        expect(resolveMacroStep(single, 'macro_safe_edit')?.source.kind).toBe('macro');
    });

    it('starts small, loads an exact operation, and preserves backend danger classification', () => {
        const registry = buildToolRegistry([], []);
        const exposure = new ToolExposure('turn', registry);
        expect(exposure.tools()).toHaveLength(4);
        expect(exposure.permits('turn', 'local_read', registry)).toBe(false);
        const result = exposure.search('local_read');
        expect(result.tools[0]).toMatchObject({ name: 'local_read', approval: 'medium', source: 'builtin' });
        expect(exposure.permits('turn', 'local_read', registry)).toBe(true);
        expect(exposure.definitions().find(t => t.name === 'local_read')?.parameters).toHaveProperty('required', ['path']);
        exposure.search('local_delete');
        expect(exposure.tools().find(t => t.name === 'local_delete')?.dangerLevel).toBe('high');
        expect(JSON.stringify(exposure.definitions()).length).toBeLessThan(JSON.stringify(AGENT_TOOLS).length / 4);
    });

    it('bounds discovery, rejects invalid queries and never serializes plugin commands', () => {
        const registry = buildToolRegistry([plugin('one', 'secret_read')], []);
        const exposure = new ToolExposure('turn', registry);
        const result = exposure.search('secret_read');
        expect(JSON.stringify(result)).not.toContain('private executable');
        expect(JSON.stringify(exposure.definitions())).not.toContain('secret-path');
        for (const query of [null, '', 'a'.repeat(257), '读取']) expect(() => exposure.search(query)).toThrow();
        expect(exposure.search('read').tools.length).toBeLessThanOrEqual(8);
        for (const e of registry) exposure.search(e.tool.name);
        expect(exposure.tools().length).toBeLessThanOrEqual(40);
        expect(exposure.search('nonexistent_xyz').tools).toEqual([]);
    });

    it('gives deterministic selection across catalog ordering', () => {
        const plugins = [plugin('a', 'one_read'), plugin('b', 'two_read')];
        expect(new ToolExposure('t', buildToolRegistry(plugins, DEFAULT_MACROS)).search('read'))
            .toEqual(new ToolExposure('t', buildToolRegistry([...plugins].reverse(), DEFAULT_MACROS)).search('read'));
    });

    it('resets on new turns and rejects a delayed approval after cancellation or model switch', async () => {
        const registry = buildToolRegistry([], []);
        const exposure = new ToolExposure('first', registry);
        exposure.search('local_delete');
        expect(new ToolExposure('next', registry).permits('next', 'local_delete', registry)).toBe(false);
        expect(exposure.permits('next', 'local_delete', registry)).toBe(false);
        const execute = vi.fn();
        for (const current of [null, 'next']) {
            await expect((async () => {
                await Promise.resolve('backend approval finished');
                assertToolExecutionCurrent(resolveRegisteredTool(registry, 'local_delete')!, registry, 'first', current, STALE);
                execute();
            })()).rejects.toThrow(STALE.turnExpired);
        }
        expect(execute).not.toHaveBeenCalled();
    });

    it('uses identical selected tool schemas in text fallback and native requests', () => {
        const exposure = new ToolExposure('turn', buildToolRegistry([], []));
        const text = generateToolsPrompt(exposure.tools());
        for (const tool of exposure.definitions()) expect(text).toContain(`- ${tool.name}:`);
        expect(text).not.toContain('- local_read:');
        const discovered = exposure.search('local_read');
        expect(discovered.tools[0].parameters).toEqual(exposure.definitions().find(e => e.name === 'local_read')?.parameters);
    });

    it('pairs search and failed hidden-tool results before continuation', () => {
        const exposure = new ToolExposure('turn', buildToolRegistry([], []));
        const history: Array<Record<string, unknown>> = [];
        appendAssistantTurn(history, '', [{ id: 'search-id', name: 'tool_search', arguments: { query: 'local_read' } }]);
        appendToolResult(history, 'search-id', JSON.stringify(exposure.search('local_read')));
        expect(() => assertToolResultsComplete(history)).not.toThrow();
        appendAssistantTurn(history, '', [{ id: 'bad-id', name: 'invented_tool', arguments: {} }]);
        appendToolResult(history, 'bad-id', 'Error: tool not loaded; call tool_search first');
        expect(() => assertToolResultsComplete(history)).not.toThrow();
        expect(exposure.definitions().some(e => e.name === 'local_read')).toBe(true);
    });
});

it('allows the same call after hidden-tool rejection, discovery and retry', () => {
    const registry = buildToolRegistry([], []);
    const exposure = new ToolExposure('turn', registry);
    const executed = new Set<string>();
    const args = { path: '/tmp/fixture.txt' };
    expect(recordToolDispatch(exposure, 'turn', registry, 'local_read', args, executed)).toBe(false);
    expect(executed.size).toBe(0);
    exposure.search('local_read');
    expect(executed.has(`local_read::${JSON.stringify(args)}`)).toBe(false);
    expect(recordToolDispatch(exposure, 'turn', registry, 'local_read', args, executed)).toBe(true);
    expect(executed.has(`local_read::${JSON.stringify(args)}`)).toBe(true);
});

it('classifies high-risk and external macro children conservatively', () => {
    const base = DEFAULT_MACROS[0];
    for (const name of ['local_delete', 'custom_plugin_operation', 'macro_other']) {
        const registry = buildToolRegistry([], [{ ...base, steps: [{ toolName: name, args: {} }] }]);
        expect(registry.find(e => e.source.kind === 'macro')?.tool.dangerLevel).toBe('high');
    }
    expect(buildToolRegistry([], [base]).find(e => e.source.kind === 'macro')?.tool.dangerLevel).toBe('medium');
});
