// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { AIModel, AIProvider, AISettings } from './ai';
import { applyDiscoveredModelDefaults, reconcilePersistedModel } from './aiModelRegistry';
import { usesModelStudioContract } from './aiModelStudio';

export type CapabilityFlags = Pick<AIModel, 'supportsStreaming' | 'supportsTools' | 'supportsVision' | 'supportsThinking' | 'supportsParallelTools'>;
export interface DiscoveredModelInfo extends Partial<CapabilityFlags> {
    id: string;
    maxContextTokens?: number;
    maxTokens?: number;
}
export interface ProviderCapabilitySnapshot extends DiscoveredModelInfo {
    providerType: AIProvider['type'];
    baseUrl: string;
    sourceUrl: string;
    reviewedAt: string;
}

/** IPC discovery may return names only or enriched records; availability stays usable in either case. */
export function normalizeModelCatalog(catalog: unknown): DiscoveredModelInfo[] {
    if (!Array.isArray(catalog)) return [];
    return catalog.flatMap(item => {
        if (typeof item === 'string' && item.trim()) return [{ id: item }];
        if (!item || typeof item !== 'object' || typeof item.id !== 'string' || !item.id.trim()) return [];
        const model: DiscoveredModelInfo = { id: item.id };
        for (const key of CAPABILITY_KEYS) if (typeof item[key] === 'boolean') model[key] = item[key];
        for (const key of ['maxContextTokens', 'maxTokens'] as const) {
            if (Number.isSafeInteger(item[key]) && item[key] > 0) model[key] = item[key];
        }
        return [model];
    });
}

// Exact NVIDIA endpoint IDs, reviewed against the endpoint's own model cards.
// Never strip namespaces or import first-party hosted-tool/Responses privileges.
const NVIDIA_PROFILES: Record<string, { vision: boolean; context: number; card: string }> = {
    'nvidia/nemotron-3-ultra-550b-a55b': { vision: false, context: 1_000_000, card: 'nvidia/nemotron-3-ultra-550b-a55b' },
    'moonshotai/kimi-k3': { vision: true, context: 1_048_576, card: 'moonshotai/kimi-k3' },
    'z-ai/glm-5.3': { vision: false, context: 1_048_576, card: 'z-ai/glm-5-3' },
    'z-ai/glm-5.3-flash': { vision: true, context: 1_048_576, card: 'z-ai/glm-5-3-flash' },
    'deepseek-ai/deepseek-v4.1-flash': { vision: true, context: 1_048_576, card: 'deepseek-ai/deepseek-v4.1-flash' },
};
const normalizedUrl = (url: string) => url.replace(/\/+$/, '');
// Exact Singapore deployment capabilities; the model list API supplies names only.
const MODEL_STUDIO_PROFILES: Record<string, { vision: boolean; context: number; output?: number; doc: string }> = {
    'qwen3.8-flash': { vision: true, context: 1_000_000, output: 131072, doc: 'qwen3-8-flash' },
    'qwen3.8-max-0902': { vision: true, context: 1_000_000, doc: 'newly-released-models' },
    // The exact endpoint's capability table lists Text input, despite broader family marketing.
    'qwen3.8-2.4t-a95b': { vision: false, context: 1_000_000, output: 131072, doc: 'qwen3-8-2-4t-a95b' },
    'deepseek-v4-pro-0813': { vision: false, context: 1_000_000, output: 393216, doc: 'deepseek-v4-pro' },
    'deepseek-v4.1-flash': { vision: true, context: 1_000_000, output: 393216, doc: 'deepseek-v4-1-flash' },
    'kimi-k3': { vision: true, context: 1_048_576, output: 1_048_576, doc: 'kimi-k3' },
};
export const CAPABILITY_KEYS: Array<keyof CapabilityFlags> = ['supportsStreaming', 'supportsTools', 'supportsVision', 'supportsThinking', 'supportsParallelTools'];

