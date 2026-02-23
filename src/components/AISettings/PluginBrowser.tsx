import React, { useState, useEffect, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { X, Search, Download, Star, ExternalLink, RefreshCw, Package, Puzzle } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface RegistryFile {
    path: string;
    url: string;
    sha256: string;
}

interface RegistryEntry {
    id: string;
    name: string;
    version: string;
    author: string;
    description: string;
    category: string;
    downloads: number;
    stars: number;
    repo_url: string;
    manifest_url: string;
    files: RegistryFile[];
}

interface PluginBrowserProps {
    isOpen: boolean;
    onClose: () => void;
    installedPluginIds: Set<string>;
    onInstalled: () => void;
}

const CATEGORY_COLORS: Record<string, string> = {
    'file-management': 'bg-blue-500/20 text-blue-400',
    'ai-tools': 'bg-purple-500/20 text-purple-400',
    'automation': 'bg-green-500/20 text-green-400',
    'integration': 'bg-orange-500/20 text-orange-400',
};

type BrowserTab = 'browse' | 'installed';

export const PluginBrowser: React.FC<PluginBrowserProps> = ({
    isOpen,
    onClose,
    installedPluginIds,
    onInstalled,
}) => {
    const t = useTranslation();
    const [activeTab, setActiveTab] = useState<BrowserTab>('browse');
    const [registry, setRegistry] = useState<RegistryEntry[]>([]);
    const [loading, setLoading] = useState(false);
    const [installing, setInstalling] = useState<string | null>(null);
    const [search, setSearch] = useState('');
    const [error, setError] = useState<string | null>(null);
    const searchRef = useRef<HTMLInputElement>(null);

    // Fetch registry on open
    useEffect(() => {
        if (!isOpen) return;
        setSearch('');
        setActiveTab('browse');
        setError(null);
        fetchRegistry();
        setTimeout(() => searchRef.current?.focus(), 50);
    }, [isOpen]);

    // Escape handler
    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('keydown', handler);
        return () => document.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const fetchRegistry = async () => {
        setLoading(true);
        try {
            const entries = await invoke<RegistryEntry[]>('fetch_plugin_registry');
            setRegistry(entries);
            setError(null);
        } catch (err) {
            setError(String(err));
            setRegistry([]);
        } finally {
            setLoading(false);
        }
    };

    const installPlugin = async (pluginId: string) => {
        setInstalling(pluginId);
        try {
            await invoke('install_plugin_from_registry', { pluginId });
            onInstalled();
        } catch (err) {
            setError(String(err));
        } finally {
            setInstalling(null);
        }
    };

    const filtered = useMemo(() => {
        if (!search.trim()) return registry;
        const q = search.toLowerCase();
        return registry.filter(
            p =>
                p.name.toLowerCase().includes(q) ||
                p.description.toLowerCase().includes(q) ||
                p.author.toLowerCase().includes(q) ||
                p.category.toLowerCase().includes(q)
        );
    }, [registry, search]);

    if (!isOpen) return null;

    return (
        <div
            className="fixed inset-0 z-[60] flex items-center justify-center p-4"
            onClick={onClose}
            role="dialog"
            aria-modal="true"
        >
            <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" />
            <div
                className="relative w-full max-w-3xl max-h-[80vh] rounded-xl overflow-hidden
                    bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 shadow-2xl flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3">
                        <Puzzle size={20} className="text-purple-500" />
                        <div>
                            <h2 className="text-lg font-bold text-gray-900 dark:text-gray-100">
                                {t('plugins.browser.title')}
                            </h2>
                            <p className="text-xs text-gray-500 dark:text-gray-400">
                                {t('plugins.browser.subtitle')}
                            </p>
                        </div>
                    </div>
                    <div className="flex items-center gap-2">
                        <button
                            onClick={fetchRegistry}
                            className="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 transition-colors"
                            title={t('common.refresh')}
                        >
                            <RefreshCw size={14} className={loading ? 'animate-spin' : ''} />
                        </button>
                        <button
                            onClick={onClose}
                            className="p-1.5 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-500 transition-colors"
                            title={t('common.close')}
                        >
                            <X size={18} />
                        </button>
                    </div>
                </div>

                {/* Tabs + Search */}
                <div className="px-5 py-3 border-b border-gray-200 dark:border-gray-700 space-y-3">
                    <div className="flex gap-2">
                        {(['browse', 'installed'] as BrowserTab[]).map(tab => (
                            <button
                                key={tab}
                                onClick={() => setActiveTab(tab)}
                                className={`px-3 py-1.5 rounded-lg text-xs font-medium transition-colors ${
                                    activeTab === tab
                                        ? 'bg-purple-600 text-white'
                                        : 'bg-gray-100 dark:bg-gray-800 text-gray-500 hover:text-gray-900 dark:hover:text-gray-100'
                                }`}
                            >
                                {t(`plugins.browser.tab.${tab}`)}
                            </button>
                        ))}
                    </div>
                    {activeTab === 'browse' && (
                        <div className="relative">
                            <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
                            <input
                                ref={searchRef}
                                type="text"
                                value={search}
                                onChange={e => setSearch(e.target.value)}
                                placeholder={t('plugins.browser.search')}
                                className="w-full pl-9 pr-3 py-2 rounded-lg text-sm
                                    bg-gray-50 dark:bg-gray-800 text-gray-900 dark:text-gray-100
                                    border border-gray-200 dark:border-gray-700
                                    placeholder:text-gray-400 focus:outline-none focus:border-purple-500"
                            />
                        </div>
                    )}
                </div>

                {/* Content */}
                <div className="flex-1 overflow-y-auto p-5">
                    {error && (
                        <div className="mb-4 p-3 rounded-lg bg-red-50 dark:bg-red-900/20 text-red-600 dark:text-red-400 text-xs">
                            {error}
                        </div>
                    )}

                    {loading ? (
                        <div className="text-center py-12 text-gray-500 text-sm">
                            <RefreshCw size={24} className="animate-spin mx-auto mb-2" />
                            {t('plugins.browser.loading')}
                        </div>
                    ) : activeTab === 'browse' ? (
                        filtered.length === 0 ? (
                            <div className="text-center py-12">
                                <Package size={48} className="mx-auto mb-3 text-gray-300 dark:text-gray-600" />
                                <p className="text-sm text-gray-500">{t('plugins.browser.empty')}</p>
                            </div>
                        ) : (
                            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                                {filtered.map(entry => {
                                    const isInstalled = installedPluginIds.has(entry.id);
                                    const isInstalling = installing === entry.id;
                                    return (
                                        <div
                                            key={entry.id}
                                            className={`rounded-lg border p-4 transition-all ${
                                                isInstalled
                                                    ? 'border-gray-300 dark:border-gray-600 opacity-60'
                                                    : 'border-gray-200 dark:border-gray-700 hover:border-purple-500 hover:shadow-lg'
                                            } bg-gray-50 dark:bg-gray-800`}
                                        >
                                            <div className="flex items-start justify-between gap-2">
                                                <div className="min-w-0">
                                                    <div className="flex items-center gap-2">
                                                        <h3 className="font-semibold text-sm text-gray-900 dark:text-gray-100 truncate">
                                                            {entry.name}
                                                        </h3>
                                                        <span className="text-[10px] text-gray-400">v{entry.version}</span>
                                                    </div>
                                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                        {entry.author}
                                                    </p>
                                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-1 line-clamp-2">
                                                        {entry.description}
                                                    </p>
                                                    <div className="flex items-center gap-3 mt-2">
                                                        <span className={`px-1.5 py-0.5 text-[9px] rounded font-medium ${CATEGORY_COLORS[entry.category] || 'bg-gray-500/20 text-gray-400'}`}>
                                                            {entry.category}
                                                        </span>
                                                        {entry.stars > 0 && (
                                                            <span className="flex items-center gap-0.5 text-[10px] text-yellow-500">
                                                                <Star size={10} /> {entry.stars}
                                                            </span>
                                                        )}
                                                        {entry.repo_url && (
                                                            <a
                                                                href={entry.repo_url}
                                                                target="_blank"
                                                                rel="noopener noreferrer"
                                                                className="text-[10px] text-gray-400 hover:text-purple-400 transition-colors"
                                                                onClick={e => e.stopPropagation()}
                                                            >
                                                                <ExternalLink size={10} />
                                                            </a>
                                                        )}
                                                    </div>
                                                </div>
                                                <div className="shrink-0">
                                                    {isInstalled ? (
                                                        <span className="px-2 py-1 text-[10px] font-medium text-green-400 bg-green-400/10 rounded">
                                                            {t('plugins.browser.installed')}
                                                        </span>
                                                    ) : (
                                                        <button
                                                            onClick={() => installPlugin(entry.id)}
                                                            disabled={isInstalling}
                                                            className="flex items-center gap-1 px-3 py-1.5 rounded-lg text-xs font-medium
                                                                bg-purple-600 text-white hover:bg-purple-700 transition-colors
                                                                disabled:opacity-50 disabled:cursor-not-allowed"
                                                        >
                                                            <Download size={10} className={isInstalling ? 'animate-bounce' : ''} />
                                                            {isInstalling ? t('plugins.browser.installing') : t('plugins.browser.install')}
                                                        </button>
                                                    )}
                                                </div>
                                            </div>
                                        </div>
                                    );
                                })}
                            </div>
                        )
                    ) : (
                        /* Installed tab — shows a message to manage from Plugins tab */
                        <div className="text-center py-12">
                            <Puzzle size={48} className="mx-auto mb-3 text-gray-300 dark:text-gray-600" />
                            <p className="text-sm text-gray-500">{t('plugins.browser.manageInSettings')}</p>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
};
