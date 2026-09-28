// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { AGENT_TOOLS, toNativeDefinitions, type AITool } from '../../types/tools';
import type { PluginManifest } from '../../types/plugins';
import type { ToolMacro } from './aiChatToolMacros';

type Source = { kind: 'builtin' | 'discovery' }
    | { kind: 'plugin'; ownerId: string; toolName: string; version: string }
    | { kind: 'macro'; ownerId: string; macro: ToolMacro };
export interface RegisteredTool {
    identity: string;
    revision: string;
    source: Source;
    tool: AITool;
}
export type ToolRegistry = ReadonlyArray<RegisteredTool>;
export const TOOL_SEARCH_NAME = 'tool_search';
export const TOOL_SEARCH: AITool = {
    name: TOOL_SEARCH_NAME,
    description: 'Find and load available tools for this turn. Search by exact tool name or English keywords, including plugin or macro names. Returns callable schemas. Discovery does not approve execution.',
    parameters: [{ name: 'query', type: 'string', description: 'Exact tool name or English keywords describing the operation', required: true }],
    dangerLevel: 'safe',
};
export const TOOL_EXPOSURE_GUIDE = 'Only the supplied tools are currently loaded. Before any operation whose tool is missing, call tool_search using an exact tool name or English keywords. Its results load additional callable schemas for the next step. Discovery never grants permission; normal approvals still apply. Plugin and macro tools use their returned qualified names.';
const CORE = new Set(['tool_search', 'local_list', 'remote_list', 'server_list_saved']);
const SEARCH_LIMIT = 8;
const TURN_LIMIT = 40;
const compare = (a: string, b: string) => a < b ? -1 : a > b ? 1 : 0;

// Stable alias suffix, not an authorization token. All collisions are excluded below.
function hash(value: string): string {
    let h = 0xcbf29ce484222325n;
    for (const byte of new TextEncoder().encode(value)) h = BigInt.asUintN(64, (h ^ BigInt(byte)) * 0x100000001b3n);
    return h.toString(16).padStart(16, '0');
}
function freeze<T>(value: T): T {
    if (value && typeof value === 'object') {
        Object.values(value).forEach(freeze);
        Object.freeze(value);
    }
    return value;
}
function uniqueOwners<T extends { id: string }>(values: T[]): T[] {
    return values.filter(v => v.id && values.filter(other => other.id === v.id).length === 1);
}

/** Snapshot only. No credentials, commands or approval grants are exposed to the model. */
export function buildToolRegistry(plugins: PluginManifest[], macros: ToolMacro[]): ToolRegistry {
    const entries: RegisteredTool[] = [];
    function add(tool: AITool, source: Source, rawName = tool.name, implementation: unknown = null) {
        const identity = JSON.stringify([source.kind, 'ownerId' in source ? source.ownerId : 'aeroftp', rawName]);
        const revision = JSON.stringify([identity, tool, implementation]);
        const wireName = source.kind === 'plugin' || source.kind === 'macro'
            ? `${source.kind}_${rawName.replace(/[^a-zA-Z0-9_-]/g, '_').slice(0, 30)}_${hash(revision)}`
            : rawName;
        // Clone before freezing so React settings and source constants remain editable.
        entries.push(freeze(JSON.parse(JSON.stringify({ identity, revision, source, tool: { ...tool, name: wireName } }))));
    }
    AGENT_TOOLS.forEach(tool => add(tool, { kind: 'builtin' }));
    add(TOOL_SEARCH, { kind: 'discovery' });
    for (const plugin of uniqueOwners(plugins).filter(p => p.enabled === true)) {
        for (const tool of plugin.tools) {
            add({ name: tool.name, parameters: tool.parameters, description: `[Plugin: ${plugin.name}] ${tool.description}`, dangerLevel: tool.dangerLevel === 'safe' ? 'medium' : tool.dangerLevel },
                { kind: 'plugin', ownerId: plugin.id, toolName: tool.name, version: plugin.version }, tool.name,
                [plugin.version, tool.command, tool.integrity]);
        }
    }
    for (const macro of uniqueOwners(macros)) {
        add({ name: macro.name, description: `[Macro: ${macro.displayName}] ${macro.description}`, parameters: macro.parameters, dangerLevel: macro.steps.some(step => {
            const builtin = AGENT_TOOLS.find(tool => tool.name === step.toolName);
            return !builtin || builtin.dangerLevel === 'high';
        }) ? 'high' : 'medium' },
            { kind: 'macro', ownerId: macro.id, macro }, macro.name, macro);
    }
    // Fail closed on duplicate identities or wire collisions, independently of input order.
    return Object.freeze(entries.filter(e => entries.filter(x => x.identity === e.identity).length === 1
        && entries.filter(x => x.tool.name === e.tool.name).length === 1)
        .sort((a, b) => compare(a.identity, b.identity)));
}

