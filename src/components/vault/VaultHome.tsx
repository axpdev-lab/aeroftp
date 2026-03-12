import * as React from 'react';
import { Lock, FolderPlus, Download, Loader2, Clock, X as XIcon, Trash2 } from 'lucide-react';
import { VaultIcon } from '../icons/VaultIcon';
import { useTranslation } from '../../i18n';
import { VaultState, securityLevels, SecurityLevel, IconProvider } from './useVaultState';
import { formatSize } from '../../utils/formatters';

interface VaultHomeProps {
    state: VaultState;
    isConnected?: boolean;
    iconProvider?: IconProvider;
}

/** Format a timestamp as relative time (e.g. "2 hours ago") */
function relativeTime(timestamp: number): string {
    const now = Date.now();
    const diffMs = now - timestamp * 1000; // timestamp is seconds
    const diffMin = Math.floor(diffMs / 60000);
    if (diffMin < 1) return 'just now';
    if (diffMin < 60) return `${diffMin}m ago`;
    const diffH = Math.floor(diffMin / 60);
    if (diffH < 24) return `${diffH}h ago`;
    const diffD = Math.floor(diffH / 24);
    if (diffD < 30) return `${diffD}d ago`;
    const diffM = Math.floor(diffD / 30);
    return `${diffM}mo ago`;
}

/** Map security_level string to SecurityLevel type */
function toSecurityLevel(s: string): SecurityLevel {
    if (s === 'standard' || s === 'advanced' || s === 'paranoid') return s;
    return 'advanced';
}

