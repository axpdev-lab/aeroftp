// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect, vi } from 'vitest';
import { createChatRequests, type ChatInvoke } from './aiChatCancellableChat';

/** An `invoke` whose `ai_chat` stays pending until `finish` is called. */
function pendingBackend() {
    const calls: Array<{ cmd: string; args?: Record<string, unknown> }> = [];
    let finish: (value: unknown) => void = () => {};
    const invoke = vi.fn((cmd: string, args?: Record<string, unknown>) => {
        calls.push({ cmd, args });
        if (cmd === 'ai_chat') return new Promise(resolve => { finish = resolve; });
        return Promise.resolve(undefined);
    }) as unknown as ChatInvoke;
    return { invoke, calls, finish: (value: unknown) => finish(value) };
}

// M9 of the 4.2.1 review: Stop cancelled only the HTTP stream. The
// non-streaming `ai_chat` of a multi-step continuation ran on to completion in
// the backend (and was billed) while the chat threw its answer away.
describe('non-streaming ai_chat cancellation', () => {
    it('cancels the request in flight in the backend, by the id it was sent with', async () => {
        const backend = pendingBackend();
        const chat = createChatRequests(backend.invoke);
        const answer = chat.call({ model: 'm' });
        chat.cancel();
        const sent = backend.calls.find(c => c.cmd === 'ai_chat');
        const cancelled = backend.calls.find(c => c.cmd === 'ai_cancel_chat');
        expect(sent?.args?.requestId).toEqual(expect.any(String));
        expect(cancelled?.args).toEqual({ requestId: sent?.args?.requestId });
        backend.finish({ content: '' });
        await answer;
    });

    it('does not cancel a request that has already answered', async () => {
        const backend = pendingBackend();
        const chat = createChatRequests(backend.invoke);
        const answer = chat.call({ model: 'm' });
        backend.finish({ content: 'done' });
        await expect(answer).resolves.toEqual({ content: 'done' });
        chat.cancel();
        expect(backend.calls.some(c => c.cmd === 'ai_cancel_chat')).toBe(false);
    });

    // M9, tool side: Stop cancelled the chat request and the stream, and a
    // tool still running for the turn (an upload of many files, a tree
    // search) ran on to its end in the backend.
    it('stops the tools of the current turn, by the id they were sent with', () => {
        const backend = pendingBackend();
        const chat = createChatRequests(backend.invoke);
        chat.setTurn('turn-1');
        expect(chat.turnId()).toBe('turn-1');
        chat.cancel();
        const stopped = backend.calls.find(c => c.cmd === 'ai_cancel_tool_turn');
        expect(stopped?.args).toEqual({ turnId: 'turn-1' });
        // The turn is over: a second Stop, or one between turns, sends nothing.
        expect(chat.turnId()).toBeNull();
        chat.cancel();
        expect(backend.calls.filter(c => c.cmd === 'ai_cancel_tool_turn')).toHaveLength(1);
    });

    it('sends nothing for the tools when no turn is running', () => {
        const backend = pendingBackend();
        const chat = createChatRequests(backend.invoke);
        chat.cancel();
        expect(backend.calls.some(c => c.cmd === 'ai_cancel_tool_turn')).toBe(false);
    });
});
