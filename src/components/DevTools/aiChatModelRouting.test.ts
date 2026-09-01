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

    it('refuses a stopped request at the guard, not by falling through', () => {
        // Each of these also carries a marker that would otherwise fail over, so
        // only the guard can produce false. Asserting on a bare 'AbortError'
        // proves nothing: it is false because nothing matches at all.
        expect(isFailoverWorthy('AbortError: network timeout')).toBe(false);
        expect(isFailoverWorthy('CancelledError: 503 from upstream')).toBe(false);
        expect(isFailoverWorthy('CanceledError: 429 too many requests')).toBe(false);
        expect(isFailoverWorthy('Monthly budget exhausted, request 429')).toBe(false);
    });

    it('lets a transport abort through, because another provider may well work', () => {
        // "connection aborted by peer" is not the user stopping anything. The
        // guard anchors on the error name for exactly this reason.
        expect(isFailoverWorthy('connection aborted by peer, network unreachable')).toBe(true);
    });

    it('leaves unrelated errors alone rather than burning a second call', () => {
        expect(isFailoverWorthy('No model selected. Click to configure a provider.')).toBe(false);
    });

    it('reads a status code rather than a digit sequence that happens to look like one', () => {
        // A bare substring test saw the "500" inside this limit message and sent
        // a pointless second request to another provider.
        expect(isFailoverWorthy('max_tokens must be <= 5000')).toBe(false);
        expect(isFailoverWorthy('context length 50000 exceeded')).toBe(false);
    });

    it('treats a 404 as an unknown model, which another provider may well have', () => {
        // Only the phrase "model not found" used to match, so a provider that
        // answers a missing model with a bare HTTP 404 never failed over.
        expect(isFailoverWorthy('HTTP 404 Not Found')).toBe(true);
        expect(isFailoverWorthy('Request failed with status 503')).toBe(true);
    });

    it('does not fail over on a request the endpoint rejected as malformed', () => {
        expect(isFailoverWorthy('HTTP 400 Bad Request: invalid role')).toBe(false);
    });

    it('fails over on an unknown model whichever status the endpoint chose for it', () => {
        // OpenAI-compatible servers are not uniform here: some answer 404 with
        // `model_not_found`, others answer 400 with the same fact in the body.
        // A status is only ever a positive shortcut, so the prose still decides
        // when the status is not one of the failover codes.
        expect(isFailoverWorthy('HTTP 404 model_not_found')).toBe(true);
        expect(isFailoverWorthy('HTTP 400 Bad Request: model does not exist')).toBe(true);
        expect(isFailoverWorthy('HTTP 400: model not found')).toBe(true);
    });
});
