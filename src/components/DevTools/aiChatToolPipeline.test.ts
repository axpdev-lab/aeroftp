// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { buildExecutionLevels } from './aiChatToolPipeline';
import type { AgentToolCall } from '../../types/tools';

const call = (id: string, toolName: string, args: Record<string, unknown> = {}): AgentToolCall =>
    ({ id, toolName, args, status: 'approved' });
const ids = (calls: AgentToolCall[]) => buildExecutionLevels(calls).map(level => level.tools.map(t => t.id));

describe('execution levels', () => {
    it('keeps independent calls in one parallel level', () => {
        expect(ids([call('a', 'remote_list', { path: '/a' }), call('b', 'remote_list', { path: '/b' })])).toEqual([['a', 'b']]);
    });

    it('runs a connection measurement alone, after the calls before it and before the calls after it', () => {
        // A transfer beside the benchmark or the speed test would be measured too.
        const calls = [
            call('a', 'remote_list', { path: '/a' }),
            call('b', 'remote_list', { path: '/b' }),
            call('bench', 'aeroftp_benchmark', { level: 'quick' }),
            call('c', 'remote_download', { path: '/c' }),
            call('speed', 'remote_speed', { remote_path: '/tmp' }),
            call('d', 'remote_list', { path: '/d' }),
        ];
        expect(ids(calls)).toEqual([['a', 'b'], ['bench'], ['c'], ['speed'], ['d']]);
    });

    it('runs two measurements one after the other, never together', () => {
        expect(ids([call('s', 'remote_speed'), call('b', 'aeroftp_benchmark')])).toEqual([['s'], ['b']]);
    });
});
