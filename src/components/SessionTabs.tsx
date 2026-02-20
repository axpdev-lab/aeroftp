import * as React from 'react';
import { useState, useRef, useCallback } from 'react';
import { X, Plus, Loader2, Wifi, WifiOff, Database, Cloud, CloudOff, Server, Lock, ShieldCheck } from 'lucide-react';
import { FtpSession, SessionStatus, ProviderType, isOAuthProvider, isFourSharedProvider } from '../types';
import { MegaLogo, BoxLogo, PCloudLogo, AzureLogo, FilenLogo, FourSharedLogo, ZohoWorkDriveLogo, PROVIDER_LOGOS } from './ProviderLogos';
import { useTranslation } from '../i18n';

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
    onCloseAll: () => void;
    onNewTab: () => void;
    // Cloud tab props
    cloudTab?: CloudTabState;
    onCloudTabClick?: () => void;
    // Tab reorder
    onReorder?: (sessions: FtpSession[]) => void;
}

// Status config factory (requires t() call, so moved inside component)
const createStatusConfig = (t: (key: string) => string): Record<SessionStatus, { icon: React.ReactNode; color: string; title: string }> => ({
    connected: { icon: <Wifi size={12} />, color: 'text-green-500', title: t('ui.session.connected') },
    connecting: { icon: <Loader2 size={12} className="animate-spin" />, color: 'text-yellow-500', title: t('ui.session.connecting') },
    cached: { icon: <Server size={12} />, color: 'text-blue-500', title: t('ui.session.cached') },
    disconnected: { icon: <Server size={12} />, color: 'text-gray-400', title: t('ui.session.disconnected') },
});

// Check if protocol is a provider (not standard FTP)
const isProviderProtocol = (protocol: ProviderType | undefined): boolean => {
    return protocol !== undefined && ['s3', 'webdav', 'googledrive', 'dropbox', 'onedrive', 'mega', 'sftp', 'box', 'pcloud', 'azure', 'filen', 'fourshared', 'zohoworkdrive'].includes(protocol);
};

// Provider-specific icons with status awareness
const ProviderIcon: React.FC<{
    protocol: ProviderType | undefined;
    providerId?: string;
    size?: number;
    className?: string;
    isConnected?: boolean;
}> = ({
    protocol,
    providerId,
    size = 14,
    className = '',
    isConnected = true
}) => {
    // Apply opacity for disconnected state
    const opacityClass = isConnected ? '' : 'opacity-50';
    const combinedClass = `${className} ${opacityClass}`.trim();

    // Check for provider-specific logo first (S3/WebDAV presets)
    if (providerId) {
        const LogoComponent = PROVIDER_LOGOS[providerId];
        if (LogoComponent) return <span className={opacityClass}><LogoComponent size={size} /></span>;
    }

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
            return <Cloud size={size} className={`${combinedClass} text-orange-500`} />;
        case 's3':
            return <Database size={size} className={`${combinedClass} text-amber-600`} />;
        case 'mega':
            return <MegaLogo size={size} />;
        case 'box':
            return <BoxLogo size={size} />;
        case 'pcloud':
            return <PCloudLogo size={size} />;
        case 'azure':
            return <AzureLogo size={size} />;
        case 'filen':
            return <FilenLogo size={size} />;
        case 'fourshared':
            return <FourSharedLogo size={size} />;
        case 'zohoworkdrive':
            return <ZohoWorkDriveLogo size={size} />;
        case 'sftp':
            return <Lock size={size} className={`${combinedClass} text-emerald-500`} />;
        case 'ftps':
            return <ShieldCheck size={size} className={`${combinedClass} text-green-500`} />;
        default:
            return <Wifi size={size} className={combinedClass} />;
    }
};

// Get color for provider (matches icons)
const getProviderColor = (protocol: ProviderType | undefined): string => {
    switch (protocol) {
        case 'googledrive': return 'text-red-500';
        case 'dropbox': return 'text-blue-500';
        case 'onedrive': return 'text-sky-500';
        case 's3': return 'text-amber-600';      // S3 - amber
        case 'webdav': return 'text-orange-500'; // WebDAV - orange
        case 'mega': return 'text-red-600';
        case 'box': return 'text-blue-600';
        case 'pcloud': return 'text-cyan-500';
        case 'azure': return 'text-blue-500';
        case 'filen': return 'text-emerald-600';
        case 'fourshared': return 'text-blue-500';
        case 'zohoworkdrive': return 'text-red-500';
        case 'sftp': return 'text-emerald-500';  // SFTP - emerald (lock)
        case 'ftps': return 'text-green-500';    // FTPS - green (shield)
        default: return 'text-green-500';        // FTP - green
    }
};

