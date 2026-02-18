import { AIProviderType } from '../../types/ai';

export type MarketplaceCategory = 'flagship' | 'fast' | 'specialized' | 'local' | 'chinese' | 'gateway';

export type ProviderFeature = 'streaming' | 'tools' | 'vision' | 'thinking' | 'search' | 'rag' | 'local';

export interface MarketplaceProvider {
    type: AIProviderType;
    name: string;
    description: string;
    category: MarketplaceCategory;
    features: ProviderFeature[];
    pricingTier: 'free' | 'freemium' | 'paid';
    highlight?: string;
}

export const MARKETPLACE_CATEGORIES: { id: MarketplaceCategory | 'all'; i18nKey: string; fallback: string }[] = [
    { id: 'all', i18nKey: 'ai.marketplace.all', fallback: 'All' },
    { id: 'flagship', i18nKey: 'ai.marketplace.flagship', fallback: 'Flagship' },
    { id: 'fast', i18nKey: 'ai.marketplace.fast', fallback: 'Fast Inference' },
    { id: 'specialized', i18nKey: 'ai.marketplace.specialized', fallback: 'Specialized' },
    { id: 'local', i18nKey: 'ai.marketplace.local', fallback: 'Local' },
    { id: 'chinese', i18nKey: 'ai.marketplace.chinese', fallback: 'Chinese' },
    { id: 'gateway', i18nKey: 'ai.marketplace.gateway', fallback: 'Gateway' },
];

export const MARKETPLACE_PROVIDERS: MarketplaceProvider[] = [
    // Flagship
    {
        type: 'openai',
        name: 'OpenAI',
        description: 'GPT-4o, o3 reasoning models. Industry standard.',
        category: 'flagship',
        features: ['streaming', 'tools', 'vision', 'thinking'],
        pricingTier: 'paid',
        highlight: 'Most capable models',
    },
    {
        type: 'anthropic',
        name: 'Anthropic',
        description: 'Claude Opus, Sonnet, Haiku. Exceptional reasoning.',
        category: 'flagship',
        features: ['streaming', 'tools', 'vision', 'thinking'],
        pricingTier: 'paid',
        highlight: 'Best for code & analysis',
    },
    {
        type: 'google',
        name: 'Google Gemini',
        description: 'Gemini 2.0 Flash & Pro. Code execution built-in.',
        category: 'flagship',
        features: ['streaming', 'tools', 'vision', 'thinking'],
        pricingTier: 'freemium',
        highlight: 'Free tier available',
    },
    // Fast Inference
    {
        type: 'groq',
        name: 'Groq',
        description: 'LPU-powered ultra-fast inference. Llama, Mixtral, Gemma.',
        category: 'fast',
        features: ['streaming', 'tools'],
        pricingTier: 'freemium',
        highlight: 'Fastest inference',
    },
    {
        type: 'together',
        name: 'Together AI',
        description: 'Open-source models at scale. Llama, Qwen, DeepSeek.',
        category: 'fast',
        features: ['streaming', 'tools'],
        pricingTier: 'paid',
        highlight: 'Cheapest open models',
    },
    {
        type: 'xai',
        name: 'xAI (Grok)',
        description: 'Grok 3 & Grok 3 Mini. Fast and capable.',
        category: 'fast',
        features: ['streaming', 'tools', 'vision', 'thinking'],
        pricingTier: 'paid',
    },
    // Specialized
    {
        type: 'mistral',
        name: 'Mistral',
        description: 'Mistral Large, Codestral, Pixtral. European AI leader.',
        category: 'specialized',
        features: ['streaming', 'tools', 'vision'],
        pricingTier: 'paid',
        highlight: 'Best code specialist',
    },
    {
        type: 'perplexity',
        name: 'Perplexity',
        description: 'Sonar models with built-in web search and citations.',
        category: 'specialized',
        features: ['streaming', 'search'],
        pricingTier: 'paid',
        highlight: 'Built-in web search',
    },
    {
        type: 'cohere',
        name: 'Cohere',
        description: 'Command R+ with built-in RAG and retrieval capabilities.',
        category: 'specialized',
        features: ['streaming', 'tools', 'rag'],
        pricingTier: 'freemium',
        highlight: 'Built-in RAG',
    },
    // Local
    {
        type: 'ollama',
        name: 'Ollama',
        description: 'Run open-source models locally. Full privacy, no API key.',
        category: 'local',
        features: ['streaming', 'tools', 'vision', 'local'],
        pricingTier: 'free',
        highlight: '100% private & free',
    },
    // Chinese
    {
        type: 'kimi',
        name: 'Kimi (Moonshot)',
        description: 'Moonshot v1 with 128K context. Web search support.',
        category: 'chinese',
        features: ['streaming', 'tools', 'search'],
        pricingTier: 'paid',
        highlight: '128K context window',
    },
    {
        type: 'qwen',
        name: 'Qwen (Alibaba)',
        description: 'Qwen Max, Plus, Turbo. Multilingual and vision capable.',
        category: 'chinese',
        features: ['streaming', 'tools', 'vision', 'thinking'],
        pricingTier: 'freemium',
        highlight: '1M context (Turbo)',
    },
    {
        type: 'deepseek',
        name: 'DeepSeek',
        description: 'DeepSeek V3 and R1 reasoning. Exceptional at coding.',
        category: 'chinese',
        features: ['streaming', 'tools', 'thinking'],
        pricingTier: 'paid',
        highlight: 'Best value reasoning',
    },
    // Gateway
    {
        type: 'openrouter',
        name: 'OpenRouter',
        description: 'Access 200+ models through a single API. Pay-per-token.',
        category: 'gateway',
        features: ['streaming', 'tools', 'vision', 'thinking'],
        pricingTier: 'paid',
        highlight: '200+ models',
    },
    {
        type: 'custom',
        name: 'Custom',
        description: 'Connect any OpenAI-compatible API endpoint.',
        category: 'gateway',
        features: ['streaming', 'tools'],
        pricingTier: 'free',
        highlight: 'Any endpoint',
    },
];

export const FEATURE_LABELS: Record<ProviderFeature, { i18nKey: string; fallback: string }> = {
    streaming: { i18nKey: 'ai.marketplace.streaming', fallback: 'Streaming' },
    tools: { i18nKey: 'ai.marketplace.tools', fallback: 'Tools' },
    vision: { i18nKey: 'ai.marketplace.vision', fallback: 'Vision' },
    thinking: { i18nKey: 'ai.marketplace.thinking', fallback: 'Thinking' },
    search: { i18nKey: 'ai.marketplace.searchFeature', fallback: 'Web Search' },
    rag: { i18nKey: 'ai.marketplace.rag', fallback: 'RAG' },
    local: { i18nKey: 'ai.marketplace.localFeature', fallback: 'Local' },
};
