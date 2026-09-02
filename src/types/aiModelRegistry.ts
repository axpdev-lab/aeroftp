// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { AIModel, AIModelNativeCapabilities, AIProviderType } from './ai';

export const MODEL_REGISTRY_REVIEWED_AT = '2026-09-02';
export const UNKNOWN_MODEL_CONTEXT_BUDGET = 8192;

export type ModelCapabilitySource = 'registry' | 'user' | 'unknown';

export interface ModelContextResolution {
    tokens: number;
    source: 'model' | 'registry' | 'conservative';
    verified: boolean;
}

/**
 * Specification for a known AI model, including capabilities, pricing, and quality ratings.
 */
export interface KnownModelSpec {
    displayName: string;
    maxTokens: number;
    maxContextTokens: number;
    inputCostPer1k?: number;
    outputCostPer1k?: number;
    supportsStreaming: boolean;
    supportsTools: boolean;
    supportsVision: boolean;
    supportsThinking: boolean;
    supportsParallelTools: boolean;
    toolCallQuality: 1 | 2 | 3 | 4 | 5;
    bestFor: string[];
    metadataReviewedAt?: string;
    metadataSource?: string;
    nativeCapabilities?: AIModelNativeCapabilities;
    lifecycleStatus?: 'active' | 'deprecated' | 'retired';
}

