// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// CC-10: output styles / response personas. A selected style appends an
// "## Output Style" directive to the system prompt, adjusting how the agent
// phrases its answers without changing its tools or behavior rules.

export type ResponseStyleId = 'default' | 'concise' | 'explanatory' | 'learning';

export interface ResponseStyle {
    id: ResponseStyleId;
    name: string;
    description: string;
    /** Directive appended to the system prompt; empty for the default style. */
    directive: string;
}

export const RESPONSE_STYLES: ResponseStyle[] = [
    {
        id: 'default',
        name: 'Default',
        description: 'Balanced responses tuned per provider profile.',
        directive: '',
    },
    {
        id: 'concise',
        name: 'Concise',
        description: 'Short, direct answers with minimal preamble.',
        directive:
            'Respond as concisely as possible. Lead with the answer or result, omit preamble and restated context, and prefer short bullet points over prose. Expand only when the user explicitly asks for more detail.',
    },
    {
        id: 'explanatory',
        name: 'Explanatory',
        description: 'Detailed answers with reasoning and context.',
        directive:
            'Explain your reasoning and the relevant context. Describe why each step or recommendation matters, surface trade-offs, and include short examples where they aid understanding.',
    },
    {
        id: 'learning',
        name: 'Learning',
        description: 'Teaching tone that builds understanding step by step.',
        directive:
            'Adopt a teaching tone. Build understanding step by step, define key terms the first time they appear, and after completing a task summarize what was learned and suggest what to explore next.',
    },
];

/**
 * Resolve a response-style id to its system-prompt directive. Returns an empty
 * string for the default (or an unknown) style so the caller appends nothing.
 */
export function getResponseStyleDirective(id?: string): string {
    const style = RESPONSE_STYLES.find(s => s.id === id);
    return style && style.id !== 'default' ? style.directive : '';
}