export const VaultHome: React.FC<VaultHomeProps> = ({ state, isConnected }) => {
    const t = useTranslation();

    return (
        <div className="p-6 flex flex-col items-center gap-5">
            {/* AeroVault icon */}
            <svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" width={56} height={56} fill="none" stroke="currentColor" className="text-emerald-400">
                <path d="M12 21l.88-.38a11 11 0 006.63-9.26l.43-5.52a1 1 0 00-.76-1L12 3 4.82 4.8a1 1 0 00-.76 1l.43 5.52a11 11 0 006.63 9.26z" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" />
                <rect x="9.25" y="11" width="5.5" height="4" rx="0.75" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
                <path d="M10.25 11V9.5a1.75 1.75 0 013.5 0V11" strokeWidth="1.5" strokeLinecap="round" strokeLinejoin="round" />
            </svg>

            <p className="text-gray-600 dark:text-gray-300 text-center text-sm max-w-md">
                {t('vault.descriptionV2')}
            </p>

            {/* Security levels preview */}
            <div className="flex gap-2 text-xs">
                {Object.entries(securityLevels).map(([key, config]) => {
                    const Icon = config.icon;
                    return (
                        <div key={key} className={`flex items-center gap-1.5 px-2 py-1 rounded border ${config.borderColor} bg-opacity-10`}>
                            <Icon size={12} className={config.color} />
                            <span className={config.color}>{config.label}</span>
                        </div>
                    );
                })}
            </div>

            <div className="flex gap-3 mt-1">
                <button onClick={() => { state.resetState(); state.setMode('create'); }} className="flex items-center gap-2 px-4 py-2 bg-emerald-600 hover:bg-emerald-500 text-white rounded text-sm font-medium">
                    <FolderPlus size={16} /> {t('vault.createNew')}
                </button>
                <button onClick={state.handleOpen} className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-500 text-white rounded text-sm font-medium">
                    <Lock size={16} /> {t('vault.openExisting')}
                </button>
            </div>

            {/* Recent Vaults */}
            {state.recentVaults.length > 0 && (
                <div className="w-full max-w-md mt-2">
                    <div className="flex items-center justify-between mb-2">
                        <h3 className="text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wide flex items-center gap-1.5">
                            <Clock size={12} />
                            {t('vault.recentVaults') || 'Recent Vaults'}
                        </h3>
                        <button
                            onClick={state.clearHistory}
                            className="text-[10px] text-gray-400 hover:text-red-400 transition-colors"
                        >
                            {t('vault.clearHistory') || 'Clear'}
                        </button>
                    </div>
                    <div className="space-y-1.5 max-h-[200px] overflow-y-auto">
                        {state.recentVaults.map((vault) => {
                            const level = toSecurityLevel(vault.security_level);
                            const config = securityLevels[level];
                            return (
                                <div
                                    key={vault.id}
                                    className="group flex items-center gap-3 px-3 py-2 rounded-lg border border-gray-200 dark:border-gray-700 hover:border-emerald-500/50 hover:bg-gray-50 dark:hover:bg-gray-800/50 cursor-pointer transition-all"
                                    onClick={async () => {
                                        state.setVaultPath(vault.vault_path);
                                        try {
                                            const sec = await state.detectVaultVersion(vault.vault_path);
                                            state.setVaultSecurity(sec);
                                        } catch { /* ignore — VaultOpen will re-detect */ }
                                        state.setMode('open');
                                    }}
                                >
                                    <VaultIcon size={20} className="text-emerald-400 shrink-0" />
                                    <div className="flex-1 min-w-0">
                                        <div className="flex items-center gap-2">
                                            <span className="text-sm font-medium text-gray-800 dark:text-gray-200 truncate">
                                                {vault.vault_name}
                                            </span>
                                            <span className={`px-1.5 py-0.5 rounded text-[10px] font-medium ${config.bgColor} bg-opacity-20 ${config.color}`}>
                                                {config.label}
                                            </span>
                                        </div>
                                        <div className="flex items-center gap-2 text-[10px] text-gray-400">
                                            <span className="truncate">{vault.vault_path}</span>
                                            <span className="shrink-0">{relativeTime(vault.last_opened_at)}</span>
                                            {vault.file_count > 0 && (
                                                <span className="shrink-0 px-1 py-0 bg-gray-200 dark:bg-gray-700 rounded">
                                                    {vault.file_count} files
                                                </span>
                                            )}
                                        </div>
                                    </div>
                                    <button
                                        onClick={(e) => {
                                            e.stopPropagation();
                                            state.removeFromHistory(vault.vault_path);
                                        }}
                                        className="p-1 rounded opacity-0 group-hover:opacity-100 hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-400 transition-all"
                                        title={t('common.remove') || 'Remove'}
                                    >
                                        <XIcon size={12} />
                                    </button>
                                </div>
                            );
                        })}
                    </div>
                </div>
            )}

            {/* Empty recent vaults */}
            {state.recentVaults.length === 0 && (
                <p className="text-xs text-gray-400 dark:text-gray-500 mt-1">
                    {t('vault.noRecentVaults') || 'No recently opened vaults'}
                </p>
            )}

            {/* Remote Vault — only when connected to a server */}
            {isConnected && (
                <div className="w-full max-w-md mt-2 space-y-2">
                    {!state.showRemoteInput ? (
                        <button
                            onClick={() => state.setShowRemoteInput(true)}
                            className="flex items-center gap-2 px-4 py-2 rounded text-sm font-medium
                                bg-purple-600/20 text-purple-400 hover:bg-purple-600/30 transition-colors w-full justify-center"
                        >
                            <Download size={16} />
                            {t('vault.remote.open')}
                        </button>
                    ) : (
                        <div className="p-3 rounded-lg border border-purple-500/30 bg-purple-500/5 space-y-2">
                            <p className="text-xs text-purple-400">{t('vault.remote.title')}</p>
                            <input
                                type="text"
                                value={state.remoteVaultPath}
                                onChange={e => state.setRemoteVaultPath(e.target.value)}
                                placeholder="/path/to/vault.aerovault"
                                className="w-full px-3 py-1.5 rounded text-sm bg-gray-800 border border-gray-600 text-white placeholder:text-gray-500"
                            />
                            <div className="flex gap-2">
                                <button
                                    onClick={() => { state.setShowRemoteInput(false); state.setRemoteVaultPath(''); }}
                                    className="flex-1 py-1.5 rounded text-xs bg-gray-700 text-gray-300"
                                >
                                    {t('security.totp.back')}
                                </button>
                                <button
                                    onClick={state.handleOpenRemoteVault}
                                    disabled={state.remoteLoading || !state.remoteVaultPath.endsWith('.aerovault')}
                                    className="flex-1 py-1.5 rounded text-xs bg-purple-600 text-white disabled:opacity-50 flex items-center justify-center gap-1"
                                >
                                    {state.remoteLoading ? <Loader2 size={12} className="animate-spin" /> : <Download size={12} />}
                                    {state.remoteLoading ? t('vault.remote.downloading') : t('vault.remote.open')}
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};
