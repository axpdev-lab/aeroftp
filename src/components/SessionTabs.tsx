import * as React from 'react';
import { X, Plus, Loader2, Wifi, WifiOff, Database, Cloud, CloudOff, Globe, Server } from 'lucide-react';
import { FtpSession, SessionStatus, ProviderType, isOAuthProvider } from '../types';

interface CloudTabState {
    enabled: boolean;
    syncing: boolean;
    active: boolean;  // background sync running
    serverName?: string;
}

interface SessionTabsProps {
    sessions: FtpSession[];
    activeSessionId: string | null;
    onTabClick: (sessionId: string) => void;
    onTabClose: (sessionId: string) => void;
    onNewTab: () => void;
    // Cloud tab props
    cloudTab?: CloudTabState;
    onCloudTabClick?: () => void;
}

const statusConfig: Record<SessionStatus, { icon: React.ReactNode; color: string; title: string }> = {
    connected: { icon: <Wifi size={12} />, color: 'text-green-500', title: 'Connected' },
    connecting: { icon: <Loader2 size={12} className="animate-spin" />, color: 'text-yellow-500', title: 'Connecting...' },
    cached: { icon: <Server size={12} />, color: 'text-blue-500', title: 'Cached (reconnecting...)' },
    disconnected: { icon: <Server size={12} />, color: 'text-gray-400', title: 'Disconnected' },
};

// Check if protocol is a provider (not standard FTP)
const isProviderProtocol = (protocol: ProviderType | undefined): boolean => {
    return protocol !== undefined && ['s3', 'webdav', 'googledrive', 'dropbox', 'onedrive', 'mega'].includes(protocol);
};

// Provider-specific icons with status awareness
const ProviderIcon: React.FC<{ 
    protocol: ProviderType | undefined; 
    size?: number; 
    className?: string;
    isConnected?: boolean;
}> = ({
    protocol,
    size = 14,
    className = '',
    isConnected = true
}) => {
    // Apply opacity for disconnected state
    const opacityClass = isConnected ? '' : 'opacity-50';
    const combinedClass = `${className} ${opacityClass}`.trim();
    
    switch (protocol) {
        case 'googledrive':
            return (
                <svg className={combinedClass} width={size} height={size} viewBox="0 0 87.3 78">
                    <path fill="#0066da" d="M6.6 66.85l3.85 6.65c.8 1.4 1.95 2.5 3.3 3.3l13.75-23.8H0c0 1.55.4 3.1 1.2 4.5l5.4 9.35z" />
                    <path fill="#00ac47" d="M43.65 25L29.9 1.2c-1.35.8-2.5 1.9-3.3 3.3L1.2 52.35c-.8 1.4-1.2 2.95-1.2 4.5h27.5L43.65 25z" />
                    <path fill="#ea4335" d="M73.55 76.8c1.35-.8 2.5-1.9 3.3-3.3l1.6-2.75 7.65-13.25c.8-1.4 1.2-2.95 1.2-4.5H59.85L73.55 76.8z" />
                    <path fill="#00832d" d="M43.65 25L57.4 1.2c-1.35-.8-2.9-1.2-4.5-1.2H34.35c-1.6 0-3.15.45-4.45 1.2L43.65 25z" />
                    <path fill="#2684fc" d="M59.85 53H27.5L13.75 76.8c1.35.8 2.9 1.2 4.5 1.2h50.8c1.6 0 3.15-.45 4.5-1.2L59.85 53z" />
                    <path fill="#ffba00" d="M73.4 26.5l-12.7-22c-.8-1.4-1.95-2.5-3.3-3.3L43.65 25l16.2 28h27.45c0-1.55-.4-3.1-1.2-4.5l-12.7-22z" />
                </svg>
            );
        case 'dropbox':
            return (
                <svg className={combinedClass} width={size} height={size} viewBox="0 0 43 40" fill="#0061ff">
                    <path d="M12.5 0L0 8.1l8.5 6.9 12.5-8.2L12.5 0zM0 22l12.5 8.1 8.5-6.8-12.5-8.2L0 22zm21 1.3l8.5 6.8L42 22l-8.5-6.9-12.5 8.2zm21-15.2L29.5 0 21 6.8l12.5 8.2L42 8.1zM21.1 24.4l-8.6 6.9-3.9-2.6v2.9l12.5 7.5 12.5-7.5v-2.9l-3.9 2.6-8.6-6.9z" />
                </svg>
            );
        case 'onedrive':
            return (
                <svg className={combinedClass} width={size} height={size} viewBox="0 0 24 24">
                    <path fill="#0364b8" d="M14.5 15h6.78l.72-.53V14c0-2.48-1.77-4.6-4.17-5.05A5.5 5.5 0 0 0 7.5 10.5v.5H7c-2.21 0-4 1.79-4 4s1.79 4 4 4h7.5z" />
                    <path fill="#0078d4" d="M9.5 10.5A5.5 5.5 0 0 1 17.83 8.95 5.5 5.5 0 0 0 14.5 15H7c-2.21 0-4-1.79-4-4s1.79-4 4-4h.5v.5c0 1.66.74 3.15 1.9 4.15.4-.08.8-.15 1.1-.15z" />
                    <path fill="#1490df" d="M21.28 14.47l-.78.53H14.5 7c-2.21 0-4-1.79-4-4a3.99 3.99 0 0 1 2.4-3.67A4 4 0 0 1 9 6c.88 0 1.7.29 2.36.78A5.49 5.49 0 0 1 17.83 9a5 5 0 0 1 3.45 5.47z" />
                </svg>
            );
        case 'webdav':
            return <Globe size={size} className={combinedClass} />;
        case 's3':
            // S3 bucket icon - orange database/bucket style
            return (
                <Database size={size} className={`${combinedClass} text-orange-500`} />
            );
        case 'mega':
            // MEGA.nz logo - red circle with M
            return (
                <svg className={combinedClass} width={size} height={size} viewBox="0 0 24 24">
                    <circle cx="12" cy="12" r="12" fill="#D9272E" />
                    <path fill="#ffffff" d="M6.5 16V8h1.8l2.2 4.5L12.7 8h1.8v8h-1.5v-5.2l-1.8 3.7h-1.4l-1.8-3.7V16H6.5z" />
                </svg>
            );
        default:
            return <Wifi size={size} className={combinedClass} />;
    }
};

