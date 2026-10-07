// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Shared serialized persistence for the PUBLIC AI settings blob
// (`config_ai_settings` in the vault, `aeroftp_ai_settings` as the transient
// localStorage fallback). API keys are never part of this blob: they live in
// the OS keyring under `ai_apikey_<providerId>` and are written only by a
// deliberate human edit in AISettingsPanel. Every writer is defensively
// stripped here, so no caller can persist a key into the public blob.
//
// Both the human panel save and the GUI controller's bounded AI update run
// through the same queue, so the two never interleave a read-modify-write.

import { secureGetConfigStrict, secureGetWithFallback, secureStore } from './secureStorage';
import type { ConnectScope } from '../gui/connectScope';
import { GuiError } from '../gui/errors';
import {
    buildAiProjection, type AiSettingsProjection,
} from '../gui/settingsSchema';
import { getDefaultAISettings, type AISettings } from '../types/ai';

export const AI_SETTINGS_EVENT = 'aeroftp-ai-settings-changed';
export const AI_SETTINGS_KEY = 'aeroftp_ai_settings';
export const AI_SETTINGS_VAULT_KEY = 'ai_settings';
/** The AI Settings panel reports its real mounted visibility on this event. */
export const AI_SETTINGS_OPEN_EVENT = 'aeroftp-ai-settings-open';

const stripApiKeys = (settings: AISettings): AISettings => ({
    ...settings,
    providers: (settings.providers || []).map(p => ({ ...p, apiKey: undefined })),
});

let pending: Promise<void> = Promise.resolve();

/** The broker returns persisted public config; publication follows account/lease checks. */
export function commitAiSettingsBlob(commit: () => Promise<AISettings>, scope: ConnectScope): Promise<AISettings> {
    const operation = pending.then(async () => {
        const settings = await scope.step(commit);
        scope.assert();
        if (!Array.isArray(settings.providers) || !Array.isArray(settings.models)) throw new GuiError('action_failed');
        const stripped = stripApiKeys(settings);
        try { localStorage.removeItem(AI_SETTINGS_KEY); } catch { /* best effort */ }
        window.dispatchEvent(new CustomEvent(AI_SETTINGS_EVENT, { detail: stripped }));
        return stripped;
    });
    pending = operation.then(() => undefined, () => undefined);
    return operation;
}

/** Serialize public-blob mutations, reading the latest blob inside the queue. */
export function updateAiSettingsBlob(mutate: (existing: AISettings | null) => AISettings, scope?: ConnectScope): Promise<AISettings> {
    const operation = pending.then(async () => {
        const existing = scope ? await scope.step(() => secureGetConfigStrict<AISettings>('ai_settings')) :
            await secureGetWithFallback<AISettings>(AI_SETTINGS_VAULT_KEY, AI_SETTINGS_KEY);
        const stripped = stripApiKeys(mutate(existing));
        // localStorage stays the write-through fallback; the vault write is
        // awaited, and only a successful one clears the fallback copy.
        if (!scope) {
            try { localStorage.setItem(AI_SETTINGS_KEY, JSON.stringify(stripped)); } catch { /* cache unavailable */ }
        }
        if (scope) await scope.step(() => secureStore(AI_SETTINGS_VAULT_KEY, stripped));
        else await secureStore(AI_SETTINGS_VAULT_KEY, stripped);
        scope?.assert();
        try { localStorage.removeItem(AI_SETTINGS_KEY); } catch { /* best effort */ }
        window.dispatchEvent(new CustomEvent(AI_SETTINGS_EVENT, { detail: stripped }));
        return stripped;
    });
    // A rejected mutation does not block subsequent saves.
    pending = operation.then(() => undefined, () => undefined);
    return operation;
}

/** Read the persisted public blob; absent config resolves to the empty defaults. */
export async function readAiSettingsBlob(scope?: ConnectScope): Promise<AISettings> {
    const existing = scope ? await scope.step(() => secureGetConfigStrict<AISettings>('ai_settings')) :
        await secureGetWithFallback<AISettings>(AI_SETTINGS_VAULT_KEY, AI_SETTINGS_KEY);
    return existing ?? getDefaultAISettings();
}

export async function readAiPublicProjection(scope?: ConnectScope): Promise<AiSettingsProjection> {
    return buildAiProjection(await readAiSettingsBlob(scope));
}