// FEAT-01: best-effort metadata layer for pricing/capability hints. The live
// `ai_list_models` fetch is the source of truth for which models exist; this
// registry only supplies cost + capability defaults. Keep it fresh each release
// and DO NOT guess pricing: an entry with a wrong price shows the user a wrong
// cost (worse than the "no cost shown" fallback for an unknown model). Verify
// per-1k pricing against the provider's published rates before adding a model.
// Registry sweep last reviewed: 2026-09-02. Each newly verified provider entry
// should carry its own metadataReviewedAt + metadataSource; the sweep date does
// not silently certify older entries whose provider metadata was not rechecked.
export const MODEL_REGISTRY: Record<string, KnownModelSpec> = {
    // OpenAI
    // Official model pages, reviewed 2026-09-02:
    // https://developers.openai.com/api/docs/models/gpt-5.6-sol
    // https://developers.openai.com/api/docs/models/gpt-5.6-terra
    // https://developers.openai.com/api/docs/models/gpt-5.6-luna
    'gpt-5.6-sol': {
        displayName: 'GPT-5.6 Sol',
        maxTokens: 128000,
        maxContextTokens: 1050000,
        inputCostPer1k: 0.004,
        outputCostPer1k: 0.02,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://developers.openai.com/api/docs/models/gpt-5.6-sol',
        nativeCapabilities: {
            responses: true,
            hostedTools: true,
            toolSearch: true,
            programmaticToolCalling: true,
            multiAgent: true,
            persistedReasoning: true,
            reasoningEfforts: ['none', 'low', 'medium', 'high', 'xhigh', 'max'],
        },
    },
    'gpt-5.6': {
        displayName: 'GPT-5.6 (Sol alias)',
        maxTokens: 128000,
        maxContextTokens: 1050000,
        inputCostPer1k: 0.004,
        outputCostPer1k: 0.02,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://developers.openai.com/api/docs/models/gpt-5.6-sol',
        nativeCapabilities: {
            responses: true,
            hostedTools: true,
            toolSearch: true,
            programmaticToolCalling: true,
            multiAgent: true,
            persistedReasoning: true,
            reasoningEfforts: ['none', 'low', 'medium', 'high', 'xhigh', 'max'],
        },
    },
    'gpt-5.6-terra': {
        displayName: 'GPT-5.6 Terra',
        maxTokens: 128000,
        maxContextTokens: 1050000,
        inputCostPer1k: 0.002,
        outputCostPer1k: 0.012,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://developers.openai.com/api/docs/models/gpt-5.6-terra',
        nativeCapabilities: {
            responses: true,
            hostedTools: true,
            toolSearch: true,
            programmaticToolCalling: true,
            multiAgent: true,
            persistedReasoning: true,
            reasoningEfforts: ['none', 'low', 'medium', 'high', 'xhigh', 'max'],
        },
    },
    'gpt-5.6-luna': {
        displayName: 'GPT-5.6 Luna',
        maxTokens: 128000,
        maxContextTokens: 1050000,
        inputCostPer1k: 0.0002,
        outputCostPer1k: 0.0012,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['fast', 'code', 'reasoning', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://developers.openai.com/api/docs/models/gpt-5.6-luna',
        nativeCapabilities: {
            responses: true,
            hostedTools: true,
            toolSearch: true,
            programmaticToolCalling: true,
            multiAgent: true,
            persistedReasoning: true,
            reasoningEfforts: ['none', 'low', 'medium', 'high', 'xhigh', 'max'],
        },
    },
    'gpt-4o': {
        displayName: 'GPT-4o',
        maxTokens: 16384,
        maxContextTokens: 128000,
        inputCostPer1k: 0.0025,
        outputCostPer1k: 0.01,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'analysis', 'vision'],
    },
    'gpt-4o-mini': {
        displayName: 'GPT-4o Mini',
        maxTokens: 16384,
        maxContextTokens: 128000,
        inputCostPer1k: 0.00015,
        outputCostPer1k: 0.0006,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 4,
        bestFor: ['fast', 'code'],
    },
    'gpt-4-turbo': {
        displayName: 'GPT-4 Turbo',
        maxTokens: 4096,
        maxContextTokens: 128000,
        inputCostPer1k: 0.01,
        outputCostPer1k: 0.03,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'analysis'],
    },
    'o3-mini': {
        displayName: 'o3-mini',
        maxTokens: 16384,
        maxContextTokens: 128000,
        inputCostPer1k: 0.0011,
        outputCostPer1k: 0.0044,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: false,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning'],
    },
    'o3': {
        displayName: 'o3',
        maxTokens: 100000,
        maxContextTokens: 200000,
        inputCostPer1k: 0.01,
        outputCostPer1k: 0.04,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis'],
    },

    // Anthropic. Current model table and Models API, reviewed 2026-09-02:
    // https://platform.claude.com/docs/en/models/overview
    // https://platform.claude.com/docs/en/api/models/retrieve
    'claude-fable-5-1': {
        displayName: 'Claude Fable 5.1',
        maxTokens: 128000,
        maxContextTokens: 1000000,
        inputCostPer1k: 0.01,
        outputCostPer1k: 0.05,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.claude.com/docs/en/models/overview',
        nativeCapabilities: {
            adaptiveThinking: true,
            contextManagement: true,
            modelCapabilitiesApi: true,
        },
    },
    'claude-opus-5': {
        displayName: 'Claude Opus 5',
        maxTokens: 128000,
        maxContextTokens: 1000000,
        inputCostPer1k: 0.005,
        outputCostPer1k: 0.025,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.claude.com/docs/en/models/overview',
        nativeCapabilities: {
            adaptiveThinking: true,
            contextManagement: true,
            modelCapabilitiesApi: true,
        },
    },
    'claude-sonnet-5': {
        displayName: 'Claude Sonnet 5',
        maxTokens: 128000,
        maxContextTokens: 1000000,
        inputCostPer1k: 0.002,
        outputCostPer1k: 0.01,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.claude.com/docs/en/models/overview',
        nativeCapabilities: {
            adaptiveThinking: true,
            contextManagement: true,
            modelCapabilitiesApi: true,
        },
    },
    'claude-opus-4-8': {
        displayName: 'Claude Opus 4.8',
        maxTokens: 128000,
        maxContextTokens: 1000000,
        inputCostPer1k: 0.005,
        outputCostPer1k: 0.025,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.claude.com/docs/en/about-claude/models/overview',
        nativeCapabilities: {
            adaptiveThinking: true,
            contextManagement: true,
            modelCapabilitiesApi: true,
        },
    },
    'claude-opus-4-7': {
        displayName: 'Claude Opus 4.7',
        maxTokens: 8192,
        maxContextTokens: 200000,
        inputCostPer1k: 0.015,
        outputCostPer1k: 0.075,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis'],
    },
    'claude-opus-4-6': {
        displayName: 'Claude Opus 4.6',
        maxTokens: 8192,
        maxContextTokens: 200000,
        inputCostPer1k: 0.015,
        outputCostPer1k: 0.075,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis'],
    },
    'claude-sonnet-4-6': {
        displayName: 'Claude Sonnet 4.6',
        maxTokens: 128000,
        maxContextTokens: 1000000,
        inputCostPer1k: 0.003,
        outputCostPer1k: 0.015,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'analysis'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.claude.com/docs/en/about-claude/models/overview',
        nativeCapabilities: {
            adaptiveThinking: true,
            contextManagement: true,
            modelCapabilitiesApi: true,
        },
    },
    'claude-sonnet-4-5-20250929': {
        displayName: 'Claude Sonnet 4.5',
        maxTokens: 8192,
        maxContextTokens: 200000,
        inputCostPer1k: 0.003,
        outputCostPer1k: 0.015,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'analysis'],
    },
    'claude-haiku-4-5-20251001': {
        displayName: 'Claude Haiku 4.5',
        maxTokens: 8192,
        maxContextTokens: 200000,
        inputCostPer1k: 0.0008,
        outputCostPer1k: 0.004,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 4,
        bestFor: ['fast', 'code'],
    },
    'claude-3-5-sonnet-20241022': {
        displayName: 'Claude 3.5 Sonnet',
        maxTokens: 8192,
        maxContextTokens: 200000,
        inputCostPer1k: 0.003,
        outputCostPer1k: 0.015,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'analysis'],
    },

    // Google
    'gemini-2.5-flash': {
        displayName: 'Gemini 2.5 Flash',
        maxTokens: 8192,
        maxContextTokens: 1048576,
        inputCostPer1k: 0.00015,
        outputCostPer1k: 0.0006,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 4,
        bestFor: ['fast', 'analysis'],
    },
    'gemini-2.5-pro': {
        displayName: 'Gemini 2.5 Pro',
        maxTokens: 8192,
        maxContextTokens: 1048576,
        inputCostPer1k: 0.00125,
        outputCostPer1k: 0.01,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis'],
    },
    'gemini-2.0-flash': {
        displayName: 'Gemini 2.0 Flash',
        maxTokens: 8192,
        maxContextTokens: 1048576,
        inputCostPer1k: 0.0001,
        outputCostPer1k: 0.0004,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 4,
        bestFor: ['fast'],
    },

    // xAI, reviewed 2026-09-02:
    // https://docs.x.ai/developers/models/grok-4.6
    'grok-4.6': {
        displayName: 'Grok 4.6',
        // xAI documents the 500K context window but not a separate maximum
        // output value on the model card; keep an explicit conservative app cap.
        maxTokens: 8192,
        maxContextTokens: 500000,
        inputCostPer1k: 0.002,
        outputCostPer1k: 0.006,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://docs.x.ai/developers/models/grok-4.6',
        nativeCapabilities: {
            responses: true,
            hostedTools: true,
            multiAgent: true,
            contextManagement: true,
            encryptedReasoningReplay: true,
            reasoningEfforts: ['low', 'medium', 'high', 'xhigh'],
        },
    },
    'grok-3': {
        displayName: 'Grok 3',
        maxTokens: 8192,
        maxContextTokens: 131072,
        inputCostPer1k: 0.003,
        outputCostPer1k: 0.015,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: true,
        toolCallQuality: 4,
        bestFor: ['code', 'creative'],
        lifecycleStatus: 'retired',
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://docs.x.ai/developers/migration/may-15-retirement',
    },
    'grok-3-mini': {
        displayName: 'Grok 3 Mini',
        maxTokens: 8192,
        maxContextTokens: 131072,
        inputCostPer1k: 0.0003,
        outputCostPer1k: 0.0005,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: false,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 3,
        bestFor: ['fast', 'reasoning'],
    },

    // Ollama (common local models)
    'llama3.3:70b': {
        displayName: 'Llama 3.3 70B',
        maxTokens: 4096,
        maxContextTokens: 128000,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 2,
        bestFor: ['general'],
    },
    'llama3.1:8b': {
        displayName: 'Llama 3.1 8B',
        maxTokens: 4096,
        maxContextTokens: 128000,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 1,
        bestFor: ['fast'],
    },
    'deepseek-coder-v2': {
        displayName: 'DeepSeek Coder V2',
        maxTokens: 4096,
        maxContextTokens: 128000,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 3,
        bestFor: ['code'],
    },
    'qwen2.5-coder:32b': {
        displayName: 'Qwen 2.5 Coder 32B',
        maxTokens: 4096,
        maxContextTokens: 32768,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 3,
        bestFor: ['code'],
    },
    'codellama:34b': {
        displayName: 'Code Llama 34B',
        maxTokens: 4096,
        maxContextTokens: 16384,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 2,
        bestFor: ['code'],
    },
    'mistral:7b': {
        displayName: 'Mistral 7B',
        maxTokens: 4096,
        maxContextTokens: 32768,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 2,
        bestFor: ['fast'],
    },
    'mixtral:8x22b': {
        displayName: 'Mixtral 8x22B',
        maxTokens: 4096,
        maxContextTokens: 65536,
        inputCostPer1k: 0,
        outputCostPer1k: 0,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        toolCallQuality: 3,
        bestFor: ['general', 'code'],
    },

    // Kimi / Moonshot, reviewed 2026-09-02. K3 uses Chat Completions with
    // provider-specific reasoning and dynamic tool declarations.
    'kimi-k3': {
        displayName: 'Kimi K3',
        maxTokens: 131072,
        maxContextTokens: 1048576,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'analysis', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.kimi.ai/docs/guide/kimi-k3-quickstart',
        nativeCapabilities: {
            hostedTools: true,
            dynamicToolLoading: true,
            automaticPromptCaching: true,
            requiresFullAssistantReplay: true,
            fixedSamplingParameters: true,
            reasoningEfforts: ['low', 'high', 'max'],
        },
    },
    'kimi-k2.7-code': {
        displayName: 'Kimi K2.7 Code',
        maxTokens: 32768,
        maxContextTokens: 262144,
        supportsStreaming: true,
        supportsTools: true,
        supportsVision: true,
        supportsThinking: true,
        supportsParallelTools: true,
        toolCallQuality: 5,
        bestFor: ['code', 'reasoning', 'vision', 'agent'],
        metadataReviewedAt: '2026-09-02',
        metadataSource: 'https://platform.kimi.ai/docs/guide/kimi-k2-7-code-quickstart',
        nativeCapabilities: {
            requiresFullAssistantReplay: true,
        },
    },
    'moonshot-v1-8k': { displayName: 'Moonshot v1 8K', maxTokens: 4096, maxContextTokens: 8192, inputCostPer1k: 0.0012, outputCostPer1k: 0.0012, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['general', 'code'], lifecycleStatus: 'retired', metadataReviewedAt: '2026-09-02', metadataSource: 'https://platform.kimi.ai/docs/models' },
    'moonshot-v1-32k': { displayName: 'Moonshot v1 32K', maxTokens: 4096, maxContextTokens: 32768, inputCostPer1k: 0.0024, outputCostPer1k: 0.0024, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['general', 'long-context'], lifecycleStatus: 'retired', metadataReviewedAt: '2026-09-02', metadataSource: 'https://platform.kimi.ai/docs/models' },
    'moonshot-v1-128k': { displayName: 'Moonshot v1 128K', maxTokens: 4096, maxContextTokens: 131072, inputCostPer1k: 0.006, outputCostPer1k: 0.006, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['long-context', 'analysis'], lifecycleStatus: 'retired', metadataReviewedAt: '2026-09-02', metadataSource: 'https://platform.kimi.ai/docs/models' },

    // Qwen (Alibaba Cloud)
    'qwen-max': { displayName: 'Qwen Max', maxTokens: 8192, maxContextTokens: 32768, inputCostPer1k: 0.016, outputCostPer1k: 0.064, supportsStreaming: true, supportsTools: true, supportsVision: true, supportsThinking: true, supportsParallelTools: true, toolCallQuality: 5, bestFor: ['reasoning', 'analysis'] },
    'qwen-plus': { displayName: 'Qwen Plus', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.0004, outputCostPer1k: 0.0012, supportsStreaming: true, supportsTools: true, supportsVision: true, supportsThinking: true, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['general', 'code'] },
    'qwen-turbo': { displayName: 'Qwen Turbo', maxTokens: 8192, maxContextTokens: 1000000, inputCostPer1k: 0.00018, outputCostPer1k: 0.00072, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 3, bestFor: ['fast'] },
    'qwen-long': { displayName: 'Qwen Long', maxTokens: 8192, maxContextTokens: 10000000, inputCostPer1k: 0.00018, outputCostPer1k: 0.00072, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 3, bestFor: ['long-context'] },

    // DeepSeek
    'deepseek-chat': { displayName: 'DeepSeek V3', maxTokens: 8192, maxContextTokens: 65536, inputCostPer1k: 0.00014, outputCostPer1k: 0.00028, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['general', 'code'] },
    'deepseek-reasoner': { displayName: 'DeepSeek R1', maxTokens: 8192, maxContextTokens: 65536, inputCostPer1k: 0.00055, outputCostPer1k: 0.0022, supportsStreaming: true, supportsTools: false, supportsVision: false, supportsThinking: true, supportsParallelTools: false, toolCallQuality: 1, bestFor: ['reasoning', 'math'] },

    // Mistral
    'mistral-large-latest': { displayName: 'Mistral Large', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.002, outputCostPer1k: 0.006, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['general', 'code'] },
    'mistral-small-latest': { displayName: 'Mistral Small', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.0002, outputCostPer1k: 0.0006, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 3, bestFor: ['fast', 'general'] },
    'codestral-latest': { displayName: 'Codestral', maxTokens: 8192, maxContextTokens: 32768, inputCostPer1k: 0.0003, outputCostPer1k: 0.0009, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['code'] },
    'pixtral-large-latest': { displayName: 'Pixtral Large', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.002, outputCostPer1k: 0.006, supportsStreaming: true, supportsTools: true, supportsVision: true, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 4, bestFor: ['vision', 'general'] },

    // Groq
    'llama-3.3-70b-versatile': { displayName: 'Llama 3.3 70B', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.00059, outputCostPer1k: 0.00079, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 3, bestFor: ['fast', 'general'] },
    'llama-3.1-8b-instant': { displayName: 'Llama 3.1 8B Instant', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.00005, outputCostPer1k: 0.00008, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 2, bestFor: ['fast'] },

    // Perplexity
    'sonar-pro': { displayName: 'Sonar Pro', maxTokens: 8192, maxContextTokens: 200000, inputCostPer1k: 0.003, outputCostPer1k: 0.015, supportsStreaming: true, supportsTools: false, supportsVision: false, supportsThinking: false, supportsParallelTools: false, toolCallQuality: 1, bestFor: ['search', 'research'] },
    'sonar': { displayName: 'Sonar', maxTokens: 8192, maxContextTokens: 128000, inputCostPer1k: 0.001, outputCostPer1k: 0.001, supportsStreaming: true, supportsTools: false, supportsVision: false, supportsThinking: false, supportsParallelTools: false, toolCallQuality: 1, bestFor: ['search', 'fast'] },

    // Cohere
    'command-a-reasoning': { displayName: 'Command A Reasoning', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.0025, outputCostPer1k: 0.01, supportsStreaming: true, supportsTools: true, supportsVision: true, supportsThinking: true, supportsParallelTools: false, toolCallQuality: 4, bestFor: ['reasoning', 'rag'] },
    'command-a': { displayName: 'Command A', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.0025, outputCostPer1k: 0.01, supportsStreaming: true, supportsTools: true, supportsVision: true, supportsThinking: false, supportsParallelTools: false, toolCallQuality: 4, bestFor: ['general', 'rag'] },

    // Together AI
    'meta-llama/Llama-3.3-70B-Instruct': { displayName: 'Llama 3.3 70B (Together)', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.00054, outputCostPer1k: 0.00054, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: false, supportsParallelTools: true, toolCallQuality: 3, bestFor: ['general', 'code'] },
    'Qwen/QwQ-32B': { displayName: 'QwQ 32B (Together)', maxTokens: 8192, maxContextTokens: 131072, inputCostPer1k: 0.0003, outputCostPer1k: 0.0003, supportsStreaming: true, supportsTools: true, supportsVision: false, supportsThinking: true, supportsParallelTools: false, toolCallQuality: 3, bestFor: ['reasoning', 'code'] },
};

