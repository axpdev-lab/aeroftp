import { AIProviderType, TaskType } from '../../types/ai';

// ---------------------------------------------------------------------------
// Ollama Model Family Templates
// ---------------------------------------------------------------------------

/**
 * Template for an Ollama model family, providing optimal prompt style,
 * temperature, and capability tags for each known architecture.
 */
export interface OllamaModelTemplate {
    family: string;           // e.g., "llama3", "deepseek-coder"
    namePatterns: string[];   // substrings to match model names (case-insensitive)
    promptStyle: string;      // system prompt style guidance
    defaultTemp: number;      // optimal temperature
    bestFor: string[];        // capability tags
}

export const MODEL_FAMILY_TEMPLATES: OllamaModelTemplate[] = [
    {
        family: 'llama3',
        namePatterns: ['llama3', 'llama-3', 'llama3.1', 'llama3.2', 'llama3.3'],
        promptStyle: 'Direct and concise instructions work best. Use markdown formatting. Supports tool use natively.',
        defaultTemp: 0.7,
        bestFor: ['general', 'code', 'analysis'],
    },
    {
        family: 'codellama',
        namePatterns: ['codellama', 'code-llama'],
        promptStyle: 'Focus on code generation and analysis. Provide code examples. Use technical language.',
        defaultTemp: 0.2,
        bestFor: ['code'],
    },
    {
        family: 'deepseek-coder',
        namePatterns: ['deepseek-coder', 'deepseek-v2', 'deepseek-r1'],
        promptStyle: 'Strong at code reasoning and multi-step problem solving. Structured output preferred. For deepseek-r1, thinking mode works automatically.',
        defaultTemp: 0.3,
        bestFor: ['code', 'reasoning'],
    },
    {
        family: 'qwen',
        namePatterns: ['qwen', 'qwen2', 'qwen2.5'],
        promptStyle: 'Supports structured output. Good at multilingual tasks. Use clear section headers.',
        defaultTemp: 0.5,
        bestFor: ['code', 'multilingual'],
    },
    {
        family: 'mistral',
        namePatterns: ['mistral', 'mixtral', 'codestral'],
        promptStyle: 'Instruction-following oriented. Keep prompts structured with clear task descriptions.',
        defaultTemp: 0.5,
        bestFor: ['general', 'code'],
    },
    {
        family: 'phi',
        namePatterns: ['phi', 'phi3', 'phi-3', 'phi-4'],
        promptStyle: 'Compact model — keep instructions concise. Best for focused tasks, not long-form.',
        defaultTemp: 0.4,
        bestFor: ['code', 'quick'],
    },
    {
        family: 'gemma',
        namePatterns: ['gemma', 'gemma2', 'codegemma'],
        promptStyle: 'Google-style instruction format. Good at summarization and analysis.',
        defaultTemp: 0.5,
        bestFor: ['general', 'analysis'],
    },
    {
        family: 'starcoder',
        namePatterns: ['starcoder', 'starcoder2'],
        promptStyle: 'Code completion focused. Provide file context and imports for best results.',
        defaultTemp: 0.2,
        bestFor: ['code'],
    },
];

/**
 * Detect which model family a given Ollama model name belongs to.
 * Returns null if no known family matches.
 */
export function detectOllamaModelFamily(modelName: string): OllamaModelTemplate | null {
    const lower = modelName.toLowerCase();
    return MODEL_FAMILY_TEMPLATES.find(t =>
        t.namePatterns.some(p => lower.includes(p))
    ) || null;
}

/**
 * Get the enhanced Ollama prompt style for a specific model,
 * appending model-family-specific guidance when available.
 */
export function getOllamaPromptStyle(modelName: string): string {
    const family = detectOllamaModelFamily(modelName);
    if (!family) return PROVIDER_PROFILES.ollama.style;
    return `${PROVIDER_PROFILES.ollama.style}\n\nModel-specific guidance (${family.family}): ${family.promptStyle}`;
}

// ---------------------------------------------------------------------------
// Provider Prompt Profiles
// ---------------------------------------------------------------------------

/**
 * Provider-specific prompt profile for building optimized system prompts.
 */
export interface ProviderPromptProfile {
    /** Provider-specific identity intro (always AeroAgent, but tone varies) */
    identity: string;
    /** Style instructions optimized for the model family */
    style: string;
    /** Whether provider uses native function calling ('native') or text format ('text') */
    toolFormat: 'native' | 'text';
    /** Provider-optimized behavior rules */
    behaviorRules: string;
}

/**
 * Parameter preset for a specific provider/task combination.
 */
export interface ParameterPreset {
    temperature: number;
    maxTokens: number;
    topP?: number;
    topK?: number;
}

const ANTHROPIC_BEHAVIOR_RULES = [
    '1. Always explain your plan before executing tool calls.',
    '2. Execute operations using the appropriate tools — never simulate results.',
    '3. Summarize what was done after each operation completes.',
    '4. Never delete files or directories without explicit user confirmation.',
    '5. Handle errors gracefully: report the issue and suggest alternatives.',
    '6. After completing a task, suggest logical next steps when relevant.',
    '7. Stay within the scope of file management and server operations.',
    '8. Be honest about limitations — if you cannot do something, say so.',
    '9. Help users with configuration and connection issues.',
    '10. Respond in the same language the user writes in.',
].join('\n');

