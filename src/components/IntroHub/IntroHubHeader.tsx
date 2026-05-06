import * as React from 'react';
import { createPortal } from 'react-dom';
import { Server, PlusCircle, Cloud, FolderOpen, Search, X } from 'lucide-react';
import { ProtocolIcon } from '../ProtocolSelector';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { useTranslation } from '../../i18n';
import type { ServerProfile, ProviderType } from '../../types';

export type IntroHubTab = 'my-servers' | 'discover';

export interface FormTab {
    id: string;
    label: string;
    editingProfile?: ServerProfile;
    protocol?: ProviderType;
    providerId?: string;
}

interface IntroHubHeaderProps {
    activeTab: string; // IntroHubTab | FormTab.id
    onTabChange: (tab: string) => void;
    onCommandPalette: () => void;
    formTabs: FormTab[];
    onCloseFormTab: (tabId: string) => void;
    onCloseAllFormTabs?: () => void;
    hasExistingSessions?: boolean;
    onSkipToFileManager?: () => void;
    onAeroCloud?: () => void;
    onAeroFile?: () => void;
    isAeroCloudConnected?: boolean;
    isAeroCloudPaused?: boolean;
    isAeroCloudConfigured?: boolean;
    serverCount?: number;
    serviceCount?: number;
}

const staticTabs: { id: IntroHubTab; labelKey: string; icon: React.ReactNode }[] = [
    { id: 'my-servers', labelKey: 'introHub.tab.myServers', icon: <Server size={15} /> },
    { id: 'discover', labelKey: 'introHub.tab.discover', icon: <PlusCircle size={15} /> },
];

function FormTabIcon({ tab }: { tab: FormTab }) {
    const proto = tab.editingProfile?.protocol || tab.protocol || 'ftp';
    const pid = tab.editingProfile?.providerId || tab.providerId || proto;
    const Logo = PROVIDER_LOGOS[pid];
    if (Logo) return <Logo size={13} />;
    return <ProtocolIcon protocol={proto} size={13} />;
}

