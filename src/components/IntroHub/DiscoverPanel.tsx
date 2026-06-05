import * as React from 'react';
import { useState, useMemo, useCallback, useEffect, useRef } from 'react';
import {
    Server, Database, Globe, Cloud, Code, Camera, Layers,
    ChevronRight, Search, X, Zap, Activity, ShieldCheck, Lock, Info, LayoutGrid, List as ListIcon,
} from 'lucide-react';
import { ProviderType } from '../../types';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { ProtocolIcon, ProtocolBadge, isSecureBadge, isCipherStrengthBadge } from '../ProtocolSelector';
import { useTranslation } from '../../i18n';
import { buildDiscoverCategories, DiscoverCategory, DiscoverItem, DISCOVER_DESC_KEYS } from './discoverData';
import { CatalogCategoryId, getCatalogCategory } from '../../types/catalog';
import { useProviderHealth, type HealthStatus } from '../../hooks/useProviderHealth';
import { useIntroHubIconSize } from '../../hooks/useIntroHubIconSize';
import { CatalogTable } from './CatalogTable';
import { PROVIDER_CATALOG } from '../providerCatalog';

/** All category sidebar entries share these keys; 'all' is a virtual category. */
type DiscoverCategoryId = CatalogCategoryId | 'all';
type DiscoverViewMode = 'grid' | 'list';

const VIEW_MODE_KEY = 'aeroftp-discover-view';
const CATEGORY_KEY = 'aeroftp-discover-category';

// Reuse the My Servers chunked-sequential health pattern for the large
// All / list views: probe in small waves so opening the view does not fan
// out 50+ simultaneous outbound probes to third-party endpoints.
const HEALTH_SCAN_CHUNK_SIZE = 12;
const HEALTH_SCAN_CHUNK_DELAY_MS = 180;
const wait = (ms: number) => new Promise<void>(resolve => setTimeout(resolve, ms));

/** Generic / custom server entry points shown below the list view. */
const CUSTOM_PROFILES: { labelKey: string; protocol: ProviderType; providerId?: string }[] = [
    { labelKey: 'introHub.list.customFtp', protocol: 'ftp' },
    { labelKey: 'introHub.list.customSftp', protocol: 'sftp' },
    { labelKey: 'introHub.list.customS3', protocol: 's3', providerId: 'custom-s3' },
    { labelKey: 'introHub.list.customWebdav', protocol: 'webdav', providerId: 'custom-webdav' },
    { labelKey: 'introHub.list.customAzure', protocol: 'azure' },
];

const CATEGORY_ICONS: Record<string, React.ReactNode> = {
    Server: <Server size={16} />,
    Database: <Database size={16} />,
    Globe: <Globe size={16} />,
    Cloud: <Cloud size={16} />,
    Camera: <Camera size={16} />,
    Code: <Code size={16} />,
};

const CATEGORY_COLORS: Record<CatalogCategoryId, string> = {
    'protocols': 'text-blue-400',
    'object-storage': 'text-orange-400',
    'webdav': 'text-emerald-400',
    'cloud-storage': 'text-sky-400',
    'media-services': 'text-pink-400',
    'developer': 'text-gray-400',
};

interface DiscoverPanelProps {
    onSelectProvider: (protocol: ProviderType, providerId?: string, demo?: { server: string; port: number; username: string; password: string }) => void;
}

