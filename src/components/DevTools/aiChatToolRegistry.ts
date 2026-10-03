// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { AGENT_TOOLS, toNativeDefinitions, type AITool } from '../../types/tools';
import type { PluginManifest } from '../../types/plugins';
import type { ToolMacro } from './aiChatToolMacros';

type Source = { kind: 'builtin' | 'discovery' }
    | { kind: 'plugin'; ownerId: string; toolName: string; version: string }
    | { kind: 'macro'; ownerId: string; macro: ToolMacro }
    | { kind: 'mcp'; ownerId: string; toolName: string; transport: McpTransport; serverRevision: string; schemaRevision: string };
export type McpTransport = 'stdio' | 'http';
/** Untrusted discovery data. Process configuration and credentials belong in the backend. */
export interface McpToolSnapshot {
    name: string;
    description?: string;
    inputSchema: unknown;
    /** Backend digest of the validated schema; a call is approved against it. */
    schemaRevision: string;
    enabled: boolean;
    /** Server claims cannot lower the local approval floor. */
    dangerLevel?: 'safe' | 'medium' | 'high';
}
export interface McpServerSnapshot {
    id: string;
    transport: McpTransport;
    revision: string;
    enabled: boolean;
    tools: McpToolSnapshot[];
}
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

const MCP_MAX_SERVERS = 32;
// Covers AeroFTP primary tools and compatibility aliases with a bounded catalog.
const MCP_MAX_TOOLS = 128;
const MCP_MAX_SCHEMA_BYTES = 8192;
const MCP_MAX_PARAMETERS = 16;
const MCP_MAX_HEADERS = 24;
// root -> properties -> property -> items -> items.type
const MCP_MAX_DEPTH = 4;
const MCP_ID = /^[a-zA-Z0-9][a-zA-Z0-9_-]{0,63}$/;
const MCP_NAME = /^[a-zA-Z0-9][a-zA-Z0-9_.\/-]{0,127}$/;
const PARAM_NAME = /^[a-zA-Z_][a-zA-Z0-9_]{0,63}$/;
const SCHEMA_REVISION = /^[a-f0-9]{64}$/;
// Mirrors the backend's RFC token check for Mcp-Param-<suffix> headers.
const MCP_HEADER_SUFFIX = /^[!#$%&'*+.^_`|~A-Za-z0-9-]{1,64}$/;
const object = (value: unknown): value is Record<string, unknown> =>
    value !== null && typeof value === 'object' && !Array.isArray(value) && Object.getPrototypeOf(value) === Object.prototype;
const keysOnly = (value: Record<string, unknown>, allowed: string[]) => Object.keys(value).every(key => allowed.includes(key));
const shortText = (value: unknown, max: number): value is string => typeof value === 'string'
    && value.length <= max && !/[\u0000-\u001f\u007f]/.test(value);
// The backend compares schema numbers exactly. An integral value beyond 2^53 is
// already rounded here, so the model would be offered a value the backend refuses.
const exactNumber = (value: unknown): value is number => typeof value === 'number' && Number.isFinite(value)
    && (!Number.isInteger(value) || Number.isSafeInteger(value));
const prose = (value: unknown, max: number): value is string => typeof value === 'string'
    && value.length <= max && !/[\u0000-\u0008\u000b\u000c\u000e-\u001f\u007f]/.test(value);

/** Whether the numeric bounds leave a value to send, as the backend checks:
 *  an integer parameter needs an integer inside them. Each side has at most
 *  one bound (an inclusive and an exclusive one together are refused first). */
function boundsLeaveAValue(type: unknown, property: Record<string, unknown>): boolean {
    const low = typeof property.minimum === 'number' ? { value: property.minimum, inclusive: true }
        : typeof property.exclusiveMinimum === 'number' ? { value: property.exclusiveMinimum, inclusive: false } : undefined;
    const high = typeof property.maximum === 'number' ? { value: property.maximum, inclusive: true }
        : typeof property.exclusiveMaximum === 'number' ? { value: property.exclusiveMaximum, inclusive: false } : undefined;
    if (!low || !high) return true;
    if (type === 'integer') {
        const first = low.inclusive ? Math.ceil(low.value) : Math.floor(low.value) + 1;
        const last = high.inclusive ? Math.floor(high.value) : Math.ceil(high.value) - 1;
        return first <= last;
    }
    return low.value < high.value || (low.value === high.value && low.inclusive && high.inclusive);
}

/** The current AITool contract supports flat primitive parameters and string arrays only.
 *  Reject other JSON Schema features instead of silently weakening validation. */
function mcpParameters(input: unknown): Pick<AITool, 'parameters' | 'additionalProperties'> | undefined {
    if (!object(input)) return undefined;
    let nodes = 0;
    const withinDepth = (value: unknown, depth: number): boolean => {
        if (depth > MCP_MAX_DEPTH || ++nodes > 128) return false;
        if (Array.isArray(value)) return value.every(item => withinDepth(item, depth + 1));
        if (object(value)) return Object.entries(value).every(([key, item]) => key.length <= 128 && withinDepth(item, depth + 1));
        return value === null || typeof value === 'string' && value.length <= MCP_MAX_SCHEMA_BYTES
            || typeof value === 'number' && Number.isFinite(value) || typeof value === 'boolean';
    };
    if (!withinDepth(input, 0)) return undefined;
    let json: string;
    try { json = JSON.stringify(input); } catch { return undefined; }
    if (!json || new TextEncoder().encode(json).length > MCP_MAX_SCHEMA_BYTES
        || !keysOnly(input, ['type', 'properties', 'required', 'additionalProperties', 'description', '$schema', 'title'])
        || input.type !== 'object' || !object(input.properties)
        || input.additionalProperties !== false && input.additionalProperties !== undefined
        || input.description !== undefined && !prose(input.description, 512)
        || input.$schema !== undefined && !shortText(input.$schema, 512)
        || input.title !== undefined && !shortText(input.title, 512)) return undefined;
    const names = Object.keys(input.properties);
    if (names.length > MCP_MAX_PARAMETERS || !names.every(name => PARAM_NAME.test(name))) return undefined;
    if (!Array.isArray(input.required) && input.required !== undefined) return undefined;
    const required = (input.required ?? []) as unknown[];
    if (required.some(name => typeof name !== 'string' || !names.includes(name)) || new Set(required).size !== required.length) return undefined;
    const parameters: AITool['parameters'] = [];
    const headerNames = new Set<string>();
    for (const name of names) {
        const property = input.properties[name];
        if (!object(property) || !keysOnly(property, ['type', 'description', 'items', 'x-mcp-header', 'title', 'format', 'default', 'enum', 'minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum', 'minLength', 'maxLength'])
            || property.description !== undefined && !prose(property.description, 512)
            || property.title !== undefined && !shortText(property.title, 512)
            || property.format !== undefined && !shortText(property.format, 512)) return undefined;
        const type = property.type;
        if (!['string', 'number', 'integer', 'boolean', 'array'].includes(type as string)) return undefined;
        if (Object.prototype.hasOwnProperty.call(property, 'x-mcp-header')) {
            const suffix = property['x-mcp-header'];
            if (typeof suffix !== 'string' || !MCP_HEADER_SUFFIX.test(suffix)
                || !['string', 'integer', 'boolean'].includes(type as string)
                || headerNames.size >= MCP_MAX_HEADERS || headerNames.has(suffix.toLowerCase())) return undefined;
            headerNames.add(suffix.toLowerCase());
        }
        if (type === 'array') {
            if (!object(property.items) || !keysOnly(property.items, ['type']) || property.items.type !== 'string') return undefined;
        } else if (property.items !== undefined) return undefined;
        const typed = (value: unknown) => type === 'string' ? typeof value === 'string'
            : type === 'boolean' ? typeof value === 'boolean'
            : type === 'number' ? exactNumber(value)
            : type === 'integer' ? typeof value === 'number' && Number.isSafeInteger(value)
            : Array.isArray(value) && value.every(item => typeof item === 'string');
        if (Object.prototype.hasOwnProperty.call(property, 'default') && !typed(property.default)) return undefined;
        if (property.enum !== undefined && (type === 'array' || !Array.isArray(property.enum)
            || !property.enum.length || property.enum.length > 64
            || !property.enum.every(value => typed(value) && (typeof value !== 'string' || shortText(value, 512))))) return undefined;
        for (const bound of ['minimum', 'maximum', 'exclusiveMinimum', 'exclusiveMaximum']) {
            if (property[bound] !== undefined && (!['number', 'integer'].includes(type as string)
                || !exactNumber(property[bound]))) return undefined;
        }
        if ((property.minimum !== undefined && property.exclusiveMinimum !== undefined)
            || (property.maximum !== undefined && property.exclusiveMaximum !== undefined)) return undefined;
        if (!boundsLeaveAValue(type, property)) return undefined;
        for (const bound of ['minLength', 'maxLength']) {
            if (property[bound] !== undefined && (type !== 'string' || typeof property[bound] !== 'number'
                || !Number.isInteger(property[bound]) || (property[bound] as number) < 0
                || (property[bound] as number) > 65536)) return undefined;
        }
        if (typeof property.minLength === 'number' && typeof property.maxLength === 'number'
            && property.minLength > property.maxLength) return undefined;
        // Annotation-only metadata and defaults never become model instructions or argument values.
        parameters.push({ name, type: type as AITool['parameters'][number]['type'],
            description: (property.description as string | undefined) ?? '', required: required.includes(name),
            ...(property.enum !== undefined ? { enum: property.enum as (string | number | boolean)[] } : {}),
            ...(property.minimum !== undefined ? { minimum: property.minimum as number } : {}),
            ...(property.maximum !== undefined ? { maximum: property.maximum as number } : {}),
            ...(property.exclusiveMinimum !== undefined ? { exclusiveMinimum: property.exclusiveMinimum as number } : {}),
            ...(property.exclusiveMaximum !== undefined ? { exclusiveMaximum: property.exclusiveMaximum as number } : {}),
            ...(property.minLength !== undefined ? { minLength: property.minLength as number } : {}),
            ...(property.maxLength !== undefined ? { maxLength: property.maxLength as number } : {}),
        });
    }
    return { parameters, ...(input.additionalProperties === false ? { additionalProperties: false as const } : {}) };
}

/** Snapshot only. No credentials, commands or approval grants are exposed to the model. */
export function buildToolRegistry(plugins: PluginManifest[], macros: ToolMacro[], mcpServers: McpServerSnapshot[] = []): ToolRegistry {
    const entries: RegisteredTool[] = [];
    function add(tool: AITool, source: Source, rawName = tool.name, implementation: unknown = null) {
        const identity = JSON.stringify([source.kind, 'ownerId' in source ? source.ownerId : 'aeroftp', rawName]);
        const revision = JSON.stringify([identity, tool, implementation]);
        const wireName = source.kind === 'mcp'
            ? `mcp_${rawName.replace(/[^a-zA-Z0-9_-]/g, '_').slice(0, 30)}_${hash(identity)}`
            : source.kind === 'plugin' || source.kind === 'macro'
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
    if (mcpServers.length <= MCP_MAX_SERVERS) for (const server of uniqueOwners(mcpServers.filter(object) as McpServerSnapshot[])) {
        if (typeof server.id !== 'string' || !MCP_ID.test(server.id) || !shortText(server.revision, 128) || !server.revision
            || server.transport !== 'stdio' && server.transport !== 'http'
            || server.enabled !== true || !Array.isArray(server.tools) || server.tools.length > MCP_MAX_TOOLS) continue;
        const names = server.tools.map(tool => object(tool) ? tool.name : undefined);
        for (const tool of server.tools) {
            if (!object(tool) || tool.enabled !== true || typeof tool.name !== 'string' || !MCP_NAME.test(tool.name)
                || names.filter(name => name === tool.name).length !== 1
                || typeof tool.schemaRevision !== 'string' || !SCHEMA_REVISION.test(tool.schemaRevision)
                || !prose(tool.description ?? '', 512)) continue;
            const schema = mcpParameters(tool.inputSchema);
            if (!schema) continue;
            // Server-declared read-only/safe annotations are untrusted. Backend approval is still required.
            // Provenance names the supplying server and its transport.
            add({ name: tool.name, description: `[MCP ${server.transport}: ${server.id}] ${tool.description ?? ''}`,
                ...schema, dangerLevel: 'high' },
            { kind: 'mcp', ownerId: server.id, toolName: tool.name, transport: server.transport,
                serverRevision: server.revision, schemaRevision: tool.schemaRevision }, tool.name,
            [server.transport, server.revision, tool.schemaRevision, tool.inputSchema]);
        }
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
