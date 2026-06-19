// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import type { AISettings } from '../../types/ai';
import { buildSystemPrompt } from './aiChatSystemPrompt';

const baseSettings: AISettings = {
    providers: [],
    models: [],
    autoRouting: {
        enabled: false,
        rules: [],
    },
    advancedSettings: {
        temperature: 0.7,
        maxTokens: 4096,
        conversationStyle: 'balanced',
    },
    defaultModelId: null,
};

describe('buildSystemPrompt prompt profiles', () => {
    it('keeps the default file-manager and protocol profile', () => {
        const prompt = buildSystemPrompt(baseSettings, '', 'openai', 'full', 'gpt-5');

        expect(prompt).toContain('AI file management assistant');
        expect(prompt).toContain('## Protocol & Provider Expertise');
        expect(prompt).toContain('You are an expert on every protocol');
        expect(prompt).toContain('## AeroFTP Quick Reference');
    });

    it('switches to the coding-agent profile without the full protocol encyclopedia', () => {
        const prompt = buildSystemPrompt(baseSettings, '', 'openai', 'full', 'gpt-5', {
            promptProfile: 'coding_agent',
        });

        expect(prompt).toContain('Coding Agent profile');
        expect(prompt).toContain('understand, edit, verify, and deliver software projects');
        expect(prompt).toContain('For repository work, prefer local_read/local_grep/local_tree/local_diff');
        expect(prompt).toContain('Verification is expected after code changes');
        expect(prompt).not.toContain('## Protocol & Provider Expertise');
        expect(prompt).not.toContain('You are an expert on every protocol');
        expect(prompt).not.toContain('## AeroFTP Quick Reference');
    });

    it('adds the coding profile overlay when a custom prompt is active', () => {
        const prompt = buildSystemPrompt({
            ...baseSettings,
            advancedSettings: {
                ...baseSettings.advancedSettings,
                useCustomPrompt: true,
                customSystemPrompt: 'Custom base prompt.',
            },
        }, '', 'openai', 'full', 'gpt-5', {
            promptProfile: 'coding_agent',
        });

        expect(prompt).toContain('Custom base prompt.');
        expect(prompt).toContain('## Active Agent Profile: Coding Agent');
        expect(prompt).toContain('Workspace checkpoint, patch, and git tools are available for coding work');
    });
});