function ServiceCard({
    item,
    onSelect,
    healthStatus,
    iconSize,
}: {
    item: DiscoverItem;
    onSelect: () => void;
    healthStatus?: HealthStatus;
    iconSize: number;
}) {
    const t = useTranslation();
    const LogoComponent = PROVIDER_LOGOS[item.providerId || item.id] || PROVIDER_LOGOS[item.protocol];
    const iconFrameSize = Math.max(28, Math.min(40, iconSize + 6));
    // Mirrors the overlay-dot pattern from ServerCard so reachability is
    // visible without expanding the card. Hidden on `unknown` to avoid a
    // placeholder dot on services without a `healthCheckUrl`.
    const showHealthDot = healthStatus && healthStatus !== 'unknown';
    const healthTitle = showHealthDot ? t(`introHub.health.${healthStatus}`) : undefined;

    return (
        <button
            onClick={onSelect}
            className="group flex items-center gap-3 p-3 bg-white dark:bg-gray-800 hover:bg-gray-50 dark:hover:bg-gray-750 border border-gray-100 dark:border-gray-700/50 hover:border-blue-200 dark:hover:border-blue-500/30 rounded-lg transition-all text-left shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] hover:shadow-[0_4px_12px_rgba(0,0,0,0.1)] dark:hover:shadow-[0_4px_12px_rgba(0,0,0,0.4)]"
        >
            {/* Logo - no container box, just the icon like original ProtocolSelector */}
            <div
                className="relative shrink-0 flex items-center justify-center"
                style={{ width: iconFrameSize, height: iconFrameSize }}
            >
                {LogoComponent ? (
                    <LogoComponent size={iconSize} />
                ) : (
                    <ProtocolIcon protocol={item.protocol} size={iconSize} />
                )}
                {showHealthDot && (
                    <span
                        className={`absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2 ring-white dark:ring-gray-800 pointer-events-none ${
                            healthStatus === 'up' ? 'bg-green-500'
                            : healthStatus === 'slow' ? 'bg-amber-500'
                            : healthStatus === 'down' ? 'bg-red-500'
                            : 'bg-gray-400 animate-pulse'
                        }`}
                        title={healthTitle}
                        aria-label={healthTitle}
                    />
                )}
            </div>

            {/* Info */}
            <div className="flex-1 min-w-0">
                <div className="text-sm font-semibold text-gray-900 dark:text-gray-100 truncate">
                    {item.name}
                </div>
                {item.description && (
                    <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                        {DISCOVER_DESC_KEYS[item.id] ? t(DISCOVER_DESC_KEYS[item.id]) : item.description}
                    </div>
                )}
            </div>

            {/* Badge - same colors as original ProtocolSelector */}
            {item.badge && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded inline-flex items-center gap-0.5 font-medium shrink-0 ${
                    isSecureBadge(item.badge)
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
                    : item.badge === 'DEMO'
                        ? 'bg-amber-100 text-amber-700 dark:bg-amber-900 dark:text-amber-300'
                    : 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                }`}>
                    {isCipherStrengthBadge(item.badge)
                        ? <Lock size={10} />
                        : isSecureBadge(item.badge) && <ShieldCheck size={10} />}
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
    const introHubIconSize = useIntroHubIconSize();
    const categories = useMemo(() => buildDiscoverCategories(), []);
    const [activeCategory, setActiveCategory] = useState<DiscoverCategoryId>(() => {
        const saved = localStorage.getItem(CATEGORY_KEY);
        return (saved as DiscoverCategoryId) || 'protocols';
    });
    const [viewMode, setViewMode] = useState<DiscoverViewMode>(() => {
        const saved = localStorage.getItem(VIEW_MODE_KEY);
        return saved === 'list' ? 'list' : 'grid';
    });

    // Provider health scan: per-tab eager (small categories) + chunked
    // sequential for the large All / list views (My Servers pattern).
    const { getStatus, scanItems, scanning } = useProviderHealth();
    const healthScanRunRef = useRef(0);

    const totalItemCount = useMemo(
        () => categories.reduce((sum, c) => sum + c.items.length, 0),
        [categories],
    );

    // Grid data for the active category ('all' flattens every category).
    const activeItems = useMemo(() => {
        if (activeCategory === 'all') return categories.flatMap(c => c.items);
        const cat = categories.find(c => c.id === activeCategory);
        return cat?.items ?? [];
    }, [categories, activeCategory]);

    // List data: company-centric catalog, filtered by category ('all' = full).
    const catalogCompanies = useMemo(() => {
        if (activeCategory === 'all') return PROVIDER_CATALOG;
        return PROVIDER_CATALOG.filter(c =>
            c.protocols.some(p => getCatalogCategory(p.providerId || p.protocol) === activeCategory));
    }, [activeCategory]);

    // Whether the heavy (chunked) scan path applies: list view, or grid 'All'.
    const usesChunkedScan = viewMode === 'list' || activeCategory === 'all';

    const chunkedTargets = useMemo(() => {
        if (!usesChunkedScan) return [];
        if (viewMode === 'list') {
            return catalogCompanies
                .filter(c => c.healthCheckUrl)
                .map(c => ({ id: c.logoId, url: c.healthCheckUrl! }));
        }
        return activeItems
            .filter(item => item.healthCheckUrl)
            .map(item => ({ id: item.providerId || item.id, url: item.healthCheckUrl! }));
    }, [usesChunkedScan, viewMode, catalogCompanies, activeItems]);

    // Eager per-category scan: small categories in grid view only. The All
    // and list views take the chunked path below.
    useEffect(() => {
        if (usesChunkedScan) return;
        const targets = activeItems
            .filter(item => item.healthCheckUrl)
            .map(item => ({ id: item.providerId || item.id, url: item.healthCheckUrl! }));
        if (targets.length === 0) return;
        const timer = setTimeout(() => scanItems(targets), 600);
        return () => clearTimeout(timer);
    }, [usesChunkedScan, activeItems, scanItems]);

    // Chunked-sequential scan for All / list: 12 at a time, 180ms apart,
    // cancellable via a generation counter so switching view/category aborts
    // a running scan. Cache (5 min) is reused by the hook.
    useEffect(() => {
        if (chunkedTargets.length === 0) return;
        const runId = ++healthScanRunRef.current;
        let cancelled = false;
        const timer = window.setTimeout(() => {
            void (async () => {
                for (let i = 0; i < chunkedTargets.length; i += HEALTH_SCAN_CHUNK_SIZE) {
                    if (cancelled || healthScanRunRef.current !== runId) return;
                    await scanItems(chunkedTargets.slice(i, i + HEALTH_SCAN_CHUNK_SIZE));
                    if (i + HEALTH_SCAN_CHUNK_SIZE < chunkedTargets.length) {
                        await wait(HEALTH_SCAN_CHUNK_DELAY_MS);
                    }
                }
            })();
        }, 600);
        return () => {
            cancelled = true;
            healthScanRunRef.current++;
            window.clearTimeout(timer);
        };
    }, [chunkedTargets, scanItems]);

    const handleManualCheck = useCallback(() => {
        if (usesChunkedScan) {
            const runId = ++healthScanRunRef.current;
            void (async () => {
                for (let i = 0; i < chunkedTargets.length; i += HEALTH_SCAN_CHUNK_SIZE) {
                    if (healthScanRunRef.current !== runId) return;
                    await scanItems(chunkedTargets.slice(i, i + HEALTH_SCAN_CHUNK_SIZE), true);
                    if (i + HEALTH_SCAN_CHUNK_SIZE < chunkedTargets.length) {
                        await wait(HEALTH_SCAN_CHUNK_DELAY_MS);
                    }
                }
            })();
            return;
        }
        const targets = activeItems
            .filter(item => item.healthCheckUrl)
            .map(item => ({ id: item.providerId || item.id, url: item.healthCheckUrl! }));
        scanItems(targets, true);
    }, [usesChunkedScan, chunkedTargets, activeItems, scanItems]);

    const handleSelect = useCallback((item: DiscoverItem) => {
        onSelectProvider(item.protocol, item.providerId, item.demo);
    }, [onSelectProvider]);

    const setView = useCallback((mode: DiscoverViewMode) => {
        setViewMode(mode);
        try { localStorage.setItem(VIEW_MODE_KEY, mode); } catch { /* ignore */ }
    }, []);

    const setCategory = useCallback((id: DiscoverCategoryId) => {
        setActiveCategory(id);
        try { localStorage.setItem(CATEGORY_KEY, id); } catch { /* ignore */ }
    }, []);

    const activeCatMeta = categories.find(c => c.id === activeCategory);
    const headerLabel = activeCategory === 'all'
        ? t('introHub.category.all')
        : t(activeCatMeta?.labelKey || '');

    const headerCount = viewMode === 'list' ? catalogCompanies.length : activeItems.length;

    return (
        <div className="h-full flex gap-4">
            {/* Category Sidebar: min stays at the original 13rem (~208px); on wider
                windows it grows modestly (capped at 17rem) so longer category labels
                stay readable without dominating the layout. */}
            <div className="shrink-0 space-y-1" style={{ width: 'clamp(13rem, 16vw, 17rem)' }}>
                <div className="text-[11px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500 px-3 py-2">
                    {t('introHub.discoverTitle')}
                </div>
                {/* "All" entry: one flat list of every provider (Ehud #224). */}
                <button
                    onClick={() => setCategory('all')}
                    className={`flex items-center gap-2.5 w-full px-3 py-2 rounded-lg text-sm transition-colors ${
                        activeCategory === 'all'
                            ? 'bg-blue-50 dark:bg-blue-900/25 text-blue-600 dark:text-blue-400 font-medium'
                            : 'text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                    }`}
                >
                    <span className="text-indigo-400"><Layers size={16} /></span>
                    <span className="flex-1 text-left truncate">{t('introHub.category.all')}</span>
                    <span className="text-[10px] text-gray-400 dark:text-gray-500 tabular-nums px-1.5 py-0.5 rounded-full bg-gray-100 dark:bg-gray-700/50">
                        {totalItemCount}
                    </span>
                </button>
                {categories.map((cat) => (
                    <button
                        key={cat.id}
                        onClick={() => setCategory(cat.id)}
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
                        <span className="text-[10px] text-gray-400 dark:text-gray-500 tabular-nums px-1.5 py-0.5 rounded-full bg-gray-100 dark:bg-gray-700/50">
                            {cat.count}
                        </span>
                    </button>
                ))}
            </div>

            {/* Main content */}
            <div className="flex-1 min-w-0 flex flex-col">
                {/* Category header */}
                <div className="flex items-center gap-2 mb-3">
                    <span className={activeCategory === 'all' ? 'text-indigo-400' : CATEGORY_COLORS[activeCategory]}>
                        {activeCategory === 'all' ? <Layers size={16} /> : CATEGORY_ICONS[activeCatMeta?.icon || 'Server']}
                    </span>
                    <h3 className="text-sm font-semibold text-gray-900 dark:text-gray-100">
                        {headerLabel}
                    </h3>
                    <span className="text-[10px] text-gray-400 dark:text-gray-500 tabular-nums px-1.5 py-0.5 rounded-full bg-gray-100 dark:bg-gray-700/50">
                        {headerCount}
                    </span>
                    <div className="flex-1" />
                    {/* Grid / list view toggle (persisted), mirroring My Servers. */}
                    <div className="flex items-center rounded-md border border-gray-200 dark:border-gray-600 overflow-hidden mr-1">
                        <button
                            onClick={() => setView('grid')}
                            className={`flex items-center gap-1 px-2 py-1 text-[11px] transition-colors ${
                                viewMode === 'grid'
                                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                                    : 'text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                            }`}
                            title={t('introHub.view.grid')}
                            aria-pressed={viewMode === 'grid'}
                        >
                            <LayoutGrid size={12} />
                        </button>
                        <button
                            onClick={() => setView('list')}
                            className={`flex items-center gap-1 px-2 py-1 text-[11px] transition-colors ${
                                viewMode === 'list'
                                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400'
                                    : 'text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                            }`}
                            title={t('introHub.view.list')}
                            aria-pressed={viewMode === 'list'}
                        >
                            <ListIcon size={12} />
                        </button>
                    </div>
                    <button
                        onClick={handleManualCheck}
                        disabled={scanning}
                        className={`flex items-center gap-1.5 px-2.5 py-1 rounded-md text-[11px] transition-colors ${
                            scanning
                                ? 'text-gray-400 dark:text-gray-500 cursor-wait'
                                : 'text-gray-400 dark:text-gray-500 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                        }`}
                        title={t('introHub.checkAvailability')}
                    >
                        <Activity size={11} className={scanning ? 'animate-pulse' : ''} />
                        {scanning ? t('introHub.scanning') : t('introHub.check')}
                    </button>
                </div>

                {/* Info banner per category (grid view, the 6 named categories) */}
                {viewMode === 'grid' && activeCategory !== 'all' && (() => {
                    const infoKeyMap: Record<CatalogCategoryId, string> = {
                        'protocols': 'protocols',
                        'object-storage': 's3',
                        'webdav': 'webdav',
                        'cloud-storage': 'cloud',
                        'media-services': 'media',
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

                {viewMode === 'list' ? (
                    /* Company-centric list view (issue #224) */
                    <div className="flex-1 min-h-0 flex flex-col">
                        <CatalogTable
                            companies={catalogCompanies}
                            onSelectProvider={onSelectProvider}
                            getHealth={(logoId) => getStatus(logoId).status}
                        />
                        {/* Custom / generic servers below the table */}
                        <div className="mt-3 pt-3 border-t border-gray-200 dark:border-gray-700">
                            <div className="text-[11px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500 mb-2">
                                {t('introHub.list.customProfiles')}
                            </div>
                            <div className="flex flex-wrap gap-2">
                                {CUSTOM_PROFILES.map((p) => (
                                    <button
                                        key={p.labelKey}
                                        onClick={() => onSelectProvider(p.protocol, p.providerId)}
                                        className="flex items-center gap-1.5 px-3 py-1.5 rounded-lg text-xs bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 text-gray-700 dark:text-gray-200 hover:border-blue-300 dark:hover:border-blue-500/40 hover:bg-gray-50 dark:hover:bg-gray-750 transition-colors"
                                    >
                                        <Server size={12} className="text-gray-400" />
                                        {t(p.labelKey)}
                                    </button>
                                ))}
                            </div>
                        </div>
                    </div>
                ) : (
                    <>
                        {/* Provider grid */}
                        <div className="flex-1 overflow-y-auto">
                            {activeItems.length === 0 ? (
                                <div className="text-center py-12 text-gray-400 dark:text-gray-500">
                                    <Search size={32} className="mx-auto mb-3 opacity-50" />
                                    <p className="text-sm">{t('introHub.noResults')}</p>
                                </div>
                            ) : (
                                <div
                                    className="grid gap-2"
                                    style={{ gridTemplateColumns: 'repeat(auto-fill, minmax(260px, 1fr))' }}
                                >
                                    {activeItems.map((item) => (
                                        <ServiceCard
                                            key={item.id}
                                            item={item}
                                            onSelect={() => handleSelect(item)}
                                            healthStatus={item.healthCheckUrl ? getStatus(item.providerId || item.id).status : 'unknown'}
                                            iconSize={introHubIconSize}
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
                    </>
                )}
            </div>
        </div>
    );
}