// Get color for provider
const getProviderColor = (protocol: ProviderType | undefined): string => {
    switch (protocol) {
        case 'googledrive': return 'text-red-500';
        case 'dropbox': return 'text-blue-500';
        case 'onedrive': return 'text-sky-500';
        case 's3': return 'text-orange-500';
        case 'webdav': return 'text-purple-500';
        case 'mega': return 'text-red-600';
        default: return 'text-green-500';
    }
};

export const SessionTabs: React.FC<SessionTabsProps> = ({
    sessions,
    activeSessionId,
    onTabClick,
    onTabClose,
    onNewTab,
    cloudTab,
    onCloudTabClick,
}) => {
    const showTabs = sessions.length > 0 || (cloudTab?.enabled);

    if (!showTabs) return null;

    return (
        <div className="flex items-center gap-1 px-3 py-2 bg-gray-100 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700 overflow-x-auto">
            {/* Cloud Tab - Special tab for AeroCloud */}
            {cloudTab?.enabled && (
                <div
                    className={`group flex items-center gap-2 px-3 py-1.5 rounded-lg cursor-pointer transition-all min-w-0 max-w-[200px] ${cloudTab.active || cloudTab.syncing
                        ? 'bg-gradient-to-r from-cyan-500/20 to-blue-500/20 dark:from-cyan-900/40 dark:to-blue-900/40 border border-cyan-400/30'
                        : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
                        }`}
                    onClick={onCloudTabClick}
                    title={cloudTab.syncing ? 'Syncing...' : cloudTab.active ? 'Background sync active' : 'AeroCloud (click to open)'}
                >
                    {/* Cloud status indicator */}
                    <span className={`shrink-0 ${cloudTab.syncing
                        ? 'text-cyan-500 animate-pulse'
                        : cloudTab.active
                            ? 'text-cyan-500'
                            : 'text-gray-400'
                        }`}>
                        {cloudTab.active || cloudTab.syncing ? (
                            <Cloud size={14} className={cloudTab.syncing ? 'animate-bounce' : ''} />
                        ) : (
                            <CloudOff size={14} />
                        )}
                    </span>

                    {/* Cloud name */}
                    <span className={`truncate text-sm ${cloudTab.active || cloudTab.syncing
                        ? 'font-medium text-cyan-700 dark:text-cyan-300'
                        : 'text-gray-500 dark:text-gray-400'
                        }`}>
                        {cloudTab.serverName || 'AeroCloud'}
                    </span>

                    {/* Syncing indicator */}
                    {cloudTab.syncing && (
                        <span className="shrink-0 w-1.5 h-1.5 rounded-full bg-cyan-500 animate-ping" />
                    )}
                </div>
            )}

            {/* Separator between Cloud and FTP sessions */}
            {cloudTab?.enabled && sessions.length > 0 && (
                <div className="w-px h-5 bg-gray-300 dark:bg-gray-600 mx-1" />
            )}

            {/* Session Tabs with Provider Icons */}
            {sessions.map((session) => {
                const isActive = session.id === activeSessionId;
                const protocol = session.connectionParams?.protocol;
                const isProvider = isProviderProtocol(protocol);
                const isOAuth = protocol && isOAuthProvider(protocol);
                const isConnected = session.status === 'connected';
                const status = statusConfig[session.status];

                return (
                    <div
                        key={session.id}
                        className={`group flex items-center gap-2 px-3 py-1.5 rounded-lg cursor-pointer transition-all min-w-0 max-w-[200px] ${isActive
                            ? 'bg-white dark:bg-gray-700 shadow-sm'
                            : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
                            }`}
                        onClick={() => onTabClick(session.id)}
                    >
                        {/* Status/Provider indicator */}
                        <span 
                            className={`shrink-0 ${isProvider ? getProviderColor(protocol) : status.color}`} 
                            title={`${isProvider ? protocol?.toUpperCase() : 'FTP'} - ${status.title}`}
                        >
                            {session.status === 'connecting' ? (
                                <Loader2 size={14} className="animate-spin" />
                            ) : isProvider ? (
                                <ProviderIcon protocol={protocol} size={14} isConnected={isConnected} />
                            ) : (
                                isConnected ? <Wifi size={14} /> : <WifiOff size={14} />
                            )}
                        </span>

                        {/* Server name */}
                        <span className={`truncate text-sm ${isActive ? 'font-medium' : 'text-gray-600 dark:text-gray-400'}`}>
                            {session.serverName}
                        </span>

                        {/* Close button */}
                        <button
                            onClick={(e) => {
                                e.stopPropagation();
                                onTabClose(session.id);
                            }}
                            className="shrink-0 p-0.5 rounded hover:bg-gray-300 dark:hover:bg-gray-600 opacity-0 group-hover:opacity-100 transition-opacity"
                            title="Close tab"
                        >
                            <X size={12} />
                        </button>
                    </div>
                );
            })}

            {/* New tab button */}
            <button
                onClick={onNewTab}
                className="shrink-0 p-1.5 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors"
                title="New connection"
            >
                <Plus size={16} />
            </button>
        </div>
    );
};

export default SessionTabs;


