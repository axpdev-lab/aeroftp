import * as React from 'react';
import { useState, useEffect, useRef, useMemo, useCallback } from 'react';
import { createPortal } from 'react-dom';
import { Search, Server, Compass, Plus, ArrowRight, Play, X } from 'lucide-react';
import { ServerProfile, ProviderType } from '../../types';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { ProtocolIcon, ProtocolBadge } from '../ProtocolSelector';
import { useTranslation } from '../../i18n';
import { buildDiscoverCategories, DiscoverItem, DISCOVER_DESC_KEYS } from './discoverData';

interface CommandPaletteProps {
    isOpen: boolean;
    onClose: () => void;
    servers: ServerProfile[];
    onConnectServer: (server: ServerProfile) => void;
    onSelectProvider: (protocol: ProviderType, providerId?: string) => void;
    onQuickConnect: () => void;
    onNavigateTab: (tab: 'my-servers' | 'discover') => void;
}

interface PaletteResult {
    id: string;
    label: string;
    sublabel?: string;
    icon: React.ReactNode;
    badge?: React.ReactNode;
    category: 'server' | 'provider' | 'action';
    onSelect: () => void;
}

function getServerIcon(server: ServerProfile): React.ReactNode {
    if (server.customIconUrl) return <img src={server.customIconUrl} className="w-5 h-5 rounded object-cover" alt="" />;
    if (server.faviconUrl) return <img src={server.faviconUrl} className="w-5 h-5 rounded object-cover" alt="" />;
    const pid = server.providerId || server.protocol || 'ftp';
    const Logo = PROVIDER_LOGOS[pid];
    if (Logo) return <Logo size={16} />;
    return <ProtocolIcon protocol={server.protocol || 'ftp'} size={16} />;
}

function getProviderIcon(item: DiscoverItem): React.ReactNode {
    const Logo = PROVIDER_LOGOS[item.providerId || item.id] || PROVIDER_LOGOS[item.protocol];
    if (Logo) return <Logo size={16} />;
    return <ProtocolIcon protocol={item.protocol} size={16} />;
}

