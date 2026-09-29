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
 */
export interface ChatRequests {
    call<T>(request: Record<string, unknown>): Promise<T>;
    /** Cancel every request still in flight. Its promise rejects. */
    cancel(): void;
}

export function createChatRequests(invoke: ChatInvoke): ChatRequests {
    const inFlight = new Set<string>();
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
        cancel: () => {
            for (const requestId of inFlight) {
                invoke('ai_cancel_chat', { requestId }).catch(() => {});
            }
            inFlight.clear();
        },
    };
}
