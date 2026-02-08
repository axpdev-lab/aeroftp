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
        return { valid: true, errors: [], warnings: ['Validation unavailable \u2014 tool args could not be verified'] };
    }
}
