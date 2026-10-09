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
// A new chat gets its id from the delayed save, after its first replies:
// their tokens wait here, then move to the conversation the save creates.
let pending: TokenUsage | null = null;

/** Finite, never negative, saturating at the largest safe integer. */
function add(current: number, delta: number): number {
    const safe = typeof delta === 'number' && Number.isFinite(delta) && delta > 0 ? delta : 0;
    return Math.min(Number.MAX_SAFE_INTEGER, current + safe);
}

function plus(entry: TokenUsage | null | undefined, tokens: number, requests: number): TokenUsage {
    const base = entry ?? { tokens: 0, requests: 0 };
    return { tokens: add(base.tokens, tokens), requests: add(base.requests, requests) };
}

/** One request's tokens; `undefined` is the chat that has no id yet. */
export function recordTokenUsage(conversationId: string | undefined, tokens: number): void {
    if (!conversationId) {
        pending = plus(pending, tokens, 1);
        return;
    }
    usage.set(conversationId, plus(usage.get(conversationId), tokens, 1));
}

/** A conversation's tokens; `null` is the chat that has no id yet. */
export function conversationTokenUsage(conversationId: string | null): TokenUsage | null {
    const entry = conversationId ? usage.get(conversationId) : pending;
    return entry ? { ...entry } : null;
}

/** The save just gave the new chat its id: its tokens so far are that conversation's. */
export function adoptPendingTokenUsage(conversationId: string): void {
    if (!pending) return;
    usage.set(conversationId, plus(usage.get(conversationId), pending.tokens, pending.requests));
    pending = null;
}

/** The unsaved chat was left (new chat, another conversation): its tokens belong to no other. */
export function discardPendingTokenUsage(): void {
    pending = null;
}
