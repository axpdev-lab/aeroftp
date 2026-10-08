// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// How the chat words money. Token counts are the provider's and exact; a cost
// is our estimate from a dated price list (see `computeTokenInfo`), so it reads
// as one, names its date, and when it was withheld says so instead of showing
// nothing or $0.00. The setting `showCostEstimates` turns all of it off.

import type { Message } from './aiChatTypes';
import { formatCost } from './CostBudgetManager';
import { PRICE_LIST_MAX_AGE_DAYS } from '../../types/aiModelRegistry';

type TokenInfo = NonNullable<Message['tokenInfo']>;
type Translate = (key: string, params?: Record<string, string | number>) => string;

export interface CostEstimateView {
    kind: 'hidden' | 'estimate' | 'withheld';
    text: string;
    title: string;
}

interface ViewOptions {
    enabled: boolean;
    t: Translate;
}

const HIDDEN: CostEstimateView = { kind: 'hidden', text: '', title: '' };

/** `2026-10-08` in the reader's locale, read as the calendar day it names. */
export function formatPriceListDate(iso: string): string {
    const parsed = new Date(`${iso}T00:00:00Z`);
    if (Number.isNaN(parsed.getTime())) return iso;
    return parsed.toLocaleDateString(undefined, { dateStyle: 'medium', timeZone: 'UTC' });
}

function withheldTitle(info: Pick<TokenInfo, 'costWithheld' | 'priceListDate'>, t: Translate): string {
    return info.costWithheld === 'expired' && info.priceListDate
        ? t('ai.costEstimates.expiredTitle', { date: formatPriceListDate(info.priceListDate), days: PRICE_LIST_MAX_AGE_DAYS })
        : t('ai.costEstimates.undatedTitle');
}

/** The cost shown next to one reply's token count. */
export function costEstimateView(info: TokenInfo | undefined, { enabled, t }: ViewOptions): CostEstimateView {
    if (!enabled || !info) return HIDDEN;
    if (info.costWithheld) {
        return { kind: 'withheld', text: '≈$?', title: withheldTitle(info, t) };
    }
    if (info.cost === undefined || info.cost <= 0) return HIDDEN;
    return {
        kind: 'estimate',
        text: `≈${formatCost(info.cost)}`,
        // A message reloaded from history kept its amount but not its date.
        title: info.priceListDate
            ? t('ai.costEstimates.estimateTitle', { date: formatPriceListDate(info.priceListDate) })
            : t('ai.costEstimates.savedEstimateTitle'),
    };
}

/**
 * The conversation's running cost. `withheld` is a reply whose cost was not
 * estimated: the sum then leaves it out and says so, and a sum made only of
 * such replies is a marker, not $0.00.
 */
export function conversationCostView(
    total: number,
    withheld: TokenInfo | undefined,
    { enabled, t }: ViewOptions,
): CostEstimateView {
    if (!enabled) return HIDDEN;
    if (withheld && total <= 0) {
        return { kind: 'withheld', text: '≈$?', title: withheldTitle(withheld, t) };
    }
    const sum = t('ai.costEstimates.sumTitle');
    if (!withheld) return { kind: 'estimate', text: `≈${formatCost(total)}`, title: sum };
    return { kind: 'estimate', text: `≈${formatCost(total)} +?`, title: `${sum}\n${withheldTitle(withheld, t)}` };
}

/** The most recent reply of the conversation whose cost was withheld. */
export function latestWithheldCost(messages: readonly Message[]): TokenInfo | undefined {
    for (let i = messages.length - 1; i >= 0; i--) {
        const info = messages[i].tokenInfo;
        if (info?.costWithheld) return info;
    }
    return undefined;
}

/** The token line under a reply in a Markdown export, or nothing without a count. */
export function exportTokenLine(info: TokenInfo | undefined, includeCost: boolean): string | undefined {
    if (!info?.totalTokens) return undefined;
    const line = `> ${info.totalTokens} tokens`;
    if (!includeCost) return line;
    if (info.costWithheld === 'expired' && info.priceListDate) {
        return `${line} · cost not estimated (price list of ${info.priceListDate} older than ${PRICE_LIST_MAX_AGE_DAYS} days)`;
    }
    if (info.costWithheld) return `${line} · cost not estimated (prices without a review date)`;
    if (!info.cost) return line;
    const source = info.priceListDate ? `estimate, price list of ${info.priceListDate}` : 'estimate';
    return `${line} · ≈$${info.cost.toFixed(4)} (${source})`;
}

/** A reply's token info for a JSON export with cost estimates off: tokens only. */
export function withoutCost(info: TokenInfo | undefined): TokenInfo | undefined {
    if (!info) return undefined;
    const { cost: _cost, cacheSavings: _savings, priceListDate: _date, costWithheld: _withheld, ...tokens } = info;
    return tokens;
}
