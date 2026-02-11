import React, { useState, useEffect, useCallback } from 'react';
import { X, RefreshCw, Package, CheckCircle, AlertTriangle, ArrowUpCircle, Loader2, Copy } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';

interface DependencyInfo {
    name: string;
    version: string;
    category: string;
}

interface DependencyWithLatest extends DependencyInfo {
    latestVersion?: string;
    status: 'checking' | 'up_to_date' | 'update_available' | 'major_update' | 'error';
}

interface DependenciesPanelProps {
    isVisible: boolean;
    onClose: () => void;
}

const CATEGORIES_ORDER = ['Core', 'Protocols', 'Security', 'Archives', 'Plugins'];

const compareSemver = (current: string, latest: string): 'up_to_date' | 'update_available' | 'major_update' => {
    // Strip pre-release suffixes (e.g. "4.0.0-beta.3" -> "4.0.0")
    const parse = (v: string) => v.split('-')[0].split('.').map(Number);
    const c = parse(current);
    const l = parse(latest);
    if (c[0] < l[0]) return 'major_update';
    if (c[0] === l[0] && (c[1] < l[1] || (c[1] === l[1] && (c[2] || 0) < (l[2] || 0)))) return 'update_available';
    return 'up_to_date';
};

const StatusBadge: React.FC<{ status: DependencyWithLatest['status'] }> = ({ status }) => {
    switch (status) {
        case 'checking':
            return <Loader2 size={14} className="animate-spin text-gray-400" />;
        case 'up_to_date':
            return <CheckCircle size={14} className="text-green-500" />;
        case 'update_available':
            return <ArrowUpCircle size={14} className="text-yellow-500" />;
        case 'major_update':
            return <AlertTriangle size={14} className="text-red-500" />;
        case 'error':
            return <X size={14} className="text-gray-400" />;
    }
};

