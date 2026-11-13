// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import type { RemoteSyncDeps } from '../remoteSyncRunner';
const fixtures = new WeakMap<RemoteSyncDeps['invoke'], ReturnType<typeof createIntentFixture>>();
export function createIntentFixture() {
    const generations = new Map<string,string>();
    const leases = new Map<string, string>();
    const beginArgs: Record<string, unknown>[] = [];
    let sequence = 0;
    return { generations, beginArgs, wrap(invoke: RemoteSyncDeps['invoke']): RemoteSyncDeps['invoke'] {
        return async <T,>(cmd: string, args?: Record<string,unknown>): Promise<T> => {
            // The page generation the native side gives: the test's own answer, else 0.
            if (cmd === 'sync_page_epoch_cmd') return ((await invoke<T>(cmd, args)) ?? 0) as T;
            if (cmd === 'sync_run_lease_begin_cmd') {
                beginArgs.push({ ...args });
                // As natively: a begin that names no page generation is refused.
                if (typeof args?.pageEpoch !== 'number') throw new Error('SYNC_RUN_PAGE_REPLACED');
                const pair = JSON.stringify([args!.localPath, args!.remotePath]);
                if (leases.has(pair)) throw new Error('SYNC_RUN_BUSY');
                const token = `run-lease-${++sequence}`;
                leases.set(pair, token);
                return token as T;
            }
            if (cmd === 'sync_run_lease_end_cmd') {
                for (const [pair, token] of leases) if (token === args!.token) leases.delete(pair);
                return undefined as T;
            }
            if (cmd === 'sync_intent_snapshot_cmd') return Object.fromEntries(generations) as T;
            if (cmd === 'sync_intent_begin_cmd') {
                const pair = JSON.stringify([args!.localPath, args!.remotePath]);
                if (!leases.has(pair) || leases.get(pair) !== args!.syncRunLease) throw new Error('SYNC_RUN_BUSY');
                return (args!.paths as string[]).map(path => {
                const token = `intent-${++sequence}-${path}`;
                generations.set(path,token);
                return token;
                }) as T;
            }
            return invoke<T>(cmd,args);
        };
    } };
}
export function wrapIntentInvoke(invoke: RemoteSyncDeps['invoke']): RemoteSyncDeps['invoke'] {
    let fixture=fixtures.get(invoke);
    if (!fixture) { fixture=createIntentFixture(); fixtures.set(invoke,fixture); }
    return fixture.wrap(invoke);
}
