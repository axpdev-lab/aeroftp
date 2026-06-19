// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import type { ProjectContext } from '../../types/contextIntelligence';
import { resolveAgentPromptProfile, type CodingWorkspaceContext } from './aiChatCodingWorkspace';

vi.mock('@tauri-apps/api/core', () => ({
    invoke: vi.fn(),
}));

const projectContext: ProjectContext = {
    project_type: 'nodejs',
    name: 'demo',
    version: null,
    scripts: ['test', 'typecheck'],
    deps_count: 4,
    dev_deps_count: 8,
    entry_points: ['src/main.tsx'],
    config_files: ['package.json'],
};

const workspace = (overrides: Partial<CodingWorkspaceContext> = {}): CodingWorkspaceContext => ({
    projectPath: '/workspace/demo',
    localPath: '/workspace/demo',
    remotePath: undefined,
    projectContext: null,
    gitBranch: undefined,
    gitSummary: undefined,
    fileImports: [],
    ragSummary: null,
    codingRulesBlock: '',
    mentionContextBlock: '',
    ...overrides,
});

describe('resolveAgentPromptProfile', () => {
    it('activates coding-agent profile for coding work in a detected project', () => {
        expect(resolveAgentPromptProfile(workspace({ projectContext }), {
            userPrompt: 'Implement the new React component and run typecheck',
            taskType: 'code_generation',
        })).toBe('coding_agent');
    });

    it('keeps file-manager profile for generic folder commands with only a path', () => {
        expect(resolveAgentPromptProfile(workspace(), {
            userPrompt: 'Create a new folder named invoices',
            taskType: 'code_generation',
        })).toBe('file_manager');
    });

    it('keeps file-manager profile for remote/server questions', () => {
        expect(resolveAgentPromptProfile(workspace({ projectContext }), {
            userPrompt: 'List files on the SFTP server at /var/www',
            taskType: 'file_analysis',
        })).toBe('file_manager');
    });
});
