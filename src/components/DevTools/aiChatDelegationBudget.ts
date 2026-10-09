// SPDX-License-Identifier: GPL-3.0-or-later
import { checkBudget, recordSpending, type BudgetCheckResult } from './CostBudgetManager';
import { computeTokenInfo, type ModelCostInfo } from './aiChatTokenInfo';

/** Account for aggregate parent/worker usage on the single pinned model route. */
export async function runBudgetedDelegation<T extends { inputTokens: number; outputTokens: number }>(
    providerId: string, model: ModelCostInfo | undefined, conversationId: string | undefined,
    dispatch: () => Promise<T>, onBudget: (result: BudgetCheckResult) => void,
) {
    const budget = checkBudget(providerId);
    onBudget(budget);
    if (!budget.allowed) throw new Error(budget.message || 'Monthly budget exceeded.');
    const result = await dispatch();
    const tokenInfo = computeTokenInfo(result.inputTokens, result.outputTokens, undefined, model);
    // Successful calls have been billed even if the view was cancelled while
    // they completed. Record usage before the caller's view/current-turn checks.
    // No usage means no cost; usage without an estimate stays unknown.
    const updated = await recordSpending(providerId, tokenInfo ? tokenInfo.cost : 0,
        result.inputTokens + result.outputTokens, conversationId);
    onBudget(updated);
    return { result, tokenInfo };
}
