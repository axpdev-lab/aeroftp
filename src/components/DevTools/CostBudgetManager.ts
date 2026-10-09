// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

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
    unpricedRequests?: number; // Requests whose cost could not be estimated (absent in older records)
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
 * Initialize budget manager: load config and spending from vault
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
                // Admission runs before recordSpending: normalize persisted cost
                // now, and fail closed when the prior spend cannot be trusted.
                spendingCache.set(spendingKey(r.providerId, r.month), {
                    ...r, totalCost: persistedCost(r.totalCost), unpricedRequests: persistedUnpriced(r.unpricedRequests),
                });
            });
        }
    } catch {
        // No spending data yet
    }
}

function persistedCost(value: unknown): number {
    const parsed = typeof value === 'string' && value.trim() !== '' ? Number(value) : value;
    return typeof parsed === 'number' && Number.isFinite(parsed) && parsed >= 0
        ? Math.min(parsed, Number.MAX_SAFE_INTEGER)
        : Number.MAX_SAFE_INTEGER;
}

/** A record from before the field existed has none; anything else unreadable fails closed. */
function persistedUnpriced(value: unknown): number {
    if (value === undefined) return 0;
    return typeof value === 'number' && Number.isSafeInteger(value) && value >= 0 ? value : Number.MAX_SAFE_INTEGER;
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

    // A request whose cost could not be estimated (no current price list) is
    // not a $0 request: the spend above leaves it out, so the limit can no
    // longer be proved. A hard stop refuses, a plain budget warns.
    const unpriced = record?.unpricedRequests ?? 0;
    if (unpriced > 0) {
        const requests = unpriced === 1 ? '1 request' : `${unpriced} requests`;
        return {
            allowed: !config.hardStop,
            currentSpend,
            limit: config.monthlyLimitUsd,
            percentUsed: Math.min(100, Math.round(percentUsed)),
            warning: true,
            message: config.hardStop
                ? `Monthly budget cannot be enforced: ${requests} this month had no cost estimate (no current price list for the model).`
                : `Budget cannot be checked: ${requests} this month had no cost estimate (no current price list for the model).`,
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

/** Keep persisted counters finite; positive overflow saturates to fail closed. */
function addBoundedCounter(current: number, delta: number): number {
    const bounded = (value: number): number => typeof value !== 'number' || Number.isNaN(value) || value < 0
        ? 0
        : Math.min(value, Number.MAX_SAFE_INTEGER);
    return Math.min(Number.MAX_SAFE_INTEGER, bounded(current) + bounded(delta));
}

/**
 * Record spending after a request completes
 */
export async function recordSpending(
    providerId: string,
    /** Estimated USD; `undefined` when it could not be estimated, never meaning $0. */
    cost: number | undefined,
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
        unpricedRequests: 0,
    };
    if (cost === undefined) {
        existing.unpricedRequests = addBoundedCounter(existing.unpricedRequests ?? 0, 1);
    } else {
        existing.totalCost = addBoundedCounter(existing.totalCost, cost);
    }
    existing.requestCount = addBoundedCounter(existing.requestCount, 1);
    existing.tokenCount = addBoundedCounter(existing.tokenCount, tokens);
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
        // The conversation shows the sum of the estimates; the reply itself
        // says when its cost was not estimated.
        convCost.totalCost = addBoundedCounter(convCost.totalCost, cost ?? 0);
        convCost.totalTokens = addBoundedCounter(convCost.totalTokens, tokens);
        convCost.requestCount = addBoundedCounter(convCost.requestCount, 1);
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
        // Vault not available: in-memory tracking still works
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
