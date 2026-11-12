// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import type { RemoteSyncDeps } from '../remoteSyncRunner';
const fixtures = new WeakMap<RemoteSyncDeps['invoke'], ReturnType<typeof createIntentFixture>>();
export function createIntentFixture() {
    const generations = new Map<string,string>();
    let sequence = 0;
    return { generations, wrap(invoke: RemoteSyncDeps['invoke']): RemoteSyncDeps['invoke'] {
        return async <T,>(cmd: string, args?: Record<string,unknown>): Promise<T> => {
            if (cmd === 'sync_intent_snapshot_cmd') return Object.fromEntries(generations) as T;
            if (cmd === 'sync_intent_begin_cmd') return (args!.paths as string[]).map(path => {
                const token = `intent-${++sequence}-${path}`;
                generations.set(path,token);
                return token;
            }) as T;
            return invoke<T>(cmd,args);
        };
    } };
}
export function wrapIntentInvoke(invoke: RemoteSyncDeps['invoke']): RemoteSyncDeps['invoke'] {
    let fixture=fixtures.get(invoke);
    if (!fixture) { fixture=createIntentFixture(); fixtures.set(invoke,fixture); }
    return fixture.wrap(invoke);
}
