// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Closed safe Settings surface for the in-app GUI controller.
//
// Only allowlisted, non-secret public preferences are representable here:
// general presentation/browser toggles and AI provider/model identity,
// enabled flags, per-provider default model and bounded generation
// parameters. Credential stores, OAuth client fields, API keys, vault/master
// settings, startup/update/analytics policy, confirmBeforeDelete, prompts,
// macros, plugin/MCP management and approval levels are NOT in the schema,
// so no request can name them: unknown keys are refused as invalid_args.
// Projections are built field-by-field; a provider or settings object is
// never spread into the snapshot (AIProvider carries apiKey).

import { GuiError } from './errors';
import { KNOWN_FONT_FAMILIES } from '../utils/fontPresets';
import type { AISettings } from '../types/ai';

export type SettingsArea = 'general' | 'ai';
export const SETTINGS_INTENTS = ['settings_open', 'settings_read', 'settings_update', 'settings_close'] as const;

export type SettingsScalar = boolean | number | string;

export interface AiProviderProjection { id: string; name: string; type: string; enabled: boolean; }
export interface AiModelProjection { id: string; provider_id: string; name: string; enabled: boolean; is_default: boolean; }
export interface AiSettingsProjection {
    providers: AiProviderProjection[];
    models: AiModelProjection[];
    default_model_id: string | null;
    advanced: Record<string, SettingsScalar>;
}
export interface GuiSettingsProjection {
    open: SettingsArea | null;
    general: Record<string, SettingsScalar> | null;
    ai: AiSettingsProjection | null;
}

export const MAX_SETTINGS_PROVIDERS = 32;
export const MAX_SETTINGS_MODELS = 64;
const MAX_SETTINGS_STRING = 256;
const MAX_GENERAL_KEYS = 40;

const boundedString = (value: string) => value.slice(0, MAX_SETTINGS_STRING);
const hasOwn = (object: object, key: string) => Object.prototype.hasOwnProperty.call(object, key);

const boolField = (value: unknown): boolean => {
    if (typeof value !== 'boolean') throw new GuiError('invalid_args');
    return value;
};
const oneOfField = (values: readonly string[]) => (value: unknown): string => {
    if (typeof value !== 'string' || !values.includes(value)) throw new GuiError('invalid_args');
    return value;
};
const intRangeField = (min: number, max: number) => (value: unknown): number => {
    if (typeof value !== 'number' || !Number.isInteger(value) || value < min || value > max) throw new GuiError('invalid_args');
    return value;
};
const numRangeField = (min: number, max: number) => (value: unknown): number => {
    if (typeof value !== 'number' || !Number.isFinite(value) || value < min || value > max) throw new GuiError('invalid_args');
    return value;
};

/** Allowlisted general preferences with their UI-validated bounds. */
export const GENERAL_SETTINGS_FIELDS: Record<string, (value: unknown) => SettingsScalar> = {
    showHiddenFiles: boolField,
    showStatusBar: boolField,
    showTransferProgress: boolField,
    compactMode: boolField,
    swapPanels: boolField,
    sortFoldersFirst: boolField,
    showFileExtensions: boolField,
    showToastNotifications: boolField,
    discoverHealthCheck: boolField,
    dateFormat: oneOfField(['localized', 'iso', 'dmy', 'mdy']),
    cardLayout: oneOfField(['compact', 'detailed']),
    favoriteMarker: oneOfField(['star', 'heart']),
    fontSize: intRangeField(10, 22),
    introHubIconSize: intRangeField(18, 32),
    fontFamily: oneOfField(KNOWN_FONT_FAMILIES),
};

/** Allowlisted bounded AI generation parameters (snake_case on the wire). */
export const AI_ADVANCED_FIELDS: Record<string, (value: unknown) => SettingsScalar> = {
    temperature: numRangeField(0, 2),
    max_tokens: intRangeField(256, 32768),
    top_p: numRangeField(0, 1),
    top_k: intRangeField(1, 100),
    conversation_style: oneOfField(['precise', 'balanced', 'creative']),
    response_style: oneOfField(['default', 'concise', 'explanatory', 'learning']),
};

/** Wire key (snake_case) to persisted AISettings.advancedSettings key (camelCase). */
export const AI_ADVANCED_KEY_MAP: Record<string, string> = {
    temperature: 'temperature',
    max_tokens: 'maxTokens',
    top_p: 'topP',
    top_k: 'topK',
    conversation_style: 'conversationStyle',
    response_style: 'responseStyle',
};

