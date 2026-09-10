// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useState, useEffect, useCallback, useMemo } from 'react';
import { X, RefreshCw, Package, CheckCircle, AlertTriangle, ArrowUpCircle, Loader2, Copy, Lock } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';

/** One direct dependency, as build.rs derived it from the manifests and Cargo.lock. */
interface DependencyInfo {
    name: string;
    version: string;
    requirement: string;
    category: string;
}

/** Classification done by the backend (dependency_index.rs), which applies Cargo's semver rules. */
type UpdateStatus = 'up_to_date' | 'compatible_update' | 'incompatible_update' | 'pinned' | 'error';

interface DependencyUpdate {
    name: string;
    version: string;
    latest: string | null;
    latest_compatible: string | null;
    status: UpdateStatus;
    error: string | null;
}

interface DependencyRow extends DependencyInfo {
    latest?: string;
    latestCompatible?: string;
    status: UpdateStatus | 'checking';
}

interface DependenciesPanelProps {
    isVisible: boolean;
    onClose: () => void;
}

// A crate can be listed twice at two versions (rand 0.8 and 0.10), so the name alone is not a key.
const rowKey = (d: { name: string; version: string }) => `${d.name}@${d.version}`;

const STATUS_LABEL: Record<DependencyRow['status'], string> = {
    checking: '...',
    up_to_date: 'OK',
    compatible_update: 'UPDATE',
    incompatible_update: 'MAJOR',
    pinned: 'PINNED',
    error: 'ERR',
};

const StatusBadge: React.FC<{ status: DependencyRow['status'] }> = ({ status }) => {
    switch (status) {
        case 'checking':
            return <Loader2 size={14} className="animate-spin text-gray-400" />;
        case 'up_to_date':
            return <CheckCircle size={14} className="text-green-500" />;
        case 'compatible_update':
            return <ArrowUpCircle size={14} className="text-yellow-500" />;
        case 'incompatible_update':
            return <AlertTriangle size={14} className="text-red-500" />;
        case 'pinned':
            return <Lock size={14} className="text-blue-500" />;
        case 'error':
            return <X size={14} className="text-gray-400" />;
    }
};

/** The version a row offers: what `cargo update` reaches when that is possible, the newest release otherwise. */
const offeredVersion = (d: DependencyRow) =>
    d.status === 'compatible_update' ? d.latestCompatible : d.latest;

const DependenciesPanel: React.FC<DependenciesPanelProps> = ({ isVisible, onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [deps, setDeps] = useState<DependencyRow[]>([]);
    const [loading, setLoading] = useState(true);
    const [checking, setChecking] = useState(false);
    const [copied, setCopied] = useState(false);

    // The backend already sorts rows by category, so the first appearance order is the display order.
    const categories = useMemo(() => [...new Set(deps.map(d => d.category))], [deps]);

    const stats = useMemo(() => ({
        total: deps.length,
        upToDate: deps.filter(d => d.status === 'up_to_date').length,
        updates: deps.filter(d => d.status === 'compatible_update').length,
        major: deps.filter(d => d.status === 'incompatible_update').length,
        pinned: deps.filter(d => d.status === 'pinned').length,
    }), [deps]);

    const copyResults = useCallback(() => {
        const lines = categories.flatMap(cat => [
            `\n## ${cat}`,
            ...deps
                .filter(d => d.category === cat)
                .map(d => `${d.name.padEnd(30)} ${d.version.padEnd(18)} ${(offeredVersion(d) || '-').padEnd(18)} ${STATUS_LABEL[d.status]}`),
        ]);
        const text = `${t('dependencies.copyTitle')}\n${'='.repeat(75)}` + lines.join('\n');
        navigator.clipboard.writeText(text);
        setCopied(true);
        setTimeout(() => setCopied(false), 2000);
    }, [deps, categories, t]);

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

    // Check the crates.io index via the Rust backend (avoids CORS issues)
    const checkVersions = useCallback(async () => {
        if (deps.length === 0) return;
        setChecking(true);
        setDeps(prev => prev.map(d => ({ ...d, status: 'checking' as const, latest: undefined, latestCompatible: undefined })));

        try {
            const results: DependencyUpdate[] = await invoke('check_dependency_updates');
            const byKey = new Map(results.map(r => [rowKey(r), r]));
            setDeps(prev => prev.map(dep => {
                const result = byKey.get(rowKey(dep));
                if (!result) return { ...dep, status: 'error' as const };
                return {
                    ...dep,
                    latest: result.latest ?? undefined,
                    latestCompatible: result.latest_compatible ?? undefined,
                    status: result.status,
                };
            }));
        } catch (e) {
            console.error('Failed to check dependency updates:', e);
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

    if (!isVisible) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/50">
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[720px] max-h-[80vh] flex flex-col border border-gray-200 dark:border-gray-700 animate-scale-in"
            >
                {/* Header */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-5 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
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
                        {stats.pinned > 0 && (
                            <span className="flex items-center gap-1 text-xs px-2 py-0.5 rounded-full bg-blue-100 dark:bg-blue-900/30 text-blue-700 dark:text-blue-400">
                                <Lock size={11} /> {stats.pinned}
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
                        categories.map(category => {
                            const categoryDeps = deps.filter(d => d.category === category);
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
                                                {categoryDeps.map(dep => {
                                                    const offered = offeredVersion(dep);
                                                    return (
                                                        <tr key={rowKey(dep)} className="border-b border-gray-100 dark:border-gray-700/50 last:border-0">
                                                            <td className="py-1.5 px-3 font-mono text-xs font-medium text-gray-800 dark:text-gray-200">
                                                                {dep.name}
                                                            </td>
                                                            <td className="py-1.5 px-3 font-mono text-xs text-gray-600 dark:text-gray-400" title={dep.requirement}>
                                                                {dep.version}
                                                            </td>
                                                            <td className={`py-1.5 px-3 font-mono text-xs ${
                                                                dep.status === 'compatible_update' ? 'text-yellow-600 dark:text-yellow-400 font-semibold' :
                                                                dep.status === 'incompatible_update' ? 'text-red-600 dark:text-red-400 font-semibold' :
                                                                'text-gray-500'
                                                            }`}>
                                                                {offered || '-'}
                                                                {dep.latest && offered && dep.latest !== offered && (
                                                                    <span className="font-normal text-gray-400"> ({dep.latest})</span>
                                                                )}
                                                            </td>
                                                            <td className="py-1.5 px-3 text-center">
                                                                <StatusBadge status={dep.status} />
                                                            </td>
                                                        </tr>
                                                    );
                                                })}
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
