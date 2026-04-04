import * as React from 'react';
import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Plus, Server as ServerIcon, Play, Edit2, Copy, Trash2, Activity, Star } from 'lucide-react';
import { ServerProfile, ConnectionParams, ProviderType, isOAuthProvider, isFourSharedProvider } from '../../types';
import { MyServersViewMode, MyServersFilterBy, FILTER_CHIPS } from '../../types/catalog';
import { MyServersToolbar } from './MyServersToolbar';
import { ServerCard } from './ServerCard';
import { useTranslation } from '../../i18n';
import { ContextMenu, useContextMenu } from '../ContextMenu';
import type { ContextMenuItem } from '../ContextMenu';
import { secureGetWithFallback, secureStoreAndClean } from '../../utils/secureStorage';
import { getProviderById } from '../../providers';
import { logger } from '../../utils/logger';
import { ServerHealthCheck } from '../ServerHealthCheck';

const STORAGE_KEY = 'aeroftp-saved-servers';
const VIEW_MODE_KEY = 'aeroftp-intro-view-mode';

/** Load credential from vault with retry if store not ready */
const getCredentialWithRetry = async (account: string, maxRetries = 3): Promise<string> => {
    for (let attempt = 0; attempt < maxRetries; attempt++) {
        try {
            return await invoke<string>('get_credential', { account });
        } catch (err) {
            const errorMsg = String(err);
            if (errorMsg.includes('STORE_NOT_READY') && attempt < maxRetries - 1) {
                await new Promise(resolve => setTimeout(resolve, 200 * (attempt + 1)));
                continue;
            }
            throw err;
        }
    }
    throw new Error('Failed to get credential after retries');
};

function deriveProviderId(server: ServerProfile): string | undefined {
    const proto = server.protocol;
    if (!proto) return undefined;
    if (['mega', 'box', 'pcloud', 'azure', 'filen', 'internxt', 'kdrive', 'drime', 'filelu', 'koofr', 'opendrive', 'yandexdisk', 'googledrive', 'dropbox', 'onedrive', 'fourshared', 'zohoworkdrive', 'github', 'gitlab'].includes(proto)) return proto;
    const host = (server.host || '').toLowerCase();
    if (proto === 's3') {
        if (host.includes('backblaze')) return 'backblaze';
        if (host.includes('r2.cloudflarestorage')) return 'cloudflare-r2';
        if (host.includes('wasabi')) return 'wasabi';
        if (host.includes('idrive')) return 'idrive-e2';
        if (host.includes('storj')) return 'storj';
        if (host.includes('mega.io') || host.includes('mega.nz')) return 'mega-s4';
        if (host.includes('amazonaws.com')) return 'amazon-s3';
        if (host.includes('aliyuncs.com')) return 'alibaba-oss';
        if (host.includes('myqcloud.com')) return 'tencent-cos';
        if (host.includes('oraclecloud')) return 'oracle-cloud';
        if (host.includes('digitaloceanspaces')) return 'digitalocean-spaces';
        if (host.includes('storage.yandex')) return 'yandex-storage';
        if (host.includes('filelu')) return 'filelu-s5';
    }
    if (proto === 'webdav') {
        if (host.includes('koofr')) return 'koofr-webdav';
        if (host.includes('nextcloud') || host.includes('cloud.')) return 'nextcloud';
        if (host.includes('seafile')) return 'seafile';
        if (host.includes('jianguoyun')) return 'jianguoyun';
        if (host.includes('cloudme')) return 'cloudme';
        if (host.includes('drivehq')) return 'drivehq';
        if (host.includes('infini-cloud') || host.includes('teracloud')) return 'infinicloud';
        if (host.includes('filelu')) return 'filelu-webdav';
        if (host.includes('felicloud')) return 'felicloud-webdav';
    }
    return undefined;
}

function getSavedServers(): ServerProfile[] {
    try {
        const stored = localStorage.getItem(STORAGE_KEY);
        if (!stored) return [];
        const servers: ServerProfile[] = JSON.parse(stored);
        let migrated = false;
        for (const s of servers) {
            if (!s.providerId) {
                const derived = deriveProviderId(s);
                if (derived) { s.providerId = derived; migrated = true; }
            }
        }
        if (migrated) localStorage.setItem(STORAGE_KEY, JSON.stringify(servers));
        return servers;
    } catch {
        return [];
    }
}

interface MyServersPanelProps {
    onConnect: (params: ConnectionParams, initialPath?: string, localInitialPath?: string) => void | Promise<void>;
    onEdit: (profile: ServerProfile) => void;
    onQuickConnect: () => void;
    lastUpdate?: number;
    onOpenExportImport?: () => void;
}