export function IntroHubHeader({
    activeTab,
    onTabChange,
    onCommandPalette,
    formTabs,
    onCloseFormTab,
    onCloseAllFormTabs,
    hasExistingSessions,
    onSkipToFileManager,
    onAeroCloud,
    onAeroFile,
    isAeroCloudConnected,
    isAeroCloudPaused,
    isAeroCloudConfigured,
    serverCount = 0,
    serviceCount = 0,
}: IntroHubHeaderProps) {
    const t = useTranslation();
    const [ctxMenu, setCtxMenu] = React.useState<{ x: number; y: number; tabId: string } | null>(null);
    const ctxMenuRef = React.useRef<HTMLDivElement | null>(null);

    // Close context menu on outside click (same pattern as SessionTabs)
    React.useEffect(() => {
        if (!ctxMenu) return;
        const handleClick = (e: MouseEvent) => {
            if (ctxMenuRef.current && !ctxMenuRef.current.contains(e.target as Node)) {
                setCtxMenu(null);
            }
        };
        document.addEventListener('mousedown', handleClick);
        return () => document.removeEventListener('mousedown', handleClick);
    }, [ctxMenu]);

    return (
        <div className="flex items-center gap-1 px-4 py-2 bg-white/50 dark:bg-gray-800/80 backdrop-blur-sm border-b border-gray-200 dark:border-gray-700 rounded-t-xl overflow-hidden">
            {/* Static tabs with counters */}
            {staticTabs.map((tab) => {
                const count = tab.id === 'my-servers' ? serverCount : tab.id === 'discover' ? serviceCount : 0;
                return (
                    <button
                        key={tab.id}
                        onClick={() => onTabChange(tab.id)}
                        className={`flex items-center gap-2 px-3.5 py-1.5 rounded-lg text-sm font-medium transition-all shrink-0 ${
                            activeTab === tab.id
                                ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 border border-blue-200 dark:border-blue-500/30 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)]'
                                : 'text-gray-500 dark:text-gray-400 border border-transparent hover:bg-gray-100 dark:hover:bg-gray-700/50 hover:border-gray-200 dark:hover:border-gray-600/50 hover:shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:hover:shadow-[0_1px_3px_rgba(0,0,0,0.3)] hover:text-gray-700 dark:hover:text-gray-300'
                        }`}
                    >
                        {tab.icon}
                        <span>{t(tab.labelKey)}</span>
                        {count > 0 && (
                            <span className={`text-[10px] tabular-nums px-1.5 py-0.5 rounded-full ${
                                activeTab === tab.id ? 'bg-blue-100 dark:bg-blue-900/40 text-blue-500 dark:text-blue-300' : 'bg-gray-100 dark:bg-gray-700/50 text-gray-400 dark:text-gray-500'
                            }`}>{count}</span>
                        )}
                    </button>
                );
            })}

            {/* Dynamic form tabs */}
            {formTabs.length > 0 && (
                <>
                    <div className="w-px h-5 bg-gray-300 dark:bg-gray-600 mx-1 shrink-0" />
                    {formTabs.map((ft) => (
                        <div
                            key={ft.id}
                            className={`group flex items-center gap-1.5 pl-2.5 pr-1.5 py-1.5 rounded-lg text-sm transition-all min-w-0 cursor-pointer ${
                                activeTab === ft.id
                                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 border border-blue-200 dark:border-blue-500/30 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)]'
                                    : 'text-gray-500 dark:text-gray-400 border border-transparent hover:bg-gray-100 dark:hover:bg-gray-700/50 hover:border-gray-200 dark:hover:border-gray-600/50 hover:shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:hover:shadow-[0_1px_3px_rgba(0,0,0,0.3)]'
                            }`}
                            onClick={() => onTabChange(ft.id)}
                            onContextMenu={(e) => { e.preventDefault(); setCtxMenu({ x: e.clientX, y: e.clientY, tabId: ft.id }); }}
                        >
                            <span className="shrink-0"><FormTabIcon tab={ft} /></span>
                            <span className="truncate text-xs font-medium">{ft.label}</span>
                            <button
                                onClick={(e) => { e.stopPropagation(); onCloseFormTab(ft.id); }}
                                className="p-0.5 rounded hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors opacity-60 group-hover:opacity-100"
                                title={t('common.close')}
                            >
                                <X size={12} />
                            </button>
                        </div>
                    ))}
                </>
            )}

            {/* Spacer */}
            <div className="flex-1" />

            {/* Right side actions */}
            <div className="flex items-center gap-1.5 shrink-0">
                {/* Active sessions badge */}
                {hasExistingSessions && onSkipToFileManager && (
                    <button
                        onClick={onSkipToFileManager}
                        className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg bg-green-50 dark:bg-green-900/30 border border-green-200 dark:border-green-800 text-green-700 dark:text-green-400 hover:bg-green-100 dark:hover:bg-green-800/40 transition-colors"
                        title={t('connection.activeSessions')}
                    >
                        <span className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
                        <span className="text-xs font-medium">{t('connection.activeSessions')}</span>
                    </button>
                )}

                {/* AeroCloud */}
                {onAeroCloud && (
                    <button
                        onClick={onAeroCloud}
                        className={`flex items-center gap-1.5 p-1.5 rounded-lg transition-colors ${
                            isAeroCloudConnected && isAeroCloudPaused
                                ? 'bg-amber-50 dark:bg-amber-900/30 hover:bg-amber-100 dark:hover:bg-amber-800/40 text-amber-600 dark:text-amber-400'
                                : isAeroCloudConnected
                                    ? 'bg-sky-50 dark:bg-sky-900/30 hover:bg-sky-100 dark:hover:bg-sky-800/40 text-sky-600 dark:text-sky-400'
                                    : 'bg-gray-50 dark:bg-gray-700 hover:bg-gray-100 dark:hover:bg-gray-600 text-gray-400 dark:text-gray-500'
                        }`}
                        title={
                            isAeroCloudConnected && isAeroCloudPaused
                                ? t('cloud.paused')
                                : isAeroCloudConfigured ? 'AeroCloud' : 'Configure AeroCloud'
                        }
                    >
                        <Cloud size={16} />
                        {isAeroCloudConnected && (
                            <span className={`w-1.5 h-1.5 rounded-full ${isAeroCloudPaused ? 'bg-amber-500' : 'bg-green-500'}`} />
                        )}
                    </button>
                )}

                {/* AeroFile */}
                {onAeroFile && (
                    <button
                        onClick={onAeroFile}
                        className="flex items-center p-1.5 bg-blue-50 dark:bg-blue-900/30 hover:bg-blue-100 dark:hover:bg-blue-800/40 text-blue-600 dark:text-blue-400 rounded-lg transition-colors"
                        title="AeroFile"
                    >
                        <FolderOpen size={16} />
                    </button>
                )}

                {/* Cross-Profile Transfer entry point lives in the My Servers
                    toolbar next to the selection ring/badge, where it pairs
                    with the per-card selection flow. The header duplicate was
                    removed to avoid two parallel paths to the same modal. */}
            </div>

            {/* Form tab context menu: portal to body to escape overflow:hidden */}
            {ctxMenu && createPortal(
                <div
                    ref={ctxMenuRef}
                    className="fixed z-[9999] min-w-[160px] py-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-lg text-xs"
                    style={{ left: ctxMenu.x, top: ctxMenu.y }}
                >
                    <button
                        className="w-full text-left px-3 py-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-300"
                        onClick={() => { onCloseFormTab(ctxMenu.tabId); setCtxMenu(null); }}
                    >
                        {t('common.close')}
                    </button>
                    {formTabs.length > 1 && (
                        <>
                            <button
                                className="w-full text-left px-3 py-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-300"
                                onClick={() => {
                                    formTabs.filter(ft => ft.id !== ctxMenu.tabId).forEach(ft => onCloseFormTab(ft.id));
                                    setCtxMenu(null);
                                }}
                            >
                                {t('ui.session.closeOthers')}
                            </button>
                            <div className="my-1 border-t border-gray-100 dark:border-gray-700" />
                            <button
                                className="w-full text-left px-3 py-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 text-red-500 dark:text-red-400"
                                onClick={() => { onCloseAllFormTabs?.(); setCtxMenu(null); }}
                            >
                                {t('ui.session.closeAll')}
                            </button>
                        </>
                    )}
                </div>,
                document.body
            )}
        </div>
    );
}
