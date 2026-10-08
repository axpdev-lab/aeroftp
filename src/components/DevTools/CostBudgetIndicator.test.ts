// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

vi.mock('../../i18n', () => ({ useTranslation: () => (key: string) => key }));

import { CostBudgetIndicator } from './CostBudgetIndicator';

const conversation = (totalCost: number) => ({ conversationId: 'c', totalCost, totalTokens: 5000, requestCount: 2, lastUpdated: '' });
const render = (props: Parameters<typeof CostBudgetIndicator>[0]) => renderToStaticMarkup(createElement(CostBudgetIndicator, props));

describe('the conversation cost under the chat', () => {
    it('reads as an estimate next to the exact token count', () => {
        const html = render({ conversationCost: conversation(0.0234), budgetCheck: null, showCostEstimates: true });
        expect(html).toContain('≈$0.023');
        expect(html).toContain('ai.costEstimates.sumTitle');
        expect(html).toContain('tok');
    });

    it('keeps the tokens and drops every amount when cost estimates are off', () => {
        const html = render({ conversationCost: conversation(0.0234), budgetCheck: null, showCostEstimates: false });
        expect(html).not.toContain('$');
        expect(html).toContain('tok');
    });

    it('shows a marker, not $0.00, when no reply could be estimated', () => {
        const html = render({
            conversationCost: conversation(0),
            budgetCheck: null,
            showCostEstimates: true,
            withheldCost: { totalTokens: 5000, costWithheld: 'undated' },
        });
        expect(html).toContain('≈$?');
        expect(html).not.toContain('$0.00');
        expect(html).toContain('ai.costEstimates.undatedTitle');
    });
});
