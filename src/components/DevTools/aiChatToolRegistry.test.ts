// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import everything from '../../../src-tauri/tests/fixtures/mcp-client/server-everything-2026.8.31-tools-list.json';
import { AGENT_TOOLS, generateToolsPrompt, toJSONSchema } from '../../types/tools';
import type { PluginManifest } from '../../types/plugins';
import { DEFAULT_MACROS } from './aiChatToolMacros';
import { appendAssistantTurn, appendToolResult, assertToolResultsComplete } from './aiChatNativeTurn';
import { recordToolDispatch, assertToolExecutionCurrent, buildToolRegistry, resolveMacroStep, resolveRegisteredTool, ToolExposure, type McpServerSnapshot } from './aiChatToolRegistry';

const STALE = { turnExpired: 'fixture: turn expired', identityChanged: 'fixture: identity changed' };

const plugin = (id: string, name = 'local_read'): PluginManifest => ({
    id, name: id, author: 'test', version: '1', enabled: true,
    tools: [{ name, description: 'Read sample plugin data', parameters: [], dangerLevel: 'safe', command: 'private executable --secret-path' }],
});

const mcp = (id: string, name = 'remote.read'): McpServerSnapshot => ({
    id, transport: 'stdio', revision: 'rev-1', enabled: true, tools: [{ name, description: 'Fetch remote data', enabled: true,
        schemaRevision: 'a'.repeat(64),
        dangerLevel: 'safe', inputSchema: { type: 'object', properties: {
            path: { type: 'string', description: 'Remote path' }, limit: { type: 'number' },
        }, required: ['path'], additionalProperties: false } }],
});

