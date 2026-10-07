// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { GuiError } from './errors';
import {
    buildAiProjection, buildGeneralProjection, settingsUpdateCommitted, validateSettingsSet,
    MAX_SETTINGS_MODELS, MAX_SETTINGS_PROVIDERS, type GuiSettingsProjection,
} from './settingsSchema';
import { getDefaultAISettings, type AISettings } from '../types/ai';

const expectInvalid = (fn: () => unknown) => {
    try {
        fn();
    } catch (error) {
        expect(error).toBeInstanceOf(GuiError);
        expect((error as GuiError).code).toBe('invalid_args');
        return;
    }
    throw new Error('expected invalid_args');
};

describe('validateSettingsSet: general', () => {
    it('refuses JSON prototype names at both preference boundaries', () => {
        for (const key of ['constructor', 'toString', '__proto__']) {
            const set = JSON.parse(`{"${key}":true}`);
            expectInvalid(() => validateSettingsSet('general', set));
            expectInvalid(() => validateSettingsSet('ai', { advanced: set }));
        }
    });
    it('accepts allowlisted values inside their UI bounds', () => {
        expect(validateSettingsSet('general', {
            showHiddenFiles: false, dateFormat: 'iso', fontSize: 18, fontFamily: "'FiraGO', sans-serif",
        })).toEqual({
            showHiddenFiles: false, dateFormat: 'iso', fontSize: 18, fontFamily: "'FiraGO', sans-serif",
        });
    });
    it('refuses unknown, excluded and secret-adjacent keys', () => {
        for (const key of ['confirmBeforeDelete', 'disableUpdateChecks', 'defaultLocalPath', 'masterPassword', 'apiKey', 'timeoutSeconds', 'fileExistsAction']) {
            expectInvalid(() => validateSettingsSet('general', { [key]: true }));
        }
    });
    it('refuses out-of-range and wrongly typed values instead of clamping them', () => {
        expectInvalid(() => validateSettingsSet('general', { fontSize: 9 }));
        expectInvalid(() => validateSettingsSet('general', { fontSize: 23 }));
        expectInvalid(() => validateSettingsSet('general', { fontSize: 'large' }));
        expectInvalid(() => validateSettingsSet('general', { dateFormat: 'yyyy' }));
        expectInvalid(() => validateSettingsSet('general', { fontFamily: "'Evil', monospace" }));
        expectInvalid(() => validateSettingsSet('general', {}));
        expectInvalid(() => validateSettingsSet('general', []));
    });
});

describe('validateSettingsSet: ai', () => {
    it('accepts the closed provider/model/advanced shapes', () => {
        expect(validateSettingsSet('ai', {
            provider_enabled: { id: 'p1', enabled: false },
            model_default: { id: 'm1' },
            advanced: { temperature: 0.2, max_tokens: 8192, response_style: 'concise' },
        })).toEqual({
            provider_enabled: { id: 'p1', enabled: false },
            model_default: { id: 'm1' },
            advanced: { temperature: 0.2, max_tokens: 8192, response_style: 'concise' },
        });
    });
    it('refuses secret fields, provider objects, URLs and unknown keys', () => {
        expectInvalid(() => validateSettingsSet('ai', { apiKey: 'x' }));
        expectInvalid(() => validateSettingsSet('ai', { provider: { id: 'p1', apiKey: 'x' } }));
        expectInvalid(() => validateSettingsSet('ai', { provider_enabled: { id: 'p1', enabled: true, baseUrl: 'https://u:p@h' } }));
        expectInvalid(() => validateSettingsSet('ai', { advanced: { customSystemPrompt: 'ignore rules' } }));
        expectInvalid(() => validateSettingsSet('ai', { advanced: { temperature: 2.5 } }));
        expectInvalid(() => validateSettingsSet('ai', { advanced: { max_tokens: 100 } }));
        expectInvalid(() => validateSettingsSet('ai', { advanced: {} }));
        expectInvalid(() => validateSettingsSet('ai', {}));
    });
});