export function providerModelSnapshot(provider: AIProvider, name: string, info?: DiscoveredModelInfo): ProviderCapabilitySnapshot | undefined {
    const baseUrl = normalizedUrl(provider.baseUrl);
    const studio = usesModelStudioContract(provider.type, baseUrl, name) ? MODEL_STUDIO_PROFILES[name] : undefined;
    if (studio) return {
        id: name, providerType: provider.type, baseUrl,
        supportsStreaming: true, supportsTools: true, supportsVision: studio.vision,
        supportsThinking: true, supportsParallelTools: false,
        maxContextTokens: studio.context, maxTokens: studio.output,
        sourceUrl: `https://www.alibabacloud.com/help/en/model-studio/${studio.doc}`, reviewedAt: '2026-09-26',
    };
    const profile = provider.type === 'nvidia' && baseUrl === 'https://integrate.api.nvidia.com/v1' ? NVIDIA_PROFILES[name] : undefined;
    if (profile) return {
        id: name, providerType: provider.type, baseUrl,
        supportsStreaming: true, supportsTools: true, supportsVision: profile.vision,
        supportsThinking: true, supportsParallelTools: false,
        maxContextTokens: profile.context,
        sourceUrl: `https://build.nvidia.com/${profile.card}/modelcard`, reviewedAt: '2026-09-26',
    };
    if (info?.id === name && provider.type === 'openrouter' && baseUrl === 'https://openrouter.ai/api/v1'
        && CAPABILITY_KEYS.some(key => typeof info[key] === 'boolean')) return {
        ...info, providerType: provider.type, baseUrl,
        sourceUrl: 'https://openrouter.ai/api/v1/models', reviewedAt: new Date().toISOString().slice(0, 10),
    };
    return undefined;
}

/** Resolve capabilities for this exact provider/endpoint, preserving deliberate overrides. */
export function resolveProviderModel(model: Partial<AIModel> & {name: string}, provider?: AIProvider, info?: DiscoveredModelInfo): Partial<AIModel> {
    if (!provider) return reconcilePersistedModel(model);
    const saved = model.providerCapabilities;
    const savedMatches = saved?.id === model.name && saved.providerType === provider.type && saved.baseUrl === normalizedUrl(provider.baseUrl);
    if (saved && !savedMatches) {
        // Invalidate the old scope before resolving the destination, including
        // when both endpoints/models have their own verified profiles.
        model = {
            ...model,
            // Values equal to the old endpoint ceiling were derived/clamped by
            // discovery. Only a strictly lower user budget may survive a move.
            maxContextTokens: saved.maxContextTokens && model.maxContextTokens === saved.maxContextTokens ? undefined : model.maxContextTokens,
            maxTokens: saved.maxTokens && model.maxTokens === saved.maxTokens ? undefined : model.maxTokens,
            providerCapabilities: undefined, capabilityOverrides: undefined, nativeCapabilities: undefined, capabilitySource: 'unknown',
        };
    }
    const snapshot = providerModelSnapshot(provider, model.name, info) || (savedMatches ? saved : undefined);
    if (!snapshot) {
        if (saved) return applyDiscoveredModelDefaults(model);
        return reconcilePersistedModel(model);
    }
    const resolved: Partial<AIModel> = {
        ...model,
        maxTokens: Math.min(model.maxTokens && model.maxTokens > 0 ? model.maxTokens : 4096, snapshot.maxTokens || Number.MAX_SAFE_INTEGER),
        maxContextTokens: snapshot.maxContextTokens && model.maxContextTokens && model.maxContextTokens > 0
            ? Math.min(model.maxContextTokens, snapshot.maxContextTokens) : snapshot.maxContextTokens,
        providerCapabilities: snapshot,
        capabilitySource: 'provider',
        capabilitiesVerifiedAt: snapshot.reviewedAt,
        capabilitiesSourceUrl: snapshot.sourceUrl,
        nativeCapabilities: undefined,
    };
    for (const key of CAPABILITY_KEYS) resolved[key] = model.capabilityOverrides?.[key] ?? snapshot[key] ?? (key === 'supportsStreaming');
    return resolved;
}

export function reconcileProviderModels(models: AIModel[] | undefined, providers: AIProvider[]): AIModel[] {
    return (models || []).map(model => resolveProviderModel(model, providers.find(p => p.id === model.providerId)) as AIModel);
}

/** Apply an edit of one provider to the settings. Its models are re-resolved
 *  only when the edit is committed: a Base URL still being typed must not
 *  invalidate verified capabilities, overrides and ceilings on each keystroke. */
export function withProviderEdit(settings: AISettings, provider: AIProvider, commit: boolean): AISettings {
    const providers = settings.providers.map(p => p.id === provider.id ? { ...provider, updatedAt: new Date() } : p);
    return { ...settings, providers, models: commit ? reconcileProviderModels(settings.models, providers) : settings.models };
}

/** Keep stable IDs, endpoints and user names while updating the old preset label. */
export function reconcileProviderNames(providers: AIProvider[]): AIProvider[] {
    return providers.map(provider => provider.type === 'qwen' && provider.name === 'Qwen (Alibaba)'
        ? { ...provider, name: 'Alibaba Model Studio' } : provider);
}
