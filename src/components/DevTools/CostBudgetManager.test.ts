// SPDX-License-Identifier: GPL-3.0-or-later
import { beforeEach, expect, it, vi } from 'vitest';
const mocks = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock('@tauri-apps/api/core', () => ({ invoke: mocks.invoke }));
beforeEach(() => { vi.resetModules(); mocks.invoke.mockReset(); mocks.invoke.mockResolvedValue(undefined); });

it('does not subtract spending or persist NaN deltas', async () => {
    const budget = await import('./CostBudgetManager');
    await budget.recordSpending('p', 2, 10, 'c');
    await budget.recordSpending('p', -3, Number.NaN, 'c');
    expect(budget.getMonthlySpending()[0]).toMatchObject({ totalCost: 2, tokenCount: 10, requestCount: 2 });
    expect(budget.getConversationCost('c')).toMatchObject({ totalCost: 2, totalTokens: 10, requestCount: 2 });
});

it('saturates provider and conversation counters and keeps the budget blocked', async () => {
    const budget = await import('./CostBudgetManager');
    await budget.saveBudgetConfig([{ providerId: 'p', monthlyLimitUsd: 1, warningThreshold: 80, hardStop: true }]);
    await budget.recordSpending('p', Number.MAX_SAFE_INTEGER, Number.MAX_SAFE_INTEGER, 'c');
    const result = await budget.recordSpending('p', Number.POSITIVE_INFINITY, Number.MAX_VALUE, 'c');
    expect(result.allowed).toBe(false);
    expect(budget.getMonthlySpending()[0]).toMatchObject({ totalCost: Number.MAX_SAFE_INTEGER, tokenCount: Number.MAX_SAFE_INTEGER });
    expect(budget.getConversationCost('c')).toMatchObject({ totalCost: Number.MAX_SAFE_INTEGER, totalTokens: Number.MAX_SAFE_INTEGER });
    const writes = mocks.invoke.mock.calls.filter(([name]) => name === 'vault_set');
    const stored = writes[writes.length - 1]?.[1].value;
    expect(JSON.parse(stored)[0]).toMatchObject({ totalCost: Number.MAX_SAFE_INTEGER, tokenCount: Number.MAX_SAFE_INTEGER });
});

it('bounds corrupt loaded counters before adding a valid delta', async () => {
    const budget = await import('./CostBudgetManager');
    const month = `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}`;
    mocks.invoke.mockImplementation(async (command: string, args: { key: string }) => command === 'vault_get' && args.key.startsWith('ai_spending_')
        ? JSON.stringify([{ providerId: 'p', month, totalCost: -3, tokenCount: Number.MAX_VALUE, requestCount: Number.MAX_VALUE }]) : undefined);
    await budget.initBudgetManager();
    await budget.recordSpending('p', 2, 5);
    expect(budget.getMonthlySpending()[0]).toMatchObject({ totalCost: Number.MAX_SAFE_INTEGER, tokenCount: Number.MAX_SAFE_INTEGER, requestCount: Number.MAX_SAFE_INTEGER });
});

it('normalizes missing and non-numeric persisted counter fields', async () => {
    const budget = await import('./CostBudgetManager');
    const month = `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}`;
    mocks.invoke.mockImplementation(async (command: string, args: { key: string }) => command === 'vault_get' && args.key.startsWith('ai_spending_')
        ? JSON.stringify([{ providerId: 'p', month, totalCost: 'invalid', tokenCount: null }]) : undefined);
    await budget.initBudgetManager();
    await budget.recordSpending('p', 2, 5);
    expect(budget.getMonthlySpending()[0]).toMatchObject({ totalCost: Number.MAX_SAFE_INTEGER, tokenCount: 5, requestCount: 1 });
});

it.each(['invalid', '', null, undefined, -3])('blocks budget admission for corrupt persisted cost %s before spending', async totalCost => {
    const budget = await import('./CostBudgetManager');
    const month = `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}`;
    mocks.invoke.mockImplementation(async (command: string, args: { key: string }) => {
        if (command !== 'vault_get') return undefined;
        return JSON.stringify(args.key === 'ai_budget_config'
            ? [{ providerId: 'p', monthlyLimitUsd: 10, warningThreshold: 80, hardStop: true }]
            : [{ providerId: 'p', month, totalCost }]);
    });
    await budget.initBudgetManager();
    expect(budget.checkBudget('p')).toMatchObject({ allowed: false, currentSpend: Number.MAX_SAFE_INTEGER });
});

