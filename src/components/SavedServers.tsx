import * as React from 'react';
import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Server, Plus, Trash2, Edit2, X, Check, FolderOpen, Cloud, AlertCircle, Clock, GripVertical, Search } from 'lucide-react';
import { ImportExportIcon } from './icons/ImportExportIcon';
import { open } from '@tauri-apps/plugin-dialog';
import { ServerProfile, ConnectionParams, ProviderType, isOAuthProvider, isFourSharedProvider } from '../types';
import { useTranslation } from '../i18n';
import { getProtocolInfo, ProtocolBadge, ProtocolIcon } from './ProtocolSelector';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { logger } from '../utils/logger';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

// OAuth settings storage key (same as SettingsPanel)
const OAUTH_SETTINGS_KEY = 'aeroftp_oauth_settings';

interface OAuthSettings {
    googledrive: { clientId: string; clientSecret: string };
    dropbox: { clientId: string; clientSecret: string };
    onedrive: { clientId: string; clientSecret: string };
    [key: string]: { clientId: string; clientSecret: string };
}

// Get OAuth settings from localStorage
const getOAuthSettings = (): OAuthSettings | null => {
    try {
        const stored = localStorage.getItem(OAUTH_SETTINGS_KEY);
        return stored ? JSON.parse(stored) : null;
    } catch {
        return null;
    }
};

// Map protocol to OAuth provider key for localStorage settings
const getOAuthProviderKey = (protocol: ProviderType): keyof OAuthSettings | null => {
    switch (protocol) {
        case 'googledrive': return 'googledrive';
        case 'dropbox': return 'dropbox';
        case 'onedrive': return 'onedrive';
        default: return null;
    }
};

// Helper: get credential with retry if vault not ready yet (race condition on app startup)
const getCredentialWithRetry = async (account: string, maxRetries = 3): Promise<string> => {
    for (let attempt = 0; attempt < maxRetries; attempt++) {
        try {
            return await invoke<string>('get_credential', { account });
        } catch (err) {
            const errorMsg = String(err);
            if (errorMsg.includes('STORE_NOT_READY') && attempt < maxRetries - 1) {
                // Vault not initialized yet, wait and retry
                await new Promise(resolve => setTimeout(resolve, 200 * (attempt + 1)));
                continue;
            }
            throw err;
        }
    }
    throw new Error('Failed to get credential after retries');
};

// Load OAuth credentials from credential vault
const loadOAuthCredentials = async (provider: string): Promise<{ clientId: string; clientSecret: string } | null> => {
    try {
        const clientId = await getCredentialWithRetry(`oauth_${provider}_client_id`);
        const clientSecret = await getCredentialWithRetry(`oauth_${provider}_client_secret`);
        if (clientId && clientSecret) {
            return { clientId, clientSecret };
        }
    } catch {
        // Not found in vault
    }
    return null;
};

interface SavedServersProps {
    onConnect: (params: ConnectionParams, initialPath?: string, localInitialPath?: string) => void;
    currentProfile?: ServerProfile; // For highlighting active connection
    className?: string;
    onEdit: (profile: ServerProfile) => void;
    lastUpdate?: number;
    onOpenExportImport?: () => void;
}

const STORAGE_KEY = 'aeroftp-saved-servers';