export function CommandPalette({
    isOpen,
    onClose,
    servers,
    onConnectServer,
    onSelectProvider,
    onQuickConnect,
    onNavigateTab,
}: CommandPaletteProps) {
    const t = useTranslation();
    const [query, setQuery] = useState('');
    const [selectedIndex, setSelectedIndex] = useState(0);
    const inputRef = useRef<HTMLInputElement>(null);
    const listRef = useRef<HTMLDivElement>(null);

    // All discover items (flattened)
    const allProviders = useMemo(() => {
        const cats = buildDiscoverCategories();
        const items: DiscoverItem[] = [];
        for (const cat of cats) items.push(...cat.items);
        return items;
    }, []);

    // Build results
    const results = useMemo((): PaletteResult[] => {
        const q = query.toLowerCase().trim();
        const out: PaletteResult[] = [];

        // Servers
        const matchingServers = q
            ? servers.filter(s =>
                (s.name || '').toLowerCase().includes(q) ||
                (s.host || '').toLowerCase().includes(q) ||
                (s.protocol || '').toLowerCase().includes(q)
            )
            : [...servers].sort((a, b) => (b.lastConnected || '').localeCompare(a.lastConnected || '')).slice(0, 5);

        for (const s of matchingServers.slice(0, 8)) {
            out.push({
                id: `srv-${s.id}`,
                label: s.name,
                sublabel: s.host,
                icon: getServerIcon(s),
                badge: <ProtocolBadge protocol={s.protocol || 'ftp'} />,
                category: 'server',
                onSelect: () => onConnectServer(s),
            });
        }

        // Providers
        const matchingProviders = q
            ? allProviders.filter(p =>
                p.name.toLowerCase().includes(q) ||
                (p.description || '').toLowerCase().includes(q) ||
                (p.providerId || '').toLowerCase().includes(q) ||
                p.protocol.toLowerCase().includes(q)
            )
            : [];

        for (const p of matchingProviders.slice(0, 6)) {
            out.push({
                id: `prov-${p.id}`,
                label: p.name,
                sublabel: DISCOVER_DESC_KEYS[p.id] ? t(DISCOVER_DESC_KEYS[p.id]) : p.description,
                icon: getProviderIcon(p),
                badge: p.badge ? (
                    <span className={`text-[9px] font-bold px-1.5 py-0.5 rounded ${
                        p.badge === 'TLS' || p.badge === 'SSH' ? 'bg-green-500/15 text-green-400' :
                        p.badge === 'OAuth' ? 'bg-purple-500/15 text-purple-400' :
                        p.badge === 'HMAC' ? 'bg-orange-500/15 text-orange-400' :
                        p.badge === 'E2E' ? 'bg-rose-500/15 text-rose-400' :
                        'bg-amber-500/15 text-amber-400'
                    }`}>{p.badge}</span>
                ) : null,
                category: 'provider',
                onSelect: () => onSelectProvider(p.protocol, p.providerId),
            });
        }

        // Actions (always show when no query, or when query matches)
        const actions: PaletteResult[] = [
            {
                id: 'action-quick',
                label: t('introHub.quickConnect'),
                sublabel: t('connection.quickConnect'),
                icon: <Plus size={16} className="text-blue-400" />,
                category: 'action',
                onSelect: onQuickConnect,
            },
            {
                id: 'action-servers',
                label: t('introHub.tab.myServers'),
                icon: <Server size={16} className="text-gray-400" />,
                category: 'action',
                onSelect: () => onNavigateTab('my-servers'),
            },
            {
                id: 'action-discover',
                label: t('introHub.tab.discover'),
                icon: <Compass size={16} className="text-gray-400" />,
                category: 'action',
                onSelect: () => onNavigateTab('discover'),
            },
        ];

        if (q) {
            const filtered = actions.filter(a => a.label.toLowerCase().includes(q));
            out.push(...filtered);
        } else {
            out.push(...actions);
        }

        return out;
    }, [query, servers, allProviders, t, onConnectServer, onSelectProvider, onQuickConnect, onNavigateTab]);

    // Reset state on open
    useEffect(() => {
        if (isOpen) {
            setQuery('');
            setSelectedIndex(0);
            setTimeout(() => inputRef.current?.focus(), 50);
        }
    }, [isOpen]);

    // Clamp selected index
    useEffect(() => {
        if (selectedIndex >= results.length) setSelectedIndex(Math.max(0, results.length - 1));
    }, [results.length, selectedIndex]);

    // Scroll selected into view
    useEffect(() => {
        const el = listRef.current?.querySelector(`[data-index="${selectedIndex}"]`);
        el?.scrollIntoView({ block: 'nearest' });
    }, [selectedIndex]);

    const handleKeyDown = useCallback((e: React.KeyboardEvent) => {
        switch (e.key) {
            case 'ArrowDown':
                e.preventDefault();
                setSelectedIndex(i => Math.min(i + 1, results.length - 1));
                break;
            case 'ArrowUp':
                e.preventDefault();
                setSelectedIndex(i => Math.max(i - 1, 0));
                break;
            case 'Enter':
                e.preventDefault();
                if (results[selectedIndex]) {
                    results[selectedIndex].onSelect();
                    onClose();
                }
                break;
            case 'Escape':
                e.preventDefault();
                onClose();
                break;
            case 'Tab':
                e.preventDefault();
                // Jump to next category
                const currentCat = results[selectedIndex]?.category;
                const nextIdx = results.findIndex((r, i) => i > selectedIndex && r.category !== currentCat);
                if (nextIdx >= 0) setSelectedIndex(nextIdx);
                break;
        }
    }, [results, selectedIndex, onClose]);

    if (!isOpen) return null;

    // Group results by category for headers
    const grouped: { category: string; label: string; items: (PaletteResult & { globalIdx: number })[] }[] = [];
    let lastCat = '';
    for (let i = 0; i < results.length; i++) {
        const r = results[i];
        if (r.category !== lastCat) {
            const label = r.category === 'server' ? t('connection.savedServers')
                : r.category === 'provider' ? t('introHub.tab.discover')
                : t('common.actions');
            grouped.push({ category: r.category, label, items: [] });
            lastCat = r.category;
        }
        grouped[grouped.length - 1].items.push({ ...r, globalIdx: i });
    }

    return createPortal(
        <div
            className="fixed inset-0 z-[100] bg-black/50 backdrop-blur-sm flex items-start justify-center pt-[15vh]"
            onClick={onClose}
        >
            <div
                className="w-full max-w-xl bg-white dark:bg-gray-800 rounded-2xl shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden"
                onClick={(e) => e.stopPropagation()}
            >
                {/* Search input */}
                <div className="relative border-b border-gray-200 dark:border-gray-700">
                    <Search size={18} className="absolute left-5 top-1/2 -translate-y-1/2 text-gray-400" />
                    <input
                        ref={inputRef}
                        type="text"
                        value={query}
                        onChange={(e) => { setQuery(e.target.value); setSelectedIndex(0); }}
                        onKeyDown={handleKeyDown}
                        placeholder={t('introHub.searchServices')}
                        className="w-full px-5 py-4 pl-12 bg-transparent text-base text-gray-900 dark:text-white placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none"
                    />
                    {query && (
                        <button
                            onClick={() => { setQuery(''); setSelectedIndex(0); }}
                            className="absolute right-4 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                        >
                            <X size={16} />
                        </button>
                    )}
                </div>

                {/* Results */}
                <div ref={listRef} className="max-h-[400px] overflow-y-auto py-2">
                    {results.length === 0 ? (
                        <div className="px-5 py-8 text-center text-gray-400 dark:text-gray-500 text-sm">
                            {t('introHub.noResults')}
                        </div>
                    ) : (
                        grouped.map((group) => (
                            <div key={group.category}>
                                <div className="px-5 py-1.5 text-[10px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500">
                                    {group.label}
                                </div>
                                {group.items.map((item) => (
                                    <button
                                        key={item.id}
                                        data-index={item.globalIdx}
                                        onClick={() => { item.onSelect(); onClose(); }}
                                        onMouseEnter={() => setSelectedIndex(item.globalIdx)}
                                        className={`w-full flex items-center gap-3 px-5 py-2.5 text-left transition-colors ${
                                            selectedIndex === item.globalIdx
                                                ? 'bg-blue-50 dark:bg-blue-900/20 border-l-2 border-blue-500'
                                                : 'border-l-2 border-transparent hover:bg-gray-50 dark:hover:bg-gray-700/30'
                                        }`}
                                    >
                                        <div className="w-7 h-7 shrink-0 rounded-lg bg-gray-100 dark:bg-gray-700 flex items-center justify-center">
                                            {item.icon}
                                        </div>
                                        <div className="flex-1 min-w-0">
                                            <div className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">
                                                {item.label}
                                            </div>
                                            {item.sublabel && (
                                                <div className="text-[11px] text-gray-500 dark:text-gray-400 truncate">
                                                    {item.sublabel}
                                                </div>
                                            )}
                                        </div>
                                        {item.badge}
                                        {item.category === 'server' && (
                                            <Play size={12} className="text-green-500 shrink-0" fill="currentColor" />
                                        )}
                                        {item.category === 'provider' && (
                                            <ArrowRight size={12} className="text-gray-400 shrink-0" />
                                        )}
                                    </button>
                                ))}
                            </div>
                        ))
                    )}
                </div>

                {/* Footer */}
                <div className="px-5 py-2.5 border-t border-gray-200 dark:border-gray-700 flex items-center gap-4 text-[11px] text-gray-400 dark:text-gray-500">
                    <span className="flex items-center gap-1">
                        <kbd className="px-1 py-0.5 bg-gray-100 dark:bg-gray-700 rounded text-[10px] font-mono">↑↓</kbd>
                        navigate
                    </span>
                    <span className="flex items-center gap-1">
                        <kbd className="px-1 py-0.5 bg-gray-100 dark:bg-gray-700 rounded text-[10px] font-mono">↵</kbd>
                        select
                    </span>
                    <span className="flex items-center gap-1">
                        <kbd className="px-1 py-0.5 bg-gray-100 dark:bg-gray-700 rounded text-[10px] font-mono">Tab</kbd>
                        next section
                    </span>
                    <span className="flex items-center gap-1">
                        <kbd className="px-1 py-0.5 bg-gray-100 dark:bg-gray-700 rounded text-[10px] font-mono">Esc</kbd>
                        close
                    </span>
                </div>
            </div>
        </div>,
        document.body
    );
}
