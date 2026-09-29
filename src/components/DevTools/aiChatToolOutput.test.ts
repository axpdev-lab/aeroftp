// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { buildExecutionLevels, executePipeline } from './aiChatToolPipeline';
import { toolOutputBelongsToActiveTurn } from './aiChatToolOutput';
import type { AgentToolCall } from '../../types/tools';

describe('tool output after the turn is cancelled', () => {
    it('publishes for the active turn and for a call with no turn scope', () => {
        expect(toolOutputBelongsToActiveTurn('plan', 'plan')).toBe(true);
        expect(toolOutputBelongsToActiveTurn(undefined, null)).toBe(true);
        expect(toolOutputBelongsToActiveTurn(undefined, 'other')).toBe(true);
    });

    it('drops output when Stop clears the turn or a new chat replaces it', () => {
        expect(toolOutputBelongsToActiveTurn('plan', null)).toBe(false);
        expect(toolOutputBelongsToActiveTurn('plan', 'later-chat')).toBe(false);
    });

    // The plan pipeline does not stop when the in-flight tool settles. The
    // next step still runs, and neither step may append after the turn is gone.
    it('publishes nothing when a two-step plan resumes after the turn is cleared', async () => {
        const published: string[] = [];
        const scope = 'plan';
        let active: string | null = scope;
        const calls: AgentToolCall[] = [
            { id: 'a', toolName: 'remote_upload', args: { path: '/a' }, status: 'approved' },
            { id: 'b', toolName: 'remote_mkdir', args: { path: '/b' }, status: 'approved', dependsOn: ['a'] },
        ];

        await executePipeline(buildExecutionLevels(calls), async (toolCall) => {
            await Promise.resolve();
            if (toolCall.id === 'a') active = null;
            if (!toolOutputBelongsToActiveTurn(scope, active)) return 'Error: execution cancelled';
            published.push(toolCall.toolName);
            return toolCall.toolName;
        });

        expect(published).toEqual([]);
    });

    it('still publishes every step while the plan turn stays active', async () => {
        const published: string[] = [];
        const scope = 'plan';
        const calls: AgentToolCall[] = [
            { id: 'a', toolName: 'remote_upload', args: { path: '/a' }, status: 'approved' },
            { id: 'b', toolName: 'remote_mkdir', args: { path: '/b' }, status: 'approved', dependsOn: ['a'] },
        ];

        await executePipeline(buildExecutionLevels(calls), async (toolCall) => {
            if (!toolOutputBelongsToActiveTurn(scope, scope)) return 'Error: execution cancelled';
            published.push(toolCall.toolName);
            return toolCall.toolName;
        });

        expect(published).toEqual(['remote_upload', 'remote_mkdir']);
    });
});
