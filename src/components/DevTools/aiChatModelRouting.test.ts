// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { AISettings, getDefaultAISettings } from '../../types/ai';
import { resolveRoutedModels } from './aiChatModelRouting';
import { isFailoverWorthy } from './aiChatUtils';

/** A prompt `detectTaskType` classifies as 'code_generation'. */
const CODE_PROMPT = 'create a function that parses the config file';

function settingsWith(rules: AISettings['autoRouting']['rules'], defaultModelId: string | null = null): AISettings {
    return {
        ...getDefaultAISettings(),
        providers: [
            { id: 'p1', name: 'Groq', type: 'groq', baseUrl: '', isEnabled: true, isDefault: false, createdAt: new Date(), updatedAt: new Date() },
            { id: 'p2', name: 'Ollama', type: 'ollama', baseUrl: '', isEnabled: true, isDefault: false, createdAt: new Date(), updatedAt: new Date() },
        ],
        models: [
            { id: 'm1', providerId: 'p1', name: 'llama-3.3-70b', displayName: 'Llama 3.3 70B', isEnabled: true },
            { id: 'm2', providerId: 'p2', name: 'qwen2.5-coder', displayName: 'Qwen2.5 Coder', isEnabled: true },
        ],
        autoRouting: { enabled: true, rules },
        defaultModelId,
    } as AISettings;
}

describe('resolveRoutedModels', () => {
    it('respects an explicit pick in the chat header and offers no fallback', () => {
        const picked = {
            providerId: 'p1', providerName: 'Groq', providerType: 'groq' as const,
            modelId: 'm1', modelName: 'llama-3.3-70b', displayName: 'Llama 3.3 70B',
        };
        const routed = resolveRoutedModels(picked, settingsWith([{ taskType: 'code_generation', preferredModelId: 'm2', fallbackModelId: 'm1' }]), CODE_PROMPT);
        expect(routed.primary).toBe(picked);
        expect(routed.fallback).toBeNull();
    });

    it('resolves the matching rule and carries its fallback', () => {
        const routed = resolveRoutedModels(null, settingsWith([{ taskType: 'code_generation', preferredModelId: 'm2', fallbackModelId: 'm1' }]), CODE_PROMPT);
        expect(routed.primary?.modelId).toBe('m2');
        expect(routed.fallback?.modelId).toBe('m1');
    });

    it('drops a fallback that points at the preferred model, which would retry the endpoint that just failed', () => {
        const routed = resolveRoutedModels(null, settingsWith([{ taskType: 'code_generation', preferredModelId: 'm2', fallbackModelId: 'm2' }]), CODE_PROMPT);
        expect(routed.primary?.modelId).toBe('m2');
        expect(routed.fallback).toBeNull();
    });

    it('promotes the fallback when the preferred model no longer resolves', () => {
        const routed = resolveRoutedModels(null, settingsWith([{ taskType: 'code_generation', preferredModelId: 'deleted', fallbackModelId: 'm1' }], 'm2'), CODE_PROMPT);
        expect(routed.primary?.modelId).toBe('m1');
        expect(routed.fallback).toBeNull();
    });

    it('falls back to the default model when no rule matches, with no rule fallback attached', () => {
        const routed = resolveRoutedModels(null, settingsWith([], 'm1'), CODE_PROMPT);
        expect(routed.primary?.modelId).toBe('m1');
        expect(routed.fallback).toBeNull();
    });

    it('uses the default model when auto-routing is off, ignoring any stored rules', () => {
        const settings = { ...settingsWith([{ taskType: 'code_generation', preferredModelId: 'm2' }], 'm1'), autoRouting: { enabled: false, rules: [{ taskType: 'code_generation' as const, preferredModelId: 'm2' }] } };
        const routed = resolveRoutedModels(null, settings, CODE_PROMPT);
        expect(routed.primary?.modelId).toBe('m1');
    });

    it('returns no model at all when nothing resolves, so the caller can report it', () => {
        const routed = resolveRoutedModels(null, settingsWith([], null), CODE_PROMPT);
        expect(routed.primary).toBeNull();
    });
});

describe('isFailoverWorthy', () => {
    it('fails over on failures that belong to the endpoint', () => {
        for (const err of ['HTTP 429 Too Many Requests', 'upstream 503', 'network error', 'Stream timeout after 120s', 'model not found', '401 unauthorized', 'The model is overloaded']) {
            expect(isFailoverWorthy(err), err).toBe(true);
        }
    });

    it('never routes around a limit the user set or a request the user stopped', () => {
        for (const err of ['Monthly budget exhausted ($5.00 / $5.00)', 'Request cancelled by user', 'AbortError']) {
            expect(isFailoverWorthy(err), err).toBe(false);
        }
    });

    it('leaves unrelated errors alone rather than burning a second call', () => {
        expect(isFailoverWorthy('No model selected. Click to configure a provider.')).toBe(false);
    });
});
