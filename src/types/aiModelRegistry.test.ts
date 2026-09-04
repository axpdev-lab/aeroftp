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
    getModelCapabilitySource,
    lookupModelSpec,
    resolveModelContext,
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
        expect(MODEL_REGISTRY_REVIEWED_AT).toBe('2026-09-02');
        const reviewedAt = Date.parse(`${MODEL_REGISTRY_REVIEWED_AT}T00:00:00Z`);
        expect(Number.isNaN(reviewedAt)).toBe(false);
        const ageDays = (Date.now() - reviewedAt) / 86_400_000;
        expect(ageDays).toBeGreaterThanOrEqual(0);
        expect(ageDays).toBeLessThanOrEqual(120);
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
            expect(spec.metadataReviewedAt).toBe(MODEL_REGISTRY_REVIEWED_AT);
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
            expect(MODEL_REGISTRY[name].metadataReviewedAt).toBe(MODEL_REGISTRY_REVIEWED_AT);
            expect(MODEL_REGISTRY[name].metadataSource).toMatch(/^https:\/\//);
        }
    });

    it('records provider retirements instead of advertising stale models as current', () => {
        expect(MODEL_REGISTRY['grok-3'].lifecycleStatus).toBe('retired');
        expect(MODEL_REGISTRY['moonshot-v1-128k'].lifecycleStatus).toBe('retired');
    });
});

describe('model capability resolution', () => {
    it('matches exact IDs and provider snapshot suffixes without loose prefixes', () => {
        expect(lookupModelSpec('gpt-5.6-sol')).toBe(MODEL_REGISTRY['gpt-5.6-sol']);
        expect(lookupModelSpec('gpt-5.6-sol-2026-08-01')).toBe(MODEL_REGISTRY['gpt-5.6-sol']);
        expect(lookupModelSpec('gpt-5.60')).toBeNull();
        expect(lookupModelSpec('gpt-5.6-sol-preview')).toBeNull();
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
        expect(applied.capabilitiesVerifiedAt).toBe(MODEL_REGISTRY_REVIEWED_AT);
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

    it('derives provenance for saved models created before the provenance field existed', () => {
        expect(getModelCapabilitySource({ name: 'gpt-5.6' })).toBe('registry');
        expect(getModelCapabilitySource({ name: 'private-model', maxContextTokens: 32_000 })).toBe('user');
        expect(getModelCapabilitySource({ name: 'private-model', maxTokens: 4096 })).toBe('unknown');
    });

    it('enables first-party Responses only for verified OpenAI models', () => {
        const sol = applyRegistryDefaults({ name: 'gpt-5.6-sol' });
        expect(shouldUseOpenAIResponses('openai', sol, true)).toBe(true);
        expect(shouldUseOpenAIResponses('openai', sol, false)).toBe(false);
        expect(shouldUseOpenAIResponses('xai', sol, true)).toBe(false);
        expect(shouldUseOpenAIResponses('anthropic', sol, true)).toBe(false);
        expect(shouldUseOpenAIResponses('kimi', sol, true)).toBe(false);
        expect(shouldUseOpenAIResponses('openai', baseModel(), true)).toBe(false);
    });
});
