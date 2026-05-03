// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Server, Plus, Trash2, Edit2, Copy, X, Check, Cloud, AlertCircle, GripVertical, Search, Activity, Play, Loader2, Eye, EyeOff } from 'lucide-react';
import { ImportExportIcon } from './icons/ImportExportIcon';
import { open } from '@tauri-apps/plugin-dialog';
import { ServerProfile, ConnectionParams, ProviderType, isOAuthProvider, isFourSharedProvider } from '../types';
import { useTranslation } from '../i18n';
import { getProtocolInfo, ProtocolBadge, ProtocolIcon } from './ProtocolSelector';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { getProviderById } from '../providers';
import { logger } from '../utils/logger';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';
import { getGitHubConnectionBadge, getMegaConnectionBadge } from '../utils/providerConnectionMeta';
import { getFilenAuthVersion } from '../utils/filenAuthVersion';
import { mergeSavedServerProfile } from '../utils/serverProfileStore';
import { maskCredential } from '../utils/maskCredential';
import { useContextMenu, ContextMenu, ContextMenuItem } from './ContextMenu';
import { ServerHealthCheck } from './ServerHealthCheck';
import { AlertDialog } from './Dialogs';

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
    onConnect: (params: ConnectionParams, initialPath?: string, localInitialPath?: string) => void | Promise<void>;
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
    if (['mega', 'box', 'pcloud', 'azure', 'filen', 'internxt', 'kdrive', 'drime', 'filelu', 'koofr', 'opendrive', 'yandexdisk', 'googledrive', 'dropbox', 'onedrive', 'fourshared', 'zohoworkdrive', 'github', 'gitlab', 'immich'].includes(proto)) return proto;
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
        if (host.includes('minio') || host.includes(':9000')) return 'minio';
        if (host.includes('amazonaws') || host === '' || host === 's3.amazonaws.com') return 'amazon-s3';
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
    // Hosting providers (FTP/FTPS/SFTP)
    if (host.includes('aspnix')) return 'aspnix';
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

// Save servers to localStorage (sync backup) and vault (async, encrypted)
const saveServers = (servers: ServerProfile[]) => {
    secureStoreAndClean('server_profiles', STORAGE_KEY, servers).catch(() => {});
};

