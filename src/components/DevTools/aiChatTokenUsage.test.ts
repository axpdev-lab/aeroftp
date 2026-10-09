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

    it("counts a new chat's replies whether they land before or after the save gives it an id", async () => {
        // A new chat records under a key of its own; the delayed save binds
        // that key to the conversation it creates. A reply still in flight
        // when the save runs records under the key it started with.
        const usage = await import('./aiChatTokenUsage');
        usage.recordTokenUsage('chat-key', 100);
        usage.bindTokenUsage('chat-key', 'conv');
        usage.recordTokenUsage('chat-key', 20);
        usage.recordTokenUsage('conv', 5);
        expect(usage.conversationTokenUsage('conv')).toEqual({ tokens: 125, requests: 3 });
        expect(usage.conversationTokenUsage('chat-key')).toEqual({ tokens: 125, requests: 3 });
    });

    it('keeps two chats apart', async () => {
        const usage = await import('./aiChatTokenUsage');
        usage.recordTokenUsage('first-key', 100);
        usage.bindTokenUsage('first-key', 'first');
        usage.recordTokenUsage('second-key', 7);
        expect(usage.conversationTokenUsage('first')).toEqual({ tokens: 100, requests: 1 });
        expect(usage.conversationTokenUsage('second-key')).toEqual({ tokens: 7, requests: 1 });
    });
});

describe('the chat counts the tokens of every model request', () => {
    // Each kind of request the chat sends, with the first early return its
    // flow can take once the response is in. A request whose tokens are not
    // recorded before that return goes uncounted when the turn stops there,
    // and a tool-loop step has no message of its own to carry them later.
    const sites = [
        { request: 'chatRequestsRef.current.call<', earlyReturn: 'if (autoStopRef.current' },
        { request: "invoke('ai_chat_stream'", earlyReturn: 'if (autoStopRef.current || activeTurnRef.current !== turnScope) return;' },
        { request: "invoke<DelegationResult>('ai_delegate_local'", earlyReturn: 'if (activeDelegationIdRef.current !== requestId) return;' },
    ];
    const at = (needle: string) => {
        const found: number[] = [];
        for (let i = chatSource.indexOf(needle); i !== -1; i = chatSource.indexOf(needle, i + 1)) found.push(i);
        return found;
    };
    const line = (index: number) => chatSource.slice(0, index).split('\n').length;

    it('knows every request the chat sends', () => {
        // Two calls (the tool loop and the non-streaming turn), the stream, the
        // delegation. A request added, removed or renamed changes this count.
        expect(sites.reduce((n, site) => n + at(site.request).length, 0)).toBe(4);
    });

    it('records each one before its first early return, tool-loop steps included', () => {
        for (const site of sites) {
            for (const request of at(site.request)) {
                const earlyReturn = chatSource.indexOf(site.earlyReturn, request);
                const recorded = chatSource.indexOf('recordTokenUsage(', request);
                expect(earlyReturn, `no early return found after AIChat.tsx:${line(request)}`).toBeGreaterThan(request);
                expect(recorded, `request at AIChat.tsx:${line(request)} records no tokens`).toBeGreaterThan(request);
                expect(recorded, `request at AIChat.tsx:${line(request)} records its tokens after an early return`).toBeLessThan(earlyReturn);
            }
        }
    });
});
