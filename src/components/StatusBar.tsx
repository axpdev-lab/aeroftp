import * as React from 'react';
import { Globe, HardDrive, Wifi, WifiOff, Code, FolderSync, Cloud, ArrowUpDown, ScrollText, Download } from 'lucide-react';
import { useTranslation } from '../i18n';

interface UpdateInfo {
    has_update: boolean;
    latest_version?: string;
    current_version?: string;
    download_url?: string;
    install_format?: string;
}

interface StatusBarProps {
    isConnected: boolean;
    serverInfo?: string;
    remotePath?: string;
    localPath?: string;
    remoteFileCount?: number;
    localFileCount?: number;
    activePanel: 'remote' | 'local';
    devToolsOpen?: boolean;
    cloudEnabled?: boolean;
    cloudSyncing?: boolean;
    transferQueueActive?: boolean;
    transferQueueCount?: number;
    showActivityLog?: boolean;
    activityLogCount?: number;
    updateAvailable?: UpdateInfo | null;
    onToggleDevTools?: () => void;
    onToggleSync?: () => void;
    onToggleCloud?: () => void;
    onToggleTransferQueue?: () => void;
    onToggleActivityLog?: () => void;
}

export const StatusBar: React.FC<StatusBarProps> = ({
    isConnected,
    serverInfo,
    remotePath,
    localPath,
    remoteFileCount = 0,
    localFileCount = 0,
    activePanel,
    devToolsOpen = false,
    cloudEnabled = false,
    cloudSyncing = false,
    transferQueueActive = false,
    transferQueueCount = 0,
    showActivityLog = true,
    activityLogCount = 0,
    updateAvailable,
    onToggleDevTools,
    onToggleSync,
    onToggleCloud,
    onToggleTransferQueue,
    onToggleActivityLog,
}) => {
    const t = useTranslation();

    return (
        <div className="h-7 bg-gray-100 dark:bg-gray-800 border-t border-gray-200 dark:border-gray-700 px-4 flex items-center justify-between text-xs text-gray-600 dark:text-gray-400 select-none shrink-0">
            {/* Left: Connection Status */}
            <div className="flex items-center gap-4">
                <div className="flex items-center gap-1.5">
                    {isConnected ? (
                        <>
                            <div className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
                            <Wifi size={12} className="text-green-500" />
                            <span className="font-medium text-green-600 dark:text-green-400">
                                {serverInfo || t('statusBar.connected')}
                            </span>
                        </>
                    ) : (
                        <>
                            <div className="w-2 h-2 rounded-full bg-gray-400" />
                            <WifiOff size={12} className="text-gray-400" />
                            <span className="text-gray-500">{t('statusBar.notConnected')}</span>
                        </>
                    )}
                </div>

                {/* Update Available Button */}
                {updateAvailable?.has_update && (
                    <a
                        href={updateAvailable.download_url || 'https://github.com/axpnet/aeroftp/releases/latest'}
                        target="_blank"
                        rel="noopener noreferrer"
                        className="flex items-center gap-1.5 px-2.5 py-0.5 bg-gradient-to-r from-violet-500 to-purple-600 text-white rounded-full text-[11px] font-medium hover:from-violet-600 hover:to-purple-700 transition-all shadow-sm hover:shadow-md animate-pulse"
                        title={`Download AeroFTP v${updateAvailable.latest_version}`}
                    >
                        <Download size={11} />
                        <span>Update Available</span>
                    </a>
                )}

                {/* Separator */}
                <div className="w-px h-4 bg-gray-300 dark:bg-gray-600" />

                {/* Current Path */}
                <div className="flex items-center gap-1.5 max-w-md truncate">
                    {activePanel === 'remote' ? (
                        <>
                            <Globe size={12} className="text-blue-500 shrink-0" />
                            <span className="truncate" title={remotePath}>
                                {isConnected ? (remotePath || '/') : '—'}
                            </span>
                        </>
                    ) : (
                        <>
                            <HardDrive size={12} className="text-amber-500 shrink-0" />
                            <span className="truncate" title={localPath}>
                                {localPath || '~'}
                            </span>
                        </>
                    )}
                </div>
            </div>

            {/* Right: File Count + Sync + DevTools */}
            <div className="flex items-center gap-4">
                {isConnected && (
                    <div className="flex items-center gap-1.5">
                        <Globe size={12} className="text-blue-500" />
                        <span>{remoteFileCount} {t('browser.files')}</span>
                    </div>
                )}
                <div className="flex items-center gap-1.5">
                    <HardDrive size={12} className="text-amber-500" />
                    <span>{localFileCount} {t('browser.files')}</span>
                </div>

                {/* Separator */}
                <div className="w-px h-4 bg-gray-300 dark:bg-gray-600" />

                {/* Transfer Queue Toggle */}
                {onToggleTransferQueue && (
                    <button
                        onClick={onToggleTransferQueue}
                        className={`flex items-center gap-1.5 px-2 py-0.5 rounded transition-colors ${transferQueueActive
                                ? 'bg-orange-100 dark:bg-orange-900/40 text-orange-600 dark:text-orange-400'
                                : 'hover:bg-gray-200 dark:hover:bg-gray-700'
                            }`}
                        title="Transfer Queue"
                    >
                        <ArrowUpDown size={12} className={transferQueueActive ? 'animate-pulse' : ''} />
                        <span>Queue</span>
                        {transferQueueCount > 0 && (
                            <span className="px-1.5 py-0.5 text-[10px] font-medium bg-orange-500 text-white rounded-full min-w-[18px] text-center">
                                {transferQueueCount}
                            </span>
                        )}
                    </button>
                )}

                {/* Cloud Button */}
                {onToggleCloud && (
                    <button
                        onClick={onToggleCloud}
                        className={`flex items-center gap-1.5 px-2 py-0.5 rounded transition-colors ${cloudSyncing
                            ? 'bg-cyan-100 dark:bg-cyan-900/40 text-cyan-500'
                            : cloudEnabled
                                ? 'bg-cyan-100 dark:bg-cyan-900/40 text-cyan-600 dark:text-cyan-400'
                                : 'hover:bg-gray-200 dark:hover:bg-gray-700'
                            }`}
                        title="AeroCloud - Personal Cloud Sync"
                    >
                        <Cloud size={12} className={cloudSyncing ? 'animate-pulse' : ''} />
                        <span>{cloudSyncing ? t('statusBar.syncing') : t('cloud.title')}</span>
                        {cloudSyncing && <span className="w-1.5 h-1.5 rounded-full bg-cyan-500 animate-ping" />}
                    </button>
                )}

                {/* Sync Button */}
                {onToggleSync && isConnected && (
                    <button
                        onClick={onToggleSync}
                        className="flex items-center gap-1.5 px-2 py-0.5 rounded transition-colors hover:bg-gray-200 dark:hover:bg-gray-700"
                        title={t('statusBar.syncFiles')}
                    >
                        <FolderSync size={12} />
                        <span>{t('statusBar.syncFiles')}</span>
                    </button>
                )}

                {/* Activity Log Toggle */}
                {onToggleActivityLog && (
                    <button
                        onClick={onToggleActivityLog}
                        className={`flex items-center gap-1.5 px-2 py-0.5 rounded transition-colors ${
                            showActivityLog
                                ? 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-600 dark:text-emerald-400'
                                : 'hover:bg-gray-200 dark:hover:bg-gray-700'
                        }`}
                        title="Activity Log"
                    >
                        <ScrollText size={12} />
                        <span>Log</span>
                        {activityLogCount > 0 && (
                            <span className="px-1.5 py-0.5 text-[10px] font-medium bg-emerald-500 text-white rounded-full min-w-[18px] text-center">
                                {activityLogCount > 99 ? '99+' : activityLogCount}
                            </span>
                        )}
                    </button>
                )}

                {/* DevTools Toggle */}
                {onToggleDevTools && (
                    <button
                        onClick={onToggleDevTools}
                        className={`flex items-center gap-1.5 px-2 py-0.5 rounded transition-colors ${devToolsOpen
                            ? 'bg-purple-100 dark:bg-purple-900/40 text-purple-600 dark:text-purple-400'
                            : 'hover:bg-gray-200 dark:hover:bg-gray-700'
                            }`}
                        title={t('statusBar.devTools')}
                    >
                        <Code size={12} />
                        <span>{t('statusBar.devTools')}</span>
                    </button>
                )}
            </div>
        </div>
    );
};

export default StatusBar;
