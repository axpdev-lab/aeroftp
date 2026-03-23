// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import { invoke } from '@tauri-apps/api/core';

export interface ValidationResult {
    valid: boolean;
    errors: string[];
    warnings: string[];
}

export async function validateToolArgs(toolName: string, args: Record<string, unknown>): Promise<ValidationResult> {
    try {
        const result = await invoke<ValidationResult>('validate_tool_args', { toolName, args });
        return result;
    } catch {
        // Fail-closed: require manual approval when backend validation is unavailable
        return { valid: false, errors: ['Tool validation failed \u2014 manual approval required'], warnings: ['Backend validation unavailable, verify parameters before approving'] };
    }
}
