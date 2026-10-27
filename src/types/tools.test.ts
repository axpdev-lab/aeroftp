// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { AGENT_TOOLS, isSafeTool, requestsNonAtomicReplace } from './tools';
import { buildToolRegistry, ToolExposure } from '../components/DevTools/aiChatToolRegistry';

describe('remote_edit allow_non_atomic', () => {
    it('is listed in the GUI tool schema as an optional boolean', () => {
        const tool = AGENT_TOOLS.find(t => t.name === 'remote_edit');
        const param = tool?.parameters.find(p => p.name === 'allow_non_atomic');
        expect(param).toBeDefined();
        expect(param?.type).toBe('boolean');
        expect(param?.required).toBe(false);
    });

    it('is requested only by a JSON true on remote_edit', () => {
        expect(requestsNonAtomicReplace({ toolName: 'remote_edit', args: { allow_non_atomic: true } })).toBe(true);
        expect(requestsNonAtomicReplace({ toolName: 'remote_edit', args: { allow_non_atomic: 'true' } })).toBe(false);
        expect(requestsNonAtomicReplace({ toolName: 'remote_edit', args: { allow_non_atomic: 1 } })).toBe(false);
        expect(requestsNonAtomicReplace({ toolName: 'remote_edit', args: {} })).toBe(false);
        expect(requestsNonAtomicReplace({ toolName: 'local_edit', args: { allow_non_atomic: true } })).toBe(false);
    });
});


it('keeps workspace diagnostics out of safe auto-execution after tool discovery', () => {
    const registry = buildToolRegistry([], []);
    const exposure = new ToolExposure('turn', registry);
    const discovery = exposure.search('coding_diagnostics');
    expect(discovery.tools.find(tool => tool.name === 'coding_diagnostics')?.approval).toBe('medium');
    expect(isSafeTool('coding_diagnostics')).toBe(false);
    expect(isSafeTool('coding_diagnostics', registry.map(entry => entry.tool))).toBe(false);
    expect(isSafeTool('coding_search', registry.map(entry => entry.tool))).toBe(true);
});
