// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { computeTokenInfo } from './aiChatTokenInfo';

describe('the token count of a reply', () => {
    it('counts the cached input Anthropic reports apart from input_tokens', () => {
        // Streaming: no provider total, the sum is ours.
        const streamed = computeTokenInfo(3429, 414, undefined, 120, 6763)!;
        expect(streamed.totalTokens).toBe(3429 + 414 + 120 + 6763);
        // Non-streaming: the backend's tokens_used is input plus output only.
        const whole = computeTokenInfo(3429, 414, 3429 + 414, 120, 6763)!;
        expect(whole.totalTokens).toBe(3429 + 414 + 120 + 6763);
        expect(whole).toMatchObject({ inputTokens: 3429, outputTokens: 414, cacheCreationTokens: 120, cacheReadTokens: 6763 });
    });

    it('keeps a reply whose usage is all cached input', () => {
        expect(computeTokenInfo(0, 0, undefined, 0, 5000)?.totalTokens).toBe(5000);
    });

    it('reports nothing for a reply without usage', () => {
        expect(computeTokenInfo(undefined, undefined, undefined)).toBeUndefined();
        expect(computeTokenInfo(0, 0, 0, 0, 0)).toBeUndefined();
    });

    it('carries token counts and nothing priced', () => {
        // Prices change per model and no provider returns them per request:
        // the reply shows what the provider counted, never an amount.
        const info = computeTokenInfo(100, 50, undefined, 10, 20)!;
        expect(Object.keys(info).sort()).toEqual(['cacheCreationTokens', 'cacheReadTokens', 'inputTokens', 'outputTokens', 'totalTokens']);
    });
});