describe('untrusted MCP registry snapshot', () => {
    it('exposes all thirteen real reference tools and preserves enforceable constraints', () => {
        const server = mcp('everything');
        server.tools = everything.result.tools.map(tool => ({ name: tool.name, description: tool.description,
            enabled: true, schemaRevision: 'a'.repeat(64), inputSchema: tool.inputSchema }));
        const entries = buildToolRegistry([], [], [server]).filter(entry => entry.source.kind === 'mcp');
        expect(entries).toHaveLength(13);
        const links = entries.find(entry => entry.source.kind === 'mcp' && entry.source.toolName === 'get-resource-links')!;
        expect((toJSONSchema(links.tool).properties as Record<string, unknown>).count).toEqual({
            type: 'number', description: 'Number of resource links to return (1-10)', minimum: 1, maximum: 10,
        });
        const weather = entries.find(entry => entry.source.kind === 'mcp' && entry.source.toolName === 'get-structured-content')!;
        expect((toJSONSchema(weather.tool).properties as Record<string, unknown>).location).toEqual({
            type: 'string', description: 'Choose city', enum: ['New York', 'Chicago', 'Los Angeles'],
        });
        expect(JSON.stringify(toJSONSchema(links.tool))).not.toContain('default');
        expect(JSON.stringify(toJSONSchema(weather.tool))).not.toContain('$schema');
    });

    it('rejects malformed accepted metadata and scalar constraints', () => {
        for (const property of [
            { type: 'string', title: false }, { type: 'string', format: 'bad\n' },
            { type: 'string', default: 1 }, { type: 'integer', default: 1.5 },
            { type: 'array', items: { type: 'string' }, default: [1] },
            { type: 'string', enum: [true] }, { type: 'string', enum: Array(65).fill('x') },
            { type: 'string', enum: ['x'.repeat(513)] },
            { type: 'array', items: { type: 'string' }, enum: [['x']] },
            { type: 'number', minimum: '0' }, { type: 'string', maximum: 3 },
            { type: 'number', minimum: 5, maximum: 4 },
        ]) {
            const server = mcp('invalid');
            server.tools[0].inputSchema = { type: 'object', properties: { value: property } };
            expect(buildToolRegistry([], [], [server]).some(entry => entry.source.kind === 'mcp')).toBe(false);
        }
    });

    it('namespaces equal tool names by server, keeps the alias stable and never trusts a safe claim', () => {
        const registry = buildToolRegistry([], [], [mcp('alpha'), mcp('beta')]);
        const tools = registry.filter(entry => entry.source.kind === 'mcp');
        expect(tools).toHaveLength(2);
        expect(new Set(tools.map(entry => entry.tool.name)).size).toBe(2);
        for (const entry of tools) {
            expect(entry.identity).toContain('remote.read');
            expect(entry.tool.name).toMatch(/^mcp_[a-zA-Z0-9_-]+_[a-f0-9]{16}$/);
            expect(entry.tool.dangerLevel).toBe('high');
            expect(entry.tool.parameters.find(parameter => parameter.name === 'path')?.required).toBe(true);
        }
        expect(buildToolRegistry([], [], [mcp('beta'), mcp('alpha')])).toEqual(registry);
        const similarlySanitized = buildToolRegistry([], [], [mcp('alpha', 'remote.read'), mcp('beta', 'remote/read')]);
        expect(new Set(similarlySanitized.filter(entry => entry.source.kind === 'mcp').map(entry => entry.tool.name)).size).toBe(2);
        const updated = buildToolRegistry([], [], [{ ...mcp('alpha'), revision: 'rev-2' }]);
        expect(updated.find(entry => entry.source.kind === 'mcp')?.tool.name).toBe(tools[0].tool.name);
    });

    it('carries transport and backend revisions as provenance, and a new schema revision makes a call stale', () => {
        const http = { ...mcp('remote'), transport: 'http' as const, revision: 'b'.repeat(64) };
        const before = buildToolRegistry([], [], [http]);
        const entry = before.find(tool => tool.source.kind === 'mcp')!;
        expect(entry.source).toEqual({ kind: 'mcp', ownerId: 'remote', toolName: 'remote.read', transport: 'http',
            serverRevision: 'b'.repeat(64), schemaRevision: 'a'.repeat(64) });
        expect(entry.tool.description).toBe('[MCP http: remote] Fetch remote data');
        const after = buildToolRegistry([], [], [{ ...http, tools: [{ ...http.tools[0], schemaRevision: 'c'.repeat(64) }] }]);
        expect(after.find(tool => tool.source.kind === 'mcp')!.tool.name).toBe(entry.tool.name);
        expect(() => assertToolExecutionCurrent(entry, after, undefined, null, STALE)).toThrow(STALE.identityChanged);
        const moved = buildToolRegistry([], [], [{ ...http, transport: 'stdio' }]);
        expect(() => assertToolExecutionCurrent(entry, moved, undefined, null, STALE)).toThrow(STALE.identityChanged);
    });

    it('rejects duplicate server IDs, tool names, disabled entries and unsafe identifiers', () => {
        expect(buildToolRegistry([], [], [mcp('same'), mcp('same')]).some(entry => entry.source.kind === 'mcp')).toBe(false);
        const duplicate = mcp('one'); duplicate.tools.push({ ...duplicate.tools[0] });
        expect(buildToolRegistry([], [], [duplicate]).some(entry => entry.source.kind === 'mcp')).toBe(false);
        for (const server of [
            { ...mcp('off'), enabled: false }, { ...mcp('bad id'), id: '../escape' },
            { ...mcp('empty'), revision: '' },
            { ...mcp('no-transport'), transport: undefined }, { ...mcp('odd-transport'), transport: 'sse' },
            { ...mcp('no-schema-rev'), tools: [{ ...mcp('no-schema-rev').tools[0], schemaRevision: undefined }] },
            { ...mcp('short-schema-rev'), tools: [{ ...mcp('short-schema-rev').tools[0], schemaRevision: 'abc' }] },
            { ...mcp('tool-off'), tools: [{ ...mcp('tool-off').tools[0], enabled: false }] },
            { ...mcp('bad-name'), tools: [{ ...mcp('bad-name').tools[0], name: 'bad\nname' }] },
            { ...mcp('too-many'), tools: Array.from({ length: 129 }, (_, i) => ({ ...mcp('too-many').tools[0], name: `tool${i}` })) },
        ] as unknown as McpServerSnapshot[]) expect(buildToolRegistry([], [], [server]).some(entry => entry.source.kind === 'mcp')).toBe(false);
        expect(buildToolRegistry([], [], Array.from({ length: 33 }, (_, i) => mcp(`server${i}`)))
            .some(entry => entry.source.kind === 'mcp')).toBe(false);
        expect(buildToolRegistry([], [], [null, { ...mcp('malformed'), tools: [null] }] as unknown as McpServerSnapshot[])
            .some(entry => entry.source.kind === 'mcp')).toBe(false);
    });

    it('accepts a bounded catalog containing native and compatibility tool names', () => {
        for (const count of [77, 128]) {
            const server = mcp('native');
            server.tools = Array.from({ length: count }, (_, index) => ({ ...server.tools[0], name: `native_${index}` }));
            const entries = buildToolRegistry([], [], [server]).filter(entry => entry.source.kind === 'mcp');
            expect(entries).toHaveLength(count);
        }
    });

    it('rejects schemas that cannot be represented without losing constraints', () => {
        const schema = (inputSchema: unknown) => buildToolRegistry([], [], [{ ...mcp('schema'),
            tools: [{ ...mcp('schema').tools[0], inputSchema }] }]).some(entry => entry.source.kind === 'mcp');
        const base = { type: 'object', properties: { path: { type: 'string' } }, required: ['path'] };
        expect(schema(base)).toBe(true);
        for (const invalid of [
            { ...base, properties: { path: { type: 'object', properties: {} } } },
            { ...base, properties: { path: { type: 'string', enum: [] } } },
            { ...base, properties: { path: { type: 'array', items: { type: 'object' } } } },
            { ...base, oneOf: [base] }, { ...base, required: ['unknown'] },
            { ...base, properties: Object.fromEntries(Array.from({ length: 17 }, (_, i) => [`p${i}`, { type: 'string' }])) },
            { ...base, properties: { path: { type: 'string', description: 'x'.repeat(8192) } } },
            { ...base, properties: { path: { type: 'array', items: { type: 'string', extra: true } } } },
            { ...base, description: 'x'.repeat(8192) },
            { ...base, properties: { path: { type: 'array', items: { type: 'array', items: { type: 'string' } } } } },
        ]) expect(schema(invalid)).toBe(false);
    });

    it('exposes supported string arrays without admitting nested arrays', () => {
        const server = mcp('arrays');
        server.tools[0].inputSchema = { type: 'object', properties: {
            paths: { type: 'array', items: { type: 'string' } },
        }, required: ['paths'], additionalProperties: false };
        const entry = buildToolRegistry([], [], [server]).find(tool => tool.source.kind === 'mcp');
        expect(entry?.tool.parameters).toEqual([{ name: 'paths', type: 'array', description: '', required: true }]);
        server.tools[0].inputSchema = { type: 'object', properties: {
            paths: { type: 'array', items: { type: 'array', items: { type: 'string' } } },
        } };
        expect(buildToolRegistry([], [], [server]).some(tool => tool.source.kind === 'mcp')).toBe(false);
    });

    it('preserves flat header annotations and integer types without granting approval', () => {
        const server = mcp('headers');
        server.tools[0].inputSchema = { type: 'object', properties: {
            region: { type: 'string', 'x-mcp-header': 'Region' },
            retries: { type: 'integer', 'x-mcp-header': 'Retry-Count' },
            enabled: { type: 'boolean', 'x-mcp-header': 'X_Feature' },
            fraction: { type: 'number' },
        }, required: ['region'], additionalProperties: false };
        const entry = buildToolRegistry([], [], [server]).find(tool => tool.source.kind === 'mcp');
        expect(entry?.tool.dangerLevel).toBe('high');
        expect(entry?.tool.parameters.map(p => [p.name, p.type])).toEqual([
            ['region', 'string'], ['retries', 'integer'], ['enabled', 'boolean'], ['fraction', 'number'],
        ]);
        expect(toJSONSchema(entry!.tool).properties).toMatchObject({ retries: { type: 'integer' } });
        expect(toJSONSchema(entry!.tool).additionalProperties).toBe(false);
    });

    it('rejects malformed, duplicate, nested or unrepresentable header annotations', () => {
        const accepted = (inputSchema: unknown) => buildToolRegistry([], [], [{ ...mcp('headers'),
            tools: [{ ...mcp('headers').tools[0], inputSchema }] }]).some(entry => entry.source.kind === 'mcp');
        const field = (annotation: unknown, type = 'string') => ({ type: 'object', properties: {
            region: { type, 'x-mcp-header': annotation },
        } });
        expect(accepted(field('X.Region_1'))).toBe(true);
        for (const invalid of [
            field(''), field('x'.repeat(65)), field('bad name'), field('bad\rname'),
            field(42), field(undefined), field('Region', 'number'), field('Region', 'array'),
            { type: 'object', properties: { a: { type: 'string', 'x-mcp-header': 'Region' },
                b: { type: 'boolean', 'x-mcp-header': 'region' } } },
            { type: 'object', 'x-mcp-header': 'Root', properties: {} },
            { type: 'object', properties: { nested: { type: 'object', properties: {
                region: { type: 'string', 'x-mcp-header': 'Region' },
            } } } },
            { type: 'object', properties: { a: { type: 'array', items: {
                type: 'string', 'x-mcp-header': 'Region',
            } } } },
            { type: 'object', properties: { region: {
                type: 'string', 'x-mcp-header': 'Region', enum: [1],
            } } },
        ]) expect(accepted(invalid)).toBe(false);
    });

    it('invalidates exposure when only the original header annotation changes', () => {
        const server = mcp('headers');
        server.tools[0].inputSchema = { type: 'object', properties: {
            region: { type: 'string', 'x-mcp-header': 'Region' },
        } };
        const before = buildToolRegistry([], [], [server]);
        const entry = before.find(tool => tool.source.kind === 'mcp')!;
        const exposure = new ToolExposure('turn', before);
        exposure.search(entry.tool.name);
        expect(exposure.permits('turn', entry.tool.name, before)).toBe(true);
        const changed = mcp('headers');
        changed.tools[0].inputSchema = { type: 'object', properties: {
            region: { type: 'string', 'x-mcp-header': 'Other-Region' },
        } };
        const after = buildToolRegistry([], [], [changed]);
        const replacement = after.find(tool => tool.source.kind === 'mcp')!;
        expect(replacement.tool).toEqual(entry.tool);
        expect(replacement.revision).not.toBe(entry.revision);
        expect(exposure.permits('turn', entry.tool.name, after)).toBe(false);
        expect(() => assertToolExecutionCurrent(entry, after, 'turn', 'turn', STALE)).toThrow(STALE.identityChanged);
    });

    it('invalidates exposure after revision, schema or enablement changes, without shadowing builtins', () => {
        const server = mcp('alpha', 'local_delete');
        const before = buildToolRegistry([], [], [server]);
        const entry = before.find(tool => tool.source.kind === 'mcp')!;
        expect(resolveRegisteredTool(before, 'local_delete')?.source.kind).toBe('builtin');
        const exposure = new ToolExposure('turn', before);
        exposure.search(entry.tool.name);
        expect(exposure.permits('turn', entry.tool.name, before)).toBe(true);
        for (const changed of [
            { ...server, revision: 'rev-2' },
            { ...server, enabled: false },
            { ...server, tools: [{ ...server.tools[0], inputSchema: { type: 'object', properties: {} } }] },
        ]) {
            const after = buildToolRegistry([], [], [changed]);
            expect(exposure.permits('turn', entry.tool.name, after)).toBe(false);
            expect(() => assertToolExecutionCurrent(entry, after, 'turn', 'turn', STALE)).toThrow(STALE.identityChanged);
        }
    });

    it('keeps metadata out of initial tool definitions and bounds discovery', () => {
        const server = mcp('alpha'); server.tools[0].description = 'x'.repeat(512);
        const registry = buildToolRegistry([], [], [server]);
        const exposure = new ToolExposure('turn', registry);
        expect(exposure.definitions().some(tool => tool.name.startsWith('mcp_'))).toBe(false);
        expect(exposure.search('remote.read').tools.find(tool => tool.source === 'mcp')?.approval).toBe('high');
        expect(exposure.definitions().some(tool => tool.name.startsWith('mcp_'))).toBe(true);
        server.tools[0].description = 'changed';
        expect(registry.find(entry => entry.source.kind === 'mcp')?.tool.description).not.toContain('changed');
        expect(buildToolRegistry([], [], [mcp('oversized', 'a'.repeat(129))]).some(entry => entry.source.kind === 'mcp')).toBe(false);
    });
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