export const SavedServers: React.FC<SavedServersProps> = ({
    onConnect,
    currentProfile,
    className = '',
    onEdit,
    lastUpdate,
    onOpenExportImport,
}) => {
    const t = useTranslation();
    const [servers, setServers] = useState<ServerProfile[]>([]);

    const [oauthConnecting, setOauthConnecting] = useState<string | null>(null);
    const [connectingId, setConnectingId] = useState<string | null>(null);
    const [oauthError, setOauthError] = useState<string | null>(null);
    const [searchQuery, setSearchQuery] = useState('');
    const [credentialsMasked, setCredentialsMasked] = useState(true);
    const [healthCheckTarget, setHealthCheckTarget] = useState<string | false>(false);
    const [deleteTarget, setDeleteTarget] = useState<ServerProfile | null>(null);
    const { state: contextMenuState, show: showContextMenu, hide: hideContextMenu } = useContextMenu();

    useEffect(() => {
        const onFilenAuthVersionUpdated = (evt: Event) => {
            const custom = evt as CustomEvent<{ profileId?: string; authVersion?: number }>;
            const profileId = custom.detail?.profileId;
            const authVersion = custom.detail?.authVersion;
            if (!profileId || typeof authVersion !== 'number') return;

            setServers(prev => prev.map(server => {
                if (server.id !== profileId || server.protocol !== 'filen') return server;
                return {
                    ...server,
                    options: {
                        ...(server.options || {}),
                        filen_auth_version: authVersion,
                    },
                };
            }));
        };

        window.addEventListener('aeroftp-filen-auth-version-updated', onFilenAuthVersionUpdated as EventListener);
        return () => {
            window.removeEventListener('aeroftp-filen-auth-version-updated', onFilenAuthVersionUpdated as EventListener);
        };
    }, []);

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
        internxt: 'from-blue-500 to-blue-400',
        kdrive: 'from-blue-500 to-sky-400',
        drime: 'from-green-500 to-emerald-400',
        filelu: 'from-sky-500 to-cyan-400',
        koofr: 'from-green-500 to-emerald-400',
        opendrive: 'from-cyan-500 to-sky-400',
        yandexdisk: 'from-yellow-500 to-amber-400',
        fourshared: 'from-blue-500 to-cyan-400',
        zohoworkdrive: 'from-yellow-500 to-orange-400',
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

    const handleDelete = (server: ServerProfile) => {
        // Prevent deletion if connecting
        if (oauthConnecting === server.id) return;
        setDeleteTarget(server);
    };

    const confirmDelete = () => {
        if (!deleteTarget) return;
        const updated = servers.filter(s => s.id !== deleteTarget.id);
        setServers(updated);
        saveServers(updated);
        // Clean up orphaned vault credential
        invoke('delete_credential', { account: `server_${deleteTarget.id}` }).catch(() => {});
        setDeleteTarget(null);
    };

    const handleEdit = (server: ServerProfile) => {
        onEdit(server);
    };

    const handleDuplicate = async (server: ServerProfile) => {
        const newId = generateId();
        const cloned: ServerProfile = {
            ...server,
            id: newId,
            name: `${server.name} (${t('common.copy')})`,
            lastConnected: undefined,
        };
        // Copy credential from vault if stored
        if (server.hasStoredCredential) {
            try {
                const password = await getCredentialWithRetry(`server_${server.id}`);
                if (password) {
                    await invoke('store_credential', { account: `server_${newId}`, password });
                    cloned.hasStoredCredential = true;
                }
            } catch { /* Password will need to be re-entered */ }
        }
        const updated = [...servers, cloned];
        setServers(updated);
        saveServers(updated);
        // Open in edit mode so user can change name/host
        onEdit(cloned);
    };

    const handleServerContextMenu = useCallback((e: React.MouseEvent, server: ServerProfile) => {
        const items: ContextMenuItem[] = [
            { label: t('common.connect'), icon: <Play size={14} />, action: () => handleConnect(server) },
            { label: t('connection.editServer'), icon: <Edit2 size={14} />, action: () => handleEdit(server) },
            { label: t('connection.duplicateServer'), icon: <Copy size={14} />, action: () => handleDuplicate(server) },
            { label: t('healthCheck.title'), icon: <Activity size={14} />, action: () => setHealthCheckTarget(server.id), divider: true },
            { label: t('connection.deleteServer'), icon: <Trash2 size={14} />, danger: true, action: () => handleDelete(server), divider: true },
        ];
        showContextMenu(e, items);
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [servers, t]);

    const handleConnect = async (serverParam: ServerProfile) => {
        const server = serverParam;

        // Prevent double-click
        if (connectingId) return;
        setConnectingId(server.id);

        // Clear any previous OAuth error
        setOauthError(null);

        // Check if this is an OAuth provider
        if (server.protocol && isOAuthProvider(server.protocol)) {
            // SEC: Load credentials from vault only — no localStorage fallback.
            let credentials: { clientId: string; clientSecret: string } | null = null;
            credentials = await loadOAuthCredentials(server.protocol);

            if (!credentials) {
                const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', zohoworkdrive: 'Zoho WorkDrive' };
                setOauthError(t('savedServers.oauthConfigError', { provider: providerNames[server.protocol] || server.protocol }));
                return;
            }

            // Start OAuth flow
            setOauthConnecting(server.id);
            try {
                const oauthProvider = server.protocol === 'googledrive' ? 'google_drive' : server.protocol;
                // Load region from server profile or credential store (for Zoho WorkDrive)
                let region: string | undefined;
                if (server.protocol === 'zohoworkdrive') {
                    region = server.options?.region;
                    if (!region) {
                        try {
                            region = await invoke<string>('get_credential', { account: `oauth_${server.protocol}_region` });
                        } catch {
                            // Default to 'us' via Rust default_region()
                        }
                    }
                }
                const params = {
                    provider: oauthProvider,
                    client_id: credentials.clientId,
                    client_secret: credentials.clientSecret,
                    ...(region && { region }),
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
                    const lower = errMsg.toLowerCase();
                    // Retry with full re-auth for: expired tokens, invalid tokens, auth failures
                    if (lower.includes('token expired') ||
                        (lower.includes('token') && lower.includes('refresh')) ||
                        lower.includes('authentication failed') ||
                        (lower.includes('invalid') && lower.includes('access_token'))) {
                        logger.debug('[SavedServers] Token invalid/expired, re-authenticating...');
                        await invoke('oauth2_full_auth', { params });
                        result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', { params });
                    } else {
                        throw connectErr;
                    }
                }

                // Save account email to server profile if retrieved
                const updatedUsername = result.account_email || server.username;
                const connectedAt = new Date().toISOString();
                const updated = servers.map(s =>
                    s.id === server.id ? { ...s, lastConnected: connectedAt, username: updatedUsername || s.username } : s
                );
                setServers(updated);
                await mergeSavedServerProfile(server.id, latest => ({
                    ...latest,
                    lastConnected: connectedAt,
                    username: updatedUsername || latest.username,
                }));

                // Call onConnect with OAuth params
                await onConnect({
                    server: result.display_name,
                    username: updatedUsername,
                    password: '',
                    protocol: server.protocol,
                    displayName: server.name,
                    providerId: server.providerId,
                    savedServerId: server.id,
                }, server.initialPath, server.localInitialPath);

            } catch (e) {
                setOauthError(e instanceof Error ? e.message : String(e));
            } finally {
                setOauthConnecting(null);
                setConnectingId(null);
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
                const connectedAt = new Date().toISOString();
                const updated = servers.map(s =>
                    s.id === server.id ? { ...s, lastConnected: connectedAt, username: updatedUsername || s.username } : s
                );
                setServers(updated);
                await mergeSavedServerProfile(server.id, latest => ({
                    ...latest,
                    lastConnected: connectedAt,
                    username: updatedUsername || latest.username,
                }));

                await onConnect({
                    server: result.display_name,
                    username: updatedUsername,
                    password: '',
                    protocol: server.protocol,
                    displayName: server.name,
                    providerId: server.providerId,
                    savedServerId: server.id,
                }, server.initialPath, server.localInitialPath);
            } catch (e) {
                setOauthError(e instanceof Error ? e.message : String(e));
            } finally {
                setOauthConnecting(null);
                setConnectingId(null);
            }
            return;
        }

        // Non-OAuth: Update last connected
        try {
            const connectedAt = new Date().toISOString();
            const updated = servers.map(s =>
                s.id === server.id ? { ...s, lastConnected: connectedAt } : s
            );
            setServers(updated);
            await mergeSavedServerProfile(server.id, latest => ({
                ...latest,
                lastConnected: connectedAt,
            }));

            // Load password from credential vault (with retry if vault not ready yet)
            let password = '';
            try {
                password = await getCredentialWithRetry(`server_${server.id}`);
            } catch {
                // Credential not found — password empty (never saved or server without password)
            }

            // Build connection params - for providers, don't append port to host
            // SFTP/MEGA use provider_connect which handles port separately
            const isProviderProtocol = server.protocol && ['s3', 'webdav', 'sftp', 'mega', 'filelu', 'koofr', 'yandexdisk', 'github', 'gitlab', 'immich'].includes(server.protocol);
            const defaultPort = server.protocol === 'sftp' ? 22 : server.protocol === 'ftps' ? 990 : 21;
            const serverString = server.host;

            await onConnect({
                server: serverString,
                username: server.username,
                password,
                protocol: server.protocol || 'ftp',
                port: server.port,
                displayName: server.name,
                options: server.options,
                providerId: server.providerId,
                savedServerId: server.id,
            }, server.initialPath, server.localInitialPath);

            if ((server.protocol || 'ftp') === 'filen') {
                try {
                    const authVersion = await invoke<number | null>('filen_get_auth_version');
                    if (typeof authVersion === 'number') {
                        const updatedWithAuth = await mergeSavedServerProfile(server.id, latest => ({
                            ...latest,
                            options: {
                                ...(latest.options || {}),
                                filen_auth_version: authVersion,
                            },
                        }));
                        setServers(updatedWithAuth);
                    }
                } catch {
                    // best-effort badge enrichment only
                }
            }
        } catch {
            // Connection error handled by parent — reset loading state
        } finally {
            setConnectingId(null);
        }
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
                <div className="flex items-center gap-1.5">
                    {servers.length > 0 && (
                        <button
                            onClick={() => setCredentialsMasked(v => !v)}
                            className="p-1.5 bg-gray-50 dark:bg-gray-700/50 hover:bg-gray-100 dark:hover:bg-gray-600/50 text-gray-500 dark:text-gray-400 rounded-lg transition-colors"
                            title={credentialsMasked ? t('savedServers.showCredentials') : t('savedServers.hideCredentials')}
                        >
                            {credentialsMasked ? <EyeOff size={16} /> : <Eye size={16} />}
                        </button>
                    )}
                    {servers.length > 0 && (
                        <button
                            onClick={() => setHealthCheckTarget('all')}
                            className="p-1.5 bg-emerald-50 dark:bg-emerald-900/30 hover:bg-emerald-100 dark:hover:bg-emerald-800/40 text-emerald-600 dark:text-emerald-400 rounded-lg transition-colors"
                            title={t('healthCheck.title')}
                        >
                            <Activity size={16} />
                        </button>
                    )}
                    {onOpenExportImport && (
                        <button
                            onClick={onOpenExportImport}
                            className="p-1.5 bg-amber-50 dark:bg-amber-900/30 hover:bg-amber-100 dark:hover:bg-amber-800/40 text-amber-600 dark:text-amber-400 rounded-lg transition-colors"
                            title={t('settings.exportImport')}
                        >
                            <ImportExportIcon size={16} />
                        </button>
                    )}
                </div>
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

            <div ref={listRef} className="space-y-2 max-h-[calc(100vh-80px)] overflow-y-auto [&::-webkit-scrollbar]:hidden [scrollbar-width:none]">
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
                    const gitHubBadge = server.protocol === 'github'
                        ? getGitHubConnectionBadge(server.options)
                        : null;
                    const megaBadge = server.protocol === 'mega'
                        ? getMegaConnectionBadge(server.options)
                        : null;
                    const filenAuthVersion = getFilenAuthVersion(server);
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
                        onContextMenu={(e) => handleServerContextMenu(e, server)}
                        className={`flex items-center gap-3 p-3 bg-gray-100 dark:bg-gray-700 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 transition-all duration-200 group ${oauthConnecting === server.id ? 'opacity-75' : ''} ${dragIdx === idx ? 'scale-[0.97] shadow-lg ring-2 ring-blue-400/50' : ''} ${overIdx === idx && dragIdx !== null && dragIdx !== idx ? 'border-t-2 border-blue-400' : 'border-t-2 border-transparent'}`}
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
                            const hasCustomIcon = !!server.customIconUrl;
                            const hasFavicon = !!server.faviconUrl;
                            const hasIcon = hasCustomIcon || hasFavicon;
                            return (
                                <button
                                    onClick={() => handleConnect(server)}
                                    disabled={connectingId !== null}
                                    className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center transition-all hover:scale-105 hover:ring-2 hover:ring-blue-400 hover:shadow-lg disabled:cursor-wait disabled:opacity-70 ${connectingId === server.id ? 'ring-2 ring-blue-400' : ''} ${hasIcon || hasLogo ? 'bg-[#FFFFF0] dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${protocolColors[server.protocol || 'ftp']} text-white`}`}
                                    title={t('common.connect')}
                                >
                                    {connectingId === server.id ? (
                                        <Loader2 size={18} className="animate-spin text-blue-500" />
                                    ) : hasCustomIcon ? (
                                        <img src={server.customIconUrl} alt="" className="w-6 h-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
                                    ) : hasFavicon ? (
                                        <img src={server.faviconUrl} alt="" className="w-6 h-6 rounded object-contain" onError={(e) => { (e.target as HTMLImageElement).style.display = 'none'; }} />
                                    ) : hasLogo ? <LogoComponent size={20} /> : <span className="font-bold">{(server.name || server.host).charAt(0).toUpperCase()}</span>}
                                </button>
                            );
                        })()}
                        {/* Server info — not clickable for connection */}
                        <div className="flex-1 min-w-0">
                                <div className="font-medium flex items-center gap-2">
                                    {server.name || server.host}
                                    {oauthConnecting === server.id && (
                                        <span className="text-xs text-blue-500 animate-pulse">{t('connection.authenticating')}</span>
                                    )}
                                    <span
                                        className={`text-xs px-1.5 py-0.5 rounded font-medium uppercase ${server.providerId === 'felicloud' ? '' : 'bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300'}`}
                                        style={server.providerId === 'felicloud' ? { backgroundColor: '#0083ce22', color: '#0083ce' } : undefined}
                                    >
                                        {server.providerId === 'felicloud' ? 'API OCS' : (server.protocol || 'ftp')}
                                    </span>
                                    {gitHubBadge && (
                                        <span className={`text-xs px-1.5 py-0.5 rounded font-medium ${gitHubBadge.className}`}>
                                            {gitHubBadge.label}
                                        </span>
                                    )}
                                    {megaBadge && (
                                        <span className={`text-xs px-1.5 py-0.5 rounded font-medium ${megaBadge.className}`}>
                                            {megaBadge.label}
                                        </span>
                                    )}
                                    {filenAuthVersion && (
                                        <span
                                            className="text-xs px-1.5 py-0.5 rounded font-medium bg-blue-100 text-blue-700 dark:bg-blue-900/50 dark:text-blue-300"
                                            title="Detected from Filen auth/info on successful connect"
                                        >
                                            v{filenAuthVersion}
                                        </span>
                                    )}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400">
                                    {(() => {
                                        const mu = (v: string) => credentialsMasked ? maskCredential(v) : v;
                                        if (isOAuthProvider(server.protocol || 'ftp') || isFourSharedProvider(server.protocol || 'ftp')) {
                                            return t('savedServers.oauthError', { username: mu(server.username || ({ googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', fourshared: '4shared', zohoworkdrive: 'Zoho WorkDrive' } as Record<string, string>)[server.protocol || ''] || server.protocol || '') });
                                        }
                                        if (server.protocol === 'filen' || server.protocol === 'internxt') return t('savedServers.e2eAes256', { username: mu(server.username || '') });
                                        if (server.protocol === 'kdrive') return `kDrive ${server.options?.bucket || ''}`;
                                        if (server.protocol === 'drime') return 'Drime Cloud';
                                        if (server.protocol === 'mega') return `${t('savedServers.e2eAes128', { username: mu(server.username || '') })} - ${megaBadge?.longLabel || 'MEGAcmd'}`;
                                        if (server.protocol === 's3') {
                                            const bucket = server.options?.bucket || '';
                                            const registryProvider = server.providerId ? getProviderById(server.providerId) : null;
                                            const host = server.host?.replace(/^https?:\/\//, '') || '';
                                            const providerName = registryProvider?.name
                                                || (host.includes('cloudflarestorage') ? 'Cloudflare R2'
                                                : host.includes('backblazeb2') ? 'Backblaze B2'
                                                : host.includes('amazonaws') ? 'AWS S3'
                                                : host.includes('wasabisys') ? 'Wasabi'
                                                : host.includes('digitaloceanspaces') ? 'DigitalOcean'
                                                : host.includes('storjshare') ? 'Storj'
                                                : host.includes('idrivee2') ? 'iDrive e2'
                                                : (host.includes('minio') || host.includes(':9000')) ? 'MinIO'
                                                : 'S3');
                                            return bucket ? `${bucket} - ${providerName}` : providerName;
                                        }
                                        if (server.protocol === 'github') {
                                            const base = server.host;
                                            const modeLabel = gitHubBadge ? ` (${gitHubBadge.label})` : '';
                                            return `${base}${modeLabel}`;
                                        }
                                        if (server.protocol === 'webdav') {
                                            const raw = server.host?.replace(/^https?:\/\//, '') || server.host || '';
                                            try { const h = new URL(server.host?.startsWith('http') ? server.host : `https://${server.host}`).hostname; return `${mu(server.username || '')}@${h}`; } catch { return `${mu(server.username || '')}@${raw}`; }
                                        }
                                        return `${mu(server.username || '')}@${server.host}:${server.port}`;
                                    })()}
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
                                onClick={() => handleDuplicate(server)}
                                className="p-2 text-gray-500 hover:text-green-500 hover:bg-green-50 dark:hover:bg-green-900/30 rounded-lg transition-colors"
                                title={t('connection.duplicateServer')}
                            >
                                <Copy size={14} />
                            </button>
                            <button
                                onClick={() => handleDelete(server)}
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
            {deleteTarget && (
                <AlertDialog
                    title={t('connection.deleteServer')}
                    message={t('introHub.confirmDeleteServer').replace('{name}', deleteTarget.name || deleteTarget.host)}
                    type="warning"
                    onClose={() => setDeleteTarget(null)}
                    actionLabel={t('common.delete')}
                    onAction={confirmDelete}
                    actionIcon={<Trash2 size={14} />}
                />
            )}
        </div>
    );
};

export default SavedServers;
