import * as React from 'react';
import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useTranslation } from '../i18n';
import { ArrowUpDown, Folder, X, Loader2, ArrowUp, ArrowDown, Minus, AlertTriangle, Check } from 'lucide-react';
import { formatSize } from '../utils/formatters';

interface VaultSyncDialogProps {
    vaultPath: string;
    password: string;
    onClose: () => void;
    onSynced?: () => void;
}

interface VaultSyncConflict {
    name: string;
    vault_modified: string;
    local_modified: string;
    vault_size: number;
    local_size: number;
}

interface VaultSyncComparison {
    vault_only: string[];
    local_only: string[];
    conflicts: VaultSyncConflict[];
    unchanged: number;
}

interface SyncAction {
    name: string;
    action: 'to_vault' | 'to_local' | 'skip';
}

interface VaultSyncResult {
    to_vault: number;
    to_local: number;
    skipped: number;
    errors: string[];
}

type SyncStep = 'select_dir' | 'comparing' | 'review' | 'applying' | 'done';

const VaultSyncDialog: React.FC<VaultSyncDialogProps> = ({ vaultPath, password, onClose, onSynced }) => {
    const t = useTranslation();
    const [step, setStep] = useState<SyncStep>('select_dir');
    const [localDir, setLocalDir] = useState('');
    const [comparison, setComparison] = useState<VaultSyncComparison | null>(null);
    const [actions, setActions] = useState<Map<string, 'to_vault' | 'to_local' | 'skip'>>(new Map());
    const [result, setResult] = useState<VaultSyncResult | null>(null);
    const [error, setError] = useState<string | null>(null);

    const handleSelectDir = useCallback(async () => {
        const selected = await open({ directory: true, multiple: false });
        if (selected && typeof selected === 'string') {
            setLocalDir(selected);
        }
    }, []);

    const handleCompare = useCallback(async () => {
        if (!localDir) return;
        setStep('comparing');
        setError(null);
        try {
            const cmp = await invoke<VaultSyncComparison>('vault_v2_sync_compare', {
                vaultPath, password, localDir,
            });
            setComparison(cmp);

            // Set default actions
            const defaultActions = new Map<string, 'to_vault' | 'to_local' | 'skip'>();
            cmp.vault_only.forEach(name => defaultActions.set(name, 'to_local'));
            cmp.local_only.forEach(name => defaultActions.set(name, 'to_vault'));
            cmp.conflicts.forEach(c => defaultActions.set(c.name, 'skip'));
            setActions(defaultActions);
            setStep('review');
        } catch (e) {
            setError(String(e));
            setStep('select_dir');
        }
    }, [localDir, vaultPath, password]);

    const handleApply = useCallback(async () => {
        if (!comparison) return;
        setStep('applying');
        setError(null);
        try {
            const syncActions: SyncAction[] = [];
            actions.forEach((action, name) => syncActions.push({ name, action }));

            const res = await invoke<VaultSyncResult>('vault_v2_sync_apply', {
                vaultPath, password, localDir, actions: syncActions,
            });
            setResult(res);
            setStep('done');
            if (onSynced) onSynced();
        } catch (e) {
            setError(String(e));
            setStep('review');
        }
    }, [comparison, actions, vaultPath, password, localDir, onSynced]);

    const setAction = useCallback((name: string, action: 'to_vault' | 'to_local' | 'skip') => {
        setActions(prev => {
            const next = new Map(prev);
            next.set(name, action);
            return next;
        });
    }, []);

    const setBatchAction = useCallback((action: 'to_vault' | 'to_local' | 'skip') => {
        setActions(prev => {
            const next = new Map(prev);
            next.forEach((_, key) => next.set(key, action));
            return next;
        });
    }, []);

    const setKeepNewer = useCallback(() => {
        if (!comparison) return;
        setActions(prev => {
            const next = new Map(prev);
            comparison.conflicts.forEach(c => {
                const vDate = new Date(c.vault_modified).getTime();
                const lDate = new Date(c.local_modified).getTime();
                next.set(c.name, vDate >= lDate ? 'to_local' : 'to_vault');
            });
            return next;
        });
    }, [comparison]);

    const totalActions = comparison
        ? comparison.vault_only.length + comparison.local_only.length + comparison.conflicts.length
        : 0;

    const actionIcon = (action: 'to_vault' | 'to_local' | 'skip') => {
        switch (action) {
            case 'to_vault': return <ArrowUp size={12} className="text-green-400" />;
            case 'to_local': return <ArrowDown size={12} className="text-blue-400" />;
            case 'skip': return <Minus size={12} className="text-gray-400" />;
        }
    };

    const statusLabel = (type: 'vault_only' | 'local_only' | 'conflict') => {
        switch (type) {
            case 'vault_only': return <span className="text-blue-400 text-[10px] font-medium">{t('vaultSync.vaultOnly')}</span>;
            case 'local_only': return <span className="text-green-400 text-[10px] font-medium">{t('vaultSync.localOnly')}</span>;
            case 'conflict': return <span className="text-yellow-400 text-[10px] font-medium">{t('vaultSync.conflict')}</span>;
        }
    };

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" role="dialog" aria-modal="true" onClick={e => { if (e.target === e.currentTarget) onClose(); }}>
            <div className="bg-[var(--bg-primary)] border border-[var(--border-primary)] rounded-lg shadow-xl w-[640px] max-h-[80vh] flex flex-col" onClick={e => e.stopPropagation()}>
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 border-b border-[var(--border-primary)]">
                    <div className="flex items-center gap-2">
                        <ArrowUpDown size={16} className="text-blue-400" />
                        <span className="font-medium text-sm">{t('vaultSync.title')}</span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-[var(--bg-secondary)] rounded"><X size={16} /></button>
                </div>

                {/* Content */}
                <div className="flex-1 overflow-y-auto p-4">
                    {error && (
                        <div className="mb-3 p-2 bg-red-500/10 border border-red-500/30 rounded text-xs text-red-400 flex items-center gap-2">
                            <AlertTriangle size={14} /> {error}
                        </div>
                    )}

                    {/* Step: Select directory */}
                    {step === 'select_dir' && (
                        <div className="space-y-4">
                            <p className="text-xs text-[var(--text-secondary)]">{t('vaultSync.description')}</p>
                            <div className="flex items-center gap-2">
                                <button onClick={handleSelectDir} className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-blue-600 hover:bg-blue-500 text-white rounded">
                                    <Folder size={14} /> {t('vaultSync.selectDirectory')}
                                </button>
                                {localDir && <span className="text-xs text-[var(--text-secondary)] truncate flex-1">{localDir}</span>}
                            </div>
                            {localDir && (
                                <button onClick={handleCompare} className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-green-600 hover:bg-green-500 text-white rounded">
                                    <ArrowUpDown size={14} /> {t('vaultSync.compare')}
                                </button>
                            )}
                        </div>
                    )}

                    {/* Step: Comparing */}
                    {step === 'comparing' && (
                        <div className="flex items-center justify-center py-8 gap-2 text-sm text-[var(--text-secondary)]">
                            <Loader2 size={18} className="animate-spin" /> {t('vaultSync.comparing')}
                        </div>
                    )}

                    {/* Step: Review */}
                    {step === 'review' && comparison && (
                        <div className="space-y-3">
                            {/* Summary */}
                            <div className="flex items-center gap-3 text-xs text-[var(--text-secondary)]">
                                <span className="text-blue-400">{comparison.vault_only.length} {t('vaultSync.vaultOnly')}</span>
                                <span className="text-green-400">{comparison.local_only.length} {t('vaultSync.localOnly')}</span>
                                <span className="text-yellow-400">{comparison.conflicts.length} {t('vaultSync.conflicts')}</span>
                                <span className="text-gray-400">{comparison.unchanged} {t('vaultSync.unchanged')}</span>
                            </div>

                            {/* Batch actions */}
                            {totalActions > 0 && (
                                <div className="flex items-center gap-2 text-[10px]">
                                    <button onClick={() => setBatchAction('to_vault')} className="px-2 py-0.5 rounded bg-green-700/30 hover:bg-green-700/50 text-green-400">{t('vaultSync.allToVault')}</button>
                                    <button onClick={() => setBatchAction('to_local')} className="px-2 py-0.5 rounded bg-blue-700/30 hover:bg-blue-700/50 text-blue-400">{t('vaultSync.allToLocal')}</button>
                                    <button onClick={setKeepNewer} className="px-2 py-0.5 rounded bg-yellow-700/30 hover:bg-yellow-700/50 text-yellow-400">{t('vaultSync.keepNewer')}</button>
                                    <button onClick={() => setBatchAction('skip')} className="px-2 py-0.5 rounded bg-gray-700/30 hover:bg-gray-700/50 text-gray-400">{t('vaultSync.skipAll')}</button>
                                </div>
                            )}

                            {/* File list */}
                            {totalActions === 0 ? (
                                <div className="text-xs text-[var(--text-secondary)] py-4 text-center">
                                    <Check size={18} className="inline text-green-400 mr-1" /> {t('vaultSync.allInSync')}
                                </div>
                            ) : (
                                <div className="max-h-[40vh] overflow-y-auto border border-[var(--border-primary)] rounded">
                                    <table className="w-full text-xs">
                                        <thead className="sticky top-0 bg-[var(--bg-secondary)]">
                                            <tr className="text-left text-[var(--text-secondary)]">
                                                <th className="px-2 py-1.5">{t('vaultSync.file')}</th>
                                                <th className="px-2 py-1.5 w-20">{t('vaultSync.status')}</th>
                                                <th className="px-2 py-1.5 w-28 text-center">{t('vaultSync.direction')}</th>
                                                <th className="px-2 py-1.5 w-16 text-right">{t('vaultSync.size')}</th>
                                            </tr>
                                        </thead>
                                        <tbody>
                                            {comparison.vault_only.map(name => (
                                                <tr key={name} className="border-t border-[var(--border-primary)] hover:bg-[var(--bg-secondary)]">
                                                    <td className="px-2 py-1 truncate max-w-[240px]" title={name}>{name}</td>
                                                    <td className="px-2 py-1">{statusLabel('vault_only')}</td>
                                                    <td className="px-2 py-1">
                                                        <div className="flex items-center justify-center gap-1">
                                                            {actionIcon(actions.get(name) || 'to_local')}
                                                            <select value={actions.get(name) || 'to_local'} onChange={e => setAction(name, e.target.value as 'to_vault' | 'to_local' | 'skip')}
                                                                className="bg-transparent text-xs border border-[var(--border-primary)] rounded px-1 py-0.5">
                                                                <option value="to_local">{t('vaultSync.toLocal')}</option>
                                                                <option value="skip">{t('vaultSync.skip')}</option>
                                                            </select>
                                                        </div>
                                                    </td>
                                                    <td className="px-2 py-1 text-right text-[var(--text-secondary)]">{'\u2014'}</td>
                                                </tr>
                                            ))}
                                            {comparison.local_only.map(name => (
                                                <tr key={name} className="border-t border-[var(--border-primary)] hover:bg-[var(--bg-secondary)]">
                                                    <td className="px-2 py-1 truncate max-w-[240px]" title={name}>{name}</td>
                                                    <td className="px-2 py-1">{statusLabel('local_only')}</td>
                                                    <td className="px-2 py-1">
                                                        <div className="flex items-center justify-center gap-1">
                                                            {actionIcon(actions.get(name) || 'to_vault')}
                                                            <select value={actions.get(name) || 'to_vault'} onChange={e => setAction(name, e.target.value as 'to_vault' | 'to_local' | 'skip')}
                                                                className="bg-transparent text-xs border border-[var(--border-primary)] rounded px-1 py-0.5">
                                                                <option value="to_vault">{t('vaultSync.toVault')}</option>
                                                                <option value="skip">{t('vaultSync.skip')}</option>
                                                            </select>
                                                        </div>
                                                    </td>
                                                    <td className="px-2 py-1 text-right text-[var(--text-secondary)]">{'\u2014'}</td>
                                                </tr>
                                            ))}
                                            {comparison.conflicts.map(c => (
                                                <tr key={c.name} className="border-t border-[var(--border-primary)] hover:bg-[var(--bg-secondary)]">
                                                    <td className="px-2 py-1">
                                                        <div className="truncate max-w-[240px]" title={c.name}>{c.name}</div>
                                                        <div className="text-[10px] text-[var(--text-secondary)]">
                                                            V: {formatSize(c.vault_size)} | L: {formatSize(c.local_size)}
                                                        </div>
                                                    </td>
                                                    <td className="px-2 py-1">{statusLabel('conflict')}</td>
                                                    <td className="px-2 py-1">
                                                        <div className="flex items-center justify-center gap-1">
                                                            {actionIcon(actions.get(c.name) || 'skip')}
                                                            <select value={actions.get(c.name) || 'skip'} onChange={e => setAction(c.name, e.target.value as 'to_vault' | 'to_local' | 'skip')}
                                                                className="bg-transparent text-xs border border-[var(--border-primary)] rounded px-1 py-0.5">
                                                                <option value="to_vault">{t('vaultSync.toVault')}</option>
                                                                <option value="to_local">{t('vaultSync.toLocal')}</option>
                                                                <option value="skip">{t('vaultSync.skip')}</option>
                                                            </select>
                                                        </div>
                                                    </td>
                                                    <td className="px-2 py-1 text-right text-[var(--text-secondary)]">
                                                        {formatSize(Math.max(c.vault_size, c.local_size))}
                                                    </td>
                                                </tr>
                                            ))}
                                        </tbody>
                                    </table>
                                </div>
                            )}
                        </div>
                    )}

                    {/* Step: Applying */}
                    {step === 'applying' && (
                        <div className="flex items-center justify-center py-8 gap-2 text-sm text-[var(--text-secondary)]">
                            <Loader2 size={18} className="animate-spin" /> {t('vaultSync.applying')}
                        </div>
                    )}

                    {/* Step: Done */}
                    {step === 'done' && result && (
                        <div className="space-y-3">
                            <div className="flex items-center gap-2 text-green-400 text-sm">
                                <Check size={18} /> {t('vaultSync.completed')}
                            </div>
                            <div className="text-xs space-y-1 text-[var(--text-secondary)]">
                                {result.to_vault > 0 && <p>{result.to_vault} {t('vaultSync.filesToVault')}</p>}
                                {result.to_local > 0 && <p>{result.to_local} {t('vaultSync.filesToLocal')}</p>}
                                {result.skipped > 0 && <p>{result.skipped} {t('vaultSync.filesSkipped')}</p>}
                            </div>
                            {result.errors.length > 0 && (
                                <div className="p-2 bg-red-500/10 border border-red-500/30 rounded text-xs text-red-400 space-y-1">
                                    <div className="flex items-center gap-1 font-medium"><AlertTriangle size={12} /> {t('vaultSync.errorsOccurred')}</div>
                                    {result.errors.map((err, i) => <p key={i}>{err}</p>)}
                                </div>
                            )}
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="flex items-center justify-end gap-2 px-4 py-3 border-t border-[var(--border-primary)]">
                    {step === 'review' && totalActions > 0 && (
                        <button onClick={handleApply} className="flex items-center gap-1.5 px-3 py-1.5 text-xs bg-blue-600 hover:bg-blue-500 text-white rounded">
                            <ArrowUpDown size={14} /> {t('vaultSync.applySync')}
                        </button>
                    )}
                    <button onClick={onClose} className="px-3 py-1.5 text-xs bg-[var(--bg-secondary)] hover:bg-[var(--bg-tertiary,var(--bg-secondary))] rounded">
                        {step === 'done' ? t('vaultSync.close') : t('vault.cancel')}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default VaultSyncDialog;
