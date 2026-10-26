// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { secureGetConfigStrict, secureStore } from './secureStorage';
import type { ConnectScope } from '../gui/connectScope';

export const APP_SETTINGS_EVENT = 'aeroftp-settings-changed';
export type Settings = Record<string, unknown>;
let pending: Promise<void> = Promise.resolve();

// The settings live in the vault only (docs/UNIVERSAL-VAULT.md). This key held
// them in plaintext before the vault (v1.9.0), and again as a write-through
// copy in 4.2.3 and 4.2.4: it is read only as a migration source, never
// written, and removed once the vault holds the settings.
const LEGACY_SETTINGS_KEY = 'aeroftp_settings';

function readLegacySettings(): Settings | null {
    try {
        const raw = localStorage.getItem(LEGACY_SETTINGS_KEY);
        return raw ? JSON.parse(raw) as Settings : null;
    } catch {
        return null;
    }
}

function removeLegacySettings(): void {
    try { localStorage.removeItem(LEGACY_SETTINGS_KEY); } catch { /* storage unavailable */ }
}

function publish(updated: Settings, scope?: ConnectScope): Settings {
    scope?.assert();
    // Reached only after a confirmed vault write, so a plaintext copy goes.
    removeLegacySettings();
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
        // Only an absent blob counts as none. A vault that is still starting is
        // waited for, and one that cannot be read rejects: a write based on
        // either would replace every setting with the one being changed.
        const existing = scope ? await scope.step(() => secureGetConfigStrict<Settings>('app_settings')) : strict ? await secureGetConfigStrict<Settings>('app_settings') :
            (await secureGetConfigStrict<Settings>('app_settings', { awaitReady: true })) ?? readLegacySettings();
        const updated = mutate(existing);
        if (scope) await scope.step(() => secureStore('app_settings', updated));
        else await secureStore('app_settings', updated);
        return publish(updated, scope);
    });
    // A rejected mutation does not block subsequent saves.
    pending = operation.then(() => undefined, () => undefined);
    return operation;
}

/** One-way migration of settings found outside the vault. It writes only when
 *  the vault holds none, and reads strictly: a vault that is not ready rejects,
 *  so the caller retries later instead of taking it for an empty one. When the
 *  vault already holds the settings, only the plaintext copy is removed. */
export async function migrateLegacySettings(found: Settings): Promise<void> {
    if (await secureGetConfigStrict<Settings>('app_settings')) {
        removeLegacySettings();
        return;
    }
    await updateAppSettings(existing => existing ?? found, undefined, true);
}