export function resolveRegisteredTool(registry: ToolRegistry, name: string): RegisteredTool | undefined {
    return registry.find(e => e.tool.name === name);
}

/** Legacy macro steps may use a raw plugin name only when ownership is unambiguous. */
export function resolveMacroStep(registry: ToolRegistry, name: string): RegisteredTool | undefined {
    const exact = resolveRegisteredTool(registry, name);
    if (exact) return exact;
    const matches = registry.filter(e => e.source.kind === 'plugin' && e.source.toolName === name
        || e.source.kind === 'macro' && `macro_${e.source.macro.name}` === name);
    return matches.length === 1 ? matches[0] : undefined;
}

/** Frontend exposure only; every operation still goes through its backend approval path. */
export class ToolExposure {
    private readonly selected = new Set<string>();
    constructor(readonly scope: string, readonly registry: ToolRegistry) {
        registry.filter(e => CORE.has(e.tool.name)).forEach(e => this.selected.add(e.tool.name));
    }
    tools(): AITool[] { return this.registry.filter(e => this.selected.has(e.tool.name)).map(e => e.tool); }
    definitions() { return toNativeDefinitions(this.tools()); }
    permits(scope: unknown, name: string, current: ToolRegistry): boolean {
        if (scope !== this.scope || !this.selected.has(name)) return false;
        const saved = resolveRegisteredTool(this.registry, name);
        const live = resolveRegisteredTool(current, name);
        return !!saved && saved.revision === live?.revision;
    }
    search(query: unknown) {
        if (typeof query !== 'string' || !query.trim() || query.length > 256) throw new Error('tool_search requires a nonempty query of at most 256 characters');
        const words = [...new Set(query.toLowerCase().split(/[^a-z0-9_-]+/).filter(Boolean))];
        if (!words.length) throw new Error('Search with an exact tool name or English keywords');
        const ranked = this.registry.filter(e => e.source.kind !== 'discovery').map(entry => {
            const text = `${entry.tool.name} ${entry.tool.description}`.toLowerCase();
            const raw = entry.source.kind === 'plugin' ? entry.source.toolName : entry.source.kind === 'macro' ? entry.source.macro.name : entry.tool.name;
            const exact = [entry.tool.name, raw].some(name => name.toLowerCase() === query.toLowerCase().trim());
            const score = exact ? 10000 : words.reduce((n, word) => n + (text.includes(word) ? 1 : 0), 0);
            return { entry, score };
        }).filter(x => x.score > 0).sort((a, b) => b.score - a.score || compare(a.entry.identity, b.entry.identity));
        const found: RegisteredTool[] = [];
        for (const { entry } of ranked) {
            if (found.length === SEARCH_LIMIT) break;
            if (!this.selected.has(entry.tool.name) && this.selected.size >= TURN_LIMIT) continue;
            this.selected.add(entry.tool.name);
            found.push(entry);
        }
        return {
            tools: found.map(e => ({ ...toNativeDefinitions([e.tool])[0], source: e.source.kind,
                ...('ownerId' in e.source ? { ownerId: e.source.ownerId } : {}), approval: e.tool.dangerLevel })),
            selectedCount: this.selected.size, limit: TURN_LIMIT,
            note: 'Loaded for this turn only. Execution remains subject to normal approval. Refine the query if the required tool is absent.',
        };
    }
}

/** Recheck after asynchronous approval and before dispatch, including macro children.
 *  `messages` are the translated texts for the two ways a call can go stale. */
export function assertToolExecutionCurrent(entry: RegisteredTool, current: ToolRegistry, scope: string | undefined, activeScope: string | null, messages: { turnExpired: string; identityChanged: string }): void {
    if (scope !== undefined && scope !== activeScope) throw new Error(messages.turnExpired);
    if (resolveRegisteredTool(current, entry.tool.name)?.revision !== entry.revision) throw new Error(messages.identityChanged);
}

/** Only dispatched calls count as duplicates; failed exposure checks remain retryable. */
export function recordToolDispatch(exposure: ToolExposure | null, scope: unknown, current: ToolRegistry, name: string, args: Record<string, unknown>, executed: Set<string>): boolean {
    if (!exposure?.permits(scope, name, current)) return false;
    executed.add(`${name}::${JSON.stringify(args)}`);
    return true;
}
