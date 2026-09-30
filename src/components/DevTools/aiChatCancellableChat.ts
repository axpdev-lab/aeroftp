// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

export type ChatInvoke = <T>(cmd: string, args?: Record<string, unknown>) => Promise<T>;

/**
 * The non-streaming `ai_chat` requests of the chat, cancellable by Stop.
 *
 * Stop used to cancel only the HTTP stream: a non-streaming request (the
 * multi-step continuation, or a model without streaming) ran on to completion
 * in the backend, and was billed, while the chat discarded its answer. Each
 * request now carries an id that `ai_cancel_chat` drops in the backend.
 *
 * A tool of the turn is the same story (M9, tool side): Stop reached the
 * request and the stream, and an upload of many files or a tree search ran
 * on to its end. The turn's id goes with every tool call, and Stop hands it
 * to `ai_cancel_tool_turn`.
 */
export interface ChatRequests {
    call<T>(request: Record<string, unknown>): Promise<T>;
    /**
     * The turn whose tools Stop cancels next (M9): every `execute_ai_tool`
     * of a message is sent with this id, and `ai_cancel_tool_turn` stops the
     * tool running for it and refuses the ones not started. `null` between
     * turns.
     */
    setTurn(turnId: string | null): void;
    /** The id `execute_ai_tool` is sent with for the current turn. */
    turnId(): string | null;
    /**
     * Cancel every request still in flight (its promise rejects) and the
     * tools of the current turn, running or not yet started.
     */
    cancel(): void;
}

export function createChatRequests(invoke: ChatInvoke): ChatRequests {
    const inFlight = new Set<string>();
    let turn: string | null = null;
    return {
        call: async <T,>(request: Record<string, unknown>) => {
            const requestId = crypto.randomUUID();
            inFlight.add(requestId);
            try {
                return await invoke<T>('ai_chat', { request, requestId });
            } finally {
                inFlight.delete(requestId);
            }
        },
        setTurn: (turnId) => { turn = turnId; },
        turnId: () => turn,
        cancel: () => {
            for (const requestId of inFlight) {
                invoke('ai_cancel_chat', { requestId }).catch(() => {});
            }
            inFlight.clear();
            if (turn) {
                invoke('ai_cancel_tool_turn', { turnId: turn }).catch(() => {});
                turn = null;
            }
        },
    };
}