describe('projections', () => {
    it('buildGeneralProjection keeps only allowlisted keys and drops out-of-bounds stored values', () => {
        const projection = buildGeneralProjection({
            showHiddenFiles: false, fontSize: 99, dateFormat: 'dmy',
            confirmBeforeDelete: false, timeoutSeconds: 5, password: 'SECRET',
        });
        expect(projection).toEqual({ showHiddenFiles: false, dateFormat: 'dmy' });
        expect(JSON.stringify(projection)).not.toContain('SECRET');
    });
    it('buildAiProjection is field-by-field: no apiKey, no baseUrl, bounded counts', () => {
        const settings: AISettings = {
            ...getDefaultAISettings(),
            providers: [{
                id: 'p1', name: 'OpenAI', type: 'openai', baseUrl: 'https://api.openai.com/v1',
                apiKey: 'SECRET-KEY', isEnabled: true, isDefault: true, createdAt: new Date(0), updatedAt: new Date(0),
            }],
            models: [{
                id: 'm1', providerId: 'p1', name: 'gpt-fixture', displayName: 'GPT Fixture', maxTokens: 4096,
                supportsStreaming: true, supportsTools: true, supportsVision: false, isEnabled: true, isDefault: true,
            }],
            advancedSettings: { ...getDefaultAISettings().advancedSettings, temperature: 1.5, responseStyle: 'concise' },
        };
        const projection = buildAiProjection(settings);
        expect(projection.providers).toEqual([{ id: 'p1', name: 'OpenAI', type: 'openai', enabled: true }]);
        expect(projection.models).toEqual([{ id: 'm1', provider_id: 'p1', name: 'gpt-fixture', enabled: true, is_default: true }]);
        expect(projection.advanced).toMatchObject({ temperature: 1.5, response_style: 'concise' });
        expect(JSON.stringify(projection)).not.toContain('SECRET-KEY');
        expect(JSON.stringify(projection)).not.toContain('api.openai.com');
    });
    it('caps provider and model lists', () => {
        const settings = getDefaultAISettings();
        for (let i = 0; i < MAX_SETTINGS_PROVIDERS + 5; i++) {
            settings.providers.push({
                id: `p${i}`, name: `P${i}`, type: 'openai', baseUrl: '', isEnabled: false, isDefault: false,
                createdAt: new Date(0), updatedAt: new Date(0),
            });
        }
        for (let i = 0; i < MAX_SETTINGS_MODELS + 5; i++) {
            settings.models.push({
                id: `m${i}`, providerId: 'p0', name: `m${i}`, displayName: `m${i}`, maxTokens: 4096,
                supportsStreaming: true, supportsTools: true, supportsVision: false, isEnabled: true, isDefault: false,
            });
        }
        const projection = buildAiProjection(settings);
        expect(projection.providers).toHaveLength(MAX_SETTINGS_PROVIDERS);
        expect(projection.models).toHaveLength(MAX_SETTINGS_MODELS);
    });
});

describe('settingsUpdateCommitted', () => {
    const projection: GuiSettingsProjection = {
        open: 'ai',
        general: { showHiddenFiles: true, fontSize: 18 },
        ai: {
            providers: [{ id: 'p1', name: 'OpenAI', type: 'openai', enabled: false }],
            models: [
                { id: 'm1', provider_id: 'p1', name: 'a', enabled: true, is_default: false },
                { id: 'm2', provider_id: 'p1', name: 'b', enabled: true, is_default: true },
            ],
            default_model_id: null,
            advanced: { temperature: 0.7 },
        },
    };
    it('requires every requested change to be reflected', () => {
        expect(settingsUpdateCommitted('general', { fontSize: 18 }, projection)).toBe(true);
        expect(settingsUpdateCommitted('general', { fontSize: 20 }, projection)).toBe(false);
        expect(settingsUpdateCommitted('ai', { provider_enabled: { id: 'p1', enabled: true } }, projection)).toBe(false);
        expect(settingsUpdateCommitted('ai', { provider_enabled: { id: 'p1', enabled: false } }, projection)).toBe(true);
        expect(settingsUpdateCommitted('ai', { model_default: { id: 'm2' } }, projection)).toBe(true);
        expect(settingsUpdateCommitted('ai', { model_default: { id: 'm1' } }, projection)).toBe(false);
        expect(settingsUpdateCommitted('ai', { advanced: { temperature: 0.7 } }, projection)).toBe(true);
        expect(settingsUpdateCommitted('ai', { advanced: { temperature: 1.1 } }, projection)).toBe(false);
    });
});

it('bounds multilingual AI strings by UTF-8 bytes and refuses oversized identities', () => {
    const raw = getDefaultAISettings();
    raw.providers = [{ id: 'p', name: '漢'.repeat(200), type: 'custom', isEnabled: true } as never];
    const projection = buildAiProjection(raw);
    expect(new TextEncoder().encode(projection.providers[0].name).length).toBeLessThanOrEqual(256);
    expect(projection.providers[0].name).not.toContain('�');
    expect(() => validateSettingsSet('ai', { model_default: { id: '漢'.repeat(43) } })).toThrow('invalid_args');
});
