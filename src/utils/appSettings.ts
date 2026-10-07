// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { secureGetConfigStrict, secureGetWithFallback, secureStore } from './secureStorage';
import type { ConnectScope } from '../gui/connectScope';

export const APP_SETTINGS_EVENT = 'aeroftp-settings-changed';
export type Settings = Record<string, unknown>;
let pending: Promise<void> = Promise.resolve();

function publish(updated: Settings, scope?: ConnectScope): Settings {
    scope?.assert();
    try { localStorage.setItem('aeroftp_settings', JSON.stringify(updated)); } catch { /* cache unavailable */ }
    window.dispatchEvent(new CustomEvent(APP_SETTINGS_EVENT, { detail: updated }));
    return updated;
}
/** Use the same queue as human saves, but commit through the approved broker only. */
export function commitAppSettings(commit: () => Promise<Settings>, scope: ConnectScope): Promise<Settings> {
    const operation = pending.then(async () => publish(await scope.step(commit), scope));
    pending = operation.then(() => undefined, () => undefined);
    return operation;
}

/** Serialize profile-view mutations, reading the latest blob inside the queue. */
export function updateAppSettings(mutate: (existing: Settings | null) => Settings, scope?: ConnectScope, strict = false): Promise<Settings> {
    const operation = pending.then(async () => {
        const existing = scope ? await scope.step(() => secureGetConfigStrict<Settings>('app_settings')) : strict ? await secureGetConfigStrict<Settings>('app_settings') :
            await secureGetWithFallback<Settings>('app_settings', 'aeroftp_settings');
        const updated = mutate(existing);
        if (scope) await scope.step(() => secureStore('app_settings', updated));
        else await secureStore('app_settings', updated);
        // A failed vault write must never become the next fallback value.
        return publish(updated, scope);
    });
    // A rejected mutation does not block subsequent saves.
    pending = operation.then(() => undefined, () => undefined);
    return operation;
}