export const SessionTabs: React.FC<SessionTabsProps> = ({
    sessions,
    activeSessionId,
    onTabClick,
    onTabClose,
    onCloseAll,
    onNewTab,
    cloudTab,
    onCloudTabClick,
    onReorder,
}) => {
    const t = useTranslation();
    const statusConfig = createStatusConfig(t);
    const showTabs = sessions.length > 0 || (cloudTab?.enabled);

    // Drag-to-reorder state
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);
    const dragNodeRef = useRef<HTMLDivElement | null>(null);

    // Context menu state
    const [contextMenu, setContextMenu] = useState<{ x: number; y: number; sessionId: string } | null>(null);
    const contextMenuRef = useRef<HTMLDivElement | null>(null);

    // Close context menu on outside click
    React.useEffect(() => {
        if (!contextMenu) return;
        const handleClick = (e: MouseEvent) => {
            if (contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
                setContextMenu(null);
            }
        };
        document.addEventListener('mousedown', handleClick);
        return () => document.removeEventListener('mousedown', handleClick);
    }, [contextMenu]);

    const handleTabDragStart = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
        setDragIdx(idx);
        dragNodeRef.current = e.currentTarget;
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('application/x-session-tab', String(idx));
        requestAnimationFrame(() => {
            if (dragNodeRef.current) dragNodeRef.current.style.opacity = '0.4';
        });
    }, []);

    const handleTabDragOver = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        if (dragIdx === null || idx === dragIdx) return;
        setOverIdx(idx);
    }, [dragIdx]);

    const handleTabDrop = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
        e.preventDefault();
        if (dragIdx === null || dragIdx === idx || !onReorder) return;
        const reordered = [...sessions];
        const [moved] = reordered.splice(dragIdx, 1);
        reordered.splice(idx, 0, moved);
        onReorder(reordered);
    }, [dragIdx, sessions, onReorder]);

    const handleTabDragEnd = useCallback(() => {
        if (dragNodeRef.current) dragNodeRef.current.style.opacity = '1';
        dragNodeRef.current = null;
        setDragIdx(null);
        setOverIdx(null);
    }, []);

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
                    title={cloudTab.syncing ? t('ui.session.syncing') : cloudTab.active ? t('ui.session.backgroundSyncActive') : t('ui.session.aerocloudClickToOpen')}
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
                        {cloudTab.serverName || t('statusBar.aerofile')}
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
            {sessions.map((session, idx) => {
                const isActive = session.id === activeSessionId;
                const protocol = session.connectionParams?.protocol;
                const isProvider = isProviderProtocol(protocol);
                const isOAuth = protocol && (isOAuthProvider(protocol) || isFourSharedProvider(protocol));
                const isConnected = session.status === 'connected';
                const status = statusConfig[session.status];
                const isDragTarget = overIdx === idx && dragIdx !== null && dragIdx !== idx;

                return (
                    <div
                        key={session.id}
                        draggable={!!onReorder}
                        onDragStart={(e) => handleTabDragStart(e, idx)}
                        onDragOver={(e) => handleTabDragOver(e, idx)}
                        onDrop={(e) => handleTabDrop(e, idx)}
                        onDragEnd={handleTabDragEnd}
                        className={`group flex items-center gap-2 px-3 py-1.5 rounded-lg cursor-pointer transition-all min-w-0 max-w-[200px] ${isActive
                            ? 'bg-white dark:bg-gray-700 shadow-sm'
                            : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
                            } ${dragIdx === idx ? 'scale-95' : ''} ${isDragTarget ? 'border-l-2 border-blue-500' : ''}`}
                        onClick={() => onTabClick(session.id)}
                        onContextMenu={(e) => {
                            e.preventDefault();
                            setContextMenu({ x: e.clientX, y: e.clientY, sessionId: session.id });
                        }}
                    >
                        {/* Status/Provider indicator */}
                        <span 
                            className={`shrink-0 ${isProvider ? getProviderColor(protocol) : status.color}`} 
                            title={`${isProvider ? protocol?.toUpperCase() : 'FTP'} - ${status.title}`}
                        >
                            {session.status === 'connecting' ? (
                                <Loader2 size={14} className="animate-spin" />
                            ) : isProvider ? (
                                <ProviderIcon protocol={protocol} providerId={session.providerId} size={14} isConnected={isConnected} />
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
                            title={t('ui.session.closeTab')}
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
                title={t('ui.session.newConnection')}
            >
                <Plus size={16} />
            </button>

            {/* Tab context menu */}
            {contextMenu && (
                <div
                    ref={contextMenuRef}
                    className="fixed z-[9999] bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl py-1 min-w-[180px]"
                    style={{ left: contextMenu.x, top: contextMenu.y }}
                >
                    <button
                        className="w-full px-3 py-1.5 text-sm text-left hover:bg-gray-100 dark:hover:bg-gray-700 flex items-center gap-2 text-gray-700 dark:text-gray-300"
                        onClick={() => { onTabClose(contextMenu.sessionId); setContextMenu(null); }}
                    >
                        <X size={14} />
                        {t('ui.session.closeTab')}
                    </button>
                    {sessions.length > 1 && (
                        <>
                            <button
                                className="w-full px-3 py-1.5 text-sm text-left hover:bg-gray-100 dark:hover:bg-gray-700 flex items-center gap-2 text-gray-700 dark:text-gray-300"
                                onClick={() => {
                                    sessions.filter(s => s.id !== contextMenu.sessionId).forEach(s => onTabClose(s.id));
                                    setContextMenu(null);
                                }}
                            >
                                <X size={14} />
                                {t('ui.session.closeOthers')}
                            </button>
                            <div className="border-t border-gray-200 dark:border-gray-700 my-1" />
                            <button
                                className="w-full px-3 py-1.5 text-sm text-left hover:bg-red-50 dark:hover:bg-red-900/20 flex items-center gap-2 text-red-600 dark:text-red-400"
                                onClick={() => { onCloseAll(); setContextMenu(null); }}
                            >
                                <X size={14} />
                                {t('ui.session.closeAll')}
                            </button>
                        </>
                    )}
                </div>
            )}
        </div>
    );
};

export default SessionTabs;


