// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { appendAssistantTurn, appendToolResult, assertToolResultsComplete, nativeTurnMatches, requiresNativeTurn, type NativeTurn } from './aiChatNativeTurn';

const turn: NativeTurn = { provider: 'kimi', model: 'kimi-k3', endpoint: 'https://api.moonshot.ai/v1', scope: 'turn-a', transport: 'chat', payload: { reasoning_content: 'opaque' } };
const request = { provider_type: turn.provider, model: turn.model, base_url: `${turn.endpoint}/`, turn_scope: turn.scope };

describe('native foreground continuation', () => {
    it('keeps exact IDs and errors through two consecutive tool rounds', () => {
        const history: Array<Record<string, unknown>> = [];
        for (const id of ['call1', 'call2']) {
            appendAssistantTurn(history, '', [{ id, name: 'inspect', arguments: { path: 'a' } }], turn);
            expect(() => assertToolResultsComplete(history)).toThrow();
            appendToolResult(history, id, 'Error: file absent');
            expect(() => assertToolResultsComplete(history)).not.toThrow();
        }
        expect(history.map(m => m.role)).toEqual(['assistant', 'tool', 'assistant', 'tool']);
        expect(history[0].native_turn).toEqual(turn);
        expect(history[3].tool_call_id).toBe('call2');
    });

    it('waits for all mixed safe and approval-required results, regardless of completion order', () => {
        const history: Array<Record<string, unknown>> = [];
        appendAssistantTurn(history, '', [
            { id: 'safe', name: 'read', arguments: {} },
            { id: 'approved1', name: 'write', arguments: {} },
            { id: 'approved2', name: 'write', arguments: {} },
        ], turn);
        appendToolResult(history, 'safe', 'contents');
        appendToolResult(history, 'approved2', 'updated');
        expect(() => assertToolResultsComplete(history)).toThrow();
        appendToolResult(history, 'approved1', 'Error: permission denied');
        expect(() => assertToolResultsComplete(history)).not.toThrow();
        expect(() => appendToolResult(history, 'approved1', 'duplicate')).toThrow();
        expect(() => appendToolResult(history, 'other-turn', 'orphan')).toThrow();
    });

    it('refuses duplicate or missing call IDs instead of inventing replacements', () => {
        expect(() => appendAssistantTurn([], '', [{ id: '', name: 'read', arguments: {} }])).toThrow();
        expect(() => appendAssistantTurn([], '', Array(2).fill({ id: 'same', name: 'read', arguments: {} }))).toThrow();
    });

    it('binds native state to scope, provider, endpoint, model and transport', () => {
        expect(nativeTurnMatches(turn, request)).toBe(true);
        for (const changed of [{ turn_scope: 'branch-b' }, { model: 'other' }, { provider_type: 'custom' }, { base_url: 'https://another.test/v1' }, { use_responses_api: true }]) {
            expect(nativeTurnMatches(turn, { ...request, ...changed })).toBe(false);
        }
    });

    it('requires native completion before running modern-provider tools', () => {
        expect(requiresNativeTurn(request)).toBe(true);
        expect(requiresNativeTurn({ provider_type: 'openai', use_responses_api: true })).toBe(true);
        expect(requiresNativeTurn({ provider_type: 'anthropic', model: 'claude-opus-5-5' })).toBe(true);
        expect(requiresNativeTurn({ provider_type: 'custom', model: 'custom' })).toBe(false);
    });
});
