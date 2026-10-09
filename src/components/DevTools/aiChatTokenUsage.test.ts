// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { beforeEach, describe, expect, it, vi } from 'vitest';
import chatSource from './AIChat.tsx?raw';

describe('the token count of a conversation', () => {
    beforeEach(() => vi.resetModules());

    it('adds up every request, tool-loop steps included', async () => {
        const usage = await import('./aiChatTokenUsage');
        usage.recordTokenUsage('c', 1200);
        usage.recordTokenUsage('c', 300);
        usage.recordTokenUsage('other', 50);
        expect(usage.conversationTokenUsage('c')).toEqual({ tokens: 1500, requests: 2 });
        expect(usage.conversationTokenUsage('other')).toEqual({ tokens: 50, requests: 1 });
        expect(usage.conversationTokenUsage('none')).toBeNull();
    });

    it('never subtracts, never turns NaN, and saturates instead of overflowing', async () => {
        const usage = await import('./aiChatTokenUsage');
        usage.recordTokenUsage('c', 10);
        usage.recordTokenUsage('c', -5);
        usage.recordTokenUsage('c', Number.NaN);
        expect(usage.conversationTokenUsage('c')).toEqual({ tokens: 10, requests: 3 });
        usage.recordTokenUsage('c', Number.MAX_SAFE_INTEGER);
        usage.recordTokenUsage('c', Number.MAX_SAFE_INTEGER);
        expect(usage.conversationTokenUsage('c')?.tokens).toBe(Number.MAX_SAFE_INTEGER);
    });

    it('counts nothing for a request outside any conversation', async () => {
        const usage = await import('./aiChatTokenUsage');
        usage.recordTokenUsage(undefined, 100);
        expect(usage.conversationTokenUsage('')).toBeNull();
    });
});

describe('the chat counts the tokens of every model request', () => {
    it('records each request before any early return, tool-loop steps included', () => {
        // A request that ends in a tool call has no message of its own: unless
        // its tokens are recorded where it returns, the count misses it.
        const requests = [...chatSource.matchAll(/chatRequestsRef\.current\.call</g)].map(match => match.index!);
        expect(requests.length).toBeGreaterThanOrEqual(2);
        for (const at of requests) {
            const firstReturn = chatSource.indexOf('if (autoStopRef.current', at);
            const recorded = chatSource.indexOf('recordTokenUsage(', at);
            const line = chatSource.slice(0, at).split('\n').length;
            expect(recorded, `request at AIChat.tsx:${line} records no tokens`).toBeGreaterThan(at);
            expect(recorded, `request at AIChat.tsx:${line} records its tokens after an early return`).toBeLessThan(firstReturn);
        }
    });
});
