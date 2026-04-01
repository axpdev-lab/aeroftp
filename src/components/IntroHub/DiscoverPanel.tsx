import * as React from 'react';
import { useState, useMemo, useCallback } from 'react';
import {
    Server, Database, Globe, Cloud, Code,
    ChevronRight, Search, X, Zap, ShieldCheck, Lock, Info,
} from 'lucide-react';
import { ProviderType } from '../../types';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { ProtocolIcon, ProtocolBadge } from '../ProtocolSelector';
import { useTranslation } from '../../i18n';
import { buildDiscoverCategories, DiscoverCategory, DiscoverItem } from './discoverData';
import { CatalogCategoryId } from '../../types/catalog';

const CATEGORY_ICONS: Record<string, React.ReactNode> = {
    Server: <Server size={16} />,
    Database: <Database size={16} />,
    Globe: <Globe size={16} />,
    Cloud: <Cloud size={16} />,
    Code: <Code size={16} />,
};

const CATEGORY_COLORS: Record<CatalogCategoryId, string> = {
    'protocols': 'text-blue-400',
    'object-storage': 'text-orange-400',
    'webdav': 'text-emerald-400',
    'cloud-storage': 'text-sky-400',
    'developer': 'text-gray-400',
};

interface DiscoverPanelProps {
    onSelectProvider: (protocol: ProviderType, providerId?: string) => void;
}

