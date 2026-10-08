// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { computeTokenInfo } from './aiChatTokenInfo';
import { MODEL_REGISTRY } from '../../types/aiModelRegistry';

const spec = (name: string) => MODEL_REGISTRY[name];

describe('computeTokenInfo on a model with a published price list', () => {
    it('prices the cache reads Anthropic bills apart from input_tokens', () => {
        // The live AeroAgent turn of 2026-10-08 on Claude Sonnet 5.5: the old
        // estimate showed $0.0110 and left the 6763 cached tokens out.
        const info = computeTokenInfo(3429, 414, undefined, spec('claude-sonnet-5-5'), 0, 6763)!;
        // 3429 x $2 + 414 x $10 + 6763 x $2 x 0.05, per million.
        expect(info.cost).toBeCloseTo(0.0116743, 6);
        // Against sending the 6763 tokens uncached: 95% of their input price.
        expect(info.cacheSavings).toBeCloseTo(0.0128497, 6);
    });

    it('prices cache writes at 1.25x the input price', () => {
        const info = computeTokenInfo(100, 0, undefined, spec('claude-opus-5-5'), 4000, 0)!;
        // 100 x $4 + 4000 x $4 x 1.25, per million.
        expect(info.cost).toBeCloseTo(0.0204, 6);
        expect(info.cacheSavings).toBeCloseTo(-0.004, 6);
    });

    it('uses the Fable 5.1 cache read rate of 2.5%', () => {
        const info = computeTokenInfo(0, 0, 1, spec('claude-fable-5-1'), 0, 1_000_000)!;
        expect(info.cost).toBeCloseTo(0.25, 6);
    });

    it('applies the Haiku 5.5 tier only above 100,000 prompt tokens, cached tokens included', () => {
        const atLimit = computeTokenInfo(100_000, 1000, undefined, spec('claude-haiku-5-5'))!;
        // $0.10 / $0.50 per million.
        expect(atLimit.cost).toBeCloseTo(0.0105, 6);
        const above = computeTokenInfo(50_000, 1000, undefined, spec('claude-haiku-5-5'), 0, 60_000)!;
        // 110,000-token prompt: $0.50 / $2.50, reads at 0.1x.
        expect(above.cost).toBeCloseTo(0.025 + 0.0025 + 0.003, 6);
    });

    it('keeps the old estimate for a model without a price list', () => {
        const info = computeTokenInfo(1000, 1000, undefined, { inputCostPer1k: 0.001, outputCostPer1k: 0.002 }, 0, 1000)!;
        expect(info.cost).toBeCloseTo(0.003, 6);
        expect(info.cacheSavings).toBeCloseTo(0.0009, 6);
    });
});

describe('the Anthropic price lists in the registry', () => {
    const anthropic = Object.keys(MODEL_REGISTRY).filter(name => name.startsWith('claude-'));

    it('gives every Anthropic model its prices and cache multipliers', () => {
        for (const name of anthropic) {
            expect(spec(name).inputCostPer1k, name).toBeGreaterThan(0);
            expect(spec(name).outputCostPer1k, name).toBeGreaterThan(0);
            expect(spec(name).pricing?.cacheWriteMultiplier, name).toBe(1.25);
        }
    });

    it('records the reduced cache read rates the price list names', () => {
        const reads = Object.fromEntries(anthropic.map(name => [name, spec(name).pricing?.cacheReadMultiplier]));
        expect(reads['claude-opus-5-5']).toBe(0.05);
        expect(reads['claude-sonnet-5-5']).toBe(0.05);
        expect(reads['claude-fable-5-1']).toBe(0.025);
        for (const name of anthropic.filter(n => !['claude-opus-5-5', 'claude-sonnet-5-5', 'claude-fable-5-1'].includes(n))) {
            expect(reads[name], name).toBe(0.1);
        }
    });
});
