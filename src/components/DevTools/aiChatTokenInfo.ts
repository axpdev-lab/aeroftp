import { Message } from './aiChatTypes';
import { determineBudgetMode } from './aiChatSmartContext';
import { BudgetMode } from '../../types/contextIntelligence';

export interface ModelCostInfo {
    inputCostPer1k?: number;
    outputCostPer1k?: number;
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
): Message['tokenInfo'] | undefined {
    if (!inputTokens && !outputTokens && !tokensUsed) return undefined;

    const cost = modelCost?.inputCostPer1k && modelCost?.outputCostPer1k
        ? ((inputTokens || 0) / 1000) * modelCost.inputCostPer1k +
          ((outputTokens || 0) / 1000) * modelCost.outputCostPer1k
        : undefined;

    // Anthropic prompt caching savings calculation:
    // Cache reads are 90% cheaper than normal input tokens.
    // Cache creation costs 25% more than normal input tokens.
    // Net savings = read discount - creation surcharge.
    let cacheSavings: number | undefined;
    if (modelCost?.inputCostPer1k && (cacheCreationTokens || cacheReadTokens)) {
        const readDiscount = ((cacheReadTokens || 0) / 1000) * modelCost.inputCostPer1k * 0.9;
        const creationSurcharge = ((cacheCreationTokens || 0) / 1000) * modelCost.inputCostPer1k * 0.25;
        cacheSavings = readDiscount - creationSurcharge;
    }

    return {
        inputTokens,
        outputTokens,
        totalTokens: tokensUsed ?? ((inputTokens || 0) + (outputTokens || 0)),
        cost,
        cacheCreationTokens,
        cacheReadTokens,
        cacheSavings,
    };
}
