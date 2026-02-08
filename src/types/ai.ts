// AI Provider and Model Types for AeroFTP AI Agent

export type AIProviderType = 'openai' | 'anthropic' | 'google' | 'xai' | 'openrouter' | 'ollama' | 'custom' | 'kimi' | 'qwen' | 'deepseek';

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
        baseUrl: 'https://dashscope.aliyuncs.com/compatible-mode/v1',
        isEnabled: false,
        isDefault: false,
    },
    {
        name: 'DeepSeek',
        type: 'deepseek',
        baseUrl: 'https://api.deepseek.com/v1',
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
    kimi: [
        {
            name: 'moonshot-v1-8k',
            displayName: 'Moonshot v1 8K',
            maxTokens: 4096,
            maxContextTokens: 8192,
            inputCostPer1k: 0.0012,
            outputCostPer1k: 0.0012,
            supportsStreaming: true,
            supportsTools: true,
            supportsVision: false,
            supportsThinking: false,
            supportsParallelTools: true,
            toolCallQuality: 4,
            bestFor: ['general', 'code'],
            isEnabled: true,
            isDefault: false,
        },
        {
            name: 'moonshot-v1-128k',
            displayName: 'Moonshot v1 128K',
            maxTokens: 4096,
            maxContextTokens: 131072,
            inputCostPer1k: 0.006,
            outputCostPer1k: 0.006,
            supportsStreaming: true,
            supportsTools: true,
            supportsVision: false,
            supportsThinking: false,
            supportsParallelTools: true,
            toolCallQuality: 4,
            bestFor: ['long-context', 'analysis'],
            isEnabled: true,
            isDefault: true,
        },
    ],
    qwen: [
        {
            name: 'qwen-max',
            displayName: 'Qwen Max',
            maxTokens: 8192,
            maxContextTokens: 32768,
            inputCostPer1k: 0.016,
            outputCostPer1k: 0.064,
            supportsStreaming: true,
            supportsTools: true,
            supportsVision: true,
            supportsThinking: true,
            supportsParallelTools: true,
            toolCallQuality: 5,
            bestFor: ['reasoning', 'analysis'],
            isEnabled: true,
            isDefault: true,
        },
        {
            name: 'qwen-plus',
            displayName: 'Qwen Plus',
            maxTokens: 8192,
            maxContextTokens: 131072,
            inputCostPer1k: 0.0004,
            outputCostPer1k: 0.0012,
            supportsStreaming: true,
            supportsTools: true,
            supportsVision: true,
            supportsThinking: true,
            supportsParallelTools: true,
            toolCallQuality: 4,
            bestFor: ['general', 'code'],
            isEnabled: true,
            isDefault: false,
        },
        {
            name: 'qwen-turbo',
            displayName: 'Qwen Turbo',
            maxTokens: 8192,
            maxContextTokens: 1000000,
            inputCostPer1k: 0.00018,
            outputCostPer1k: 0.00072,
            supportsStreaming: true,
            supportsTools: true,
            supportsVision: false,
            supportsThinking: false,
            supportsParallelTools: true,
            toolCallQuality: 3,
            bestFor: ['fast'],
            isEnabled: true,
            isDefault: false,
        },
    ],
    deepseek: [
        {
            name: 'deepseek-chat',
            displayName: 'DeepSeek V3',
            maxTokens: 8192,
            maxContextTokens: 65536,
            inputCostPer1k: 0.00014,
            outputCostPer1k: 0.00028,
            supportsStreaming: true,
            supportsTools: true,
            supportsVision: false,
            supportsThinking: false,
            supportsParallelTools: true,
            toolCallQuality: 4,
            bestFor: ['general', 'code'],
            isEnabled: true,
            isDefault: true,
        },
        {
            name: 'deepseek-reasoner',
            displayName: 'DeepSeek R1',
            maxTokens: 8192,
            maxContextTokens: 65536,
            inputCostPer1k: 0.00055,
            outputCostPer1k: 0.0022,
            supportsStreaming: true,
            supportsTools: false,
            supportsVision: false,
            supportsThinking: true,
            supportsParallelTools: false,
            toolCallQuality: 1,
            bestFor: ['reasoning', 'math'],
            isEnabled: true,
            isDefault: false,
        },
    ],
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