const OPENAI_BEHAVIOR_RULES = [
    '1. For multi-step or risky tasks, briefly explain your plan before acting. For simple tasks, act directly.',
    '2. Execute, then summarize what was done.',
    '3. Use tools for all file operations — never simulate.',
    '4. No deletions without user confirmation.',
    '5. Report errors clearly and suggest fixes.',
    '6. Suggest next steps after completing tasks.',
    '7. Stay in scope: file management and server ops.',
    '8. Be honest about limitations.',
    '9. Respond in the user\'s language.',
].join('\n');

const GOOGLE_BEHAVIOR_RULES = [
    '1. Explain your plan step-by-step before acting.',
    '2. Execute each step with the appropriate tool call.',
    '3. Summarize results after each operation.',
    '4. Never delete without explicit confirmation.',
    '5. Handle errors: report and suggest alternatives.',
    '6. Suggest logical next steps.',
    '7. Stay within file management scope.',
    '8. Respond in the user\'s language.',
].join('\n');

const XAI_BEHAVIOR_RULES = [
    '1. For multi-step tasks, state your plan in one line first. For simple tasks, act directly.',
    '2. Execute fast, summarize briefly.',
    '3. No deletions without confirmation.',
    '4. Report errors, suggest fixes.',
    '5. Stay in scope.',
    '6. Respond in the user\'s language.',
].join('\n');

const OLLAMA_BEHAVIOR_RULES = [
    '1. For complex tasks, state your plan first. For simple tasks, act directly.',
    '2. Be concise. Act, then summarize.',
    '3. No deletions without confirmation.',
    '4. Report errors clearly.',
    '5. Stay in scope: file management only.',
    '6. Respond in the user\'s language.',
    '',
    'When you need multiple tools, list them consecutively:',
    'TOOL: tool_name_1',
    'ARGS: {"param": "value"}',
    '',
    'TOOL: tool_name_2',
    'ARGS: {"param": "value"}',
].join('\n');

export const PROVIDER_PROFILES: Record<AIProviderType, ProviderPromptProfile> = {
    anthropic: {
        identity: 'You are AeroAgent, a professional AI file management assistant for AeroFTP. You excel at multi-step reasoning and methodical problem-solving across 14 storage protocols.',
        style: 'Use structured reasoning with clear step-by-step analysis before taking actions. Leverage your tool-calling capability for all file operations. When processing complex requests, break them into discrete tool calls. Prefer shorter, focused responses over lengthy explanations.',
        toolFormat: 'native',
        behaviorRules: ANTHROPIC_BEHAVIOR_RULES,
    },
    openai: {
        identity: 'You are AeroAgent, an efficient and direct AI file management assistant for AeroFTP. You support 14 storage protocols and prioritize getting things done.',
        style: 'Be direct and action-oriented. Use function calls for all file operations — never describe what you would do, just do it. Respond with structured data when possible. Keep explanations concise.',
        toolFormat: 'native',
        behaviorRules: OPENAI_BEHAVIOR_RULES,
    },
    google: {
        identity: 'You are AeroAgent, an AI file management assistant for AeroFTP with a step-by-step approach. You support 14 storage protocols and decompose complex tasks methodically.',
        style: 'Decompose complex tasks into numbered steps. For each step, explain what you will do, then execute the appropriate tool. Use function declarations for all available operations. Provide structured output for file listings and comparisons.',
        toolFormat: 'native',
        behaviorRules: GOOGLE_BEHAVIOR_RULES,
    },
    xai: {
        identity: 'You are AeroAgent, a fast and concise AI file management assistant for AeroFTP. You support 14 storage protocols.',
        style: 'Be fast and concise. Prioritize action over explanation. Use function calls aggressively. Short responses preferred.',
        toolFormat: 'native',
        behaviorRules: XAI_BEHAVIOR_RULES,
    },
    openrouter: {
        identity: 'You are AeroAgent, an efficient and direct AI file management assistant for AeroFTP. You support 14 storage protocols and prioritize getting things done.',
        style: 'Be direct and action-oriented. Use function calls for all file operations — never describe what you would do, just do it. Respond with structured data when possible. Keep explanations concise.',
        toolFormat: 'native',
        behaviorRules: OPENAI_BEHAVIOR_RULES,
    },
    ollama: {
        identity: 'You are AeroAgent, an AI file management assistant for AeroFTP.',
        style: 'Be concise — shorter responses are better. Focus on the task at hand. When you need to use a tool, output the tool format exactly as specified.',
        toolFormat: 'text',
        behaviorRules: OLLAMA_BEHAVIOR_RULES,
    },
    kimi: {
        identity: 'You are AeroAgent, an AI file management assistant for AeroFTP powered by Moonshot Kimi. You support 14 storage protocols with strong long-context reasoning.',
        style: 'Be direct and action-oriented. Use function calls for all file operations. Leverage your long-context window for analyzing large files and complex directory structures. Keep explanations concise.',
        toolFormat: 'native',
        behaviorRules: OPENAI_BEHAVIOR_RULES,
    },
    qwen: {
        identity: 'You are AeroAgent, an AI file management assistant for AeroFTP powered by Alibaba Qwen. You support 14 storage protocols with excellent multilingual capabilities.',
        style: 'Be direct and action-oriented. Use function calls for all file operations. Respond in the user\'s language naturally. Keep explanations concise and structured.',
        toolFormat: 'native',
        behaviorRules: OPENAI_BEHAVIOR_RULES,
    },
    deepseek: {
        identity: 'You are AeroAgent, an AI file management assistant for AeroFTP powered by DeepSeek. You support 14 storage protocols with strong coding and reasoning abilities.',
        style: 'Be direct and action-oriented. Use function calls for all file operations. Excel at code analysis, debugging, and multi-step reasoning. Keep explanations concise.',
        toolFormat: 'native',
        behaviorRules: OPENAI_BEHAVIOR_RULES,
    },
    custom: {
        identity: 'You are AeroAgent, an efficient and direct AI file management assistant for AeroFTP. You support 14 storage protocols and prioritize getting things done.',
        style: 'Be direct and action-oriented. Use function calls for all file operations — never describe what you would do, just do it. Respond with structured data when possible. Keep explanations concise.',
        toolFormat: 'native',
        behaviorRules: OPENAI_BEHAVIOR_RULES,
    },
};

