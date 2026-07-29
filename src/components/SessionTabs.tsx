// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useRef, useCallback } from 'react';
import { X, Plus, Loader2, Wifi, WifiOff, Database, Cloud, CloudOff, Server, Lock, ShieldCheck, Folder, Flame } from 'lucide-react';
import { FtpSession, SessionStatus, ProviderType, isOAuthProvider, isFourSharedProvider } from '../types';
import type { LocalTab } from '../types/aerofile';
import { MegaLogo, BoxLogo, PCloudLogo, AzureLogo, FilenLogo, FourSharedLogo, ZohoWorkDriveLogo, InternxtLogo, KDriveLogo, JottacloudLogo, DrimeCloudLogo, FileLuLogo, KoofrLogo, OpenDriveLogo, YandexDiskLogo, GitHubLogo, GitLabLogo, ImmichLogo, PROVIDER_LOGOS } from './ProviderLogos';
import { useTranslation } from '../i18n';
import { getGitHubConnectionBadge, getMegaConnectionBadge } from '../utils/providerConnectionMeta';
import { middleClickClose } from '../utils/middleClick';

interface CloudTabState {
    enabled: boolean;
    syncing: boolean;
    active: boolean;  // background sync running
    paused?: boolean; // user paused: config retained, worker stopped
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
    // Local path tabs (AeroFile)
    localTabs?: LocalTab[];
    activeLocalTabId?: string | null;
    onLocalTabClick?: (tabId: string) => void;
    onLocalTabClose?: (tabId: string) => void;
    onLocalNewTab?: () => void;
    onLocalReorder?: (tabs: LocalTab[]) => void;
    maxLocalTabs?: number;
    localTabs2?: LocalTab[];
    activeLocalTabId2?: string | null;
    onLocalTabClick2?: (tabId: string) => void;
    onLocalTabClose2?: (tabId: string) => void;
    onLocalNewTab2?: () => void;
    onLocalReorder2?: (tabs: LocalTab[]) => void;
    showDualLocalTabs?: boolean;
    // Lock tabs during active transfers to prevent accidental session switch
    transferLocked?: boolean;
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
    return protocol !== undefined && ['s3', 'webdav', 'googledrive', 'dropbox', 'onedrive', 'mega', 'sftp', 'box', 'pcloud', 'azure', 'filen', 'fourshared', 'zohoworkdrive', 'internxt', 'kdrive', 'jottacloud', 'drime', 'filelu', 'koofr', 'opendrive', 'yandexdisk', 'github', 'gitlab', 'immich', 'imagekit', 'uploadcare', 'cloudinary', 'backblaze'].includes(protocol);
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
    if (protocol && PROVIDER_LOGOS[protocol]) {
        const LogoComponent = PROVIDER_LOGOS[protocol];
        return <span className={opacityClass}><LogoComponent size={size} /></span>;
    }

