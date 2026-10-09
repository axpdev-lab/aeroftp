// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { computeTokenInfo, priceListStatus } from './aiChatTokenInfo';
import { MODEL_REGISTRY, PRICE_LIST_MAX_AGE_DAYS } from '../../types/aiModelRegistry';

const spec = (name: string) => MODEL_REGISTRY[name];
// The day after the Anthropic price list was checked: every test names its
// clock, so none of them starts failing when the list ages.
const NOW = new Date('2026-10-09T12:00:00Z');

describe('computeTokenInfo on a model with a published price list', () => {
    it('prices the cache reads Anthropic bills apart from input_tokens', () => {
        // The live AeroAgent turn of 2026-10-08 on Claude Sonnet 5.5: the old
        // estimate showed $0.0110 and left the 6763 cached tokens out.
        const info = computeTokenInfo(3429, 414, undefined, spec('claude-sonnet-5-5'), 0, 6763, NOW)!;
        // 3429 x $2 + 414 x $10 + 6763 x $2 x 0.05, per million.
        expect(info.cost).toBeCloseTo(0.0116743, 6);
        // Against sending the 6763 tokens uncached: 95% of their input price.
        expect(info.cacheSavings).toBeCloseTo(0.0128497, 6);
    });

    it('prices cache writes at 1.25x the input price', () => {
        const info = computeTokenInfo(100, 0, undefined, spec('claude-opus-5-5'), 4000, 0, NOW)!;
        // 100 x $4 + 4000 x $4 x 1.25, per million.
        expect(info.cost).toBeCloseTo(0.0204, 6);
        expect(info.cacheSavings).toBeCloseTo(-0.004, 6);
    });

    it('uses the Fable 5.1 cache read rate of 2.5%', () => {
        const info = computeTokenInfo(0, 0, 1, spec('claude-fable-5-1'), 0, 1_000_000, NOW)!;
        expect(info.cost).toBeCloseTo(0.25, 6);
    });

    it('applies the Haiku 5.5 tier only above 100,000 prompt tokens, cached tokens included', () => {
        const atLimit = computeTokenInfo(100_000, 1000, undefined, spec('claude-haiku-5-5'), 0, 0, NOW)!;
        // $0.10 / $0.50 per million.
        expect(atLimit.cost).toBeCloseTo(0.0105, 6);
        const above = computeTokenInfo(50_000, 1000, undefined, spec('claude-haiku-5-5'), 0, 60_000, NOW)!;
        // 110,000-token prompt: $0.50 / $2.50, reads at 0.1x.
        expect(above.cost).toBeCloseTo(0.025 + 0.0025 + 0.003, 6);
    });

    it('keeps the old estimate for a model without cache multipliers', () => {
        const info = computeTokenInfo(1000, 1000, undefined, { inputCostPer1k: 0.001, outputCostPer1k: 0.002, priceReviewedAt: '2026-10-01' }, 0, 1000, NOW)!;
        expect(info.cost).toBeCloseTo(0.003, 6);
        expect(info.cacheSavings).toBeCloseTo(0.0009, 6);
    });
});

describe('the date of the price list', () => {
    const sonnet = spec('claude-sonnet-5-5');

    it('shows the estimate with the date of the price list it came from', () => {
        const info = computeTokenInfo(3429, 414, undefined, sonnet, 0, 6763, NOW)!;
        expect(info.cost).toBeGreaterThan(0);
        expect(info.priceListDate).toBe('2026-10-08');
        expect(info.costWithheld).toBeUndefined();
    });

    it('withholds the cost, never the tokens, once the price list is more than 90 days old', () => {
        expect(PRICE_LIST_MAX_AGE_DAYS).toBe(90);
        const lastDay = computeTokenInfo(3429, 414, undefined, sonnet, 0, 6763, new Date('2027-01-06T23:59:59Z'))!;
        expect(lastDay.cost).toBeGreaterThan(0);
        const expired = computeTokenInfo(3429, 414, undefined, sonnet, 0, 6763, new Date('2027-01-07T00:00:00Z'))!;
        expect(expired.cost).toBeUndefined();
        expect(expired.cacheSavings).toBeUndefined();
        expect(expired.costWithheld).toBe('expired');
        expect(expired.priceListDate).toBe('2026-10-08');
        expect(expired.inputTokens).toBe(3429);
        expect(expired.outputTokens).toBe(414);
        expect(expired.cacheReadTokens).toBe(6763);
    });

    it('withholds the cost of a model whose prices carry no review date', () => {
        const info = computeTokenInfo(1000, 1000, undefined, { inputCostPer1k: 0.001, outputCostPer1k: 0.002 }, 0, 1000, NOW)!;
        expect(info.cost).toBeUndefined();
        expect(info.cacheSavings).toBeUndefined();
        expect(info.costWithheld).toBe('undated');
        expect(info.priceListDate).toBeUndefined();
    });

    it('says nothing about cost for a model without prices', () => {
        for (const model of [undefined, {}]) {
            const info = computeTokenInfo(1000, 1000, undefined, model, 0, 0, NOW)!;
            expect(info.cost).toBeUndefined();
            expect(info.costWithheld).toBeUndefined();
        }
    });

    it('prices a free model at zero, a known amount rather than an unknown one', () => {
        // A local model: nothing to estimate and nothing to date.
        const info = computeTokenInfo(1000, 1000, undefined, { inputCostPer1k: 0, outputCostPer1k: 0 }, 0, 0, NOW)!;
        expect(info.cost).toBe(0);
        expect(info.costWithheld).toBeUndefined();
    });

    it('reads a date it cannot parse as no date, and a date ahead of the clock as current', () => {
        expect(priceListStatus(undefined, NOW)).toBe('undated');
        expect(priceListStatus('next week', NOW)).toBe('undated');
        expect(priceListStatus('2026-13-45', NOW)).toBe('undated');
        expect(priceListStatus('2026-10-20', NOW)).toBe('current');
    });
});

describe('the token count', () => {
    const sonnet = spec('claude-sonnet-5-5');

    it('counts the cached input Anthropic reports apart from input_tokens', () => {
        // Streaming: no provider total, the sum is ours.
        const streamed = computeTokenInfo(3429, 414, undefined, sonnet, 120, 6763, NOW)!;
        expect(streamed.totalTokens).toBe(3429 + 414 + 120 + 6763);
        // Non-streaming: the backend's tokens_used is input plus output only.
        const whole = computeTokenInfo(3429, 414, 3429 + 414, sonnet, 120, 6763, NOW)!;
        expect(whole.totalTokens).toBe(3429 + 414 + 120 + 6763);
    });

    it('keeps a reply whose usage is all cached input', () => {
        const info = computeTokenInfo(0, 0, undefined, sonnet, 0, 5000, NOW);
        expect(info?.totalTokens).toBe(5000);
        expect(info?.cost).toBeGreaterThan(0);
    });
});

describe('the Anthropic price lists in the registry', () => {
    const anthropic = Object.keys(MODEL_REGISTRY).filter(name => name.startsWith('claude-'));

    it('gives every Anthropic model its prices, cache multipliers and the date they were checked', () => {
        for (const name of anthropic) {
            expect(spec(name).inputCostPer1k, name).toBeGreaterThan(0);
            expect(spec(name).outputCostPer1k, name).toBeGreaterThan(0);
            expect(spec(name).pricing?.cacheWriteMultiplier, name).toBe(1.25);
            expect(spec(name).priceReviewedAt, name).toBe('2026-10-08');
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
