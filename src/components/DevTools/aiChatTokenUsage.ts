// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Tokens each conversation has used in this session, as the providers
// reported them. Every request counts, tool-loop steps included, which is why
// the count is kept here and not summed from the messages: a step that ends
// in a tool call has no message of its own. Tokens only: AeroFTP does not
// estimate what they cost, since prices change per model and no provider
// returns them per request.

export interface TokenUsage {
    tokens: number;
    requests: number;
}

const usage = new Map<string, TokenUsage>();

/** Finite, never negative, saturating at the largest safe integer. */
function add(current: number, delta: number): number {
    const safe = typeof delta === 'number' && Number.isFinite(delta) && delta > 0 ? delta : 0;
    return Math.min(Number.MAX_SAFE_INTEGER, current + safe);
}

export function recordTokenUsage(conversationId: string | undefined, tokens: number): void {
    if (!conversationId) return;
    const entry = usage.get(conversationId) ?? { tokens: 0, requests: 0 };
    usage.set(conversationId, { tokens: add(entry.tokens, tokens), requests: add(entry.requests, 1) });
}

export function conversationTokenUsage(conversationId: string): TokenUsage | null {
    const entry = usage.get(conversationId);
    return entry ? { ...entry } : null;
}