    // No brand mark is redrawn below: `PROVIDER_LOGOS` above already answers for
    // every provider that has one, and the copies that used to live here for
    // Google Drive, Dropbox and OneDrive were unreachable *and* stale, still the
    // pre-2026 marks the refresh in #347 replaced. What is left is the protocol
    // glyphs, which have no canonical logo to defer to.
    switch (protocol) {
        case 'webdav':
            return <Cloud size={size} className={`${combinedClass} text-orange-500`} />;
        case 's3':
            return <Database size={size} className={`${combinedClass} text-amber-600`} />;
        case 'mega':
            return <span className={opacityClass}><MegaLogo size={size} /></span>;
        case 'box':
            return <span className={opacityClass}><BoxLogo size={size} /></span>;
        case 'pcloud':
            return <span className={opacityClass}><PCloudLogo size={size} /></span>;
        case 'azure':
            return <span className={opacityClass}><AzureLogo size={size} /></span>;
        case 'filen':
            return <span className={opacityClass}><FilenLogo size={size} /></span>;
        case 'fourshared':
            return <span className={opacityClass}><FourSharedLogo size={size} /></span>;
        case 'zohoworkdrive':
            return <span className={opacityClass}><ZohoWorkDriveLogo size={size} /></span>;
        case 'internxt':
            return <span className={opacityClass}><InternxtLogo size={size} /></span>;
        case 'kdrive':
            return <span className={opacityClass}><KDriveLogo size={size} /></span>;
        case 'jottacloud':
            return <span className={opacityClass}><JottacloudLogo size={size} /></span>;
        case 'drime':
            return <span className={opacityClass}><DrimeCloudLogo size={size} /></span>;
        case 'filelu':
            return <span className={opacityClass}><FileLuLogo size={size} /></span>;
        case 'koofr':
            return <span className={opacityClass}><KoofrLogo size={size} /></span>;
        case 'opendrive':
            return <span className={opacityClass}><OpenDriveLogo size={size} /></span>;
        case 'yandexdisk':
            return <span className={opacityClass}><YandexDiskLogo size={size} /></span>;
        case 'github':
            return <span className={opacityClass}><GitHubLogo size={size} /></span>;
        case 'gitlab':
            return <span className={opacityClass}><GitLabLogo size={size} /></span>;
        case 'immich':
            return <span className={opacityClass}><ImmichLogo size={size} /></span>;
        case 'backblaze':
            return <Flame size={size} className={`${combinedClass} text-red-600`} />;
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
        case 'internxt': return 'text-blue-600';
        case 'kdrive': return 'text-blue-500';
        case 'jottacloud': return 'text-purple-500';
        case 'drime': return 'text-green-500';
        case 'filelu': return 'text-sky-500';
        case 'koofr': return 'text-green-500';
        case 'opendrive': return 'text-cyan-500';
        case 'yandexdisk': return 'text-yellow-500';
        case 'github': return 'text-gray-400';
        case 'gitlab': return 'text-orange-500';
        case 'immich': return 'text-indigo-500';
        case 'backblaze': return 'text-red-600';
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
    localTabs = [],
    activeLocalTabId,
    onLocalTabClick,
    onLocalTabClose,
    onLocalNewTab,
    onLocalReorder,
    maxLocalTabs = 12,
    localTabs2 = [],
    activeLocalTabId2,
    onLocalTabClick2,
    onLocalTabClose2,
    onLocalNewTab2,
    showDualLocalTabs = false,
    transferLocked = false,
}) => {
    const t = useTranslation();
    const statusConfig = createStatusConfig(t);
    const showTabs = sessions.length > 0 || cloudTab?.enabled || onLocalNewTab;

    // Drag-to-reorder state
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);
    const dragNodeRef = useRef<HTMLDivElement | null>(null);

    // Context menu state (sessions)
    const [contextMenu, setContextMenu] = useState<{ x: number; y: number; sessionId: string } | null>(null);
    const contextMenuRef = useRef<HTMLDivElement | null>(null);

    // Context menu state (local tabs)
    const [localCtxMenu, setLocalCtxMenu] = useState<{ x: number; y: number; tabId: string } | null>(null);
    const localCtxMenuRef = useRef<HTMLDivElement | null>(null);

    // Close context menus on outside click
    React.useEffect(() => {
        if (!contextMenu && !localCtxMenu) return;
        const handleClick = (e: MouseEvent) => {
            if (contextMenu && contextMenuRef.current && !contextMenuRef.current.contains(e.target as Node)) {
                setContextMenu(null);
            }
            if (localCtxMenu && localCtxMenuRef.current && !localCtxMenuRef.current.contains(e.target as Node)) {
                setLocalCtxMenu(null);
            }
        };
        document.addEventListener('mousedown', handleClick);
        return () => document.removeEventListener('mousedown', handleClick);
    }, [contextMenu, localCtxMenu]);

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
        <div className="aero-tabstrip flex items-center gap-1 px-3 py-2 bg-gray-100 dark:bg-gray-800 border-b border-gray-200 dark:border-gray-700 flex-shrink-0 overflow-x-auto overflow-y-hidden">
            {/* Cloud Tab - Special tab for AeroCloud */}
            {cloudTab?.enabled && (
                <div
                    className={`group flex items-center gap-2 px-3 py-1.5 rounded-lg cursor-pointer transition-all min-w-0 max-w-[200px] ${cloudTab.syncing
                        ? 'bg-gradient-to-r from-cyan-500/20 to-blue-500/20 dark:from-cyan-900/40 dark:to-blue-900/40 border border-cyan-400/30'
                        : cloudTab.paused
                            ? 'bg-amber-500/10 dark:bg-amber-900/30 border border-amber-400/30'
                            : cloudTab.active
                                ? 'bg-gradient-to-r from-cyan-500/20 to-blue-500/20 dark:from-cyan-900/40 dark:to-blue-900/40 border border-cyan-400/30'
                                : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
                        }`}
                    onClick={onCloudTabClick}
                    title={cloudTab.syncing ? t('ui.session.syncing') : cloudTab.paused ? t('cloud.paused') : cloudTab.active ? t('ui.session.backgroundSyncActive') : t('ui.session.aerocloudClickToOpen')}
                >
                    {/* Cloud status indicator */}
                    <span className={`shrink-0 ${cloudTab.syncing
                        ? 'text-cyan-500 animate-pulse'
                        : cloudTab.paused
                            ? 'text-amber-500'
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
                    <span className={`truncate text-sm ${cloudTab.syncing
                        ? 'font-medium text-cyan-700 dark:text-cyan-300'
                        : cloudTab.paused
                            ? 'font-medium text-amber-700 dark:text-amber-300'
                            : cloudTab.active
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
                // Badge computation kept for future colored-dot indicator
                const _gitHubBadge = protocol === 'github'
                    ? getGitHubConnectionBadge(session.connectionParams?.options)
                    : null;
                const _megaBadge = protocol === 'mega'
                    ? getMegaConnectionBadge(session.connectionParams?.options)
                    : null;
                const isDragTarget = overIdx === idx && dragIdx !== null && dragIdx !== idx;
                const isLocked = transferLocked && !isActive;

                return (
                    <div
                        key={session.id}
                        draggable={!!onReorder && !isLocked}
                        onDragStart={(e) => { if (isLocked) { e.preventDefault(); return; } handleTabDragStart(e, idx); }}
                        onDragOver={(e) => handleTabDragOver(e, idx)}
                        onDrop={(e) => handleTabDrop(e, idx)}
                        onDragEnd={handleTabDragEnd}
                        className={`group flex items-center gap-2 px-3 py-1.5 rounded-lg transition-all min-w-0 max-w-[200px] ${isActive
                            ? 'bg-white dark:bg-gray-700 shadow-sm cursor-pointer'
                            : isLocked
                                ? 'opacity-40 cursor-not-allowed'
                                : 'hover:bg-gray-200 dark:hover:bg-gray-700/50 cursor-pointer'
                            } ${dragIdx === idx ? 'scale-95' : ''} ${isDragTarget ? 'border-l-2 border-blue-500' : ''}`}
                        onClick={() => { if (!isLocked) onTabClick(session.id); }}
                        // Middle click closes the tab (Ehud #274), on the ✖ and
                        // anywhere else on the chip. Never while the tab is locked
                        // by a running transfer, which is what blocks the ✖ too.
                        {...middleClickClose(() => onTabClose(session.id), !isLocked)}
                        onContextMenu={(e) => {
                            e.preventDefault();
                            if (!isLocked) setContextMenu({ x: e.clientX, y: e.clientY, sessionId: session.id });
                        }}
                        title={isLocked ? t('ui.session.transferInProgress') : undefined}
                    >
                        {/* Status/Provider indicator */}
                        <span
                            className={`shrink-0 ${isProvider ? getProviderColor(protocol) : status.color}`}
                            title={isLocked ? undefined : `${isProvider ? protocol?.toUpperCase() : 'FTP'} - ${status.title}`}
                        >
                            {session.status === 'connecting' ? (
                                <Loader2 size={14} className="animate-spin" />
                            ) : session.customIconUrl ? (
                                <img src={session.customIconUrl} alt="" className="w-3.5 h-3.5 rounded-sm object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
                            ) : session.faviconUrl ? (
                                <img src={session.faviconUrl} alt="" className="w-3.5 h-3.5 rounded-sm object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
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

                        {/* Mode badges disabled in tabs: will be replaced by colored dot indicator */}

                        {/* Close button: hidden during transfer lock */}
                        {!isLocked && (
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
                        )}
                    </div>
                );
            })}

            {/* New tab button */}
            <button
                onClick={onNewTab}
                className="shrink-0 p-1.5 ml-1.5 rounded-lg border border-dashed border-gray-300 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-700 hover:border-gray-400 dark:hover:border-gray-500 transition-colors text-gray-400 dark:text-gray-500"
                title={t('ui.session.newConnection')}
            >
                <Plus size={13} />
            </button>

            {/* AeroFile local tabs: right-aligned */}
            {onLocalNewTab && (
                <>
                    <div className="flex-1" />
                    {(sessions.length > 0 || cloudTab?.enabled) && localTabs.length > 0 && (
                        <div className="w-px h-5 bg-gray-300 dark:bg-gray-600 mx-1" />
                    )}
                    {localTabs.map((tab) => {
                        const isActive = tab.id === activeLocalTabId;
                        const canClose = localTabs.length > 1;
                        return (
                            <div
                                key={tab.id}
                                className={`group flex items-center gap-1.5 pl-2.5 pr-1.5 py-1.5 rounded-lg cursor-pointer transition-all min-w-0 max-w-[180px] ${
                                    isActive
                                        ? 'bg-white dark:bg-gray-700 shadow-sm'
                                        : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
                                }`}
                                onClick={() => onLocalTabClick?.(tab.id)}
                                {...middleClickClose(() => onLocalTabClose?.(tab.id), canClose)}
                                onContextMenu={(e) => {
                                    e.preventDefault();
                                    setLocalCtxMenu({ x: e.clientX, y: e.clientY, tabId: tab.id });
                                }}
                                title={tab.path}
                            >
                                {showDualLocalTabs && (
                                    <span className={`shrink-0 px-1 rounded text-[10px] font-semibold ${isActive ? 'bg-blue-500 text-white' : 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300'}`}>
                                        L
                                    </span>
                                )}
                                <Folder size={12} className={`shrink-0 ${isActive ? 'text-amber-500' : 'text-gray-400'}`} />
                                <span className={`truncate text-sm ${isActive ? 'font-medium' : 'text-gray-600 dark:text-gray-400'}`}>
                                    {tab.label || '/'}
                                </span>
                                <button
                                    onClick={(e) => { e.stopPropagation(); if (canClose) onLocalTabClose?.(tab.id); }}
                                    disabled={!canClose}
                                    className={`shrink-0 p-0.5 rounded transition-opacity ${canClose ? 'hover:bg-gray-300 dark:hover:bg-gray-600 opacity-0 group-hover:opacity-100' : 'opacity-20 cursor-default'}`}
                                >
                                    <X size={10} />
                                </button>
                            </div>
                        );
                    })}
                    {localTabs.length < maxLocalTabs && (
                        <button
                            onClick={onLocalNewTab}
                            className="shrink-0 p-1.5 ml-1.5 rounded-lg border border-dashed border-gray-300 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-700 hover:border-gray-400 dark:hover:border-gray-500 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors flex items-center gap-1"
                            title={t('localTabs.newTab')}
                        >
                            <Plus size={13} />
                            {localTabs.length === 0 && <span className="text-xs">{t('localTabs.newTab')}</span>}
                        </button>
                    )}
                    {showDualLocalTabs && (
                        <>
                            <div className="w-px h-5 bg-gray-300 dark:bg-gray-600 mx-1" />
                            {localTabs2.map((tab) => {
                                const isActive = tab.id === activeLocalTabId2;
                                const canClose = localTabs2.length > 1;
                                return (
                                    <div
                                        key={tab.id}
                                        className={`group flex items-center gap-1.5 pl-2.5 pr-1.5 py-1.5 rounded-lg cursor-pointer transition-all min-w-0 max-w-[180px] ${
                                            isActive
                                                ? 'bg-white dark:bg-gray-700 shadow-sm'
                                                : 'hover:bg-gray-200 dark:hover:bg-gray-700/50'
                                        }`}
                                        onClick={() => onLocalTabClick2?.(tab.id)}
                                        {...middleClickClose(() => onLocalTabClose2?.(tab.id), canClose)}
                                        onContextMenu={(e) => e.preventDefault()}
                                        title={tab.path}
                                    >
                                        <span className={`shrink-0 px-1 rounded text-[10px] font-semibold ${isActive ? 'bg-amber-500 text-white' : 'bg-amber-100 text-amber-700 dark:bg-amber-900/40 dark:text-amber-300'}`}>
                                            R
                                        </span>
                                        <Folder size={12} className={`shrink-0 ${isActive ? 'text-amber-500' : 'text-gray-400'}`} />
                                        <span className={`truncate text-sm ${isActive ? 'font-medium' : 'text-gray-600 dark:text-gray-400'}`}>
                                            {tab.label || '/'}
                                        </span>
                                        <button
                                            onClick={(e) => { e.stopPropagation(); if (canClose) onLocalTabClose2?.(tab.id); }}
                                            disabled={!canClose}
                                            className={`shrink-0 p-0.5 rounded transition-opacity ${canClose ? 'hover:bg-gray-300 dark:hover:bg-gray-600 opacity-0 group-hover:opacity-100' : 'opacity-20 cursor-default'}`}
                                        >
                                            <X size={10} />
                                        </button>
                                    </div>
                                );
                            })}
                            {localTabs2.length < maxLocalTabs && (
                                <button
                                    onClick={onLocalNewTab2}
                                    className="shrink-0 p-1.5 ml-1.5 rounded-lg border border-dashed border-gray-300 dark:border-gray-600 hover:bg-gray-200 dark:hover:bg-gray-700 hover:border-gray-400 dark:hover:border-gray-500 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors flex items-center gap-1"
                                    title={t('localTabs.newTab')}
                                >
                                    <Plus size={13} />
                                    {localTabs2.length === 0 && <span className="text-xs">{t('localTabs.newTab')}</span>}
                                </button>
                            )}
                        </>
                    )}
                </>
            )}

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

            {/* Local tab context menu */}
            {localCtxMenu && (
                <div
                    ref={localCtxMenuRef}
                    className="fixed z-[9999] bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl py-1 min-w-[180px]"
                    style={{ left: localCtxMenu.x, top: localCtxMenu.y }}
                >
                    {localTabs.length > 1 && (
                        <>
                            <button
                                className="w-full px-3 py-1.5 text-sm text-left hover:bg-gray-100 dark:hover:bg-gray-700 flex items-center gap-2 text-gray-700 dark:text-gray-300"
                                onClick={() => { onLocalTabClose?.(localCtxMenu.tabId); setLocalCtxMenu(null); }}
                            >
                                <X size={14} />
                                {t('localTabs.closeTab')}
                            </button>
                            {localTabs.length > 2 && (
                                <button
                                    className="w-full px-3 py-1.5 text-sm text-left hover:bg-gray-100 dark:hover:bg-gray-700 flex items-center gap-2 text-gray-700 dark:text-gray-300"
                                    onClick={() => {
                                        localTabs.filter(tab => tab.id !== localCtxMenu.tabId).forEach(tab => onLocalTabClose?.(tab.id));
                                        setLocalCtxMenu(null);
                                    }}
                                >
                                    <X size={14} />
                                    {t('localTabs.closeOthers')}
                                </button>
                            )}
                            <div className="border-t border-gray-200 dark:border-gray-700 my-1" />
                            <button
                                className="w-full px-3 py-1.5 text-sm text-left hover:bg-red-50 dark:hover:bg-red-900/20 flex items-center gap-2 text-red-600 dark:text-red-400"
                                onClick={() => {
                                    // Close all except the first tab
                                    localTabs.slice(1).forEach(tab => onLocalTabClose?.(tab.id));
                                    setLocalCtxMenu(null);
                                }}
                            >
                                <X size={14} />
                                {t('localTabs.closeAll')}
                            </button>
                        </>
                    )}
                    {localTabs.length <= 1 && (
                        <div className="px-3 py-1.5 text-sm text-gray-400 dark:text-gray-500 italic">
                            {t('localTabs.closeTab')}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};

export default SessionTabs;