/**
 * Lookup a model spec by name. Tries exact match first, then prefix match
 * for versioned model names (e.g., "gpt-4o-2024-11-20" matches "gpt-4o").
 */
export function lookupModelSpec(modelName: string): KnownModelSpec | null {
    if (MODEL_REGISTRY[modelName]) return MODEL_REGISTRY[modelName];
    const keys = Object.keys(MODEL_REGISTRY).sort((a, b) => b.length - a.length);
    for (const key of keys) {
        if (modelName.startsWith(`${key}-`)) return MODEL_REGISTRY[key];
    }
    return null;
}

/** Resolve how much context AeroAgent may safely budget without confusing output tokens for context. */
export function resolveModelContext(model: Partial<AIModel> | null | undefined): ModelContextResolution {
    if (model?.maxContextTokens && model.maxContextTokens > 0) {
        const source = getModelCapabilitySource(model);
        return {
            tokens: model.maxContextTokens,
            source: 'model',
            verified: source !== 'unknown',
        };
    }

    const spec = model?.name ? lookupModelSpec(model.name) : null;
    if (spec) {
        return { tokens: spec.maxContextTokens, source: 'registry', verified: true };
    }

    return {
        tokens: UNKNOWN_MODEL_CONTEXT_BUDGET,
        source: 'conservative',
        verified: false,
    };
}

