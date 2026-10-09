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
// A new chat records under a key of its own, since its conversation id comes
// from the save that runs after its first message, often while the first
// reply is still on its way. The save binds the key to that id: what was
// counted moves there, and replies that started under the key follow it.
const aliases = new Map<string, string>();

/** Finite, never negative, saturating at the largest safe integer. */
function add(current: number, delta: number): number {
    const safe = typeof delta === 'number' && Number.isFinite(delta) && delta > 0 ? delta : 0;
    return Math.min(Number.MAX_SAFE_INTEGER, current + safe);
}

function plus(entry: TokenUsage | undefined, tokens: number, requests: number): TokenUsage {
    const base = entry ?? { tokens: 0, requests: 0 };
    return { tokens: add(base.tokens, tokens), requests: add(base.requests, requests) };
}

const resolve = (key: string) => aliases.get(key) ?? key;

const listeners = new Set<() => void>();
const changed = () => listeners.forEach(listener => listener());

/** Called after every change, so a display can refresh without waiting for a message. */
export function subscribeTokenUsage(listener: () => void): () => void {
    listeners.add(listener);
    return () => { listeners.delete(listener); };
}

/** One request's tokens, under the key its chat had when the request started. */
export function recordTokenUsage(key: string, tokens: number): void {
    const target = resolve(key);
    usage.set(target, plus(usage.get(target), tokens, 1));
    changed();
}

export function conversationTokenUsage(key: string): TokenUsage | null {
    const entry = usage.get(resolve(key));
    return entry ? { ...entry } : null;
}

/** The save gave the new chat `chatKey` the id `conversationId`. */
export function bindTokenUsage(chatKey: string, conversationId: string): void {
    if (chatKey === conversationId) return;
    aliases.set(chatKey, conversationId);
    const counted = usage.get(chatKey);
    if (counted) {
        usage.delete(chatKey);
        usage.set(conversationId, plus(usage.get(conversationId), counted.tokens, counted.requests));
    }
    changed();
}