function ServiceCard({ item, onSelect }: { item: DiscoverItem; onSelect: () => void }) {
    const LogoComponent = PROVIDER_LOGOS[item.providerId || item.id] || PROVIDER_LOGOS[item.protocol];

    return (
        <button
            onClick={onSelect}
            className="group flex items-center gap-3 p-3 bg-white dark:bg-gray-800 hover:bg-gray-50 dark:hover:bg-gray-750 border border-gray-100 dark:border-gray-700/50 hover:border-blue-200 dark:hover:border-blue-500/30 rounded-lg transition-all text-left shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] hover:shadow-[0_4px_12px_rgba(0,0,0,0.1)] dark:hover:shadow-[0_4px_12px_rgba(0,0,0,0.4)]"
        >
            {/* Logo - no container box, just the icon like original ProtocolSelector */}
            <div className="w-7 h-7 shrink-0 flex items-center justify-center">
                {LogoComponent ? (
                    <LogoComponent size={22} />
                ) : (
                    <ProtocolIcon protocol={item.protocol} size={22} />
                )}
            </div>

            {/* Info */}
            <div className="flex-1 min-w-0">
                <div className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                    {item.name}
                </div>
                {item.description && (
                    <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                        {item.description}
                    </div>
                )}
            </div>

            {/* Badge - same colors as original ProtocolSelector */}
            {item.badge && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded inline-flex items-center gap-0.5 font-medium shrink-0 ${
                    ['TLS', 'SSH', 'E2E'].includes(item.badge)
                        ? 'bg-green-100 text-green-700 dark:bg-green-900 dark:text-green-300'
                    : item.badge === 'OAuth'
                        ? 'bg-purple-100 text-purple-700 dark:bg-purple-900 dark:text-purple-300'
                    : item.badge === 'API'
                        ? 'bg-blue-100 text-blue-700 dark:bg-blue-900 dark:text-blue-300'
                    : item.badge === 'HMAC'
                        ? 'bg-teal-100 text-teal-700 dark:bg-teal-900 dark:text-teal-300'
                    : item.badge === 'API OCS' || item.badge === 'OCS'
                        ? 'bg-sky-100 text-sky-700 dark:bg-sky-900 dark:text-sky-300'
                    : item.badge === 'Swift'
                        ? 'bg-violet-100 text-violet-700 dark:bg-violet-900 dark:text-violet-300'
                    : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                }`}>
                    {['TLS', 'SSH', 'HMAC', 'E2E'].includes(item.badge) && <ShieldCheck size={10} />}
                    {item.badge === 'OAuth' && <Lock size={10} />}
                    {(item.badge === 'API OCS' || item.badge === 'OCS') && <Globe size={10} />}
                    {item.badge}
                </span>
            )}

            {/* Arrow */}
            <ChevronRight size={14} className="text-gray-400 dark:text-gray-500 opacity-0 group-hover:opacity-100 transition-opacity shrink-0" />
        </button>
    );
}

export function DiscoverPanel({ onSelectProvider }: DiscoverPanelProps) {
    const t = useTranslation();
    const categories = useMemo(() => buildDiscoverCategories(), []);
    const [activeCategory, setActiveCategory] = useState<CatalogCategoryId>(() => {
        const saved = localStorage.getItem('aeroftp-discover-category');
        return (saved as CatalogCategoryId) || 'protocols';
    });

    const activeItems = useMemo(() => {
        const cat = categories.find(c => c.id === activeCategory);
        return cat?.items ?? [];
    }, [categories, activeCategory]);

    const handleSelect = useCallback((item: DiscoverItem) => {
        onSelectProvider(item.protocol, item.providerId);
    }, [onSelectProvider]);

    return (
        <div className="h-full flex gap-4">
            {/* Category Sidebar */}
            <div className="w-52 shrink-0 space-y-1">
                <div className="text-[11px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500 px-3 py-2">
                    {t('introHub.discoverTitle')}
                </div>
                {categories.map((cat) => (
                    <button
                        key={cat.id}
                        onClick={() => { setActiveCategory(cat.id); localStorage.setItem('aeroftp-discover-category', cat.id); }}
                        className={`flex items-center gap-2.5 w-full px-3 py-2 rounded-lg text-sm transition-colors ${
                            activeCategory === cat.id
                                ? 'bg-blue-50 dark:bg-blue-900/25 text-blue-600 dark:text-blue-400 font-medium'
                                : 'text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                        }`}
                    >
                        <span className={CATEGORY_COLORS[cat.id]}>
                            {CATEGORY_ICONS[cat.icon]}
                        </span>
                        <span className="flex-1 text-left truncate">{t(cat.labelKey)}</span>
                        <span className="text-[10px] text-gray-400 dark:text-gray-500 tabular-nums">
                            {cat.count}
                        </span>
                    </button>
                ))}
            </div>

            {/* Main content */}
            <div className="flex-1 min-w-0 flex flex-col">
                {/* Category header */}
                <div className="flex items-center gap-2 mb-3">
                    <span className={CATEGORY_COLORS[activeCategory]}>
                        {CATEGORY_ICONS[categories.find(c => c.id === activeCategory)?.icon || 'Server']}
                    </span>
                    <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
                        {t(categories.find(c => c.id === activeCategory)?.labelKey || '')}
                    </h3>
                    <span className="text-xs text-gray-400 dark:text-gray-500">
                        {activeItems.length} {activeItems.length === 1 ? 'service' : 'services'}
                    </span>
                </div>

                {/* Info banner for each category */}
                {(() => {
                    const infoKeyMap: Record<CatalogCategoryId, string> = {
                        'protocols': 'protocols',
                        'object-storage': 's3',
                        'webdav': 'webdav',
                        'cloud-storage': 'cloud',
                        'developer': 'developer',
                    };
                    const key = infoKeyMap[activeCategory];
                    if (!key) return null;
                    return (
                        <div className="flex items-start gap-2.5 p-3 mb-3 bg-blue-50/50 dark:bg-blue-900/10 border border-blue-200/50 dark:border-blue-800/30 rounded-lg text-xs text-gray-600 dark:text-gray-400">
                            <Info size={14} className="text-blue-500 shrink-0 mt-0.5" />
                            <p>
                                {t(`protocol.${key}InfoLine1`)}{' '}
                                {t(`protocol.${key}InfoLine2`)}
                            </p>
                        </div>
                    );
                })()}

                {/* Provider grid */}
                <div className="flex-1 overflow-y-auto">
                    {activeItems.length === 0 ? (
                        <div className="text-center py-12 text-gray-400 dark:text-gray-500">
                            <Search size={32} className="mx-auto mb-3 opacity-50" />
                            <p className="text-sm">{t('introHub.noResults')}</p>
                        </div>
                    ) : (
                        <div className="grid grid-cols-1 md:grid-cols-2 lg:grid-cols-3 gap-2">
                            {activeItems.map((item) => (
                                <ServiceCard
                                    key={item.id}
                                    item={item}
                                    onSelect={() => handleSelect(item)}
                                />
                            ))}
                        </div>
                    )}
                </div>

                {/* Bottom info */}
                <div className="mt-4 pt-3 border-t border-gray-200 dark:border-gray-700 flex items-center gap-2">
                    <Zap size={13} className="text-yellow-500" />
                    <span className="text-[11px] text-gray-400 dark:text-gray-500">
                        {t('introHub.discoverHint')}
                    </span>
                </div>
            </div>
        </div>
    );
}