// Generate a unique ID
const generateId = () => `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

// Derive providerId from protocol/host for legacy servers without it
const deriveProviderId = (server: ServerProfile): string | undefined => {
    const proto = server.protocol;
    if (!proto) return undefined;
    // Native providers map directly
    if (['mega', 'box', 'pcloud', 'azure', 'filen', 'googledrive', 'dropbox', 'onedrive'].includes(proto)) return proto;
    const host = (server.host || '').toLowerCase();
    if (proto === 's3') {
        if (host.includes('cloudflarestorage')) return 'cloudflare-r2';
        if (host.includes('backblazeb2')) return 'backblaze';
        if (host.includes('wasabisys')) return 'wasabi';
        if (host.includes('storjshare') || host.includes('gateway.storj')) return 'storj';
        if (host.includes('digitaloceanspaces')) return 'digitalocean-spaces';
        if (host.includes('idrivee2') || host.includes('idrivecloud')) return 'idrive-e2';
        if (host.includes('aliyuncs') || host.includes('oss')) return 'alibaba-oss';
        if (host.includes('myqcloud') || host.includes('cos.')) return 'tencent-cos';
        if (host.includes('oraclecloud')) return 'oracle-cloud';
        if (host.includes('amazonaws')) return 'aws-s3';
    }
    if (proto === 'webdav') {
        if (host.includes('drivehq')) return 'drivehq';
        if (host.includes('nextcloud')) return 'nextcloud';
        if (host.includes('koofr')) return 'koofr';
        if (host.includes('jianguoyun')) return 'jianguoyun';
        if (host.includes('teracloud') || host.includes('infini-cloud')) return 'infinicloud';
        if (host.includes('4shared')) return '4shared';
        if (host.includes('cloudme')) return 'cloudme';
    }
    return undefined;
};

// Get saved servers from localStorage (auto-migrate missing providerId)
const getSavedServers = (): ServerProfile[] => {
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
};

// Save servers to localStorage (sync) and vault (async)
const saveServers = (servers: ServerProfile[]) => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(servers));
    secureStoreAndClean('server_profiles', STORAGE_KEY, servers).catch(() => {});
};

export const SavedServers: React.FC<SavedServersProps> = ({
    onConnect,
    currentProfile,
    className = '',
    onEdit,
    lastUpdate,
    onOpenExportImport
}) => {
    const t = useTranslation();
    const [servers, setServers] = useState<ServerProfile[]>([]);

    const [oauthConnecting, setOauthConnecting] = useState<string | null>(null);
    const [oauthError, setOauthError] = useState<string | null>(null);
    const [searchQuery, setSearchQuery] = useState('');

    // Filter servers by search query (name, host, protocol, username)
    const SEARCH_THRESHOLD = 10;
    const showSearch = servers.length >= SEARCH_THRESHOLD;
    const filteredServers = useMemo(() => {
        if (!searchQuery.trim()) return servers;
        const q = searchQuery.toLowerCase();
        return servers.filter(s =>
            (s.name || '').toLowerCase().includes(q) ||
            (s.host || '').toLowerCase().includes(q) ||
            (s.protocol || '').toLowerCase().includes(q) ||
            (s.username || '').toLowerCase().includes(q)
        );
    }, [servers, searchQuery]);

    // Drag-to-reorder state
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);
    const dragNodeRef = useRef<HTMLDivElement | null>(null);
    const listRef = useRef<HTMLDivElement>(null);

    const handleReorderDragStart = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
        setDragIdx(idx);
        dragNodeRef.current = e.currentTarget;
        // Use a translucent clone as drag image
        e.dataTransfer.effectAllowed = 'move';
        e.dataTransfer.setData('text/plain', String(idx));
        // Delay adding dragging class so the ghost image captures the original look
        requestAnimationFrame(() => {
            if (dragNodeRef.current) dragNodeRef.current.style.opacity = '0.4';
        });
    }, []);

    const handleReorderDragOver = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
        e.preventDefault();
        e.dataTransfer.dropEffect = 'move';
        if (dragIdx === null || idx === dragIdx) return;
        setOverIdx(idx);
    }, [dragIdx]);

    const handleReorderDrop = useCallback((e: React.DragEvent<HTMLDivElement>, idx: number) => {
        e.preventDefault();
        if (dragIdx === null || dragIdx === idx) return;
        const reordered = [...servers];
        const [moved] = reordered.splice(dragIdx, 1);
        reordered.splice(idx, 0, moved);
        setServers(reordered);
        saveServers(reordered);
    }, [dragIdx, servers]);

    const handleReorderDragEnd = useCallback(() => {
        if (dragNodeRef.current) dragNodeRef.current.style.opacity = '1';
        dragNodeRef.current = null;
        setDragIdx(null);
        setOverIdx(null);
    }, []);

    // Protocol colors for avatar
    const protocolColors: Record<string, string> = {
        ftp: 'from-blue-500 to-cyan-400',
        ftps: 'from-green-500 to-emerald-400',
        sftp: 'from-purple-500 to-violet-400',
        webdav: 'from-orange-500 to-amber-400',
        s3: 'from-amber-500 to-yellow-400',
        aerocloud: 'from-sky-400 to-blue-500',
        googledrive: 'from-red-500 to-red-400',
        dropbox: 'from-blue-600 to-blue-400',
        onedrive: 'from-sky-500 to-sky-400',
        mega: 'from-red-600 to-red-500',  // MEGA brand red
        box: 'from-blue-500 to-blue-600',
        pcloud: 'from-green-500 to-teal-400',
        azure: 'from-blue-600 to-indigo-500',
        filen: 'from-emerald-500 to-green-400',
        fourshared: 'from-blue-500 to-cyan-400',
    };

    useEffect(() => {
        // Load from localStorage immediately (sync), then try vault
        setServers(getSavedServers());
        (async () => {
            const vaultServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', STORAGE_KEY);
            if (vaultServers && vaultServers.length > 0) {
                // Migrate providerId if needed
                let migrated = false;
                for (const s of vaultServers) {
                    if (!s.providerId) {
                        const derived = deriveProviderId(s);
                        if (derived) { s.providerId = derived; migrated = true; }
                    }
                }
                if (migrated) saveServers(vaultServers);
                setServers(vaultServers);
            }
        })();
    }, [lastUpdate]);

    const handleDelete = (id: string) => {
        // Prevent deletion if connecting
        if (oauthConnecting === id) return;

        const updated = servers.filter(s => s.id !== id);
        setServers(updated);
        saveServers(updated);
    };

    const handleEdit = (server: ServerProfile) => {
        onEdit(server);
    };

    const handleConnect = async (server: ServerProfile) => {
        // Check expiry for MEGA (Beta v0.5.0)
        if (server.protocol === 'mega' && server.options?.session_expires_at && Date.now() > server.options.session_expires_at) {
            onEdit(server); // Redirect to edit to renew session
            return;
        }

        // Clear any previous OAuth error
        setOauthError(null);

        // Check if this is an OAuth provider
        if (server.protocol && isOAuthProvider(server.protocol)) {
            // Try localStorage settings first (Google Drive, Dropbox, OneDrive)
            const providerKey = getOAuthProviderKey(server.protocol);
            const oauthSettings = getOAuthSettings();
            let credentials: { clientId: string; clientSecret: string } | null = null;

            if (providerKey && oauthSettings) {
                const stored = oauthSettings[providerKey];
                if (stored?.clientId && stored?.clientSecret) {
                    credentials = stored;
                }
            }

            // Fallback: load from credential vault
            if (!credentials) {
                credentials = await loadOAuthCredentials(server.protocol);
            }

            if (!credentials) {
                const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud' };
                setOauthError(t('savedServers.oauthConfigError', { provider: providerNames[server.protocol] || server.protocol }));
                return;
            }

            // Start OAuth flow
            setOauthConnecting(server.id);
            try {
                const oauthProvider = server.protocol === 'googledrive' ? 'google_drive' : server.protocol;
                const params = {
                    provider: oauthProvider,
                    client_id: credentials.clientId,
                    client_secret: credentials.clientSecret,
                };

                // Check if tokens already exist - if so, try to connect directly
                const hasTokens = await invoke<boolean>('oauth2_has_tokens', { provider: oauthProvider });

                if (!hasTokens) {
                    // No tokens - need full auth flow (opens browser)
                    logger.debug('[SavedServers] No OAuth tokens, starting full auth...');
                    await invoke('oauth2_full_auth', { params });
                } else {
                    logger.debug('[SavedServers] OAuth tokens found, skipping auth flow');
                }

                // Connect using stored tokens; if expired without refresh token, re-auth
                let result: { display_name: string; account_email: string | null };
                try {
                    result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params });
                } catch (connectErr) {
                    const errMsg = connectErr instanceof Error ? connectErr.message : String(connectErr);
                    if (errMsg.includes('Token expired') || errMsg.includes('token') && errMsg.includes('refresh')) {
                        logger.debug('[SavedServers] Token expired, re-authenticating...');
                        await invoke('oauth2_full_auth', { params });
                        result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params });
                    } else {
                        throw connectErr;
                    }
                }

                // Save account email to server profile if retrieved
                const updatedUsername = result.account_email || server.username;
                const updated = servers.map(s =>
                    s.id === server.id ? { ...s, lastConnected: new Date().toISOString(), username: updatedUsername || s.username } : s
                );
                setServers(updated);
                saveServers(updated);

                // Call onConnect with OAuth params
                onConnect({
                    server: result.display_name,
                    username: updatedUsername,
                    password: '',
                    protocol: server.protocol,
                    displayName: server.name,
                    providerId: server.providerId,
                }, server.initialPath, server.localInitialPath);

            } catch (e) {
                setOauthError(e instanceof Error ? e.message : String(e));
            } finally {
                setOauthConnecting(null);
            }
            return;
        }

        // 4shared OAuth 1.0 — separate flow from OAuth2
        if (server.protocol && isFourSharedProvider(server.protocol)) {
            // Load consumer credentials from vault
            let consumerKey = '';
            let consumerSecret = '';
            try {
                consumerKey = await getCredentialWithRetry('oauth_fourshared_client_id');
                consumerSecret = await getCredentialWithRetry('oauth_fourshared_client_secret');
            } catch {
                // ignore
            }
            if (!consumerKey || !consumerSecret) {
                setOauthError(t('savedServers.foursharedConfigError'));
                return;
            }

            setOauthConnecting(server.id);
            try {
                const params = { consumer_key: consumerKey, consumer_secret: consumerSecret };
                const hasTokens = await invoke<boolean>('fourshared_has_tokens');

                if (!hasTokens) {
                    await invoke('fourshared_full_auth', { params });
                }

                let result: { display_name: string; account_email: string | null };
                try {
                    result = await invoke<{ display_name: string; account_email: string | null }>('fourshared_connect', { params });
                } catch (connectErr) {
                    // Token expired — re-authenticate
                    await invoke('fourshared_full_auth', { params });
                    result = await invoke<{ display_name: string; account_email: string | null }>('fourshared_connect', { params });
                }

                const updatedUsername = result.account_email || server.username;
                const updated = servers.map(s =>
                    s.id === server.id ? { ...s, lastConnected: new Date().toISOString(), username: updatedUsername || s.username } : s
                );
                setServers(updated);
                saveServers(updated);

                onConnect({
                    server: result.display_name,
                    username: updatedUsername,
                    password: '',
                    protocol: server.protocol,
                    displayName: server.name,
                    providerId: server.providerId,
                }, server.initialPath, server.localInitialPath);
            } catch (e) {
                setOauthError(e instanceof Error ? e.message : String(e));
            } finally {
                setOauthConnecting(null);
            }
            return;
        }

        // Non-OAuth: Update last connected
        const updated = servers.map(s =>
            s.id === server.id ? { ...s, lastConnected: new Date().toISOString() } : s
        );
        setServers(updated);
        saveServers(updated);

        // Load password from credential vault (with retry if vault not ready yet)
        let password = '';
        try {
            password = await getCredentialWithRetry(`server_${server.id}`);
        } catch {
            // Credential not found — password empty (never saved or server without password)
        }

        // Build connection params - for providers, don't append port to host
        // SFTP/MEGA use provider_connect which handles port separately
        const isProviderProtocol = server.protocol && ['s3', 'webdav', 'sftp', 'mega'].includes(server.protocol);
        const defaultPort = server.protocol === 'sftp' ? 22 : server.protocol === 'ftps' ? 990 : 21;
        const serverString = isProviderProtocol
            ? server.host  // S3/WebDAV/SFTP/MEGA: use host only
            : (server.port !== defaultPort ? `${server.host}:${server.port}` : server.host);

        onConnect({
            server: serverString,
            username: server.username,
            password,
            protocol: server.protocol || 'ftp',
            port: server.port,
            displayName: server.name,
            options: server.options,
            providerId: server.providerId,
        }, server.initialPath, server.localInitialPath);
    };



    return (
        <div className={`${className}`}>
            <div className="mb-4 flex items-start justify-between">
                <div>
                    <h3 className="text-lg font-semibold flex items-center gap-2">
                        <Server size={20} />
                        {t('connection.savedServers')}
                    </h3>
                    <div className="text-xs text-gray-500 font-normal mt-1">
                        {t('connection.savedServersHelp')}
                    </div>
                </div>
                {onOpenExportImport && (
                    <button
                        onClick={onOpenExportImport}
                        className="p-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                        title={t('settings.exportImport')}
                    >
                        <ImportExportIcon size={18} />
                    </button>
                )}
            </div>

            {/* Server list */}
            {servers.length === 0 && (
                <p className="text-gray-500 dark:text-gray-400 text-sm text-center py-4">
                    {t('connection.noSavedServers')}
                </p>
            )}

            {/* OAuth Error Message */}
            {oauthError && (
                <div className="mb-3 p-3 bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded-lg">
                    <div className="flex items-start gap-2 text-red-700 dark:text-red-300">
                        <AlertCircle className="w-5 h-5 flex-shrink-0 mt-0.5" />
                        <div className="flex-1">
                            <span className="text-sm">{oauthError}</span>
                            <button
                                onClick={() => setOauthError(null)}
                                className="ml-2 text-xs underline hover:no-underline"
                            >
                                {t('connection.dismiss')}
                            </button>
                        </div>
                    </div>
                </div>
            )}

            <div ref={listRef} className="space-y-2 max-h-[calc(100vh-265px)] overflow-y-auto [&::-webkit-scrollbar]:hidden [scrollbar-width:none]">
                {/* Search bar inside scrollable container — sticky at top, same width as servers */}
                {showSearch && (
                    <div className="sticky top-0 z-10 bg-white dark:bg-gray-800 pb-1">
                        <div className="relative">
                            <Search size={14} className="absolute left-3 top-1/2 -translate-y-1/2 text-gray-400" />
                            <input
                                type="text"
                                value={searchQuery}
                                onChange={(e) => setSearchQuery(e.target.value)}
                                placeholder={t('connection.searchServers')}
                                className="w-full pl-9 pr-8 py-2 text-sm bg-gray-100 dark:bg-gray-700/80 border border-gray-200 dark:border-gray-600 rounded-lg focus:outline-none"
                            />
                            {searchQuery && (
                                <button
                                    onClick={() => setSearchQuery('')}
                                    className="absolute right-2.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                                >
                                    <X size={14} />
                                </button>
                            )}
                        </div>
                    </div>
                )}
                {filteredServers.map((server, idx) => {
                    // Disable drag-to-reorder when search is active (indices don't match full list)
                    const isDraggable = !searchQuery;
                    return (
                    <div
                        key={server.id}
                        draggable={isDraggable}
                        onDragStart={isDraggable ? (e) => handleReorderDragStart(e, idx) : undefined}
                        onDragOver={isDraggable ? (e) => handleReorderDragOver(e, idx) : undefined}
                        onDrop={isDraggable ? (e) => handleReorderDrop(e, idx) : undefined}
                        onDragEnd={isDraggable ? handleReorderDragEnd : undefined}
                        className={`flex items-center gap-3 p-3 bg-gray-100 dark:bg-gray-700 rounded-xl hover:bg-gray-200 dark:hover:bg-gray-600 transition-all duration-200 group ${oauthConnecting === server.id ? 'opacity-75' : ''} ${dragIdx === idx ? 'scale-[0.97] shadow-lg ring-2 ring-blue-400/50' : ''} ${overIdx === idx && dragIdx !== null && dragIdx !== idx ? 'border-t-2 border-blue-400' : 'border-t-2 border-transparent'}`}
                    >
                        {/* Drag handle (hidden during search) */}
                        <div className={`cursor-grab active:cursor-grabbing text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-opacity shrink-0 -ml-1 ${isDraggable ? 'opacity-0 group-hover:opacity-100' : 'opacity-0 pointer-events-none w-0 -ml-0'}`}
                             title={t('savedServers.dragToReorder')}>
                            <GripVertical size={16} />
                        </div>
                            {/* Server icon — click to connect */}
                        {(() => {
                            const logoKey = server.providerId || server.protocol || '';
                            const LogoComponent = PROVIDER_LOGOS[logoKey];
                            const hasLogo = !!LogoComponent;
                            return (
                                <button
                                    onClick={() => handleConnect(server)}
                                    disabled={oauthConnecting !== null}
                                    className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center transition-all hover:scale-105 hover:ring-2 hover:ring-blue-400 hover:shadow-lg disabled:cursor-wait ${oauthConnecting === server.id ? 'animate-pulse' : ''} ${hasLogo ? 'bg-[#FFFFF0] dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${protocolColors[server.protocol || 'ftp']} text-white`}`}
                                    title={t('common.connect')}
                                >
                                    {hasLogo ? <LogoComponent size={20} /> : <span className="font-bold">{(server.name || server.host).charAt(0).toUpperCase()}</span>}
                                </button>
                            );
                        })()}
                        {/* Server info — not clickable for connection */}
                        <div className="flex-1 min-w-0">
                                <div className="font-medium flex items-center gap-2">
                                    {server.name || server.host}
                                    {server.protocol === 'mega' && server.options?.session_expires_at && Date.now() > server.options.session_expires_at && (
                                        <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-red-100 dark:bg-red-900/60 text-red-600 dark:text-red-300 font-bold border border-red-200 dark:border-red-800 flex items-center gap-1" title="Session expired (24h)">
                                            <Clock size={10} /> EXP
                                        </span>
                                    )}
                                    {oauthConnecting === server.id && (
                                        <span className="text-xs text-blue-500 animate-pulse">{t('connection.authenticating')}</span>
                                    )}
                                    <span className="text-xs px-1.5 py-0.5 rounded bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300 uppercase">
                                        {server.protocol || 'ftp'}
                                    </span>
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400">
                                    {(isOAuthProvider(server.protocol || 'ftp') || isFourSharedProvider(server.protocol || 'ftp'))
                                        ? `OAuth — ${server.username || ({ googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', fourshared: '4shared' } as Record<string, string>)[server.protocol || ''] || server.protocol}`
                                        : server.protocol === 'filen'
                                            ? `E2E AES-256 — ${server.username}`
                                            : server.protocol === 'mega'
                                                ? `E2E AES-128 — ${server.username}`
                                                : server.protocol === 's3'
                                                    ? (() => {
                                                        const bucket = server.options?.bucket || 'S3';
                                                        const host = server.host?.replace(/^https?:\/\//, '') || '';
                                                        const provider = host.includes('cloudflarestorage') ? 'Cloudflare R2'
                                                            : host.includes('backblazeb2') ? 'Backblaze B2'
                                                            : host.includes('amazonaws') ? 'AWS S3'
                                                            : host.includes('wasabisys') ? 'Wasabi'
                                                            : host.includes('digitaloceanspaces') ? 'DigitalOcean'
                                                            : host.split('.')[0];
                                                        return `${bucket} — ${provider}`;
                                                    })()
                                                    : server.protocol === 'webdav'
                                                        ? server.username + '@' + (server.host?.replace(/^https?:\/\//, '') || server.host)
                                                        : `${server.username}@${server.host}:${server.port}`
                                    }
                                </div>
                        </div>
                        <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                            <button
                                onClick={() => handleEdit(server)}
                                className="p-2 text-gray-500 hover:text-blue-500 hover:bg-blue-50 dark:hover:bg-blue-900/30 rounded-lg transition-colors"
                                title={t('connection.editServer')}
                            >
                                <Edit2 size={14} />
                            </button>
                            <button
                                onClick={() => handleDelete(server.id)}
                                className="p-2 text-gray-500 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-900/30 rounded-lg transition-colors"
                                title={t('connection.deleteServer')}
                            >
                                <Trash2 size={14} />
                            </button>
                        </div>
                    </div>
                    );
                })}
                {/* No results message when search is active */}
                {searchQuery && filteredServers.length === 0 && servers.length > 0 && (
                    <p className="text-gray-400 text-sm text-center py-6">
                        {t('search.results', { count: '0' })}
                    </p>
                )}
            </div>
        </div>
    );
};

export default SavedServers;
