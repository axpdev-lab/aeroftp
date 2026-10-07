// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { ConnectScope, type ProfileConnectOutcome } from './connectScope';
import { TOOLS_INTENTS, TOOL_PANELS, buildToolsProjection, type ToolPanel, type GuiToolsProjection } from './toolsSchema';
import { GuiError, type GuiErrorCode } from './errors';
import {
    SETTINGS_INTENTS, buildSettingsProjection, settingsUpdateCommitted, validateSettingsSet,
    type GuiSettingsProjection, type SettingsArea,
} from './settingsSchema';

export { GuiError, type GuiErrorCode };

export const GUI_INTENTS = ['state', 'wait', 'show_view', 'navigate', 'refresh', 'select', 'connect', 'disconnect', 'stop', ...SETTINGS_INTENTS, ...TOOLS_INTENTS] as const;
export type GuiIntent = typeof GUI_INTENTS[number];
export type GuiPanel = 'remote' | 'local' | 'local2';
export type GuiView = 'servers' | 'files' | 'other';

interface PanelSource {
    path: string; loading: boolean; selection: Iterable<string>; entriesCount: number;
}
export interface GuiSource {
    version: string; locked: boolean; blocked: boolean; view: GuiView; connected: boolean;
    activeSessionId: string | null;
    sessions: readonly { id: string; name: string; protocol: string; status: string; savedProfileId?: string }[];
    panels: Partial<Record<GuiPanel, PanelSource>>;
    queue: { active: number; pending: number; failed: number };
    /** Safe Settings projection: allowlisted public values only, never secrets. */
    settings?: GuiSettingsProjection;
    tools?: GuiToolsProjection;
}
export interface GuiSnapshot {
    schema_version: 1; state_revision: number; version: string; locked: boolean; blocked: boolean;
    view: GuiView; connected: boolean; active_session_id: string | null;
    sessions: { id: string; name: string; protocol: string; status: string; saved_profile_id?: string }[];
    panels: Partial<Record<GuiPanel, { path: string; loading: boolean; selection: string[];
        selection_count: number; entries_count: number }>>;
    queue: { active: number; pending: number; failed: number };
    settings?: GuiSettingsProjection;
    tools?: GuiToolsProjection;
}
const boundedText = (value: string) => value.slice(0, 4096);
/** Explicit projection: never spread a session, credential form or provider options. */
export function buildGuiSnapshot(source: GuiSource, revision = 0): GuiSnapshot {
    const result: GuiSnapshot = {
        schema_version: 1, state_revision: revision, version: boundedText(source.version),
        locked: source.locked, blocked: source.blocked, view: source.locked ? 'other' : source.view,
        connected: !source.locked && source.connected,
        active_session_id: source.locked ? null : source.activeSessionId,
        sessions: [], panels: {}, queue: { active: 0, pending: 0, failed: 0 },
    };
    if (source.locked) return result;
    // Keep the active session observable even when older tabs fill the cap.
    const sessions = source.sessions.slice(0, 32);
    const active = source.sessions.find(s => s.id === source.activeSessionId);
    if (active && !sessions.includes(active)) sessions[31] = active;
    result.sessions = sessions.map(s => ({
        id: boundedText(s.id), name: boundedText(s.name), protocol: boundedText(s.protocol), status: boundedText(s.status),
        ...(s.savedProfileId ? { saved_profile_id: boundedText(s.savedProfileId) } : {}),
    }));
    for (const panel of ['remote', 'local', 'local2'] as const) {
        const p = source.panels[panel];
        if (!p) continue;
        const selection = Array.from(p.selection);
        result.panels[panel] = { path: boundedText(p.path), loading: p.loading,
            selection: selection.slice(0, 100).map(boundedText), selection_count: selection.length, entries_count: p.entriesCount };
    }
    result.queue = { active: source.queue.active, pending: source.queue.pending, failed: source.queue.failed };
    if (source.settings) result.settings = buildSettingsProjection(source.settings);
    if (source.tools) result.tools = buildToolsProjection(source.tools);
    return result;
}

