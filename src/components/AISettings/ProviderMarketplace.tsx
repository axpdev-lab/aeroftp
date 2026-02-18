import React, { useState, useMemo, useEffect, useRef } from 'react';
import { X, Search, Check, Zap, Star } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { AIProviderType, PROVIDER_PRESETS } from '../../types/ai';
import {
    MARKETPLACE_PROVIDERS, MARKETPLACE_CATEGORIES, FEATURE_LABELS,
    MarketplaceCategory, MarketplaceProvider
} from './providerMarketplace';
import {
    GeminiIcon, OpenAIIcon, AnthropicIcon, XAIIcon, OpenRouterIcon,
    OllamaIcon, KimiIcon, QwenIcon, DeepSeekIcon, MistralIcon,
    GroqIcon, PerplexityIcon, CohereIcon, TogetherIcon
} from '../DevTools/AIIcons';

interface ProviderMarketplaceProps {
    isOpen: boolean;
    onClose: () => void;
    onAddProvider: (preset: typeof PROVIDER_PRESETS[0]) => void;
    addedProviderTypes: Set<AIProviderType>;
}

const PROVIDER_ICON_MAP: Record<AIProviderType, React.FC<{ size?: number; className?: string }>> = {
    openai: OpenAIIcon,
    anthropic: AnthropicIcon,
    google: GeminiIcon,
    xai: XAIIcon,
    openrouter: OpenRouterIcon,
    ollama: OllamaIcon,
    kimi: KimiIcon,
    qwen: QwenIcon,
    deepseek: DeepSeekIcon,
    mistral: MistralIcon,
    groq: GroqIcon,
    perplexity: PerplexityIcon,
    cohere: CohereIcon,
    together: TogetherIcon,
    custom: OpenAIIcon,
};

const PRICING_COLORS: Record<string, string> = {
    free: 'text-green-400',
    freemium: 'text-blue-400',
    paid: 'text-yellow-400',
};

function ProviderCard({
    provider,
    isAdded,
    onAdd,
    t,
}: {
    provider: MarketplaceProvider;
    isAdded: boolean;
    onAdd: () => void;
    t: (key: string) => string;
}) {
    const Icon = PROVIDER_ICON_MAP[provider.type];

    return (
        <div className={`
            relative rounded-lg border p-4 transition-all duration-200
            ${isAdded
                ? 'border-gray-300 dark:border-gray-600 opacity-60'
                : 'border-gray-200 dark:border-gray-700 hover:border-purple-500 hover:shadow-lg cursor-pointer'
            }
            bg-gray-50 dark:bg-gray-800
        `}>
            <div className="flex items-start gap-3">
                <div className="flex-shrink-0 w-8 h-8 rounded-lg bg-gray-200 dark:bg-gray-700 flex items-center justify-center">
                    <Icon size={18} />
                </div>
                <div className="flex-1 min-w-0">
                    <div className="flex items-center gap-2">
                        <h3 className="font-semibold text-sm text-gray-900 dark:text-gray-100 truncate">
                            {provider.name}
                        </h3>
                        <span className={`text-[10px] font-medium ${PRICING_COLORS[provider.pricingTier]}`}>
                            {provider.pricingTier === 'free' ? 'FREE' : provider.pricingTier === 'freemium' ? 'FREEMIUM' : 'PAID'}
                        </span>
                    </div>
                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5 line-clamp-2">
                        {provider.description}
                    </p>
                    {provider.highlight && (
                        <div className="flex items-center gap-1 mt-1.5">
                            <Star size={10} className="text-yellow-500" />
                            <span className="text-[10px] font-medium text-yellow-500">
                                {provider.highlight}
                            </span>
                        </div>
                    )}
                    <div className="flex flex-wrap gap-1 mt-2">
                        {provider.features.map(f => (
                            <span
                                key={f}
                                className="px-1.5 py-0.5 text-[9px] rounded bg-gray-200 dark:bg-gray-700 text-gray-500"
                            >
                                {t(FEATURE_LABELS[f].i18nKey)}
                            </span>
                        ))}
                    </div>
                </div>
                <div className="flex-shrink-0">
                    {isAdded ? (
                        <span className="flex items-center gap-1 px-2 py-1 rounded text-[10px] font-medium text-green-400 bg-green-400/10">
                            <Check size={10} />
                            {t('ai.marketplace.added')}
                        </span>
                    ) : (
                        <button
                            onClick={(e) => { e.stopPropagation(); onAdd(); }}
                            className="flex items-center gap-1 px-3 py-1.5 rounded-lg text-xs font-medium
                                bg-purple-600 text-white hover:bg-purple-700 transition-colors"
                        >
                            <Zap size={10} />
                            {t('ai.marketplace.add')}
                        </button>
                    )}
                </div>
            </div>
        </div>
    );
}

