// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { AIModel } from './ai';
import {
    MODEL_REGISTRY,
    MODEL_REGISTRY_REVIEWED_AT,
    UNKNOWN_MODEL_CONTEXT_BUDGET,
    applyDiscoveredModelDefaults,
    applyRegistryDefaults,
    buildSavedModelRecord,
    getModelCapabilitySource,
    lookupModelSpec,
    reconcilePersistedModel,
    resolveModelContext,
    resolveModelRuntimeSupport,
    shouldUseOpenAIResponses,
} from './aiModelRegistry';

const baseModel = (overrides: Partial<AIModel> = {}): AIModel => ({
    id: 'model-1',
    providerId: 'provider-1',
    name: 'unknown-model',
    displayName: 'Unknown model',
    maxTokens: 999999,
    supportsStreaming: true,
    supportsTools: true,
    supportsVision: true,
    isEnabled: true,
    isDefault: false,
    ...overrides,
});

describe('current provider model profiles', () => {
    it('keeps the registry review date parseable and current for this lane', () => {
        expect(MODEL_REGISTRY_REVIEWED_AT).toBe('2026-09-26');
        const reviewedAt = Date.parse(`${MODEL_REGISTRY_REVIEWED_AT}T00:00:00Z`);
        expect(Number.isNaN(reviewedAt)).toBe(false);
        // Fixtures validate provenance, not the machine clock or a future expiry.
        for (const spec of Object.values(MODEL_REGISTRY)) {
            if (spec.metadataReviewedAt) {
                expect(Date.parse(`${spec.metadataReviewedAt}T00:00:00Z`)).toBeLessThanOrEqual(reviewedAt);
            }
        }
    });

    it('describes the GPT-5.6 family from verified provider metadata', () => {
        for (const name of ['gpt-5.6', 'gpt-5.6-sol', 'gpt-5.6-terra', 'gpt-5.6-luna']) {
            const spec = MODEL_REGISTRY[name];
            expect(spec.maxContextTokens).toBe(1_050_000);
            expect(spec.maxTokens).toBe(128_000);
            expect(spec.supportsTools).toBe(true);
            expect(spec.supportsVision).toBe(true);
            expect(spec.nativeCapabilities?.responses).toBe(true);
            expect(spec.nativeCapabilities?.toolSearch).toBe(true);
            expect(spec.nativeCapabilities?.reasoningEfforts).toEqual([
                'none', 'low', 'medium', 'high', 'xhigh', 'max',
            ]);
            // A partial registry sweep must not re-certify older provider entries.
            expect(spec.metadataReviewedAt).toBe('2026-09-02');
            expect(spec.metadataSource).toMatch(/^https:\/\/developers\.openai\.com\//);
        }
    });

    it('captures provider differences without pretending every API is OpenAI Responses', () => {
        expect(MODEL_REGISTRY['claude-opus-5'].nativeCapabilities).toMatchObject({
            adaptiveThinking: true,
            contextManagement: true,
            modelCapabilitiesApi: true,
        });
        expect(MODEL_REGISTRY['claude-opus-5'].nativeCapabilities?.responses).toBeUndefined();

        expect(MODEL_REGISTRY['grok-4.6'].nativeCapabilities).toMatchObject({
            responses: true,
            contextManagement: true,
            encryptedReasoningReplay: true,
        });

        expect(MODEL_REGISTRY['kimi-k3'].nativeCapabilities).toMatchObject({
            dynamicToolLoading: true,
            automaticPromptCaching: true,
            requiresFullAssistantReplay: true,
            fixedSamplingParameters: true,
        });
        expect(MODEL_REGISTRY['kimi-k3'].nativeCapabilities?.responses).toBeUndefined();

        for (const name of ['claude-opus-5', 'grok-4.6', 'kimi-k3']) {
            expect(MODEL_REGISTRY[name].metadataReviewedAt).toBe(name === 'kimi-k3' ? '2026-09-26' : '2026-09-02');
            expect(MODEL_REGISTRY[name].metadataSource).toMatch(/^https:\/\//);
        }
    });

    it('records provider retirements instead of advertising stale models as current', () => {
        expect(MODEL_REGISTRY['grok-3'].lifecycleStatus).toBe('retired');
        expect(MODEL_REGISTRY['moonshot-v1-128k'].lifecycleStatus).toBe('retired');
    });
});

describe('model capability resolution', () => {
    it('matches only reviewed IDs, never inferred snapshots or prototype keys', () => {
        expect(lookupModelSpec('gpt-5.6-sol')).toBe(MODEL_REGISTRY['gpt-5.6-sol']);
        expect(lookupModelSpec('gpt-5.6-sol-2026-08-01')).toBeNull();
        expect(lookupModelSpec('gpt-6-astra-2026-09-26')).toBeNull();
        for (const name of ['toString', 'constructor', '__proto__']) {
            expect(lookupModelSpec(name)).toBeNull();
        }
        expect(lookupModelSpec('gpt-5.60')).toBeNull();
        expect(lookupModelSpec('gpt-5.6-sol-preview')).toBeNull();
        expect(lookupModelSpec('gpt-5.6-sol-2026-08-01-preview')).toBeNull();
        expect(lookupModelSpec('gpt-5.6-sol-2026-99-99')).toBeNull();
    });

    it('does not confuse an unknown model output cap with its context window', () => {
        const resolution = resolveModelContext(baseModel({ maxTokens: 128_000 }));
        expect(resolution).toEqual({
            tokens: UNKNOWN_MODEL_CONTEXT_BUDGET,
            source: 'conservative',
            verified: false,
        });
    });

    it('prefers an explicit user context and falls back to registry context', () => {
        expect(resolveModelContext(baseModel({
            maxContextTokens: 64_000,
            capabilitySource: 'user',
        }))).toEqual({ tokens: 64_000, source: 'model', verified: true });

        expect(resolveModelContext({ name: 'gpt-5.6-terra' })).toEqual({
            tokens: 1_050_000,
            source: 'registry',
            verified: true,
        });
    });

    it('makes newly discovered unknown models conservative and visibly unknown', () => {
        const discovered = applyDiscoveredModelDefaults(baseModel()) as AIModel;
        expect(discovered.maxTokens).toBe(999_999);
        expect(discovered.maxContextTokens).toBeUndefined();
        expect(discovered.supportsStreaming).toBe(true);
        expect(discovered.supportsTools).toBe(false);
        expect(discovered.supportsVision).toBe(false);
        expect(discovered.supportsThinking).toBe(false);
        expect(discovered.supportsParallelTools).toBe(false);
        expect(discovered.capabilitySource).toBe('unknown');
        expect(getModelCapabilitySource(discovered)).toBe('unknown');
    });

    it('applies registered metadata while preserving explicit user limits', () => {
        const applied = applyRegistryDefaults({
            name: 'gpt-5.6-sol',
            maxTokens: 16_000,
        });
        expect(applied.maxTokens).toBe(16_000);
        expect(applied.maxContextTokens).toBe(1_050_000);
        expect(applied.capabilitySource).toBe('registry');
        expect(applied.capabilitiesVerifiedAt).toBe(MODEL_REGISTRY['gpt-5.6-sol'].metadataReviewedAt);
        expect(applied.nativeCapabilities?.reasoningEfforts).not.toBe(
            MODEL_REGISTRY['gpt-5.6-sol'].nativeCapabilities?.reasoningEfforts,
        );
    });

    it('clamps an above-limit explicit context to the registry window', () => {
        const applied = applyRegistryDefaults({
            name: 'gpt-5.6-sol',
            maxContextTokens: 9_000_000,
        });
        expect(applied.maxContextTokens).toBe(MODEL_REGISTRY['gpt-5.6-sol'].maxContextTokens);
        expect(applied.capabilitySource).toBe('registry');

        const lowered = applyRegistryDefaults({
            name: 'gpt-5.6-sol',
            maxContextTokens: 64_000,
        });
        expect(lowered.maxContextTokens).toBe(64_000);
    });

    it('does not infer registry verification from a name alone', () => {
        expect(getModelCapabilitySource({ name: 'gpt-5.6' })).toBe('unknown');
        expect(getModelCapabilitySource({ name: 'private-model', maxContextTokens: 32_000 })).toBe('user');
        expect(getModelCapabilitySource({ name: 'private-model', maxTokens: 4096 })).toBe('unknown');
        expect(getModelCapabilitySource({
            name: 'gpt-5.6-sol',
            capabilitySource: 'registry',
        })).toBe('unknown');
    });

    it('hydrates a legacy saved known model so Responses can actually be selected', () => {
        const legacy = reconcilePersistedModel({
            ...baseModel({ name: 'gpt-5.6-sol', displayName: 'GPT-5.6 Sol' }),
        });
        expect(legacy.capabilitySource).toBe('registry');
        expect(legacy.nativeCapabilities?.responses).toBe(true);
        expect(shouldUseOpenAIResponses('openai', legacy, true, 'https://api.openai.com/v1')).toBe(true);
        expect(shouldUseOpenAIResponses('openai', { name: 'gpt-5.6-sol' }, true, 'https://api.openai.com/v1')).toBe(false);
    });

    it('enables first-party Responses only for verified OpenAI models', () => {
        const sol = applyRegistryDefaults({ name: 'gpt-5.6-sol' });
        expect(shouldUseOpenAIResponses('openai', sol, true)).toBe(true);
        expect(shouldUseOpenAIResponses('openai', sol, false)).toBe(false);
        expect(shouldUseOpenAIResponses('xai', sol, true)).toBe(false);
        expect(shouldUseOpenAIResponses('anthropic', sol, true)).toBe(false);
        expect(shouldUseOpenAIResponses('kimi', sol, true)).toBe(false);
        expect(shouldUseOpenAIResponses('openai', baseModel(), true)).toBe(false);
        expect(shouldUseOpenAIResponses('openai', sol, true, 'https://proxy.example/v1')).toBe(false);
    });

    it('clamps context when saving an existing known model, not only on create', () => {
        const previous = applyRegistryDefaults({
            ...baseModel({ name: 'gpt-5.6-sol' }),
        }) as AIModel;
        const saved = buildSavedModelRecord({
            previous,
            isNew: false,
            providerId: previous.providerId,
            id: previous.id,
            form: {
                name: 'gpt-5.6-sol',
                displayName: previous.displayName,
                maxTokens: previous.maxTokens,
                maxContextTokens: 9_000_000,
                supportsStreaming: true,
                supportsTools: true,
                supportsVision: true,
                supportsThinking: true,
                isEnabled: true,
            },
        });
        expect(saved.maxContextTokens).toBe(MODEL_REGISTRY['gpt-5.6-sol'].maxContextTokens);
        expect(saved.nativeCapabilities?.responses).toBe(true);
    });

    it('rebuilds registry fields when renaming a known model to another known model', () => {
        const previous = applyRegistryDefaults({
            ...baseModel({ name: 'gpt-5.6-sol', maxContextTokens: 1_050_000 }),
        }) as AIModel;
        const saved = buildSavedModelRecord({
            previous,
            isNew: false,
            providerId: previous.providerId,
            id: previous.id,
            form: {
                name: 'claude-opus-5',
                displayName: previous.displayName,
                maxTokens: previous.maxTokens,
                maxContextTokens: 9_000_000,
                supportsStreaming: true,
                supportsTools: true,
                supportsVision: true,
                supportsThinking: true,
                isEnabled: true,
            },
        });
        expect(saved.name).toBe('claude-opus-5');
        expect(saved.nativeCapabilities?.responses).toBeUndefined();
        expect(saved.nativeCapabilities?.adaptiveThinking).toBe(true);
        expect(saved.maxContextTokens).toBeLessThanOrEqual(MODEL_REGISTRY['claude-opus-5'].maxContextTokens);
        expect(saved.capabilitySource).toBe('registry');
    });

    it('strips registry labels when renaming a known model to an unknown id', () => {
        const previous = applyRegistryDefaults({
            ...baseModel({ name: 'gpt-5.6-sol' }),
        }) as AIModel;
        const saved = buildSavedModelRecord({
            previous,
            isNew: false,
            providerId: previous.providerId,
            id: previous.id,
            form: {
                name: 'my-local-finetune',
                displayName: 'Local',
                maxTokens: 4096,
                maxContextTokens: 8192,
                supportsStreaming: true,
                supportsTools: false,
                supportsVision: false,
                supportsThinking: false,
                isEnabled: true,
            },
        });
        expect(saved.nativeCapabilities).toBeUndefined();
        expect(saved.capabilitySource).not.toBe('registry');
        expect(shouldUseOpenAIResponses('openai', saved, true)).toBe(false);
    });
});


describe('provider contracts and implemented adapter support', () => {
    it.each([
        ['gpt-6-astra', 'responses', ['low', 'medium', 'high', 'xhigh', 'max']],
        ['gpt-6-sol', 'responses-or-chat-without-reasoning', ['none', 'low', 'medium', 'high', 'xhigh', 'max']],
        ['gpt-6-luna', 'responses-or-chat-without-reasoning', ['none', 'low', 'medium', 'high', 'xhigh', 'max']],
    ])('records the verified API contract for %s without borrowing Codex effort levels', (name, transport, efforts) => {
        const spec = MODEL_REGISTRY[name as string];
        expect(spec.maxContextTokens).toBe(1_050_000);
        expect(spec.maxTokens).toBe(128_000);
        expect(spec.nativeCapabilities?.toolCallingTransport).toBe(transport);
        expect(spec.nativeCapabilities?.reasoningEfforts).toEqual(efforts);
        expect(spec.metadataReviewedAt).toBe('2026-09-26');
        // Flat prices would undercount requests above the documented threshold.
        expect(spec.inputCostPer1k).toBeUndefined();
        expect(spec.outputCostPer1k).toBeUndefined();
    });

    it.each(['claude-opus-5-5', 'claude-fable-5-1'])('preserves the modern Anthropic constraints for %s', name => {
        expect(MODEL_REGISTRY[name].nativeCapabilities).toMatchObject({
            adaptiveThinking: true,
            thinkingAlwaysOn: true,
            forcedToolChoice: false,
            requiresFullAssistantReplay: true,
            fixedSamplingParameters: true,
        });
        expect(MODEL_REGISTRY[name].maxContextTokens).toBe(1_000_000);
        expect(MODEL_REGISTRY[name].maxTokens).toBe(128_000);
    });

    it('records Grok and Kimi effort differences without broadening their adapters', () => {
        expect(MODEL_REGISTRY['grok-4.7'].nativeCapabilities?.reasoningEfforts).toEqual(['low', 'medium', 'high', 'xhigh']);
        expect(MODEL_REGISTRY['kimi-k3'].nativeCapabilities?.reasoningEfforts).toEqual(['low', 'high', 'max']);
        expect(MODEL_REGISTRY['grok-4.7'].maxContextTokens).toBe(500_000);
        expect(MODEL_REGISTRY['grok-4.7'].inputCostPer1k).toBeUndefined();
        expect(MODEL_REGISTRY['kimi-k3'].nativeCapabilities?.requiresFullAssistantReplay).toBe(true);
        expect(MODEL_REGISTRY['kimi-k3'].nativeCapabilities?.fixedSamplingParameters).toBe(true);
    });

    it.each(['gpt-6-astra', 'gpt-6-sol', 'gpt-6-luna', 'claude-opus-5-5', 'claude-fable-5-1', 'grok-4.7', 'kimi-k3'])
    ('does not auto-enable %s until its adapter requirements are implemented', name => {
        const result = applyDiscoveredModelDefaults({ name, isEnabled: true, isDefault: true });
        expect(result.capabilitySource).toBe('registry');
        expect(result.isEnabled).toBe(false);
        expect(result.isDefault).toBe(false);
        const runtime = resolveModelRuntimeSupport(name);
        expect(runtime.discoveryReady).toBe(false);
        expect(runtime.pendingAdapterRequirements.length).toBeGreaterThan(0);
        expect(runtime.subagents).toBe(false);
        expect(runtime.toolSearch).toBe(false);
        expect(runtime.nativeTurnState).toBe(false);
        expect(shouldUseOpenAIResponses('openai', result, true)).toBe(false);
    });

    it('preserves existing explicit enablement, but refreshes stale native metadata', () => {
        const model = baseModel({
            name: 'claude-fable-5-1', isEnabled: true,
            nativeCapabilities: { responses: true, forcedToolChoice: true },
            capabilitiesVerifiedAt: '2026-01-01', lifecycleStatus: 'retired',
        });
        const result = reconcilePersistedModel(model);
        expect(result.isEnabled).toBe(true);
        expect(result.nativeCapabilities?.forcedToolChoice).toBe(false);
        expect(result.nativeCapabilities?.responses).toBeUndefined();
        expect(result.lifecycleStatus).toBe('active');
        expect(result.capabilitiesVerifiedAt).toBe('2026-09-26');
        expect(reconcilePersistedModel(result as AIModel)).toEqual(result);
        expect(model.nativeCapabilities?.forcedToolChoice).toBe(true);
    });

    it('does not let a persisted or returned object mutate registry contract arrays', () => {
        const result = applyRegistryDefaults({ name: 'gpt-6-astra', nativeCapabilities: { reasoningEfforts: ['none'] } });
        result.nativeCapabilities?.reasoningEfforts?.push('none');
        expect(MODEL_REGISTRY['gpt-6-astra'].nativeCapabilities?.reasoningEfforts).not.toContain('none');
        const runtime = resolveModelRuntimeSupport('gpt-6-astra');
        runtime.pendingAdapterRequirements.length = 0;
        expect(resolveModelRuntimeSupport('gpt-6-astra').discoveryReady).toBe(false);
    });

    it('does not promote provider features to local runtime features', () => {
        expect(MODEL_REGISTRY['gpt-5.6-sol'].nativeCapabilities?.multiAgent).toBe(true);
        expect(resolveModelRuntimeSupport('gpt-5.6-sol')).toMatchObject({
            discoveryReady: true, subagents: false, toolSearch: false, nativeTurnState: false,
        });
        expect(resolveModelRuntimeSupport('private-model').discoveryReady).toBe(false);
        expect(applyDiscoveredModelDefaults({ name: 'grok-3', isEnabled: true }).isEnabled).toBe(false);
    });

    it('rejects forged native capability metadata and cross-provider Responses claims', () => {
        for (const name of ['private-model', 'grok-4.6', 'claude-opus-5']) {
            const forged = { name, capabilitySource: 'registry' as const, nativeCapabilities: { responses: true } };
            expect(shouldUseOpenAIResponses('openai', forged, true)).toBe(false);
        }
    });

    it.each([
        'http://api.openai.com/v1', 'https://api.openai.com.evil.test/v1',
        'https://proxy.openai.com/v1', 'https://api.openai.com:444/v1',
        'https://user:secret@api.openai.com/v1', 'https://api.openai.com/v1?proxy=1',
        'https://api.openai.com/v1#proxy', 'https://api.openai.com/other',
        'https://proxy.example/v1',
    ])('does not promote endpoint %s to the first-party adapter', endpoint => {
        const model = applyRegistryDefaults({ name: 'gpt-5.6-sol' });
        expect(shouldUseOpenAIResponses('openai', model, true, endpoint)).toBe(false);
    });
});
