// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
    conversationCostView,
    costEstimateView,
    exportTokenLine,
    latestWithheldCost,
    withoutCost,
} from './aiChatCostEstimate';
import type { Message } from './aiChatTypes';

const t = (key: string, params?: Record<string, string | number>) =>
    params ? `${key} ${JSON.stringify(params)}` : key;
const on = { enabled: true, t };
const off = { enabled: false, t };

const priced = { inputTokens: 3429, outputTokens: 414, totalTokens: 3843, cost: 0.0116743, priceListDate: '2026-10-08' };
const expired = { inputTokens: 3429, outputTokens: 414, totalTokens: 3843, costWithheld: 'expired' as const, priceListDate: '2026-10-08' };
const undated = { inputTokens: 1000, outputTokens: 1000, totalTokens: 2000, costWithheld: 'undated' as const };

describe('the cost of one reply', () => {
    it('reads as an estimate and names the date of its price list', () => {
        const view = costEstimateView(priced, on);
        expect(view.kind).toBe('estimate');
        expect(view.text).toBe('≈$0.012');
        expect(view.title).toContain('ai.costEstimates.estimateTitle');
        expect(view.title).toContain('2026');
    });

    it('reads as an estimate saved with the message when it has no date (history)', () => {
        const view = costEstimateView({ totalTokens: 10, cost: 0.02 }, on);
        expect(view.kind).toBe('estimate');
        expect(view.title).toBe('ai.costEstimates.savedEstimateTitle');
    });

    it('shows a marker, never a number or nothing, when the cost was withheld', () => {
        const old = costEstimateView(expired, on);
        expect(old.kind).toBe('withheld');
        expect(old.text).toBe('≈$?');
        expect(old.title).toContain('ai.costEstimates.expiredTitle');
        expect(old.title).toContain('"days":90');
        const none = costEstimateView(undated, on);
        expect(none.kind).toBe('withheld');
        expect(none.title).toBe('ai.costEstimates.undatedTitle');
    });

    it('shows nothing for a free or unpriced reply, and nothing at all when switched off', () => {
        expect(costEstimateView({ totalTokens: 10 }, on).kind).toBe('hidden');
        expect(costEstimateView({ totalTokens: 10, cost: 0 }, on).kind).toBe('hidden');
        expect(costEstimateView(undefined, on).kind).toBe('hidden');
        for (const info of [priced, expired, undated]) {
            expect(costEstimateView(info, off).kind).toBe('hidden');
        }
    });
});

describe('the cost of the conversation', () => {
    it('sums the estimates', () => {
        const view = conversationCostView(0.0234, undefined, on);
        expect(view.kind).toBe('estimate');
        expect(view.text).toBe('≈$0.023');
        expect(view.title).toBe('ai.costEstimates.sumTitle');
    });

    it('does not pass off a sum with withheld replies as complete, nor a withheld one as $0.00', () => {
        const partial = conversationCostView(0.0234, expired, on);
        expect(partial.kind).toBe('estimate');
        expect(partial.text).toBe('≈$0.023 +?');
        expect(partial.title).toContain('ai.costEstimates.sumTitle');
        expect(partial.title).toContain('ai.costEstimates.expiredTitle');
        const nothing = conversationCostView(0, undated, on);
        expect(nothing.kind).toBe('withheld');
        expect(nothing.text).toBe('≈$?');
    });

    it('is hidden when switched off', () => {
        expect(conversationCostView(0.0234, expired, off).kind).toBe('hidden');
    });

    it('takes the latest withheld reply of the conversation', () => {
        const msg = (tokenInfo: Message['tokenInfo']): Message => ({ id: crypto.randomUUID(), role: 'assistant', content: '', timestamp: new Date(), tokenInfo });
        expect(latestWithheldCost([msg(priced), msg(undefined)])).toBeUndefined();
        expect(latestWithheldCost([msg(undated), msg(priced), msg(expired), msg(priced)])).toBe(expired);
    });
});

describe('the cost in an exported conversation', () => {
    it('writes the estimate with its date, or leaves money out', () => {
        expect(exportTokenLine(priced, true)).toBe('> 3843 tokens · ≈$0.0117 (estimate, price list of 2026-10-08)');
        expect(exportTokenLine({ totalTokens: 10, cost: 0.02 }, true)).toBe('> 10 tokens · ≈$0.0200 (estimate)');
        expect(exportTokenLine(expired, true)).toBe('> 3843 tokens · cost not estimated (price list of 2026-10-08 older than 90 days)');
        expect(exportTokenLine(undated, true)).toBe('> 2000 tokens · cost not estimated (prices without a review date)');
        expect(exportTokenLine(priced, false)).toBe('> 3843 tokens');
        expect(exportTokenLine({ cost: 0.02 }, true)).toBeUndefined();
    });

    it('strips every money field from the JSON when cost estimates are off', () => {
        const stripped = withoutCost({ ...priced, cacheSavings: 0.01, cacheReadTokens: 6763 });
        expect(stripped).toEqual({ inputTokens: 3429, outputTokens: 414, totalTokens: 3843, cacheReadTokens: 6763 });
        expect(withoutCost(undefined)).toBeUndefined();
    });
});
