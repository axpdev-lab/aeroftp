import { invoke } from '@tauri-apps/api/core';

export interface PromptTemplate {
    id: string;
    name: string;
    icon: string;
    command: string;
    description: string;
    prompt: string;
    category: 'code' | 'debug' | 'docs' | 'security' | 'general' | 'analysis';
    isBuiltIn: boolean;
}

export const DEFAULT_TEMPLATES: PromptTemplate[] = [
    {
        id: 'review',
        name: 'Code Review',
        icon: '\u{1F50D}',
        command: '/review',
        description: 'Review code for bugs, security issues, and best practices',
        prompt: 'Review the following code for bugs, security vulnerabilities, performance issues, and adherence to best practices. Provide specific, actionable feedback:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'refactor',
        name: 'Refactor',
        icon: '\u{1F527}',
        command: '/refactor',
        description: 'Suggest refactoring improvements',
        prompt: 'Refactor the following code to improve readability, maintainability, and performance. Explain each change:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'explain',
        name: 'Explain Code',
        icon: '\u{1F4D6}',
        command: '/explain',
        description: 'Explain what code does line by line',
        prompt: 'Explain what this code does, step by step. Include the purpose, data flow, and any notable patterns or potential issues:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'debug',
        name: 'Debug',
        icon: '\u{1F41B}',
        command: '/debug',
        description: 'Help debug an issue',
        prompt: 'I have a bug in my code. Help me identify the root cause and suggest a fix. Here is the relevant code and the error/behavior I am seeing:\n\n{{selection}}',
        category: 'debug',
        isBuiltIn: true,
    },
    {
        id: 'tests',
        name: 'Generate Tests',
        icon: '\u{2705}',
        command: '/tests',
        description: 'Generate unit tests',
        prompt: 'Generate comprehensive unit tests for the following code. Cover edge cases, error handling, and typical usage:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'docs',
        name: 'Documentation',
        icon: '\u{1F4DD}',
        command: '/docs',
        description: 'Generate documentation',
        prompt: 'Generate clear, concise documentation for the following code. Include purpose, parameters, return values, usage examples, and any important notes:\n\n{{selection}}',
        category: 'docs',
        isBuiltIn: true,
    },
    {
        id: 'security',
        name: 'Security Audit',
        icon: '\u{1F6E1}',
        command: '/security',
        description: 'Audit code for security vulnerabilities',
        prompt: 'Perform a security audit on the following code. Check for OWASP top 10 vulnerabilities, injection risks, authentication issues, data exposure, and other security concerns:\n\n{{selection}}',
        category: 'security',
        isBuiltIn: true,
    },
    {
        id: 'optimize',
        name: 'Optimize',
        icon: '\u{26A1}',
        command: '/optimize',
        description: 'Optimize code for performance',
        prompt: 'Optimize the following code for better performance. Identify bottlenecks, suggest algorithmic improvements, and reduce resource usage:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'fix',
        name: 'Fix Error',
        icon: '\u{1F6A8}',
        command: '/fix',
        description: 'Fix a specific error message',
        prompt: 'I am getting the following error. Help me understand and fix it:\n\nError: {{selection}}',
        category: 'debug',
        isBuiltIn: true,
    },
    {
        id: 'convert',
        name: 'Convert',
        icon: '\u{1F504}',
        command: '/convert',
        description: 'Convert code between languages/formats',
        prompt: 'Convert the following code to {{target_language}}. Maintain the same logic and use idiomatic patterns for the target language:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'commit',
        name: 'Commit Message',
        icon: '\u{1F4E8}',
        command: '/commit',
        description: 'Generate a commit message',
        prompt: 'Generate a conventional commit message for the following changes. Use the format: type(scope): description\n\nChanges:\n{{selection}}',
        category: 'general',
        isBuiltIn: true,
    },
    {
        id: 'summarize',
        name: 'Summarize',
        icon: '\u{1F4CB}',
        command: '/summarize',
        description: 'Summarize code or text',
        prompt: 'Provide a concise summary of the following. Include key points, architecture decisions, and notable patterns:\n\n{{selection}}',
        category: 'general',
        isBuiltIn: true,
    },
    {
        id: 'typedefs',
        name: 'Type Definitions',
        icon: '\u{1F3F7}',
        command: '/types',
        description: 'Generate TypeScript type definitions',
        prompt: 'Generate TypeScript type definitions for the following data structures or API responses. Use strict types, no `any`:\n\n{{selection}}',
        category: 'code',
        isBuiltIn: true,
    },
    {
        id: 'analyze-ui',
        name: 'Analyze UI',
        icon: '\u{1F5BC}\uFE0F',
        command: '/analyze-ui',
        description: 'Analyze a UI screenshot for layout, accessibility, and UX issues',
        prompt: 'Analyze this UI screenshot in detail:\n\n1. **Component Inventory**: List all visible UI elements (buttons, inputs, labels, navigation, cards, etc.)\n2. **Layout Analysis**: Evaluate the grid/flex structure, spacing consistency, alignment, and visual hierarchy\n3. **Accessibility Audit** (WCAG 2.1 AA):\n   - Color contrast ratios (text vs background)\n   - Missing alt text or aria-labels\n   - Focus indicator visibility\n   - Touch target sizes (min 44x44px)\n   - Screen reader compatibility concerns\n4. **Responsive Design**: Will this layout work on mobile (375px), tablet (768px), and desktop (1440px)?\n5. **UX Issues**: Identify confusing flows, missing feedback states, unclear CTAs, or cognitive overload\n6. **Recommendations**: Prioritized list of improvements with estimated effort (low/medium/high)\n\n{{selection}}',
        category: 'analysis',
        isBuiltIn: true,
    },
    {
        id: 'performance',
        name: 'Performance',
        icon: '\u{26A1}',
        command: '/performance',
        description: 'Analyze code for performance bottlenecks and optimization opportunities',
        prompt: 'Analyze this code for performance issues:\n\n1. **Bottlenecks**: Identify O(n\u00B2) or worse algorithms, unnecessary re-renders, memory leaks\n2. **Optimization Opportunities**: Memoization, lazy loading, caching, batching\n3. **Resource Usage**: Large allocations, frequent GC pressure, excessive DOM operations\n4. **Async Patterns**: Missing parallelization, waterfall requests, blocking operations\n5. **Bundle Impact**: Large imports that could be tree-shaken or code-split\n\n{{selection}}',
        category: 'analysis',
        isBuiltIn: true,
    },
];