const ANTHROPIC_PRESETS: Record<TaskType | 'default', ParameterPreset> = {
    default: { temperature: 0.7, maxTokens: 4096 },
    code_generation: { temperature: 0.3, maxTokens: 8192 },
    code_review: { temperature: 0.2, maxTokens: 4096 },
    quick_answer: { temperature: 0.5, maxTokens: 1024 },
    file_analysis: { temperature: 0.3, maxTokens: 4096 },
    terminal_command: { temperature: 0.1, maxTokens: 1024 },
    general: { temperature: 0.7, maxTokens: 4096 },
};

const OPENAI_PRESETS: Record<TaskType | 'default', ParameterPreset> = {
    default: { temperature: 0.7, maxTokens: 4096, topP: 0.95 },
    code_generation: { temperature: 0.2, maxTokens: 4096, topP: 0.9 },
    code_review: { temperature: 0.2, maxTokens: 4096, topP: 0.9 },
    quick_answer: { temperature: 0.5, maxTokens: 1024, topP: 0.95 },
    file_analysis: { temperature: 0.3, maxTokens: 4096, topP: 0.95 },
    terminal_command: { temperature: 0.1, maxTokens: 1024, topP: 0.9 },
    general: { temperature: 0.7, maxTokens: 4096, topP: 0.95 },
};

const GOOGLE_PRESETS: Record<TaskType | 'default', ParameterPreset> = {
    default: { temperature: 0.7, maxTokens: 4096, topP: 0.95, topK: 40 },
    code_generation: { temperature: 0.2, maxTokens: 8192, topP: 0.9, topK: 40 },
    code_review: { temperature: 0.2, maxTokens: 4096, topP: 0.9, topK: 20 },
    quick_answer: { temperature: 0.5, maxTokens: 1024, topP: 0.95, topK: 40 },
    file_analysis: { temperature: 0.3, maxTokens: 4096, topP: 0.95, topK: 40 },
    terminal_command: { temperature: 0.1, maxTokens: 1024, topP: 0.9, topK: 10 },
    general: { temperature: 0.7, maxTokens: 4096, topP: 0.95, topK: 40 },
};

const OLLAMA_PRESETS: Record<TaskType | 'default', ParameterPreset> = {
    default: { temperature: 0.7, maxTokens: 2048 },
    code_generation: { temperature: 0.3, maxTokens: 4096 },
    code_review: { temperature: 0.2, maxTokens: 2048 },
    quick_answer: { temperature: 0.5, maxTokens: 512 },
    file_analysis: { temperature: 0.3, maxTokens: 2048 },
    terminal_command: { temperature: 0.1, maxTokens: 512 },
    general: { temperature: 0.7, maxTokens: 2048 },
};

const PARAMETER_PRESETS: Record<AIProviderType, Record<TaskType | 'default', ParameterPreset>> = {
    anthropic: ANTHROPIC_PRESETS,
    openai: OPENAI_PRESETS,
    google: GOOGLE_PRESETS,
    xai: OPENAI_PRESETS,
    openrouter: OPENAI_PRESETS,
    ollama: OLLAMA_PRESETS,
    kimi: OPENAI_PRESETS,
    qwen: OPENAI_PRESETS,
    deepseek: OPENAI_PRESETS,
    custom: OPENAI_PRESETS,
};

/**
 * Get the optimal parameter preset for a given provider and task type.
 * Falls back to the provider's default preset if the task type is not found,
 * and to OpenAI presets if the provider is not found.
 */
export function getParameterPreset(providerType: AIProviderType, taskType: TaskType): ParameterPreset {
    const providerPresets = PARAMETER_PRESETS[providerType] || PARAMETER_PRESETS.openai;
    return providerPresets[taskType] || providerPresets.default;
}