/** Derive status for old saved settings that predate capabilitySource. */
export function getModelCapabilitySource(model: Partial<AIModel>): ModelCapabilitySource {
    if (model.capabilitySource) return model.capabilitySource;
    if (model.name && lookupModelSpec(model.name)) return 'registry';
    if (model.maxContextTokens && model.maxContextTokens > 0) return 'user';
    return 'unknown';
}

/** Keep the first-party Responses transport behind verified model metadata and an explicit flag. */
export function shouldUseOpenAIResponses(
    providerType: AIProviderType,
    model: Partial<AIModel> | null | undefined,
    enabled: boolean,
): boolean {
    return enabled
        && providerType === 'openai'
        && model?.nativeCapabilities?.responses === true;
}

/**
 * Apply registry defaults to a partial model definition. User-provided values
 * take precedence over registry defaults.
 */
export function applyRegistryDefaults(model: Partial<AIModel> & { name: string }): Partial<AIModel> {
    const spec = lookupModelSpec(model.name);
    if (!spec) return model;
    return {
        displayName: spec.displayName,
        maxTokens: spec.maxTokens,
        maxContextTokens: spec.maxContextTokens,
        inputCostPer1k: spec.inputCostPer1k,
        outputCostPer1k: spec.outputCostPer1k,
        supportsStreaming: spec.supportsStreaming,
        supportsTools: spec.supportsTools,
        supportsVision: spec.supportsVision,
        supportsThinking: spec.supportsThinking,
        supportsParallelTools: spec.supportsParallelTools,
        toolCallQuality: spec.toolCallQuality,
        bestFor: [...spec.bestFor],
        nativeCapabilities: spec.nativeCapabilities
            ? {
                ...spec.nativeCapabilities,
                reasoningEfforts: spec.nativeCapabilities.reasoningEfforts
                    ? [...spec.nativeCapabilities.reasoningEfforts]
                    : undefined,
            }
            : undefined,
        lifecycleStatus: spec.lifecycleStatus || 'active',
        ...model,
        capabilitySource: 'registry',
        capabilitiesVerifiedAt: spec.metadataReviewedAt,
        capabilitiesSourceUrl: spec.metadataSource,
    };
}

/**
 * Defaults for a model returned by a provider's list endpoint. Discovery proves
 * availability, not vision/tool/context support, so unknown models start from a
 * conservative and visibly unverified profile.
 */
export function applyDiscoveredModelDefaults(
    model: Partial<AIModel> & { name: string },
): Partial<AIModel> {
    if (lookupModelSpec(model.name)) return applyRegistryDefaults(model);

    return {
        ...model,
        maxTokens: model.maxTokens && model.maxTokens > 0 ? model.maxTokens : 4096,
        maxContextTokens: model.maxContextTokens && model.maxContextTokens > 0
            ? model.maxContextTokens
            : undefined,
        supportsStreaming: true,
        supportsTools: false,
        supportsVision: false,
        supportsThinking: false,
        supportsParallelTools: false,
        capabilitySource: 'unknown',
        capabilitiesVerifiedAt: undefined,
        capabilitiesSourceUrl: undefined,
        nativeCapabilities: undefined,
    };
}