/**
 * Resolve template placeholders with actual values.
 * {{selection}} is replaced with the current input text or editor selection.
 * {{fileName}} and {{filePath}} are replaced with file context if available.
 * Unresolved placeholders like {{target_language}} are kept for the user to fill.
 */
export function resolveTemplate(template: PromptTemplate, context: {
    selection?: string;
    fileName?: string;
    filePath?: string;
}): string {
    let prompt = template.prompt;
    prompt = prompt.replace(/\{\{selection\}\}/g, context.selection || '');
    prompt = prompt.replace(/\{\{fileName\}\}/g, context.fileName || '');
    prompt = prompt.replace(/\{\{filePath\}\}/g, context.filePath || '');
    return prompt;
}

/**
 * Find matching templates for a given input starting with /.
 * Returns all templates when input is just "/", otherwise filters by
 * command prefix or name substring match.
 */
export function matchTemplates(input: string, templates: PromptTemplate[]): PromptTemplate[] {
    if (!input.startsWith('/')) return [];
    const query = input.slice(1).toLowerCase();
    if (!query) return templates;
    return templates.filter(t =>
        t.command.slice(1).startsWith(query) ||
        t.name.toLowerCase().includes(query)
    );
}

/**
 * Load custom (user-created) templates from vault storage.
 * Returns an empty array if vault is unavailable or no templates are stored.
 */
export async function loadCustomTemplates(): Promise<PromptTemplate[]> {
    try {
        const json = await invoke<string>('vault_get', { key: 'ai_prompt_templates' });
        if (!json) return [];
        const parsed = JSON.parse(json) as PromptTemplate[];
        return parsed.map(t => ({ ...t, isBuiltIn: false }));
    } catch {
        return [];
    }
}

/**
 * Save custom templates to vault storage.
 * Only persists non-built-in templates; built-in templates are always
 * available from DEFAULT_TEMPLATES.
 */
export async function saveCustomTemplates(templates: PromptTemplate[]): Promise<void> {
    const custom = templates.filter(t => !t.isBuiltIn);
    try {
        await invoke('vault_set', {
            key: 'ai_prompt_templates',
            value: JSON.stringify(custom),
        });
    } catch {
        // Vault might not be available, silently fail
    }
}