const DependenciesPanel: React.FC<DependenciesPanelProps> = ({ isVisible, onClose }) => {
    const t = useTranslation();
    const [deps, setDeps] = useState<DependencyWithLatest[]>([]);
    const [loading, setLoading] = useState(true);
    const [checking, setChecking] = useState(false);
    const [stats, setStats] = useState({ total: 0, upToDate: 0, updates: 0, major: 0 });
    const [copied, setCopied] = useState(false);

    const copyResults = useCallback(() => {
        const statusLabel = (s: DependencyWithLatest['status']) =>
            s === 'up_to_date' ? 'OK' : s === 'update_available' ? 'UPDATE' : s === 'major_update' ? 'MAJOR' : s === 'checking' ? '...' : 'ERR';
        const lines = CATEGORIES_ORDER.flatMap(cat => {
            const catDeps = deps.filter(d => d.category === cat);
            if (catDeps.length === 0) return [];
            return [
                `\n## ${cat}`,
                ...catDeps.map(d => `${d.name.padEnd(30)} ${d.version.padEnd(10)} ${(d.latestVersion || '—').padEnd(10)} ${statusLabel(d.status)}`)
            ];
        });
        const text = `${t('dependencies.copyTitle')}\n${'='.repeat(65)}` + lines.join('\n');
        navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    }, [deps]);

    // Load dependencies from backend
    useEffect(() => {
        if (!isVisible) return;
        (async () => {
            try {
                const data: DependencyInfo[] = await invoke('get_dependencies');
                setDeps(data.map(d => ({ ...d, status: 'checking' as const })));
                setLoading(false);
            } catch (e) {
                console.error('Failed to load dependencies:', e);
                setLoading(false);
            }
        })();
    }, [isVisible]);

    // Check latest versions via Rust backend (avoids CORS issues)
    const checkVersions = useCallback(async () => {
        if (deps.length === 0) return;
        setChecking(true);

        // Reset all to checking
        setDeps(prev => prev.map(d => ({ ...d, status: 'checking' as const, latestVersion: undefined })));

        try {
            const crateNames = deps.map(d => d.name);
            const results: { name: string; latest_version: string | null; error: string | null }[] =
                await invoke('check_crate_versions', { crateNames });

            setDeps(prev => prev.map(dep => {
                const result = results.find(r => r.name === dep.name);
                if (result?.latest_version) {
                    return {
                        ...dep,
                        latestVersion: result.latest_version,
                        status: compareSemver(dep.version, result.latest_version),
                    };
                }
                return { ...dep, status: 'error' as const };
            }));
        } catch (e) {
            console.error('Failed to check crate versions:', e);
            setDeps(prev => prev.map(d => ({ ...d, status: 'error' as const })));
        }

        setChecking(false);
    }, [deps]);

    // Auto-check on first load
    useEffect(() => {
        if (!loading && deps.length > 0 && deps.every(d => d.status === 'checking')) {
            checkVersions();
        }
    }, [loading, deps.length]); // eslint-disable-line react-hooks/exhaustive-deps

    // Update stats
    useEffect(() => {
        setStats({
            total: deps.length,
            upToDate: deps.filter(d => d.status === 'up_to_date').length,
            updates: deps.filter(d => d.status === 'update_available').length,
            major: deps.filter(d => d.status === 'major_update').length,
        });
    }, [deps]);

    if (!isVisible) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
            <div className="bg-white dark:bg-gray-900 rounded-xl shadow-2xl w-[720px] max-h-[80vh] flex flex-col border border-gray-200 dark:border-gray-700">
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <Package size={18} className="text-blue-500" />
                        <h2 className="text-base font-semibold">{t('dependencies.title')}</h2>
                        <span className="text-xs text-gray-500">({stats.total} {t('dependencies.crates')})</span>
                    </div>
                    <div className="flex items-center gap-2">
                        {/* Stats badges */}
                        {stats.upToDate > 0 && (
                            <span className="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full bg-green-100 dark:bg-green-900/30 text-green-700 dark:text-green-400">
                                <CheckCircle size={11} /> {stats.upToDate}
                            </span>
                        )}
                        {stats.updates > 0 && (
                            <span className="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full bg-yellow-100 dark:bg-yellow-900/30 text-yellow-700 dark:text-yellow-400">
                                <ArrowUpCircle size={11} /> {stats.updates}
                            </span>
                        )}
                        {stats.major > 0 && (
                            <span className="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full bg-red-100 dark:bg-red-900/30 text-red-700 dark:text-red-400">
                                <AlertTriangle size={11} /> {stats.major}
                            </span>
                        )}
                        <button
                            onClick={copyResults}
                            className="flex items-center gap-1 text-xs px-3 py-1 rounded-lg bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors"
                            title={t('dependencies.copy')}
                        >
                            <Copy size={12} />
                            {copied ? t('dependencies.copied') : t('dependencies.copy')}
                        </button>
                        <button
                            onClick={checkVersions}
                            disabled={checking}
                            className="flex items-center gap-1 text-xs px-3 py-1 rounded-lg bg-blue-500 text-white hover:bg-blue-600 disabled:opacity-50 transition-colors"
                        >
                            <RefreshCw size={12} className={checking ? 'animate-spin' : ''} />
                            {checking ? t('dependencies.checking') : t('dependencies.checkUpdates')}
                        </button>
                        <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">
                            <X size={16} />
                        </button>
                    </div>
                </div>

                {/* Content */}
                <div className="overflow-y-auto flex-1 px-5 py-3">
                    {loading ? (
                        <div className="flex items-center justify-center py-12">
                            <Loader2 size={24} className="animate-spin text-gray-400" />
                        </div>
                    ) : (
                        CATEGORIES_ORDER.map(category => {
                            const categoryDeps = deps.filter(d => d.category === category);
                            if (categoryDeps.length === 0) return null;
                            return (
                                <div key={category} className="mb-4">
                                    <h3 className="text-xs font-semibold text-gray-500 dark:text-gray-400 uppercase tracking-wider mb-2">
                                        {category}
                                    </h3>
                                    <div className="bg-gray-50 dark:bg-gray-800/50 rounded-lg overflow-hidden">
                                        <table className="w-full text-sm">
                                            <thead>
                                                <tr className="text-xs text-gray-500 border-b border-gray-200 dark:border-gray-700">
                                                    <th className="text-left py-1.5 px-3 font-medium">{t('dependencies.crate')}</th>
                                                    <th className="text-left py-1.5 px-3 font-medium">{t('dependencies.current')}</th>
                                                    <th className="text-left py-1.5 px-3 font-medium">{t('dependencies.latest')}</th>
                                                    <th className="text-center py-1.5 px-3 font-medium w-16">{t('dependencies.status')}</th>
                                                </tr>
                                            </thead>
                                            <tbody>
                                                {categoryDeps.map(dep => (
                                                    <tr key={dep.name} className="border-b border-gray-100 dark:border-gray-700/50 last:border-0">
                                                        <td className="py-1.5 px-3 font-mono text-xs font-medium text-gray-800 dark:text-gray-200">
                                                            {dep.name}
                                                        </td>
                                                        <td className="py-1.5 px-3 font-mono text-xs text-gray-600 dark:text-gray-400">
                                                            {dep.version}
                                                        </td>
                                                        <td className={`py-1.5 px-3 font-mono text-xs ${
                                                            dep.status === 'update_available' ? 'text-yellow-600 dark:text-yellow-400 font-semibold' :
                                                            dep.status === 'major_update' ? 'text-red-600 dark:text-red-400 font-semibold' :
                                                            'text-gray-500'
                                                        }`}>
                                                            {dep.latestVersion || '—'}
                                                        </td>
                                                        <td className="py-1.5 px-3 text-center">
                                                            <StatusBadge status={dep.status} />
                                                        </td>
                                                    </tr>
                                                ))}
                                            </tbody>
                                        </table>
                                    </div>
                                </div>
                            );
                        })
                    )}
                </div>

                {/* Footer */}
                <div className="px-5 py-2 border-t border-gray-200 dark:border-gray-700 text-xs text-gray-500">
                    {t('dependencies.footer')}
                </div>
            </div>
        </div>
    );
};

export default DependenciesPanel;
