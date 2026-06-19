// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

export interface CodingRuleFile {
    path: string;
    bytes: number;
    truncated: boolean;
}

export interface CodingRulesContext {
    files: CodingRuleFile[];
    combined: string;
    truncated: boolean;
    warnings: string[];
}

let cachedProjectPath: string | null = null;
let cachedRulesBlock = '';

export async function fetchCodingRules(projectPath: string): Promise<string> {
    if (!projectPath) return '';
    if (cachedProjectPath === projectPath) return cachedRulesBlock;

    try {
        const result = await invoke<CodingRulesContext>('read_coding_rules', { projectPath });
        cachedProjectPath = projectPath;
        cachedRulesBlock = result.combined || '';
        return cachedRulesBlock;
    } catch {
        cachedProjectPath = projectPath;
        cachedRulesBlock = '';
        return '';
    }
}

export function invalidateCodingRulesCache(): void {
    cachedProjectPath = null;
    cachedRulesBlock = '';
}
