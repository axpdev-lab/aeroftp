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
    AI_ADVANCED_KEY_MAP, buildAiProjection,
    type AiSettingsProjection, type AiSettingsUpdate,
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

/**
 * Apply a validated bounded controller update to the persisted AI settings.
 * Only public fields change; provider records are rewritten field-by-field
 * without an apiKey, and no keyring record is enqueued, overwritten or
 * cleared. Unknown provider/model ids are invalid_args; an update against an
 * account that has no AI settings yet is action_failed.
 */
export function applyAgentAiSettingsUpdate(update: AiSettingsUpdate, scope?: ConnectScope): Promise<AISettings> {
    return updateAiSettingsBlob(existing => {
        if (!existing || !Array.isArray(existing.providers)) throw new GuiError('action_failed');
        const next: AISettings = {
            ...existing,
            providers: existing.providers.map(p => ({ ...p, apiKey: undefined })),
            models: (existing.models || []).map(m => ({ ...m })),
            advancedSettings: { ...(existing.advancedSettings || getDefaultAISettings().advancedSettings) },
        };
        if (update.provider_enabled) {
            const provider = next.providers.find(p => p.id === update.provider_enabled!.id);
            if (!provider) throw new GuiError('invalid_args');
            provider.isEnabled = update.provider_enabled.enabled;
            provider.updatedAt = new Date();
        }
        if (update.model_enabled) {
            const model = next.models.find(m => m.id === update.model_enabled!.id);
            if (!model) throw new GuiError('invalid_args');
            model.isEnabled = update.model_enabled.enabled;
        }
        if (update.model_default) {
            const model = next.models.find(m => m.id === update.model_default!.id);
            if (!model) throw new GuiError('invalid_args');
            // Same semantics as the panel's default toggle: one default per provider.
            next.models = next.models.map(m => ({
                ...m,
                isDefault: m.providerId === model.providerId ? m.id === model.id : m.isDefault,
            }));
        }
        if (update.advanced) {
            for (const [wireKey, value] of Object.entries(update.advanced)) {
                const storedKey = AI_ADVANCED_KEY_MAP[wireKey];
                if (!storedKey) throw new GuiError('invalid_args');
                (next.advancedSettings as Record<string, unknown>)[storedKey] = value;
            }
        }
        return next;
    }, scope);
}
