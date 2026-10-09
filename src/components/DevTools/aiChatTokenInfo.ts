// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { Message } from './aiChatTypes';
import { determineBudgetMode } from './aiChatSmartContext';
import { BudgetMode } from '../../types/contextIntelligence';

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

/**
 * The token counts of one reply, as the provider reported them. Anthropic
 * reports cache writes and reads apart from `input_tokens`, and the backend's
 * `tokens_used` is input plus output only; no other provider fills the cache
 * fields, so adding them never counts twice. Tokens only: AeroFTP does not
 * estimate what they cost, since prices change per model and no provider
 * returns them per request.
 */
export function computeTokenInfo(
    inputTokens: number | undefined,
    outputTokens: number | undefined,
    tokensUsed: number | undefined,
    cacheCreationTokens?: number,
    cacheReadTokens?: number,
): Message['tokenInfo'] | undefined {
    const input = inputTokens || 0;
    const output = outputTokens || 0;
    const written = cacheCreationTokens || 0;
    const read = cacheReadTokens || 0;
    if (!input && !output && !tokensUsed && !written && !read) return undefined;
    return {
        inputTokens,
        outputTokens,
        totalTokens: (tokensUsed ?? (input + output)) + written + read,
        cacheCreationTokens,
        cacheReadTokens,
    };
}