export interface AiSettingsUpdate {
    provider_enabled?: { id: string; enabled: boolean };
    model_enabled?: { id: string; enabled: boolean };
    model_default?: { id: string };
    advanced?: Record<string, SettingsScalar>;
}

const recordId = (value: unknown): string => {
    if (typeof value !== 'string' || !value || value.length > 128 || /[\x00-\x1f]/.test(value)) throw new GuiError('invalid_args');
    return value;
};

function validateAiSet(raw: unknown): AiSettingsUpdate {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new GuiError('invalid_args');
    const set = raw as Record<string, unknown>;
    for (const key of Object.keys(set)) {
        if (!['provider_enabled', 'model_enabled', 'model_default', 'advanced'].includes(key)) throw new GuiError('invalid_args');
    }
    const out: AiSettingsUpdate = {};
    if (set.provider_enabled !== undefined) {
        const v = set.provider_enabled as Record<string, unknown>;
        if (!v || typeof v !== 'object' || Array.isArray(v) || Object.keys(v).some(k => !['id', 'enabled'].includes(k))) throw new GuiError('invalid_args');
        out.provider_enabled = { id: recordId(v.id), enabled: boolField(v.enabled) };
    }
    if (set.model_enabled !== undefined) {
        const v = set.model_enabled as Record<string, unknown>;
        if (!v || typeof v !== 'object' || Array.isArray(v) || Object.keys(v).some(k => !['id', 'enabled'].includes(k))) throw new GuiError('invalid_args');
        out.model_enabled = { id: recordId(v.id), enabled: boolField(v.enabled) };
    }
    if (set.model_default !== undefined) {
        const v = set.model_default as Record<string, unknown>;
        if (!v || typeof v !== 'object' || Array.isArray(v) || Object.keys(v).some(k => k !== 'id')) throw new GuiError('invalid_args');
        out.model_default = { id: recordId(v.id) };
    }
    if (set.advanced !== undefined) {
        const v = set.advanced as Record<string, unknown>;
        if (!v || typeof v !== 'object' || Array.isArray(v)) throw new GuiError('invalid_args');
        const advanced: Record<string, SettingsScalar> = {};
        for (const [key, value] of Object.entries(v)) {
            const validator = AI_ADVANCED_FIELDS[key];
            if (!hasOwn(AI_ADVANCED_FIELDS, key)) throw new GuiError('invalid_args');
            advanced[key] = validator(value);
        }
        if (Object.keys(advanced).length === 0) throw new GuiError('invalid_args');
        out.advanced = advanced;
    }
    if (Object.keys(out).length === 0) throw new GuiError('invalid_args');
    return out;
}

function validateGeneralSet(raw: unknown): Record<string, SettingsScalar> {
    if (!raw || typeof raw !== 'object' || Array.isArray(raw)) throw new GuiError('invalid_args');
    const out: Record<string, SettingsScalar> = {};
    for (const [key, value] of Object.entries(raw as Record<string, unknown>)) {
        const validator = GENERAL_SETTINGS_FIELDS[key];
        if (!hasOwn(GENERAL_SETTINGS_FIELDS, key)) throw new GuiError('invalid_args');
        out[key] = validator(value);
    }
    if (Object.keys(out).length === 0) throw new GuiError('invalid_args');
    return out;
}

/** Strictly validate a settings_update `set` against the closed per-area schema. */
export function validateSettingsSet(area: SettingsArea, raw: unknown): Record<string, SettingsScalar> | AiSettingsUpdate {
    return area === 'general' ? validateGeneralSet(raw) : validateAiSet(raw);
}

/** Pick the allowlisted public general preferences out of a raw stored record. */
export function buildGeneralProjection(raw: Record<string, unknown>): Record<string, SettingsScalar> {
    const out: Record<string, SettingsScalar> = {};
    for (const [key, validator] of Object.entries(GENERAL_SETTINGS_FIELDS)) {
        if (!hasOwn(raw, key)) continue;
        try { out[key] = validator(raw[key]); } catch { /* a stored value outside bounds is omitted, not echoed */ }
        if (Object.keys(out).length >= MAX_GENERAL_KEYS) break;
    }
    return out;
}