export interface GuiHandlers {
    showView(view: 'servers' | 'files'): void | Promise<void>;
    navigate(panel: GuiPanel, path: string): Promise<void | string>;
    refresh(panel: GuiPanel): Promise<void>;
    select(panel: GuiPanel, names: string[], mode: 'names' | 'all' | 'none'): void;
    connect?(profileId: string, scope: ConnectScope): Promise<ProfileConnectOutcome>;
    disconnect(): Promise<void>;
    stop(): Promise<void>;
    settingsOpen?(area: SettingsArea, scope: ConnectScope): Promise<void>;
    settingsClose?(scope: ConnectScope): Promise<void>;
    settingsRead?(area: SettingsArea, scope: ConnectScope): Promise<void>;
    settingsUpdate?(area: SettingsArea, set: Record<string, unknown>, scope: ConnectScope): Promise<void>;
    toolsOpen?(tool: ToolPanel | undefined, scope: ConnectScope): Promise<void>;
    toolsRead?(scope: ConnectScope): Promise<void>;
    toolsClose?(scope: ConnectScope): Promise<void>;
}
export interface GuiRequest {
    name: string; args?: Record<string, unknown>; timeout_ms?: number;
    if_revision?: number; pace?: 'watch' | 'fast';
}
export interface GuiReply { ok: boolean; error: GuiErrorCode | null; snapshot: GuiSnapshot; }
export interface GuiLease { owner: string; intent: GuiIntent | null; panel?: GuiPanel; }

function record(value: unknown): asserts value is Record<string, unknown> {
    if (!value || typeof value !== 'object' || Array.isArray(value)) throw new GuiError('invalid_args');
}
function keys(args: Record<string, unknown>, allowed: string[]) {
    if (Object.keys(args).some(key => !allowed.includes(key))) throw new GuiError('invalid_args');
}
function panelArg(args: Record<string, unknown>): GuiPanel {
    if (typeof args.panel !== 'string' || !['remote', 'local', 'local2'].includes(args.panel)) throw new GuiError('invalid_args');
    return args.panel as GuiPanel;
}
export function validateGuiRequest(request: GuiRequest): GuiIntent {
    record(request); keys(request as unknown as Record<string, unknown>, ['name', 'args', 'timeout_ms', 'if_revision', 'pace']);
    if (!GUI_INTENTS.includes(request.name as GuiIntent)) throw new GuiError('unsupported_intent');
    if (request.timeout_ms !== undefined && (!Number.isInteger(request.timeout_ms) || request.timeout_ms < 100 || request.timeout_ms > 30000)) throw new GuiError('invalid_args');
    if (request.if_revision !== undefined && (!Number.isSafeInteger(request.if_revision) || request.if_revision < 0)) throw new GuiError('invalid_args');
    if (request.pace !== undefined && request.pace !== 'watch' && request.pace !== 'fast') throw new GuiError('invalid_args');
    const args = request.args === undefined ? {} : request.args; record(args);
    switch (request.name) {
        case 'state': case 'stop': case 'disconnect': keys(args, []); break;
        case 'connect':
            keys(args, ['profile_id']);
            if (typeof args.profile_id !== 'string' || !/^[A-Za-z0-9_-]{1,256}$/.test(args.profile_id)) throw new GuiError('invalid_args'); break;
        case 'show_view':
            keys(args, ['view']); if (args.view !== 'servers' && args.view !== 'files') throw new GuiError('invalid_args'); break;
        case 'wait':
            keys(args, ['condition']); if (typeof args.condition !== 'string' || !['connected', 'disconnected', 'idle', 'unlocked'].includes(args.condition)) throw new GuiError('invalid_args'); break;
        case 'navigate':
            keys(args, ['panel', 'path']); panelArg(args);
            if (typeof args.path !== 'string' || !args.path.trim() || args.path.length > 4096 || /[\x00-\x1f]/.test(args.path)) throw new GuiError('invalid_args'); break;
        case 'refresh': keys(args, ['panel']); panelArg(args); break;
        case 'select':
            keys(args, ['panel', 'names', 'mode']); panelArg(args);
            if (typeof (args.mode ?? 'names') !== 'string' || !['names', 'all', 'none'].includes((args.mode ?? 'names') as string)) throw new GuiError('invalid_args');
            if (args.mode === 'all' || args.mode === 'none') {
                if (args.names !== undefined) throw new GuiError('invalid_args');
            } else if (!Array.isArray(args.names) || args.names.length > 100 ||
                args.names.some(n => typeof n !== 'string' || !n || n.length > 4096 || /[\x00-\x1f/\\]/.test(n))) throw new GuiError('invalid_args');
            break;
        case 'settings_open': case 'settings_read':
            keys(args, ['area']); settingsAreaArg(args); break;
        case 'settings_close': keys(args, []); break;
        case 'tools_read': case 'tools_close': keys(args, []); break;
        case 'tools_open':
            keys(args, ['tool']);
            if (args.tool !== undefined && !(TOOL_PANELS as readonly unknown[]).includes(args.tool)) throw new GuiError('invalid_args');
            break;
        case 'settings_update':
            keys(args, ['area', 'set']); settingsAreaArg(args);
            // Deep field/value validation against the closed per-area schema;
            // unknown or secret-adjacent keys are refused here, before dispatch.
            validateSettingsSet(args.area as SettingsArea, args.set);
            break;
    }
    return request.name as GuiIntent;
}
function settingsAreaArg(args: Record<string, unknown>): SettingsArea {
    if (args.area !== 'general' && args.area !== 'ai') throw new GuiError('invalid_args');
    return args.area;
}

