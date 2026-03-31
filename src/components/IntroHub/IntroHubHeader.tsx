import * as React from 'react';
import { Server, Compass, Plus, Cloud, FolderOpen, Search, X } from 'lucide-react';
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
    onNewConnection: () => void;
    onCommandPalette: () => void;
    formTabs: FormTab[];
    onCloseFormTab: (tabId: string) => void;
    hasExistingSessions?: boolean;
    onSkipToFileManager?: () => void;
    onAeroCloud?: () => void;
    onAeroFile?: () => void;
    isAeroCloudConnected?: boolean;
    isAeroCloudConfigured?: boolean;
    serverCount?: number;
    serviceCount?: number;
}

const staticTabs: { id: IntroHubTab; labelKey: string; icon: React.ReactNode }[] = [
    { id: 'my-servers', labelKey: 'introHub.tab.myServers', icon: <Server size={15} /> },
    { id: 'discover', labelKey: 'introHub.tab.discover', icon: <Compass size={15} /> },
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
    onNewConnection,
    onCommandPalette,
    formTabs,
    onCloseFormTab,
    hasExistingSessions,
    onSkipToFileManager,
    onAeroCloud,
    onAeroFile,
    isAeroCloudConnected,
    isAeroCloudConfigured,
    serverCount = 0,
    serviceCount = 0,
}: IntroHubHeaderProps) {
    const t = useTranslation();

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
                                ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 shadow-sm'
                                : 'text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700/50 hover:text-gray-700 dark:hover:text-gray-300'
                        }`}
                    >
                        {tab.icon}
                        <span>{t(tab.labelKey)}</span>
                        {count > 0 && (
                            <span className={`text-[10px] tabular-nums ${
                                activeTab === tab.id ? 'text-blue-500 dark:text-blue-300' : 'text-gray-400 dark:text-gray-500'
                            }`}>{count}</span>
                        )}
                    </button>
                );
            })}

            {/* + New button (opens Discover) */}
            <button
                onClick={onNewConnection}
                className="flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg text-sm text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700/50 hover:text-gray-700 dark:hover:text-gray-300 transition-colors border border-dashed border-gray-300 dark:border-gray-600 shrink-0"
                title="Ctrl+N"
            >
                <Plus size={14} />
            </button>

            {/* Dynamic form tabs */}
            {formTabs.length > 0 && (
                <>
                    <div className="w-px h-5 bg-gray-300 dark:bg-gray-600 mx-1 shrink-0" />
                    {formTabs.map((ft) => (
                        <div
                            key={ft.id}
                            className={`group flex items-center gap-1.5 pl-2.5 pr-1.5 py-1.5 rounded-lg text-sm transition-all min-w-0 cursor-pointer ${
                                activeTab === ft.id
                                    ? 'bg-blue-50 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 shadow-sm'
                                    : 'text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                            }`}
                            onClick={() => onTabChange(ft.id)}
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
                {/* Search / Command Palette trigger */}
                <button
                    onClick={onCommandPalette}
                    className="flex items-center gap-2 px-3 py-1.5 rounded-lg bg-gray-100 dark:bg-gray-700/50 hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-400 dark:text-gray-500 text-sm transition-colors min-w-[140px]"
                    title="Ctrl+K"
                >
                    <Search size={14} />
                    <span className="text-gray-400 dark:text-gray-500 hidden sm:inline">{t('introHub.search')}</span>
                    <kbd className="ml-auto text-[10px] font-mono px-1.5 py-0.5 bg-gray-200 dark:bg-gray-600 rounded text-gray-500 dark:text-gray-400">
                        Ctrl+K
                    </kbd>
                </button>

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
                            isAeroCloudConnected
                                ? 'bg-sky-50 dark:bg-sky-900/30 hover:bg-sky-100 dark:hover:bg-sky-800/40 text-sky-600 dark:text-sky-400'
                                : 'bg-gray-50 dark:bg-gray-700 hover:bg-gray-100 dark:hover:bg-gray-600 text-gray-400 dark:text-gray-500'
                        }`}
                        title={isAeroCloudConfigured ? 'AeroCloud' : 'Configure AeroCloud'}
                    >
                        <Cloud size={16} />
                        {isAeroCloudConnected && <span className="w-1.5 h-1.5 rounded-full bg-green-500" />}
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
            </div>
        </div>
    );
}