it('accepts valid numeric persisted strings before admission without formatting errors', async () => {
    const budget = await import('./CostBudgetManager');
    const month = `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}`;
    mocks.invoke.mockImplementation(async (command: string, args: { key: string }) => {
        if (command !== 'vault_get') return undefined;
        return JSON.stringify(args.key === 'ai_budget_config'
            ? [{ providerId: 'p', monthlyLimitUsd: 10, warningThreshold: 80, hardStop: true }]
            : [{ providerId: 'p', month, totalCost: '9' }]);
    });
    await budget.initBudgetManager();
    expect(budget.checkBudget('p')).toMatchObject({ allowed: true, currentSpend: 9, warning: true });
});

it('counts a request it cannot price as unpriced, never as $0 spent', async () => {
    const budget = await import('./CostBudgetManager');
    await budget.recordSpending('p', 0.5, 10, 'c');
    await budget.recordSpending('p', undefined, 20, 'c');
    expect(budget.getMonthlySpending()[0]).toMatchObject({ totalCost: 0.5, tokenCount: 30, requestCount: 2, unpricedRequests: 1 });
    expect(budget.getConversationCost('c')).toMatchObject({ totalCost: 0.5, totalTokens: 30, requestCount: 2 });
});

it('a hard stop refuses further requests once the month has usage it could not price', async () => {
    const budget = await import('./CostBudgetManager');
    await budget.saveBudgetConfig([{ providerId: 'p', monthlyLimitUsd: 10, warningThreshold: 80, hardStop: true }]);
    expect((await budget.recordSpending('p', 1, 10)).allowed).toBe(true);
    const result = await budget.recordSpending('p', undefined, 10);
    expect(result).toMatchObject({ allowed: false, warning: true });
    expect(result.message).toContain('no cost estimate');
    expect(budget.checkBudget('p').allowed).toBe(false);
});

it('without a hard stop, unpriced usage warns instead of passing for $0', async () => {
    const budget = await import('./CostBudgetManager');
    await budget.saveBudgetConfig([{ providerId: 'p', monthlyLimitUsd: 10, warningThreshold: 80, hardStop: false }]);
    const result = await budget.recordSpending('p', undefined, 10);
    expect(result).toMatchObject({ allowed: true, warning: true });
    expect(result.message).toContain('no cost estimate');
});

it('with no budget, unpriced usage changes nothing', async () => {
    const budget = await import('./CostBudgetManager');
    expect(await budget.recordSpending('p', undefined, 10)).toMatchObject({ allowed: true, warning: false });
});

it.each(['many', -1, 1.5])('fails closed on a corrupt persisted unpriced count %s', async unpricedRequests => {
    const budget = await import('./CostBudgetManager');
    const month = `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}`;
    mocks.invoke.mockImplementation(async (command: string, args: { key: string }) => {
        if (command !== 'vault_get') return undefined;
        return JSON.stringify(args.key === 'ai_budget_config'
            ? [{ providerId: 'p', monthlyLimitUsd: 10, warningThreshold: 80, hardStop: true }]
            : [{ providerId: 'p', month, totalCost: 1, unpricedRequests }]);
    });
    await budget.initBudgetManager();
    expect(budget.checkBudget('p').allowed).toBe(false);
});

it('reads a persisted record without the unpriced count, from an older version, as none', async () => {
    const budget = await import('./CostBudgetManager');
    const month = `${new Date().getFullYear()}-${String(new Date().getMonth() + 1).padStart(2, '0')}`;
    mocks.invoke.mockImplementation(async (command: string, args: { key: string }) => {
        if (command !== 'vault_get') return undefined;
        return JSON.stringify(args.key === 'ai_budget_config'
            ? [{ providerId: 'p', monthlyLimitUsd: 10, warningThreshold: 80, hardStop: true }]
            : [{ providerId: 'p', month, totalCost: 1, tokenCount: 5, requestCount: 1 }]);
    });
    await budget.initBudgetManager();
    expect(budget.checkBudget('p')).toMatchObject({ allowed: true, warning: false });
});
