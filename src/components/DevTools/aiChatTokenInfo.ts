// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { Message } from './aiChatTypes';
import { determineBudgetMode } from './aiChatSmartContext';
import { BudgetMode } from '../../types/contextIntelligence';
import type { AIModelPricing } from '../../types/ai';
import { PRICE_LIST_MAX_AGE_DAYS } from '../../types/aiModelRegistry';

export interface ModelCostInfo {
    inputCostPer1k?: number;
    outputCostPer1k?: number;
    pricing?: AIModelPricing;
    priceReviewedAt?: string;
}

const DAY_MS = 24 * 60 * 60 * 1000;

/**
 * Whether prices checked on `reviewedAt` (an ISO day) can still back an
 * estimate at `now`: through the 90th day after the review, yes. A missing
 * or unparseable date is no date; one ahead of the clock counts as current.
 */
export function priceListStatus(reviewedAt: string | undefined, now: Date): 'current' | 'expired' | 'undated' {
    if (!reviewedAt || !/^\d{4}-\d{2}-\d{2}$/.test(reviewedAt)) return 'undated';
    const reviewed = Date.parse(`${reviewedAt}T00:00:00Z`);
    if (Number.isNaN(reviewed) || new Date(reviewed).toISOString().slice(0, 10) !== reviewedAt) return 'undated';
    return Math.floor((now.getTime() - reviewed) / DAY_MS) > PRICE_LIST_MAX_AGE_DAYS ? 'expired' : 'current';
}

export interface TokenBudgetBreakdown {
    modelMaxTokens: number;
    systemPromptTokens: number;
    contextTokens: number;
    historyTokens: number;
    currentMessageTokens: number;
    responseBuffer: number;
    availableTokens: number;
    usagePercent: number;
    mode: BudgetMode;
}

/**
 * Compute the response buffer for a given model's max token capacity.
 * Used consistently by both computeTokenBudget and buildMessageWindow.
 */
export function computeResponseBuffer(modelMaxTokens: number): number {
    return Math.min(2048, Math.max(512, Math.floor(modelMaxTokens * 0.15)));
}

/**
 * Compute token budget breakdown for the current message.
 * Determines the optimal budget mode based on available space.
 */
export function computeTokenBudget(
    modelMaxTokens: number,
    systemPromptTokens: number,
    contextTokens: number,
    historyTokens: number,
    currentMessageTokens: number,
): TokenBudgetBreakdown {
    if (modelMaxTokens <= 0) {
        return {
            modelMaxTokens: 0,
            systemPromptTokens,
            contextTokens,
            historyTokens,
            currentMessageTokens,
            responseBuffer: 0,
            availableTokens: 0,
            usagePercent: 100,
            mode: 'minimal' as const,
        };
    }

    const responseBuffer = computeResponseBuffer(modelMaxTokens);
    const used = systemPromptTokens + contextTokens + historyTokens + currentMessageTokens + responseBuffer;
    const availableTokens = Math.max(0, modelMaxTokens - used);
    const usagePercent = Math.min(100, Math.round((used / modelMaxTokens) * 100));

    // Determine mode based on model capacity (canonical logic in aiChatSmartContext.ts)
    const mode = determineBudgetMode(modelMaxTokens);

    return {
        modelMaxTokens,
        systemPromptTokens,
        contextTokens,
        historyTokens,
        currentMessageTokens,
        responseBuffer,
        availableTokens,
        usagePercent,
        mode,
    };
}

export function computeTokenInfo(
    inputTokens: number | undefined,
    outputTokens: number | undefined,
    tokensUsed: number | undefined,
    modelCost: ModelCostInfo | undefined,
    cacheCreationTokens?: number,
    cacheReadTokens?: number,
    now: Date = new Date(),
): Message['tokenInfo'] | undefined {
    if (!inputTokens && !outputTokens && !tokensUsed) return undefined;

    const input = inputTokens || 0;
    const output = outputTokens || 0;
    const written = cacheCreationTokens || 0;
    const read = cacheReadTokens || 0;
    const pricing = modelCost?.pricing;

    let cost: number | undefined;
    let cacheSavings: number | undefined;
    if (pricing && modelCost?.inputCostPer1k && modelCost?.outputCostPer1k) {
        // A model with a published price list (every Anthropic model): the
        // provider bills cache writes and reads apart from `input_tokens`, so
        // each is priced at its own multiplier, and the tier is picked from the
        // whole prompt, cached parts included.
        const prompt = input + written + read;
        const tier = [...(pricing.tiers ?? [])]
            .sort((a, b) => b.aboveTokens - a.aboveTokens)
            .find(t => prompt > t.aboveTokens);
        const inRate = tier?.inputCostPer1k ?? modelCost.inputCostPer1k;
        const outRate = tier?.outputCostPer1k ?? modelCost.outputCostPer1k;
        const writeMultiplier = pricing.cacheWriteMultiplier ?? 1;
        const readMultiplier = pricing.cacheReadMultiplier ?? 1;
        cost = (input / 1000) * inRate
            + (output / 1000) * outRate
            + (written / 1000) * inRate * writeMultiplier
            + (read / 1000) * inRate * readMultiplier;
        if (written || read) {
            // Against the same prompt sent uncached.
            cacheSavings = (read / 1000) * inRate * (1 - readMultiplier)
                - (written / 1000) * inRate * (writeMultiplier - 1);
        }
    } else {
        cost = modelCost?.inputCostPer1k && modelCost?.outputCostPer1k
            ? (input / 1000) * modelCost.inputCostPer1k + (output / 1000) * modelCost.outputCostPer1k
            : undefined;
        // Without a price list, the generic estimate: reads about 90% cheaper
        // than input, writes 25% dearer.
        if (modelCost?.inputCostPer1k && (written || read)) {
            const readDiscount = (read / 1000) * modelCost.inputCostPer1k * 0.9;
            const creationSurcharge = (written / 1000) * modelCost.inputCostPer1k * 0.25;
            cacheSavings = readDiscount - creationSurcharge;
        }
    }

    const tokens = {
        inputTokens,
        outputTokens,
        totalTokens: tokensUsed ?? (input + output),
        cacheCreationTokens,
        cacheReadTokens,
    };
    if (cost === undefined) return tokens;
    // The tokens are the provider's count; the money is ours, from a price
    // list that ages. Without a date, or past 90 days, no amount is shown,
    // summed, saved or exported: the reply says why instead.
    const status = priceListStatus(modelCost?.priceReviewedAt, now);
    const priceListDate = status === 'undated' ? undefined : modelCost?.priceReviewedAt;
    if (status !== 'current') return { ...tokens, priceListDate, costWithheld: status };
    return { ...tokens, cost, cacheSavings, priceListDate };
}