/** In-app semantic controller. No DOM click/eval, secret or filesystem-write intent. */
export class GuiController {
    private revision = 0;
    private fingerprint = '';
    private epoch = 0;
    private busy = false;
    private disposed = false;
    private pending = 0;
    private lease: GuiLease | null = null;
    private connectScope: ConnectScope | null = null;
    private settingsScope: ConnectScope | null = null;
    private expiry: ReturnType<typeof setTimeout> | undefined;
    constructor(private readonly source: () => GuiSource, private readonly handlers: () => GuiHandlers,
        private readonly changed: (lease: GuiLease | null) => void,
        private readonly audit: (intent: GuiIntent, ok: boolean, owner: string) => void = () => {}) {}
    get connecting(): boolean { return this.connectScope !== null; }
    state(): GuiSnapshot {
        const snapshot = buildGuiSnapshot(this.source());
        const fingerprint = JSON.stringify(snapshot);
        if (fingerprint !== this.fingerprint) { this.revision++; this.fingerprint = fingerprint; }
        snapshot.state_revision = this.revision;
        return snapshot;
    }
    interrupt(): void {
        this.connectScope?.cancel(new GuiError('lease_interrupted'));
        this.settingsScope?.cancel(new GuiError('lease_interrupted'));
        this.epoch++; this.lease = null; clearTimeout(this.expiry); this.changed(null);
    }
    dispose(): void { this.disposed = true; this.interrupt(); }
    private guard(epoch: number, deadline: number, checkEpoch = true): void {
        if (this.disposed || (checkEpoch && epoch !== this.epoch)) throw new GuiError('lease_interrupted');
        if (Date.now() >= deadline) throw new GuiError('gui_timeout');
    }
    private async delay(ms: number, epoch: number, deadline: number, checkEpoch = true): Promise<void> {
        const end = Date.now() + ms;
        while (Date.now() < end) {
            this.guard(epoch, deadline, checkEpoch);
            await new Promise(resolve => setTimeout(resolve, Math.min(25, end - Date.now())));
        }
        this.guard(epoch, deadline, checkEpoch);
    }
    async run(input: GuiRequest, owner = 'AeroAgent', brokerDeadline = Infinity, brokerId?: string): Promise<GuiReply> {
        let intent: GuiIntent | undefined;
        let ownsLane = false;
        let laneEpoch: number | undefined;
        let handlerSettled = true;
        let succeeded = false;
        let returned = false;
        let connectScope: ConnectScope | undefined;
        let settingsScope: ConnectScope | undefined;
        if (this.pending >= 16) return { ok: false, error: 'busy', snapshot: this.state() };
        this.pending++;
        try {
            intent = validateGuiRequest(input);
            // Copy only validated request data before any asynchronous boundary.
            const request = structuredClone(input);
            const args = request.args ?? {};
            // The claimed broker budget may be shorter than the validated request timeout.
            const deadline = Math.min(Date.now() + (request.timeout_ms ?? 10000), brokerDeadline);
            this.guard(this.epoch, deadline);
            const snapshot = this.state();
            if (request.if_revision !== undefined && request.if_revision !== snapshot.state_revision) throw new GuiError('stale_state');
            if (intent === 'state') { succeeded = true; return { ok: true, error: null, snapshot }; }
            if (intent === 'stop') {
                this.interrupt();
                const epoch = this.epoch;
                let settled = false;
                let failure: unknown;
                void this.handlers().stop().catch(error => { failure = error; }).finally(() => { settled = true; });
                while (!settled) await this.delay(25, epoch, deadline, false);
                if (failure) throw failure;
                succeeded = true;
                return { ok: true, error: null, snapshot: this.state() };
            }
            if (this.disposed) throw new GuiError('lease_interrupted');
            if (snapshot.locked && !(intent === 'wait' && args.condition === 'unlocked')) throw new GuiError('locked');
            const epoch = this.epoch;
            if (intent === 'wait') {
                while (true) {
                    this.guard(epoch, deadline);
                    const s = this.state();
                    if (s.locked && args.condition !== 'unlocked') throw new GuiError('locked');
                    const reached = args.condition === 'connected' ? s.connected : args.condition === 'disconnected' ? !s.connected :
                        args.condition === 'unlocked' ? !s.locked : !s.locked && !s.blocked &&
                            Object.values(s.panels).every(p => !p?.loading) && s.queue.active === 0 && s.queue.pending === 0;
                    if (reached) { succeeded = true; return { ok: true, error: null, snapshot: s }; }
                    await this.delay(50, epoch, deadline);
                }
            }
            if (snapshot.blocked || (['navigate', 'refresh', 'select'].includes(intent) && snapshot.view !== 'files')) throw new GuiError('blocked');
            // The owned Settings surface is exclusive: while it is open, only its
            // own typed operations run; every other mutation stays blocked.
            if (snapshot.settings?.open && !(SETTINGS_INTENTS as readonly string[]).includes(intent)) throw new GuiError('blocked');
            if (intent !== 'tools_read' && (TOOLS_INTENTS as readonly string[]).includes(intent) && snapshot.tools?.protected) throw new GuiError('blocked');
            if (this.busy || (this.lease && this.lease.owner !== owner)) throw new GuiError('busy');
            if ((intent === 'navigate' || intent === 'refresh' || intent === 'select') &&
                (!snapshot.panels[panelArg(args)] || snapshot.panels[panelArg(args)]?.loading)) throw new GuiError('blocked');
            if (args.panel === 'remote' && !snapshot.connected) throw new GuiError('not_connected');
            this.busy = true; ownsLane = true; laneEpoch = epoch;
            this.lease = { owner, intent, ...(args.panel ? { panel: panelArg(args) } : {}) }; clearTimeout(this.expiry); this.changed(this.lease);
            // Watch pace yields after the banner renders. Revalidate before acting.
            await this.delay(request.pace === 'fast' ? 0 : 250, epoch, deadline);
            if (this.state().state_revision !== snapshot.state_revision) throw new GuiError('stale_state');
            const h = this.handlers();
            handlerSettled = false;
            let handlerError: unknown;
            let connectOutcome: ProfileConnectOutcome | undefined;
            let navigationPath: void | string = undefined;
            if ([...SETTINGS_INTENTS, ...TOOLS_INTENTS].includes(intent as typeof SETTINGS_INTENTS[number] | typeof TOOLS_INTENTS[number])) {
                settingsScope = new ConnectScope(() => {
                    this.guard(epoch, deadline);
                    if (this.source().locked) throw new GuiError('locked');
                    if (this.source().blocked) throw new GuiError('blocked');
                }, undefined, brokerId);
                this.settingsScope = settingsScope;
            }
            const operation = async () => {
                switch (intent) {
                    case 'connect':
                        if (!h.connect) throw new GuiError('blocked');
                        connectScope = new ConnectScope(() => {
                            this.guard(epoch, deadline);
                            if (this.source().locked) throw new GuiError('locked');
                        });
                        this.connectScope = connectScope;
                        connectOutcome = await h.connect(args.profile_id as string, connectScope);
                        if (connectOutcome === 'pending_human') throw new GuiError('pending_human');
                        if (connectOutcome !== 'connected') throw new GuiError('action_failed');
                        break;
                    case 'show_view': await h.showView(args.view as 'servers' | 'files'); break;
                    case 'navigate': navigationPath = await h.navigate(panelArg(args), args.path as string); break;
                    case 'refresh': await h.refresh(panelArg(args)); break;
                    case 'select': h.select(panelArg(args), (args.names ?? []) as string[], (args.mode ?? 'names') as 'names' | 'all' | 'none'); break;
                    case 'disconnect': await h.disconnect(); break;
                    case 'settings_open':
                        if (!h.settingsOpen) throw new GuiError('blocked');
                        await h.settingsOpen(settingsAreaArg(args), settingsScope!); break;
                    case 'settings_close':
                        if (!h.settingsClose) throw new GuiError('blocked');
                        await h.settingsClose(settingsScope!); break;
                    case 'settings_read':
                        if (!h.settingsRead) throw new GuiError('blocked');
                        await h.settingsRead(settingsAreaArg(args), settingsScope!); break;
                    case 'settings_update':
                        if (!h.settingsUpdate) throw new GuiError('blocked');
                        await h.settingsUpdate(settingsAreaArg(args), args.set as Record<string, unknown>, settingsScope!); break;
                    case 'tools_open':
                        if (!h.toolsOpen) throw new GuiError('blocked');
                        await h.toolsOpen(args.tool as ToolPanel | undefined, settingsScope!); break;
                    case 'tools_read':
                        if (!h.toolsRead) throw new GuiError('blocked');
                        await h.toolsRead(settingsScope!); break;
                    case 'tools_close':
                        if (!h.toolsClose) throw new GuiError('blocked');
                        await h.toolsClose(settingsScope!); break;
                }
            };
            void operation().catch(error => { handlerError = error; }).finally(() => {
                handlerSettled = true;
                if (returned) this.busy = false;
            });
            while (!handlerSettled) {
                if (intent === 'connect' && this.state().blocked) throw new GuiError('pending_human');
                await this.delay(25, epoch, deadline);
            }
            if (handlerError) throw handlerError;
            // React setters may commit after their callback Promise resolves.
            // Report view/selection/disconnect only when the committed projection agrees.
            const committed = (state: GuiSnapshot): boolean => {
                if (intent === 'connect') {
                    return !state.blocked && state.connected && state.view === 'files' && state.sessions.some(session =>
                        session.id === state.active_session_id && session.saved_profile_id === args.profile_id && session.status === 'connected') &&
                        !!state.panels.remote && !state.panels.remote.loading;
                }
                if (intent === 'navigate' || intent === 'refresh') {
                    const p = state.panels[panelArg(args)];
                    return !!p && !p.loading && (typeof navigationPath !== 'string' || p.path === boundedText(navigationPath));
                }
                if (intent === 'show_view') return state.view === args.view;
                if (intent === 'disconnect') return !state.connected;
                if (intent === 'settings_open') return state.settings?.open === args.area;
                if (intent === 'tools_open') return !!state.tools?.open && (args.tool === undefined || state.tools.visible_panels.includes(args.tool as ToolPanel));
                if (intent === 'tools_read') return !!state.tools;
                if (intent === 'tools_close') return !!state.tools && !state.tools.open;
                if (intent === 'settings_close') return !state.settings?.open;
                if (intent === 'settings_read') {
                    // The handler already re-read the persisted public config; the
                    // projection must carry that area's values in the reply.
                    return args.area === 'general' ? !!state.settings?.general : !!state.settings?.ai;
                }
                if (intent === 'settings_update') {
                    return !!state.settings && settingsUpdateCommitted(args.area as SettingsArea, args.set, state.settings);
                }
                if (intent === 'select') {
                    const p = state.panels[panelArg(args)];
                    if (!p) return false;
                    if (args.mode === 'all') return p.selection_count === p.entries_count;
                    if (args.mode === 'none') return p.selection_count === 0;
                    const names = new Set(args.names as string[]);
                    return p.selection_count === names.size && p.selection.every(name => names.has(name));
                }
                return true;
            };
            while (!committed(this.state())) {
                if (intent === 'connect' && this.state().blocked) throw new GuiError('pending_human');
                await this.delay(25, epoch, deadline);
            }
            if (intent === 'navigate') {
                const panel = panelArg(args);
                const before = snapshot.panels[panel]?.path;
                const after = this.state().panels[panel]?.path;
                if (navigationPath === undefined && args.path !== '.' && args.path !== before && before === after) throw new GuiError('action_failed');
            }
            // A handler can already have acted when cancellation or timeout arrives.
            // Keep the mutation lane owned until that handler actually settles.
            this.guard(epoch, deadline);
            this.lease = { owner, intent: null }; this.changed(this.lease);
            this.expiry = setTimeout(() => this.interrupt(), 60000);
            succeeded = true;
            return { ok: true, error: null, snapshot: this.state() };
        } catch (error) {
            settingsScope?.cancel(error instanceof GuiError ? error : new GuiError('action_failed'));
            // A failed/pending-human connect leaves the human dialog intact.
            if (connectScope && (!handlerSettled || (error instanceof GuiError && ['lease_interrupted', 'gui_timeout', 'locked'].includes(error.code)))) {
                connectScope.cancel(error instanceof GuiError ? error : new GuiError('action_failed'));
            }
            if (this.connectScope === connectScope) this.connectScope = null;
            if (ownsLane && laneEpoch === this.epoch) this.interrupt();
            return { ok: false, error: error instanceof GuiError ? error.code : 'action_failed', snapshot: this.state() };
        } finally {
            if (this.settingsScope === settingsScope) this.settingsScope = null;
            returned = true;
            if (this.connectScope === connectScope) this.connectScope = null;
            this.pending--;
            if (ownsLane && handlerSettled) this.busy = false;
            if (intent) this.audit(intent, succeeded, owner);
        }
    }
}
