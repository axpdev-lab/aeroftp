// AI Provider and Model Types for AeroFTP AI Agent

export type AIProviderType = 'openai' | 'anthropic' | 'google' | 'xai' | 'openrouter' | 'ollama' | 'custom';

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
    inputCostPer1k?: number;
    outputCostPer1k?: number;
    supportsStreaming: boolean;
    supportsTools: boolean;
    supportsVision: boolean;
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
        conversationStyle: 'precise' | 'balanced' | 'creative';
        customSystemPrompt?: string;
        useCustomPrompt?: boolean;
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
];

// Default models for each provider
export const DEFAULT_MODELS: Record<AIProviderType, Omit<AIModel, 'id' | 'providerId'>[]> = {
    google: [
        { name: 'gemini-2.5-flash-preview-05-20', displayName: 'Gemini 2.5 Flash', maxTokens: 8192, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: true },
        { name: 'gemini-2.0-flash-exp', displayName: 'Gemini 2.0 Flash', maxTokens: 8192, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: false },
        { name: 'gemini-1.5-flash', displayName: 'Gemini 1.5 Flash', maxTokens: 32000, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: false },
    ],
    openai: [
        { name: 'gpt-4o', displayName: 'GPT-4o', maxTokens: 128000, inputCostPer1k: 0.005, outputCostPer1k: 0.015, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: true },
        { name: 'gpt-4o-mini', displayName: 'GPT-4o Mini', maxTokens: 128000, inputCostPer1k: 0.00015, outputCostPer1k: 0.0006, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: false },
        { name: 'gpt-4-turbo', displayName: 'GPT-4 Turbo', maxTokens: 128000, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: false },
    ],
    anthropic: [
        { name: 'claude-3-5-sonnet-20241022', displayName: 'Claude 3.5 Sonnet', maxTokens: 200000, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: true },
        { name: 'claude-3-opus-20240229', displayName: 'Claude 3 Opus', maxTokens: 200000, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: false },
    ],
    xai: [
        { name: 'grok-beta', displayName: 'Grok Beta', maxTokens: 131072, supportsStreaming: true, supportsTools: true, supportsVision: false, isEnabled: true, isDefault: true },
    ],
    openrouter: [
        { name: 'google/gemini-2.0-flash-exp:free', displayName: 'Gemini 2.0 Flash (Free)', maxTokens: 8192, supportsStreaming: true, supportsTools: true, supportsVision: true, isEnabled: true, isDefault: true },
        { name: 'meta-llama/llama-3.2-3b-instruct:free', displayName: 'Llama 3.2 3B (Free)', maxTokens: 8192, supportsStreaming: true, supportsTools: false, supportsVision: false, isEnabled: true, isDefault: false },
    ],
    ollama: [
        { name: 'llama3.2', displayName: 'Llama 3.2', maxTokens: 8192, supportsStreaming: true, supportsTools: false, supportsVision: false, isEnabled: true, isDefault: true },
        { name: 'codellama', displayName: 'Code Llama', maxTokens: 4096, supportsStreaming: true, supportsTools: false, supportsVision: false, isEnabled: true, isDefault: false },
    ],
    custom: [],
};

// Helper to generate unique IDs
export const generateId = (): string => {
    return `${Date.now().toString(36)}-${Math.random().toString(36).substr(2, 9)}`;
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
