import { AITool, AIToolParameter } from '../../types/tools';

/** Maximum total tool steps allowed across all macro nesting levels */
export const MAX_TOTAL_MACRO_STEPS = 20;

/** Mutable counter shared across recursive macro executions */
export interface MacroStepCounter {
    total: number;
}

export function createMacroStepCounter(): MacroStepCounter {
    return { total: 0 };
}

export interface MacroStep {
    toolName: string;
    args: Record<string, string>; // values can contain {{variable}} templates
}

export interface ToolMacro {
    id: string;
    name: string;
    displayName: string;
    description: string;
    parameters: AIToolParameter[];
    steps: MacroStep[];
}

/**
 * Resolve template variables in macro steps.
 * Replaces {{varName}} with actual parameter values.
 */
export function resolveMacroSteps(macro: ToolMacro, params: Record<string, unknown>): MacroStep[] {
    return macro.steps.map(step => {
        const resolvedArgs: Record<string, string> = {};
        for (const [key, value] of Object.entries(step.args)) {
            resolvedArgs[key] = value.replace(/\{\{(\w+)\}\}/g, (_, varName) => {
                const val = params[varName];
                return val !== undefined ? String(val) : `{{${varName}}}`;
            });
        }
        return { toolName: step.toolName, args: resolvedArgs };
    });
}

/**
 * Convert macros to AITool definitions for injection into the system prompt.
 */
export function macrosToToolDefinitions(macros: ToolMacro[]): AITool[] {
    return macros.map(macro => ({
        name: `macro_${macro.name}`,
        description: `[Macro] ${macro.description}`,
        parameters: macro.parameters,
        dangerLevel: 'medium' as const,
    }));
}

/**
 * Check if a tool name is a macro call.
 */
export function isMacroCall(toolName: string): boolean {
    return toolName.startsWith('macro_');
}

/**
 * Extract the macro name from a tool call name.
 */
export function getMacroName(toolName: string): string {
    return toolName.replace(/^macro_/, '');
}

/** Default built-in macros */
export const DEFAULT_MACROS: ToolMacro[] = [
    {
        id: 'builtin-backup-edit',
        name: 'safe_edit',
        displayName: 'Safe Edit',
        description: 'Read a file, show its content, then edit it (read-before-write safety pattern)',
        parameters: [
            { name: 'path', type: 'string', description: 'File path to safely edit', required: true },
            { name: 'find', type: 'string', description: 'String to find', required: true },
            { name: 'replace', type: 'string', description: 'Replacement string', required: true },
        ],
        steps: [
            { toolName: 'local_read', args: { path: '{{path}}' } },
            { toolName: 'local_edit', args: { path: '{{path}}', find: '{{find}}', replace: '{{replace}}' } },
        ],
    },
    {
        id: 'builtin-search-read',
        name: 'find_and_read',
        displayName: 'Find & Read',
        description: 'Search for files matching a pattern in a local directory',
        parameters: [
            { name: 'path', type: 'string', description: 'Directory to search', required: true },
            { name: 'pattern', type: 'string', description: 'Search pattern (e.g. "*.ts")', required: true },
        ],
        steps: [
            { toolName: 'local_search', args: { path: '{{path}}', pattern: '{{pattern}}' } },
        ],
    },
];