/** Field-by-field safe AI projection: identity, enabled/default flags, bounded parameters. Never includes apiKey or endpoint URLs. */
export function buildAiProjection(settings: AISettings): AiSettingsProjection {
    const providers = (settings.providers || []).slice(0, MAX_SETTINGS_PROVIDERS).map(p => ({
        id: boundedString(String(p.id)), name: boundedString(String(p.name)), type: boundedString(String(p.type)), enabled: p.isEnabled === true,
    }));
    const models = (settings.models || []).slice(0, MAX_SETTINGS_MODELS).map(m => ({
        id: boundedString(String(m.id)), provider_id: boundedString(String(m.providerId)), name: boundedString(String(m.name)),
        enabled: m.isEnabled === true, is_default: m.isDefault === true,
    }));
    const advanced: Record<string, SettingsScalar> = {};
    const source = (settings.advancedSettings || {}) as Record<string, unknown>;
    for (const [wireKey, storedKey] of Object.entries(AI_ADVANCED_KEY_MAP)) {
        if (!(storedKey in source)) continue;
        try { advanced[wireKey] = AI_ADVANCED_FIELDS[wireKey](source[storedKey]); } catch { /* omit out-of-bounds stored values */ }
    }
    const defaultModelId = typeof settings.defaultModelId === 'string' ? boundedString(settings.defaultModelId) : null;
    return { providers, models, default_model_id: defaultModelId, advanced };
}

/** Re-project even an already projected source: never trust extra runtime fields. */
export function buildSettingsProjection(source: GuiSettingsProjection): GuiSettingsProjection {
    const ai = source.ai;
    const advanced: Record<string, SettingsScalar> = {};
    if (ai) {
        for (const [key, validator] of Object.entries(AI_ADVANCED_FIELDS)) {
            if (!ai.advanced || !hasOwn(ai.advanced, key)) continue;
            try { advanced[key] = validator(ai.advanced[key]); } catch { /* omit invalid values */ }
        }
    }
    return {
        open: source.open === 'general' || source.open === 'ai' ? source.open : null,
        general: source.general ? buildGeneralProjection(source.general) : null,
        ai: ai ? {
            providers: Array.isArray(ai.providers) ? ai.providers.slice(0, MAX_SETTINGS_PROVIDERS).map(p => ({
                id: boundedString(String(p.id)), name: boundedString(String(p.name)),
                type: String(p.type).slice(0, 64), enabled: p.enabled === true,
            })) : [],
            models: Array.isArray(ai.models) ? ai.models.slice(0, MAX_SETTINGS_MODELS).map(m => ({
                id: boundedString(String(m.id)), provider_id: boundedString(String(m.provider_id)),
                name: boundedString(String(m.name)), enabled: m.enabled === true, is_default: m.is_default === true,
            })) : [],
            default_model_id: typeof ai.default_model_id === 'string' ? boundedString(ai.default_model_id) : null,
            advanced,
        } : null,
    };
}

/** Whether the committed projection reflects every requested change of an update. */
export function settingsUpdateCommitted(area: SettingsArea, rawSet: unknown, projection: GuiSettingsProjection): boolean {
    if (area === 'general') {
        const applied = validateGeneralSet(rawSet);
        const current = projection.general;
        if (!current) return false;
        return Object.entries(applied).every(([key, value]) => current[key] === value);
    }
    const update = validateAiSet(rawSet);
    const ai = projection.ai;
    if (!ai) return false;
    if (update.provider_enabled) {
        const provider = ai.providers.find(p => p.id === update.provider_enabled!.id);
        if (!provider || provider.enabled !== update.provider_enabled.enabled) return false;
    }
    if (update.model_enabled) {
        const model = ai.models.find(m => m.id === update.model_enabled!.id);
        if (!model || model.enabled !== update.model_enabled.enabled) return false;
    }
    if (update.model_default) {
        const model = ai.models.find(m => m.id === update.model_default!.id);
        if (!model || !model.is_default) return false;
        if (ai.models.some(m => m.provider_id === model.provider_id && m.id !== model.id && m.is_default)) return false;
    }
    if (update.advanced) {
        for (const [key, value] of Object.entries(update.advanced)) {
            if (ai.advanced[key] !== value) return false;
        }
    }
    return true;
}