export const ProviderMarketplace: React.FC<ProviderMarketplaceProps> = ({
    isOpen,
    onClose,
    onAddProvider,
    addedProviderTypes,
}) => {
    const t = useTranslation();
    const [search, setSearch] = useState('');
    const [selectedCategory, setSelectedCategory] = useState<MarketplaceCategory | 'all'>('all');
    const searchInputRef = useRef<HTMLInputElement>(null);

    // Reset state on open (FE-015)
    useEffect(() => {
        if (isOpen) {
            setSearch('');
            setSelectedCategory('all');
            // Focus search input on open (FE-013)
            setTimeout(() => searchInputRef.current?.focus(), 50);
        }
    }, [isOpen]);

    // Escape key handler (FE-001)
    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('keydown', handler);
        return () => document.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const filtered = useMemo(() => {
        let result = MARKETPLACE_PROVIDERS;
        if (selectedCategory !== 'all') {
            result = result.filter(p => p.category === selectedCategory);
        }
        if (search.trim()) {
            const q = search.toLowerCase();
            result = result.filter(p =>
                p.name.toLowerCase().includes(q) ||
                p.description.toLowerCase().includes(q) ||
                (p.highlight && p.highlight.toLowerCase().includes(q))
            );
        }
        return result;
    }, [search, selectedCategory]);

    const handleAdd = (provider: MarketplaceProvider) => {
        const preset = PROVIDER_PRESETS.find(p => p.type === provider.type);
        if (preset) {
            onAddProvider(preset);
            onClose();
        }
    };

    if (!isOpen) return null;

    return (
        <div
            className="fixed inset-0 z-[60] flex items-center justify-center p-4"
            onClick={onClose}
            role="dialog"
            aria-modal="true"
            aria-labelledby="marketplace-title"
        >
            <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />
            <div
                className="relative w-full max-w-3xl max-h-[80vh] rounded-xl overflow-hidden
                    bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 shadow-2xl flex flex-col"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div>
                        <h2 id="marketplace-title" className="text-lg font-bold text-gray-900 dark:text-gray-100">
                            {t('ai.marketplace.title')}
                        </h2>
                        <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                            {t('ai.marketplace.subtitle')}
                        </p>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 dark:text-gray-400 transition-colors"
                    >
                        <X size={18} />
                    </button>
                </div>

                {/* Search + Category filters */}
                <div className="px-5 py-3 border-b border-gray-200 dark:border-gray-700 space-y-3">
                    <div className="relative">
                        <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400 dark:text-gray-500" />
                        <input
                            ref={searchInputRef}
                            type="text"
                            value={search}
                            onChange={e => setSearch(e.target.value)}
                            placeholder={t('ai.marketplace.search')}
                            className="w-full pl-9 pr-3 py-2 rounded-lg text-sm
                                bg-gray-50 dark:bg-gray-800 text-gray-900 dark:text-gray-100
                                border border-gray-200 dark:border-gray-700
                                placeholder:text-gray-400 dark:placeholder:text-gray-500
                                focus:outline-none focus:border-purple-500"
                        />
                    </div>
                    <div className="flex flex-wrap gap-1.5">
                        {MARKETPLACE_CATEGORIES.map(cat => (
                            <button
                                key={cat.id}
                                onClick={() => setSelectedCategory(cat.id as MarketplaceCategory | 'all')}
                                className={`px-3 py-1 rounded-full text-xs font-medium transition-colors ${
                                    selectedCategory === cat.id
                                        ? 'bg-purple-600 text-white'
                                        : 'bg-gray-100 dark:bg-gray-700 text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100'
                                }`}
                            >
                                {t(cat.i18nKey)}
                            </button>
                        ))}
                    </div>
                </div>

                {/* Provider grid */}
                <div className="flex-1 overflow-y-auto p-5">
                    {filtered.length === 0 ? (
                        <div className="text-center py-12 text-gray-500 text-sm">
                            {t('ai.marketplace.noResults')}
                        </div>
                    ) : (
                        <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                            {filtered.map(provider => (
                                <ProviderCard
                                    key={provider.type}
                                    provider={provider}
                                    isAdded={addedProviderTypes.has(provider.type)}
                                    onAdd={() => handleAdd(provider)}
                                    t={t}
                                />
                            ))}
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
};
