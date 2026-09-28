// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { AIModel, AIProvider, AISettings } from './ai';
import { normalizeModelCatalog, providerModelSnapshot, reconcileProviderNames, resolveProviderModel, withProviderEdit } from './aiModelDiscovery';
import { isModelStudioEndpoint, MODEL_STUDIO_MODELS } from './aiModelStudio';
import { requiresNativeTurn } from '../components/DevTools/aiChatNativeTurn';
import { resolveModelContext } from './aiModelRegistry';

const nvidia = { id: 'n', type: 'nvidia', baseUrl: 'https://integrate.api.nvidia.com/v1' } as AIProvider;
const router = { id: 'r', type: 'openrouter', baseUrl: 'https://openrouter.ai/api/v1' } as AIProvider;
describe('provider capability discovery', () => {
    it('renames only the legacy preset label without changing credentials or endpoint', () => {
        const preset = { ...nvidia, type: 'qwen', name: 'Qwen (Alibaba)' } as AIProvider;
        const customName = { ...preset, name: 'My Singapore account' };
        expect(reconcileProviderNames([preset, customName])).toEqual([{ ...preset, name: 'Alibaba Model Studio' }, customName]);
    });
    it('recognizes all six Model Studio deployments for presets and workspace Custom providers', () => {
        for (const type of ['qwen', 'custom'] as const) {
            const provider = { id: 'a', type, baseUrl: 'https://llm-test.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1' } as AIProvider;
            for (const name of MODEL_STUDIO_MODELS) {
                const model = resolveProviderModel({ name, supportsTools: false }, provider);
                expect(model).toMatchObject({ supportsTools: true, supportsThinking: true, capabilitySource: 'provider' });
                expect(model.supportsVision).toBe(!['qwen3.8-2.4t-a95b', 'deepseek-v4-pro-0813'].includes(name));
                expect(model.nativeCapabilities).toBeUndefined();
                expect(requiresNativeTurn({ provider_type: type, base_url: provider.baseUrl, model: name })).toBe(true);
            }
        }
    });
    it('does not apply the Singapore contract to lookalikes, other regions or unknown models', () => {
        expect(isModelStudioEndpoint('https://dashscope-intl.aliyuncs.com/compatible-mode/v1/')).toBe(true);
        for (const baseUrl of ['https://dashscope-intl.aliyuncs.com.evil.test/compatible-mode/v1', 'http://dashscope-intl.aliyuncs.com/compatible-mode/v1', 'https://user@dashscope-intl.aliyuncs.com/compatible-mode/v1', 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1?route=other', 'https://dashscope-intl.aliyuncs.com:8443/compatible-mode/v1', 'https://llm-test.cn-beijing.maas.aliyuncs.com/compatible-mode/v1', 'https://nested.llm-test.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1']) {
            expect(isModelStudioEndpoint(baseUrl)).toBe(false);
            expect(providerModelSnapshot({ ...nvidia, type: 'custom', baseUrl }, 'kimi-k3')).toBeUndefined();
            expect(requiresNativeTurn({ provider_type: 'custom', base_url: baseUrl, model: 'kimi-k3' })).toBe(false);
        }
        expect(providerModelSnapshot({ ...nvidia, type: 'qwen', baseUrl: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1' }, 'unknown')).toBeUndefined();
    });
    it('displays both names-only and enriched catalogs and rejects malformed rows', () => {
        expect(normalizeModelCatalog(['vendor/model', { id: 'vendor/other', supportsTools: true }, null, {}, '', { id: 3 }])).toEqual([{ id: 'vendor/model' }, { id: 'vendor/other', supportsTools: true }]);
        expect(normalizeModelCatalog({ data: [] })).toEqual([]);
        expect(normalizeModelCatalog([{ id: 'safe', supportsTools: 'true', maxTokens: -1 }])).toEqual([{ id: 'safe' }]);
    });
    it('corrects guessed NVIDIA flags and differentiates text from vision endpoints', () => {
        for (const [name, vision] of [['nvidia/nemotron-3-ultra-550b-a55b', false], ['z-ai/glm-5.3', false], ['z-ai/glm-5.3-flash', true], ['moonshotai/kimi-k3', true], ['deepseek-ai/deepseek-v4.1-flash', true]] as const) {
            const model = resolveProviderModel({ name, supportsVision: true, supportsTools: false }, nvidia);
            expect(model).toMatchObject({ supportsVision: vision, supportsTools: true, supportsThinking: true, capabilitySource: 'provider' });
            expect(model.nativeCapabilities).toBeUndefined();
        }
    });
    it('reads exact router IDs including free variants from catalog metadata', () => {
        const model = resolveProviderModel({ name: 'new/model:free' }, router, { id: 'new/model:free', supportsTools: true, supportsVision: false, maxContextTokens: 32000 });
        expect(model).toMatchObject({ supportsTools: true, supportsVision: false, supportsThinking: false, maxContextTokens: 32000, capabilitySource: 'provider' });
        expect(resolveProviderModel(model as AIModel, router)).toEqual(model);
    });
    it('never transfers a snapshot to another endpoint, provider, or renamed model', () => {
        const model = resolveProviderModel({ name: 'new/model:free' }, router, { id: 'new/model:free', supportsTools: true });
        for (const [m, p] of [[model, { ...router, baseUrl: 'https://private.example/v1' }], [{ ...model, name: 'different' }, router], [model, { ...router, type: 'custom' }]] as const) {
            const resolved = resolveProviderModel(m as AIModel, p as AIProvider);
            expect(resolved.supportsTools).toBe(false);
            expect(resolved.capabilitySource).toBe('unknown');
        }
        expect(providerModelSnapshot({ ...nvidia, type: 'custom' }, 'moonshotai/kimi-k3')).toBeUndefined();
    });
    it('drops automatic token ceilings when leaving the endpoint that established them', () => {
        const info = { id: 'vendor/model', supportsTools: true, maxContextTokens: 1000000, maxTokens: 2048 };
        const model = resolveProviderModel({ name: info.id }, router, info) as AIModel;
        const moved = resolveProviderModel(model, { ...router, baseUrl: 'https://private.example/v1' });
        expect(moved.maxContextTokens).toBeUndefined();
        expect(resolveModelContext(moved).tokens).toBeLessThan(1000000);
        expect(moved.maxTokens).not.toBe(2048);
        const explicit = resolveProviderModel({ name: info.id, maxContextTokens: 16000, maxTokens: 1000 }, router, info) as AIModel;
        expect(resolveProviderModel(explicit, { ...router, baseUrl: 'https://private.example/v1' })).toMatchObject({ maxContextTokens: 16000, maxTokens: 1000 });
    });
    it('retains deliberate capability overrides without turning metadata into hosted features', () => {
        const model = resolveProviderModel({ name: 'moonshotai/kimi-k3', capabilityOverrides: { supportsTools: false } }, nvidia);
        expect(model.supportsTools).toBe(false);
        expect(model.supportsVision).toBe(true);
        expect(model.nativeCapabilities).toBeUndefined();
    });
    it('resets scoped overrides and automatic ceilings between two verified models', () => {
        const original = resolveProviderModel({ name: 'nvidia/nemotron-3-ultra-550b-a55b', capabilityOverrides: { supportsTools: false } }, nvidia) as AIModel;
        expect(resolveProviderModel(original, nvidia).supportsTools).toBe(false);
        const renamed = resolveProviderModel({ ...original, name: 'moonshotai/kimi-k3' }, nvidia);
        expect(renamed).toMatchObject({ supportsTools: true, supportsVision: true, maxContextTokens: 1048576, capabilitySource: 'provider' });
        expect(renamed.capabilityOverrides).toBeUndefined();
        const moved = resolveProviderModel({ ...original, name: 'vendor/model' }, router, { id: 'vendor/model', supportsTools: true, maxContextTokens: 2000000 });
        expect(moved).toMatchObject({ supportsTools: true, maxContextTokens: 2000000 });
        const limited = resolveProviderModel({ ...original, maxContextTokens: 16000, maxTokens: 1000, name: 'moonshotai/kimi-k3' }, nvidia);
        expect(limited).toMatchObject({ maxContextTokens: 16000, maxTokens: 1000 });
    });
    it('resets model overrides between recognized Alibaba workspace endpoints', () => {
        const studio = { ...router, type: 'custom', baseUrl: 'https://first.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1' } as AIProvider;
        const original = resolveProviderModel({ name: 'qwen3.8-flash', capabilityOverrides: { supportsVision: false } }, studio) as AIModel;
        expect(resolveProviderModel(original, studio).supportsVision).toBe(false);
        const moved = resolveProviderModel(original, { ...studio, baseUrl: 'https://second.ap-southeast-1.maas.aliyuncs.com/compatible-mode/v1' });
        expect(moved.supportsVision).toBe(true);
        expect(moved.capabilityOverrides).toBeUndefined();
    });
    it('does not interpret availability-only discovery as capabilities', () => {
        expect(providerModelSnapshot(router, 'vendor/new', { id: 'vendor/new' })).toBeUndefined();
        expect(resolveProviderModel({ name: 'unknown/model', supportsTools: false }, nvidia).capabilitySource).toBeUndefined();
    });
    it('clamps explicit token limits to the endpoint without increasing a lower budget', () => {
        const info = { id: 'new/model', supportsTools: true, maxTokens: 8192, maxContextTokens: 32000 };
        expect(resolveProviderModel({ name: info.id, maxTokens: 100000, maxContextTokens: 100000 }, router, info)).toMatchObject({ maxTokens: 8192, maxContextTokens: 32000 });
        expect(resolveProviderModel({ name: info.id, maxTokens: 1000, maxContextTokens: 16000 }, router, info)).toMatchObject({ maxTokens: 1000, maxContextTokens: 16000 });
    });
    it('keeps verified capabilities and overrides while a Base URL edit is being typed', () => {
        const resolved = resolveProviderModel({ name: 'new/model:free' }, router, { id: 'new/model:free', supportsTools: true, supportsVision: false, maxContextTokens: 32000 });
        const model = { ...resolved, id: 'm', providerId: router.id, capabilityOverrides: { supportsVision: true } } as AIModel;
        const settings = { providers: [router], models: [model] } as unknown as AISettings;
        // A typo and then the original URL again, keystroke by keystroke.
        let typed = withProviderEdit(settings, { ...router, baseUrl: 'https://openrouter.ai/api/v' }, false);
        typed = withProviderEdit(typed, router, false);
        expect(typed.models).toEqual(settings.models);
        // Committing the unchanged endpoint keeps the snapshot and the user override.
        expect(withProviderEdit(typed, router, true).models[0]).toMatchObject({
            capabilitySource: 'provider', supportsTools: true, supportsVision: true, maxContextTokens: 32000, capabilityOverrides: { supportsVision: true },
        });
        // Committing a real endpoint change still invalidates them.
        const moved = withProviderEdit(typed, { ...router, baseUrl: 'https://private.example/v1' }, true).models[0];
        expect(moved.capabilityOverrides).toBeUndefined();
        expect(moved.providerCapabilities).toBeUndefined();
    });
});
