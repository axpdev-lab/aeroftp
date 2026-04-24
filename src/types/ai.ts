// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

// AI Provider and Model Types for AeroFTP AI Agent

export type AIProviderType = 'openai' | 'anthropic' | 'google' | 'xai' | 'openrouter' | 'ollama' | 'custom' | 'kimi' | 'qwen' | 'deepseek' | 'mistral' | 'groq' | 'perplexity' | 'cohere' | 'together' | 'ai21' | 'cerebras' | 'sambanova' | 'fireworks' | 'nvidia' | 'zai' | 'hyperbolic' | 'novita' | 'yi';

export interface AIProvider {
    id: string;
    name: string;
    type: AIProviderType;
    baseUrl: string;
    apiKey?: string;  // Will be stored securely
    isEnabled: boolean;
    isDefault: boolean;
    createdAt: Date;
    updatedAt: Date;
}

export interface AIModel {
    id: string;
    providerId: string;
    name: string;
    displayName: string;
    maxTokens: number;
    maxContextTokens?: number;         // Input context window size (distinct from maxTokens output limit)
    inputCostPer1k?: number;
    outputCostPer1k?: number;
    supportsStreaming: boolean;
    supportsTools: boolean;
    supportsVision: boolean;
    supportsThinking?: boolean;        // Extended thinking / chain-of-thought (Claude, o3)
    supportsParallelTools?: boolean;   // Multiple tool calls in single response
    toolCallQuality?: 1 | 2 | 3 | 4 | 5;   // Tool call accuracy rating
    bestFor?: string[];                        // Capability tags
    isEnabled: boolean;
    isDefault: boolean;
}

// Auto-routing configuration
export type TaskType = 'code_generation' | 'quick_answer' | 'file_analysis' | 'terminal_command' | 'code_review' | 'general';

export interface AutoRoutingRule {
    taskType: TaskType;
    preferredModelId: string;
    fallbackModelId?: string;
}

export interface AISettings {
    providers: AIProvider[];
    models: AIModel[];
    autoRouting: {
        enabled: boolean;
        rules: AutoRoutingRule[];
    };
    advancedSettings: {
        temperature: number;        // 0.0 - 2.0
        maxTokens: number;          // Max response length
        topP?: number;              // Top-P nucleus sampling (0.0-1.0)
        topK?: number;              // Top-K sampling (1-100)
        conversationStyle: 'precise' | 'balanced' | 'creative';
        customSystemPrompt?: string;
        useCustomPrompt?: boolean;
        thinkingBudget?: number;    // Extended thinking budget tokens (0 = disabled, default 10000)
        webSearchEnabled?: boolean;    // Provider web search (Kimi $web_search, Qwen enable_search)
        streamingTimeoutSecs?: number; // Streaming response timeout in seconds (default 120)
        chatHistoryRetentionDays?: number; // 0 = unlimited, 30/60/90/180/365 days
        enableAutoRAGIndexing?: boolean;   // B07: opt-in auto-index workspace for RAG context (default: true)
    };
    defaultModelId: string | null;
}

// Chat message types
export interface ChatMessage {
    id: string;
    role: 'user' | 'assistant' | 'system';
    content: string;
    timestamp: Date;
    modelId?: string;
    toolCalls?: ToolCall[];
}

export interface ToolCall {
    id: string;
    name: string;
    arguments: Record<string, unknown>;
    result?: unknown;
    status: 'pending' | 'approved' | 'rejected' | 'completed' | 'error';
    error?: string;
}

// Built-in provider presets
export const PROVIDER_PRESETS: Omit<AIProvider, 'id' | 'apiKey' | 'createdAt' | 'updatedAt'>[] = [
    {
        name: 'Google Gemini',
        type: 'google',
        baseUrl: 'https://generativelanguage.googleapis.com/v1beta',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'OpenAI',
        type: 'openai',
        baseUrl: 'https://api.openai.com/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Anthropic',
        type: 'anthropic',
        baseUrl: 'https://api.anthropic.com/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'xAI (Grok)',
        type: 'xai',
        baseUrl: 'https://api.x.ai/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'OpenRouter',
        type: 'openrouter',
        baseUrl: 'https://openrouter.ai/api/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Ollama (Local)',
        type: 'ollama',
        baseUrl: 'http://localhost:11434',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Kimi (Moonshot)',
        type: 'kimi',
        baseUrl: 'https://api.moonshot.cn/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Qwen (Alibaba)',
        type: 'qwen',
        baseUrl: 'https://dashscope-intl.aliyuncs.com/compatible-mode/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'DeepSeek',
        type: 'deepseek',
        baseUrl: 'https://api.deepseek.com',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Mistral',
        type: 'mistral',
        baseUrl: 'https://api.mistral.ai/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Groq',
        type: 'groq',
        baseUrl: 'https://api.groq.com/openai/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Perplexity',
        type: 'perplexity',
        baseUrl: 'https://api.perplexity.ai',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Cohere',
        type: 'cohere',
        baseUrl: 'https://api.cohere.com/compatibility/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Together AI',
        type: 'together',
        baseUrl: 'https://api.together.xyz/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'AI21 Labs',
        type: 'ai21',
        baseUrl: 'https://api.ai21.com/studio/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Cerebras',
        type: 'cerebras',
        baseUrl: 'https://api.cerebras.ai/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'SambaNova',
        type: 'sambanova',
        baseUrl: 'https://api.sambanova.ai/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Fireworks AI',
        type: 'fireworks',
        baseUrl: 'https://api.fireworks.ai/inference/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'NVIDIA NIM',
        type: 'nvidia',
        baseUrl: 'https://integrate.api.nvidia.com/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Z.AI (Zhipu)',
        type: 'zai',
        baseUrl: 'https://api.z.ai/api/paas/v4',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Hyperbolic',
        type: 'hyperbolic',
        baseUrl: 'https://api.hyperbolic.xyz/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Novita AI',
        type: 'novita',
        baseUrl: 'https://api.novita.ai/v3/openai',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Yi (01.AI)',
        type: 'yi',
        baseUrl: 'https://api.lingyiwanwu.com/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'Custom',
        type: 'custom',
        baseUrl: '',
        isEnabled: false,
        isDefault: false,
    },
];

// Default models for each provider (empty — users add their own via Settings or "Models" button)
export const DEFAULT_MODELS: Record<AIProviderType, Omit<AIModel, 'id' | 'providerId'>[]> = {
    google: [],
    openai: [],
    anthropic: [],
    xai: [],
    openrouter: [],
    ollama: [],
    kimi: [],
    qwen: [],
    deepseek: [],
    mistral: [],
    groq: [],
    perplexity: [],
    cohere: [],
    together: [],
    ai21: [],
    cerebras: [],
    sambanova: [],
    fireworks: [],
    nvidia: [],
    zai: [],
    hyperbolic: [],
    novita: [],
    yi: [],
    custom: [],
};

// Helper to generate unique IDs
export const generateId = (): string => {
    return `${Date.now().toString(36)}-${Math.random().toString(36).substring(2, 11)}`;
};

// Initial empty settings
export const getDefaultAISettings = (): AISettings => ({
    providers: [],
    models: [],
    autoRouting: {
        enabled: false,
        rules: [],
    },
    advancedSettings: {
        temperature: 0.7,
        maxTokens: 4096,
        conversationStyle: 'balanced',
    },
    defaultModelId: null,
});