export function MyServersPanel({
    onConnect,
    onEdit,
    onQuickConnect,
    lastUpdate,
    onOpenExportImport,
}: MyServersPanelProps) {
    const t = useTranslation();
    const [servers, setServers] = useState<ServerProfile[]>([]);
    const [connectingId, setConnectingId] = useState<string | null>(null);
    const [oauthConnecting, setOauthConnecting] = useState<string | null>(null);
    const [searchQuery, setSearchQuery] = useState('');
    const [activeFilter, setActiveFilter] = useState<MyServersFilterBy>(() => {
        const stored = localStorage.getItem('aeroftp_myservers_filter');
        return (stored as MyServersFilterBy) || 'all';
    });
    const [viewMode, setViewMode] = useState<MyServersViewMode>(() => {
        const stored = localStorage.getItem(VIEW_MODE_KEY);
        return (stored === 'list' ? 'list' : 'grid') as MyServersViewMode;
    });
    const [credentialsMasked, setCredentialsMasked] = useState(true);
    const [healthCheckTarget, setHealthCheckTarget] = useState<string | false>(false);
    // Drag & reorder
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);
    const [favorites, setFavorites] = useState<Set<string>>(() => {
        try {
            const stored = localStorage.getItem('aeroftp-favorite-servers');
            return stored ? new Set(JSON.parse(stored)) : new Set();
        } catch { return new Set(); }
    });
    const { state: contextMenuState, show: showContextMenu, hide: hideContextMenu } = useContextMenu();

    const scrollTimeout = useRef<number | null>(null);
    const scrollContainerRef = useRef<HTMLDivElement>(null);
    // Passive scroll listener — runs on compositor thread, no React re-renders
    useEffect(() => {
        const el = scrollContainerRef.current;
        if (!el) return;
        const onScroll = () => {
            if (!el.classList.contains('is-scrolling')) el.classList.add('is-scrolling');
            if (scrollTimeout.current) window.clearTimeout(scrollTimeout.current);
            scrollTimeout.current = window.setTimeout(() => el.classList.remove('is-scrolling'), 600);
        };
        el.addEventListener('scroll', onScroll, { passive: true });
        return () => el.removeEventListener('scroll', onScroll);
    }, [viewMode]); // re-attach when container swaps between grid/list

    useEffect(() => {
        setServers(getSavedServers());
    }, [lastUpdate]);

    useEffect(() => {
        localStorage.setItem(VIEW_MODE_KEY, viewMode);
    }, [viewMode]);

    const toggleFavorite = useCallback((serverId: string) => {
        setFavorites(prev => {
            const next = new Set(prev);
            if (next.has(serverId)) next.delete(serverId);
            else next.add(serverId);
            localStorage.setItem('aeroftp-favorite-servers', JSON.stringify([...next]));
            return next;
        });
    }, []);

    const handleToggleFavorite = useCallback((s: ServerProfile) => toggleFavorite(s.id), [toggleFavorite]);

    // Drag & reorder: only works on full list (no search/filter active)
    const canDrag = !searchQuery.trim() && activeFilter === 'all';

    const handleDragStart = useCallback((idx: number) => (e: React.DragEvent) => {
        setDragIdx(idx);
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', idx.toString());
    }, []);

    const handleDragEnter = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        setOverIdx(idx);
    }, []);

    const handleDragOver = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        setOverIdx(idx);
    }, []);

    const handleDrop = useCallback((idx: number) => (e: React.DragEvent) => {
        e.preventDefault();
        if (dragIdx === null || dragIdx === idx) { setDragIdx(null); setOverIdx(null); return; }
        // Reorder
        const updated = [...servers];
        const [moved] = updated.splice(dragIdx, 1);
        updated.splice(idx, 0, moved);
        setServers(updated);
        secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});
        setDragIdx(null);
        setOverIdx(null);
    }, [dragIdx, servers]);

    const handleDragEnd = useCallback(() => {
        setDragIdx(null);
        setOverIdx(null);
    }, []);

    const filteredServers = useMemo(() => {
        let result = servers;
        if (searchQuery.trim()) {
            const q = searchQuery.toLowerCase();
            result = result.filter(s =>
                (s.name || '').toLowerCase().includes(q) ||
                (s.host || '').toLowerCase().includes(q) ||
                (s.protocol || '').toLowerCase().includes(q) ||
                (s.username || '').toLowerCase().includes(q)
            );
        }
        if (activeFilter === 'favorites') {
            result = result.filter(s => favorites.has(s.id));
        } else if (activeFilter !== 'all') {
            const chip = FILTER_CHIPS.find(c => c.id === activeFilter);
            if (chip) {
                result = result.filter(s => chip.matchFn(s.protocol || 'ftp', s.providerId));
            }
        }
        return result;
    }, [servers, searchQuery, activeFilter, favorites]);

    // Chip counts (computed once from full server list, not filtered)
    const chipCounts = useMemo(() => {
        const counts: Record<MyServersFilterBy, number> = {
            all: servers.length,
            ftp: 0, s3: 0, webdav: 0, cloud: 0, dev: 0, favorites: 0,
        };
        for (const s of servers) {
            const p = s.protocol || 'ftp';
            for (const chip of FILTER_CHIPS) {
                if (chip.id === 'all') continue;
                if (chip.id === 'favorites') {
                    if (favorites.has(s.id)) counts.favorites++;
                } else if (chip.matchFn(p, s.providerId)) {
                    counts[chip.id]++;
                }
            }
        }
        return counts;
    }, [servers, favorites]);

    // Connection handler - full logic from original SavedServers.tsx
    // Handles OAuth2, 4shared OAuth1, and standard credential-based connections
    const handleConnect = useCallback(async (server: ServerProfile) => {
        if (connectingId) return;
        setConnectingId(server.id);

        // OAuth2 providers (Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho, kDrive)
        if (server.protocol && isOAuthProvider(server.protocol)) {
            let credentials: { clientId: string; clientSecret: string } | null = null;
            try {
                const clientId = await getCredentialWithRetry(`oauth_${server.protocol}_client_id`);
                const clientSecret = await getCredentialWithRetry(`oauth_${server.protocol}_client_secret`);
                if (clientId && clientSecret) credentials = { clientId, clientSecret };
            } catch { /* not found */ }

            if (!credentials) {
                setConnectingId(null);
                return;
            }

            setOauthConnecting(server.id);
            try {
                const oauthProvider = server.protocol === 'googledrive' ? 'google_drive' : server.protocol;
                let region: string | undefined;
                if (server.protocol === 'zohoworkdrive') {
                    region = server.options?.region;
                    if (!region) {
                        try { region = await invoke<string>('get_credential', { account: `oauth_${server.protocol}_region` }); } catch { /* default */ }
                    }
                }
                const params = { provider: oauthProvider, client_id: credentials.clientId, client_secret: credentials.clientSecret, ...(region && { region }) };

                const hasTokens = await invoke<boolean>('oauth2_has_tokens', { provider: oauthProvider });
                if (!hasTokens) await invoke('oauth2_full_auth', { params });

                let result: { display_name: string; account_email: string | null };
                try {
                    result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params });
                } catch (connectErr) {
                    const errMsg = connectErr instanceof Error ? connectErr.message : String(connectErr);
                    const lower = errMsg.toLowerCase();
                    if (lower.includes('token expired') || (lower.includes('token') && lower.includes('refresh')) || lower.includes('authentication failed') || (lower.includes('invalid') && lower.includes('access_token'))) {
                        await invoke('oauth2_full_auth', { params });
                        result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params });
                    } else throw connectErr;
                }

                const updatedUsername = result.account_email || server.username;
                const updated = servers.map(s => s.id === server.id ? { ...s, lastConnected: new Date().toISOString(), username: updatedUsername || s.username } : s);
                setServers(updated);
                secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});

                await onConnect({ server: result.display_name, username: updatedUsername, password: '', protocol: server.protocol, displayName: server.name, providerId: server.providerId }, server.initialPath, server.localInitialPath);
            } catch (e) {
                logger.error('OAuth connection failed', e);
            } finally {
                setOauthConnecting(null);
                setConnectingId(null);
            }
            return;
        }

        // 4shared OAuth 1.0
        if (server.protocol && isFourSharedProvider(server.protocol)) {
            let consumerKey = '', consumerSecret = '';
            try {
                consumerKey = await getCredentialWithRetry('oauth_fourshared_client_id');
                consumerSecret = await getCredentialWithRetry('oauth_fourshared_client_secret');
            } catch { /* ignore */ }
            if (!consumerKey || !consumerSecret) { setConnectingId(null); return; }

            setOauthConnecting(server.id);
            try {
                const params = { consumer_key: consumerKey, consumer_secret: consumerSecret };
                const hasTokens = await invoke<boolean>('fourshared_has_tokens');
                if (!hasTokens) await invoke('fourshared_full_auth', { params });

                let result: { display_name: string; account_email: string | null };
                try {
                    result = await invoke<{ display_name: string; account_email: string | null }>('fourshared_connect', { params });
                } catch {
                    await invoke('fourshared_full_auth', { params });
                    result = await invoke<{ display_name: string; account_email: string | null }>('fourshared_connect', { params });
                }

                const updatedUsername = result.account_email || server.username;
                const updated = servers.map(s => s.id === server.id ? { ...s, lastConnected: new Date().toISOString(), username: updatedUsername || s.username } : s);
                setServers(updated);
                secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});

                await onConnect({ server: result.display_name, username: updatedUsername, password: '', protocol: server.protocol, displayName: server.name, providerId: server.providerId }, server.initialPath, server.localInitialPath);
            } catch (e) {
                logger.error('4shared connection failed', e);
            } finally {
                setOauthConnecting(null);
                setConnectingId(null);
            }
            return;
        }

        // Non-OAuth: standard credential-based connection
        try {
            const updated = servers.map(s => s.id === server.id ? { ...s, lastConnected: new Date().toISOString() } : s);
            setServers(updated);
            secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});

            // Load password from credential vault with retry
            let password = '';
            try {
                password = await getCredentialWithRetry(`server_${server.id}`);
            } catch { /* not found */ }

            // Build connection params - for provider protocols, use host only (no port append)
            const proto = server.protocol || 'ftp';
            const isProviderProtocol = ['s3', 'webdav', 'sftp', 'mega', 'filelu', 'koofr', 'yandexdisk', 'github', 'gitlab', 'opendrive', 'internxt', 'filen', 'drime', 'jottacloud', 'kdrive', 'swift'].includes(proto);
            const defaultPort = proto === 'sftp' ? 22 : proto === 'ftps' ? 990 : 21;
            const serverString = server.host;

            await onConnect({
                server: serverString,
                username: server.username,
                password,
                protocol: proto,
                port: server.port,
                displayName: server.name,
                options: server.options,
                providerId: server.providerId,
            }, server.initialPath, server.localInitialPath);
        } catch (e) {
            logger.error('Connection failed', e);
        } finally {
            setConnectingId(null);
        }
    }, [servers, connectingId, onConnect, t]);

    const handleDuplicate = useCallback((server: ServerProfile) => {
        const dup: ServerProfile = {
            ...server,
            id: `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
            name: `${server.name} (copy)`,
            lastConnected: undefined,
        };
        const updated = [dup, ...servers];
        setServers(updated);
        secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});
    }, [servers]);

    const handleDelete = useCallback((server: ServerProfile) => {
        const updated = servers.filter(s => s.id !== server.id);
        setServers(updated);
        secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});
    }, [servers]);

    const handleContextMenu = useCallback((e: React.MouseEvent, server: ServerProfile) => {
        const isFav = favorites.has(server.id);
        const items: ContextMenuItem[] = [
            { label: t('common.connect'), icon: <Play size={14} />, action: () => handleConnect(server) },
            { label: t('common.edit'), icon: <Edit2 size={14} />, action: () => onEdit(server) },
            { label: t('common.duplicate'), icon: <Copy size={14} />, action: () => handleDuplicate(server) },
            { label: isFav ? t('introHub.removeFavorite') : t('introHub.addFavorite'), icon: <Star size={14} />, action: () => toggleFavorite(server.id) },
            { label: t('healthCheck.title'), icon: <Activity size={14} />, action: () => setHealthCheckTarget(server.id), divider: true },
            { label: t('common.delete'), icon: <Trash2 size={14} />, action: () => handleDelete(server), danger: true },
        ];
        showContextMenu(e, items);
    }, [t, handleConnect, onEdit, handleDuplicate, handleDelete, toggleFavorite, favorites, showContextMenu]);

    return (
        <div className="h-full flex flex-col">
            <MyServersToolbar
                searchQuery={searchQuery}
                onSearchChange={setSearchQuery}
                activeFilter={activeFilter}
                onFilterChange={(f: MyServersFilterBy) => { setActiveFilter(f); localStorage.setItem('aeroftp_myservers_filter', f); }}
                viewMode={viewMode}
                onViewModeChange={setViewMode}
                credentialsMasked={credentialsMasked}
                onToggleMask={() => setCredentialsMasked(prev => !prev)}
                serverCount={servers.length}
                filteredCount={filteredServers.length}
                chipCounts={chipCounts}
                onOpenExportImport={onOpenExportImport}
                onHealthCheck={() => setHealthCheckTarget('all')}
            />

            {filteredServers.length === 0 ? (
                <div className="flex-1 flex flex-col items-center justify-center text-center py-12">
                    <ServerIcon size={48} className="text-gray-300 dark:text-gray-600 mb-4" />
                    {servers.length === 0 ? (
                        <>
                            <p className="text-gray-500 dark:text-gray-400 mb-2">{t('introHub.noServers')}</p>
                            <p className="text-sm text-gray-400 dark:text-gray-500 mb-4">{t('introHub.noServersHint')}</p>
                            <button
                                onClick={onQuickConnect}
                                className="flex items-center gap-2 px-4 py-2 bg-blue-600 hover:bg-blue-500 text-white rounded-lg text-sm font-medium transition-colors"
                            >
                                <Plus size={16} />
                                {t('introHub.quickConnect')}
                            </button>
                        </>
                    ) : (
                        <>
                            <p className="text-gray-500 dark:text-gray-400 mb-1">{t('introHub.noResults')}</p>
                            <p className="text-sm text-gray-400 dark:text-gray-500">
                                {t('introHub.noResultsHint', { query: searchQuery })}
                            </p>
                        </>
                    )}
                </div>
            ) : viewMode === 'grid' ? (
                <div
                    ref={scrollContainerRef}
                    className="flex-1 overflow-y-auto pr-3 custom-scroll-area"
                    style={{ willChange: 'scroll-position', transform: 'translateZ(0)' }}
                >
                    <div className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 gap-3" style={{ contain: 'layout style' }}>
                        {filteredServers.map((server, idx) => {
                            const realIdx = canDrag ? idx : -1;
                            return (
                                <ServerCard
                                    key={server.id}
                                    server={server}
                                    isConnecting={connectingId === server.id || oauthConnecting === server.id}
                                    credentialsMasked={credentialsMasked}
                                    isFavorite={favorites.has(server.id)}
                                    onConnect={handleConnect}
                                    onEdit={onEdit}
                                    onDuplicate={handleDuplicate}
                                    onDelete={handleDelete}
                                    onToggleFavorite={handleToggleFavorite}
                                    onContextMenu={handleContextMenu}
                                    viewMode="grid"
                                    isDraggable={canDrag}
                                    isDragging={dragIdx === realIdx}
                                    isDragTarget={overIdx === realIdx && dragIdx !== null && dragIdx !== realIdx}
                                    onDragStart={canDrag ? handleDragStart(realIdx) : undefined}
                                    onDragEnter={canDrag ? handleDragEnter(realIdx) : undefined}
                                    onDragOver={canDrag ? handleDragOver(realIdx) : undefined}
                                    onDrop={canDrag ? handleDrop(realIdx) : undefined}
                                    onDragEnd={canDrag ? handleDragEnd : undefined}
                                />
                            );
                        })}
                    </div>
                </div>
            ) : (
                <div
                    ref={scrollContainerRef}
                    className="flex-1 overflow-y-auto bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700 custom-scroll-area"
                >
                    {filteredServers.map((server, idx) => {
                        const realIdx = canDrag ? idx : -1;
                        return (
                            <ServerCard
                                key={server.id}
                                server={server}
                                isConnecting={connectingId === server.id || oauthConnecting === server.id}
                                credentialsMasked={credentialsMasked}
                                isFavorite={favorites.has(server.id)}
                                onConnect={handleConnect}
                                onEdit={onEdit}
                                onDuplicate={handleDuplicate}
                                onDelete={handleDelete}
                                onToggleFavorite={handleToggleFavorite}
                                onContextMenu={handleContextMenu}
                                viewMode="list"
                                index={idx}
                                isDraggable={canDrag}
                                isDragging={dragIdx === realIdx}
                                isDragTarget={overIdx === realIdx && dragIdx !== null && dragIdx !== realIdx}
                                onDragStart={canDrag ? handleDragStart(realIdx) : undefined}
                                onDragEnter={canDrag ? handleDragEnter(realIdx) : undefined}
                                onDragOver={canDrag ? handleDragOver(realIdx) : undefined}
                                onDrop={canDrag ? handleDrop(realIdx) : undefined}
                                onDragEnd={canDrag ? handleDragEnd : undefined}
                            />
                        );
                    })}
                </div>
            )}

            {/* Context Menu */}
            {contextMenuState.visible && (
                <ContextMenu
                    x={contextMenuState.x}
                    y={contextMenuState.y}
                    items={contextMenuState.items}
                    onClose={hideContextMenu}
                />
            )}

            {/* Health Check Modal */}
            {healthCheckTarget && (
                <ServerHealthCheck
                    servers={servers}
                    onClose={() => setHealthCheckTarget(false)}
                    singleServerId={healthCheckTarget !== 'all' ? healthCheckTarget : undefined}
                />
            )}
        </div>
    );
}
