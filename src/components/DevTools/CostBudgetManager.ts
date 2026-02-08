import { invoke } from '@tauri-apps/api/core';

/** Budget configuration per provider */
export interface ProviderBudget {
    providerId: string;
    monthlyLimitUsd: number;     // Monthly spending limit in USD (0 = unlimited)
    warningThreshold: number;    // Percentage (0-100) at which to warn (default 80)
    hardStop: boolean;           // If true, block requests when limit reached
}

/** Spending record for a single period */
export interface SpendingRecord {
    providerId: string;
    month: string;          // "2026-02" format
    totalCost: number;      // Total USD spent
    requestCount: number;   // Number of requests
    tokenCount: number;     // Total tokens used
}

/** Per-conversation cost summary */
export interface ConversationCost {
    conversationId: string;
    totalCost: number;
    totalTokens: number;
    requestCount: number;
    lastUpdated: string;
}

/** Budget check result */
export interface BudgetCheckResult {
    allowed: boolean;
    currentSpend: number;
    limit: number;
    percentUsed: number;
    warning: boolean;       // true if past warning threshold
    message?: string;       // Human-readable message for alerts
}

// In-memory cache for the current session
let spendingCache: Map<string, SpendingRecord> = new Map();
let budgetConfig: ProviderBudget[] = [];
let conversationCosts: Map<string, ConversationCost> = new Map();

/** Get current month key */
function getCurrentMonth(): string {
    const now = new Date();
    return `${now.getFullYear()}-${String(now.getMonth() + 1).padStart(2, '0')}`;
}

/** Get cache key for a provider + month */
function spendingKey(providerId: string, month: string): string {
    return `${providerId}:${month}`;
}

/**
 * Initialize budget manager — load config and spending from vault
 */
export async function initBudgetManager(): Promise<void> {
    try {
        const configJson = await invoke<string>('vault_get', { key: 'ai_budget_config' });
        if (configJson) {
            budgetConfig = JSON.parse(configJson);
        }
    } catch {
        budgetConfig = [];
    }

    try {
        const month = getCurrentMonth();
        const spendingJson = await invoke<string>('vault_get', { key: `ai_spending_${month}` });
        if (spendingJson) {
            const records: SpendingRecord[] = JSON.parse(spendingJson);
            records.forEach(r => {
                spendingCache.set(spendingKey(r.providerId, r.month), r);
            });
        }
    } catch {
        // No spending data yet
    }
}

/**
 * Check if a request is allowed within the budget
 */
export function checkBudget(providerId: string): BudgetCheckResult {
    const config = budgetConfig.find(b => b.providerId === providerId);

    // No budget configured = unlimited
    if (!config || config.monthlyLimitUsd <= 0) {
        const record = spendingCache.get(spendingKey(providerId, getCurrentMonth()));
        return {
            allowed: true,
            currentSpend: record?.totalCost || 0,
            limit: 0,
            percentUsed: 0,
            warning: false,
        };
    }

    const month = getCurrentMonth();
    const record = spendingCache.get(spendingKey(providerId, month));
    const currentSpend = record?.totalCost || 0;
    const percentUsed = (currentSpend / config.monthlyLimitUsd) * 100;
    const warning = percentUsed >= config.warningThreshold;
    const overLimit = percentUsed >= 100;

    if (overLimit && config.hardStop) {
        return {
            allowed: false,
            currentSpend,
            limit: config.monthlyLimitUsd,
            percentUsed: Math.min(100, Math.round(percentUsed)),
            warning: true,
            message: `Monthly budget exhausted ($${currentSpend.toFixed(2)} / $${config.monthlyLimitUsd.toFixed(2)}). Increase your limit in AI Settings.`,
        };
    }

    return {
        allowed: true,
        currentSpend,
        limit: config.monthlyLimitUsd,
        percentUsed: Math.min(100, Math.round(percentUsed)),
        warning,
        message: warning
            ? `Budget warning: $${currentSpend.toFixed(2)} of $${config.monthlyLimitUsd.toFixed(2)} used (${Math.round(percentUsed)}%)`
            : undefined,
    };
}

/**
 * Record spending after a request completes
 */
export async function recordSpending(
    providerId: string,
    cost: number,
    tokens: number,
    conversationId?: string,
): Promise<BudgetCheckResult> {
    const month = getCurrentMonth();
    const key = spendingKey(providerId, month);

    // Update provider spending
    const existing = spendingCache.get(key) || {
        providerId,
        month,
        totalCost: 0,
        requestCount: 0,
        tokenCount: 0,
    };
    existing.totalCost += cost;
    existing.requestCount += 1;
    existing.tokenCount += tokens;
    spendingCache.set(key, existing);

    // Update conversation cost
    if (conversationId) {
        const convCost = conversationCosts.get(conversationId) || {
            conversationId,
            totalCost: 0,
            totalTokens: 0,
            requestCount: 0,
            lastUpdated: new Date().toISOString(),
        };
        convCost.totalCost += cost;
        convCost.totalTokens += tokens;
        convCost.requestCount += 1;
        convCost.lastUpdated = new Date().toISOString();
        conversationCosts.set(conversationId, convCost);
    }

    // Persist to vault
    try {
        const allRecords = Array.from(spendingCache.values()).filter(r => r.month === month);
        await invoke('vault_set', {
            key: `ai_spending_${month}`,
            value: JSON.stringify(allRecords),
        });
    } catch {
        // Vault not available — in-memory tracking still works
    }

    return checkBudget(providerId);
}

/**
 * Get spending summary for a conversation
 */
export function getConversationCost(conversationId: string): ConversationCost | null {
    return conversationCosts.get(conversationId) || null;
}

/**
 * Get spending for current month for all providers
 */
export function getMonthlySpending(): SpendingRecord[] {
    const month = getCurrentMonth();
    return Array.from(spendingCache.values()).filter(r => r.month === month);
}

/**
 * Get budget config
 */
export function getBudgetConfig(): ProviderBudget[] {
    return [...budgetConfig];
}

/**
 * Save budget config
 */
export async function saveBudgetConfig(config: ProviderBudget[]): Promise<void> {
    budgetConfig = config;
    try {
        await invoke('vault_set', {
            key: 'ai_budget_config',
            value: JSON.stringify(config),
        });
    } catch {
        // Vault not available
    }
}

/**
 * Reset spending for a provider (admin action)
 */
export function resetProviderSpending(providerId: string): void {
    const month = getCurrentMonth();
    spendingCache.delete(spendingKey(providerId, month));
}

/**
 * Format cost for display
 */
export function formatCost(cost: number): string {
    if (cost === 0) return '$0.00';
    if (cost < 0.001) return `$${cost.toFixed(5)}`;
    if (cost < 0.01) return `$${cost.toFixed(4)}`;
    if (cost < 1) return `$${cost.toFixed(3)}`;
    return `$${cost.toFixed(2)}`;
}
