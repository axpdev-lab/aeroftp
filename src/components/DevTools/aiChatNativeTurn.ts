// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** Opaque backend envelope. Never render, edit or persist its payload. */
export interface NativeTurn {
    provider: string;
    model: string;
    endpoint: string;
    scope: string;
    transport: string;
    payload: unknown;
}

export interface NativeToolCall { id: string; name: string; arguments: unknown }
type History = Array<Record<string, unknown>>;
type Echo = { id: string; name: string; arguments: string };

export function appendAssistantTurn(history: History, content: string, calls: NativeToolCall[], native?: NativeTurn): void {
    const ids = new Set<string>();
    for (const call of calls) {
        if (!call.id || ids.has(call.id)) throw new Error('Missing or duplicate native tool call ID');
        ids.add(call.id);
    }
    history.push({
        role: 'assistant', content,
        ...(native ? { native_turn: native } : {}),
        tool_calls_echo: calls.map(call => ({
            id: call.id, name: call.name,
            arguments: typeof call.arguments === 'string' ? call.arguments : JSON.stringify(call.arguments),
        })),
    });
}

export function hasStructuredTurn(history: History): boolean {
    return history.some(message => Array.isArray(message.tool_calls_echo));
}

function lastAssistantIndex(history: History): number {
    for (let index = history.length - 1; index >= 0; index--) {
        if (Array.isArray(history[index].tool_calls_echo)) return index;
    }
    return -1;
}

export function requiresNativeTurn(request: Record<string, unknown>): boolean {
    return request.use_responses_api === true
        || (request.provider_type === 'anthropic' && ['claude-opus-5-5', 'claude-fable-5-1'].includes(String(request.model)))
        || (request.provider_type === 'kimi' && request.model === 'kimi-k3')
        || (request.provider_type === 'xai' && request.model === 'grok-4.7')
        || (request.provider_type === 'openai' && ['gpt-6-astra', 'gpt-6-sol', 'gpt-6-luna'].includes(String(request.model)));
}

export function appendToolResult(history: History, id: string, content: string): void {
    const assistantIndex = lastAssistantIndex(history);
    if (assistantIndex < 0) return; // Text-only legacy tool protocol.
    const calls = history[assistantIndex].tool_calls_echo as Echo[];
    if (!calls.some(call => call.id === id)) throw new Error('Tool result does not belong to this assistant turn');
    if (history.slice(assistantIndex + 1).some(message => message.tool_call_id === id)) {
        throw new Error('Duplicate tool result');
    }
    history.push({ role: 'tool', tool_call_id: id, content });
}

export function assertToolResultsComplete(history: History): void {
    const assistantIndex = lastAssistantIndex(history);
    if (assistantIndex < 0) return;
    const calls = history[assistantIndex].tool_calls_echo as Echo[];
    const results = history.slice(assistantIndex + 1).filter(message => message.role === 'tool');
    if (results.length !== calls.length || calls.some(call => !results.some(result => result.tool_call_id === call.id))) {
        throw new Error('Cannot continue until every tool call has a result');
    }
}

export function nativeTurnMatches(turn: NativeTurn, request: Record<string, unknown>): boolean {
    return turn.scope === request.turn_scope && turn.model === request.model
        && turn.provider === request.provider_type
        && turn.endpoint === String(request.base_url).replace(/\/+$/, '')
        && turn.transport === (request.use_responses_api ? 'responses' : request.provider_type === 'anthropic' ? 'anthropic' : 'chat');
}
