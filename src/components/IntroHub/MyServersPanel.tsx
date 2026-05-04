import * as React from 'react';
import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Plus, Server as ServerIcon, Play, Edit2, Copy, Trash2, Activity, Star, PencilLine, ArrowUpRight, ArrowDownLeft, Database, Globe, Cloud, Camera, Code, Gauge } from 'lucide-react';
import { ServerProfile, ConnectionParams, ProviderType, isOAuthProvider, isFourSharedProvider } from '../../types';
import { MyServersViewMode, MyServersFilterBy, FILTER_CHIPS, CatalogCategoryId } from '../../types/catalog';
import { MyServersToolbar } from './MyServersToolbar';
import { ServerCard } from './ServerCard';
import { MyServersTable } from './MyServersTable';
import { MyServersTableFooter } from './MyServersTableFooter';
import { MyServersProtocolBreakdown, breakdownIsAvailable } from './MyServersProtocolBreakdown';
import { useTranslation } from '../../i18n';
import { ContextMenu, useContextMenu } from '../ContextMenu';
import type { ContextMenuItem } from '../ContextMenu';
import { secureGetWithFallback, secureStoreAndClean } from '../../utils/secureStorage';
import { getProviderById } from '../../providers';
import { logger } from '../../utils/logger';
import { ServerHealthCheck } from '../ServerHealthCheck';
import { SpeedTestDialog } from '../SpeedTestDialog';
import { AlertDialog } from '../Dialogs';
import { supportsSpeedTest } from '../../utils/speedTest';
import { useProviderHealth, type HealthTarget } from '../../hooks/useProviderHealth';
import { useCardLayout } from '../../hooks/useCardLayout';
import { useStorageThresholds } from '../../hooks/useStorageThresholds';
import { useMyServersDensity } from '../../hooks/useMyServersDensity';
import { useMyServersColumns } from '../../hooks/useMyServersColumns';
import { PROVIDER_HEALTH_URLS } from './discoverData';
import { mergeSavedServerProfile } from '../../utils/serverProfileStore';

const STORAGE_KEY = 'aeroftp-saved-servers';
const VIEW_MODE_KEY = 'aeroftp-intro-view-mode';
const HEALTH_SCAN_CHUNK_SIZE = 12;
const HEALTH_SCAN_CHUNK_DELAY_MS = 180;

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
        if (host.includes('filelu')) return 'filelu-s3';
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

function parseHealthEndpoint(input: string): { url: string; host: string; port?: number } | null {
    const raw = input.trim();
    if (!raw) return null;
    const candidate = /^[a-z][a-z\d+.-]*:\/\//i.test(raw) ? raw : `https://${raw}`;

    try {
        const url = new URL(candidate);
        if (!url.hostname) return null;
        const port = url.port ? Number(url.port) : undefined;
        return {
            url: `${url.protocol}//${url.host}`,
            host: url.hostname,
            port: Number.isFinite(port) ? port : undefined,
        };
    } catch {
        const authority = raw
            .replace(/^https?:\/\//i, '')
            .split(/[/?#]/)[0]
            .split('@')
            .pop();
        if (!authority) return null;
        const idx = authority.lastIndexOf(':');
        const portPart = idx >= 0 ? authority.slice(idx + 1) : '';
        const port = portPart && /^\d+$/.test(portPart) ? Number(portPart) : undefined;
        const host = port ? authority.slice(0, idx) : authority;
        if (!host) return null;
        return {
            url: `https://${authority}`,
            host,
            port: Number.isFinite(port) ? port : undefined,
        };
    }
}

function getHealthProbeInput(server: ServerProfile): string | null {
    const proto = server.protocol || 'ftp';
    const providerUrl = server.providerId ? getProviderById(server.providerId)?.healthCheckUrl : undefined;
    const protocolUrl = PROVIDER_HEALTH_URLS[proto];
    const endpoint = server.options?.endpoint;

    if (proto === 's3') {
        return endpoint || providerUrl || protocolUrl || server.host || null;
    }
    if (proto === 'azure') {
        const host = server.host?.trim();
        const accountName = (server.options?.accountName || server.username || '').trim();
        if (endpoint) return endpoint;
        if (host && (host.includes('.') || host.includes('://'))) return host;
        if (accountName) return `https://${accountName}.blob.core.windows.net`;
        if (host) return `https://${host}.blob.core.windows.net`;
        return 'https://login.microsoftonline.com';
    }
    if (proto === 'webdav') {
        return server.host || providerUrl || protocolUrl || null;
    }
    if (isOAuthProvider(proto) || isFourSharedProvider(proto)) {
        return protocolUrl || providerUrl || server.host || null;
    }
    return server.host || endpoint || providerUrl || protocolUrl || null;
}

function buildHealthTarget(server: ServerProfile): HealthTarget | null {
    const input = getHealthProbeInput(server);
    if (!input) return null;
    const parsed = parseHealthEndpoint(input);
    if (!parsed) return null;
    const port = parsed.port ?? (server.port > 0 ? server.port : undefined);
    return {
        id: server.id,
        url: parsed.url,
        protocol: server.protocol || 'ftp',
        host: parsed.host,
        port,
    };
}

function wait(ms: number) {
    return new Promise(resolve => window.setTimeout(resolve, ms));
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
    /** Jump to Discover tab pre-filtered on a specific category. */
    onJumpToCategory?: (categoryId: CatalogCategoryId) => void;
    lastUpdate?: number;
    onOpenExportImport?: () => void;
    onServersChange?: (count: number) => void;
    /** Open the Cross-Profile Transfer modal. Pre-fills source/destination when provided. */
    onOpenCrossProfile?: (opts?: { sourceId?: string; sourcePath?: string; destId?: string; destPath?: string }) => void;
}

const EMPTY_STATE_CATEGORIES: { id: CatalogCategoryId; labelKey: string; icon: React.ReactNode; iconColor: string }[] = [
    { id: 'protocols', labelKey: 'introHub.category.protocols', icon: <ServerIcon size={18} />, iconColor: 'text-blue-500 dark:text-blue-400' },
    { id: 'object-storage', labelKey: 'introHub.category.objectStorage', icon: <Database size={18} />, iconColor: 'text-orange-500 dark:text-orange-400' },
    { id: 'webdav', labelKey: 'introHub.category.webdav', icon: <Globe size={18} />, iconColor: 'text-emerald-500 dark:text-emerald-400' },
    { id: 'cloud-storage', labelKey: 'introHub.category.cloudStorage', icon: <Cloud size={18} />, iconColor: 'text-sky-500 dark:text-sky-400' },
    { id: 'media-services', labelKey: 'introHub.category.mediaServices', icon: <Camera size={18} />, iconColor: 'text-pink-500 dark:text-pink-400' },
    { id: 'developer', labelKey: 'introHub.category.developer', icon: <Code size={18} />, iconColor: 'text-gray-500 dark:text-gray-400' },
];

export function MyServersPanel({
    onConnect,
    onEdit,
    onQuickConnect,
    onJumpToCategory,
    lastUpdate,
    onOpenExportImport,
    onServersChange,
    onOpenCrossProfile,
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
    const [hideUsername, setHideUsername] = useState<boolean>(() => {
        return localStorage.getItem('aeroftp_hide_server_username') === '1';
    });
    const toggleHideUsername = React.useCallback(() => {
        setHideUsername(prev => {
            const next = !prev;
            try { localStorage.setItem('aeroftp_hide_server_username', next ? '1' : '0'); } catch { /* noop */ }
            return next;
        });
    }, []);
    const { thresholds } = useStorageThresholds();
    const { density, setDensity } = useMyServersDensity();
    const [healthCheckTarget, setHealthCheckTarget] = useState<string | false>(false);
    const [speedTestTarget, setSpeedTestTarget] = useState<string | undefined | false>(false);
    const [deleteTarget, setDeleteTarget] = useState<ServerProfile | null>(null);
    // Drag & reorder
    const [dragIdx, setDragIdx] = useState<number | null>(null);
    const [overIdx, setOverIdx] = useState<number | null>(null);
    const [favorites, setFavorites] = useState<Set<string>>(() => {
        try {
            const stored = localStorage.getItem('aeroftp-favorite-servers');
            return stored ? new Set(JSON.parse(stored)) : new Set();
        } catch { return new Set(); }
    });
    const [renamingId, setRenamingId] = useState<string | null>(null);
    // Cross-Profile selection: ephemeral, max 2. selection[0] = source, selection[1] = destination.
    const [crossProfileSelection, setCrossProfileSelection] = useState<string[]>([]);
    const [breakdownOpen, setBreakdownOpen] = useState(false);
    const hoveredServerRef = useRef<ServerProfile | null>(null);
    const { state: contextMenuState, show: showContextMenu, hide: hideContextMenu } = useContextMenu();

    const scrollTimeout = useRef<number | null>(null);
    const scrollContainerRef = useRef<HTMLDivElement>(null);
    const healthScanRunRef = useRef(0);
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
        return () => {
            el.removeEventListener('scroll', onScroll);
            if (scrollTimeout.current) {
                window.clearTimeout(scrollTimeout.current);
                scrollTimeout.current = null;
            }
        };
    }, [viewMode]); // re-attach when container swaps between grid/list

    useEffect(() => {
        setServers(getSavedServers());
    }, [lastUpdate]);

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

    // Cross-Profile Transfer needs at least 2 servers. When the list drops below
    // that (e.g. user deletes their last server), purge any stale selection so the
    // toolbar badge and selection rings don't linger pointing at deleted ids.
    useEffect(() => {
        if (servers.length < 2 && crossProfileSelection.length > 0) {
            setCrossProfileSelection([]);
        }
    }, [servers.length, crossProfileSelection.length]);

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

    // Toggle a server in the Cross-Profile selection. Max 2: when full and a
    // new server is clicked, drop the oldest (FIFO) so the latest click always
    // becomes part of the pair.
    const toggleCrossProfileSelection = useCallback((serverId: string) => {
        setCrossProfileSelection(prev => {
            if (prev.includes(serverId)) return prev.filter(id => id !== serverId);
            if (prev.length >= 2) return [prev[1], serverId];
            return [...prev, serverId];
        });
    }, []);

    const setAsCrossProfileSource = useCallback((serverId: string) => {
        setCrossProfileSelection(prev => {
            const others = prev.filter(id => id !== serverId);
            return [serverId, ...others].slice(0, 2);
        });
    }, []);

    const setAsCrossProfileDestination = useCallback((serverId: string) => {
        setCrossProfileSelection(prev => {
            const others = prev.filter(id => id !== serverId);
            // If nothing else is selected, leave dest at index 1 by padding with
            // a sentinel-free shape: keep the array length 1 with this id at [1]
            // is awkward; simpler — make it [otherFirst, serverId] when one
            // other exists, else [serverId] (which means it becomes source).
            if (others.length === 0) return [serverId];
            return [others[0], serverId];
        });
    }, []);

    const handleSelectServer = useCallback((s: ServerProfile) => toggleCrossProfileSelection(s.id), [toggleCrossProfileSelection]);

    const handleOpenCrossProfile = useCallback(() => {
        if (!onOpenCrossProfile) return;
        onOpenCrossProfile({
            sourceId: crossProfileSelection[0],
            destId: crossProfileSelection[1],
        });
    }, [onOpenCrossProfile, crossProfileSelection]);

    // Card layout toggle (compact ↔ detailed) — read here so the toolbar
    // toggle handler below can flip it. The same hook is also consumed below
    // for the per-card health-radial gating; both readers share state via the
    // global `aeroftp-settings-changed` event.
    const cardLayout = useCardLayout();
    const tableColumns = useMyServersColumns(cardLayout);

    // Toggle compact ↔ detailed cards from the toolbar. Mirrors the Settings >
    // Appearance checkbox: persists to the same vault/localStorage key that
    // owns AppSettings, then broadcasts the change so useCardLayout and
    // useSettings re-read it without a remount.
    const handleToggleCardLayout = useCallback(async () => {
        const next: 'compact' | 'detailed' = cardLayout === 'detailed' ? 'compact' : 'detailed';
        try {
            const current = (await secureGetWithFallback<Record<string, unknown>>('app_settings', 'aeroftp_settings')) || {};
            const updated = { ...current, cardLayout: next };
            await secureStoreAndClean('app_settings', 'aeroftp_settings', updated);
            window.dispatchEvent(new CustomEvent('aeroftp-settings-changed', { detail: updated }));
        } catch (e) {
            logger.error('Failed to toggle cardLayout', e);
        }
    }, [cardLayout]);

    // Drag & reorder: works in any view (full list, search, or filter chip).
    // dragIdx/overIdx hold real indices into the full `servers` array — when
    // a filter is active we resolve the visible card to its real index by id,
    // so the reorder produces a coherent move in the underlying list.
    const canDrag = tableColumns.sort === null;

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
        const updated = [...servers];
        const [moved] = updated.splice(dragIdx, 1);
        // After splice the target index shifts left by one when the moved item
        // was earlier in the array — adjust so the dropped card lands where the
        // user actually pointed.
        const target = dragIdx < idx ? idx - 1 : idx;
        updated.splice(target, 0, moved);
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

    // Per-server reachability probe — only meaningful in detailed layout, so
    // we skip the scan otherwise to avoid burning network on a probe nobody
    // can see. `cardLayout` is read above (toolbar toggle uses it too).
    // Scans are throttled and cached for 5 minutes by the hook.
    const { getStatus: getHealthStatus, scanItems: scanHealth } = useProviderHealth();

    const healthTargets: HealthTarget[] = useMemo(() => {
        if (cardLayout !== 'detailed') return [];
        return filteredServers
            .map(buildHealthTarget)
            .filter((target): target is HealthTarget => target !== null);
    }, [cardLayout, filteredServers]);

    // Auto-scan when detailed layout is active and the visible list changes,
    // but do it in small sequential chunks. That keeps the cards responsive
    // with 50+ saved profiles and avoids one big tab-wide probe wave.
    useEffect(() => {
        if (cardLayout !== 'detailed' || healthTargets.length === 0) return;
        const runId = ++healthScanRunRef.current;
        let cancelled = false;
        const timer = window.setTimeout(() => {
            void (async () => {
                for (let i = 0; i < healthTargets.length; i += HEALTH_SCAN_CHUNK_SIZE) {
                    if (cancelled || healthScanRunRef.current !== runId) return;
                    await scanHealth(healthTargets.slice(i, i + HEALTH_SCAN_CHUNK_SIZE));
                    if (i + HEALTH_SCAN_CHUNK_SIZE < healthTargets.length) {
                        await wait(HEALTH_SCAN_CHUNK_DELAY_MS);
                    }
                }
            })();
        }, 600);
        return () => {
            cancelled = true;
            healthScanRunRef.current++;
            window.clearTimeout(timer);
        };
    }, [cardLayout, healthTargets, scanHealth]);

    /** Re-scan a single profile. Builds the same target shape as the bulk
     *  scan (so backend probe path is identical) and forces past the cache. */
    const handleRetryHealth = useCallback((server: ServerProfile) => {
        const target = buildHealthTarget(server);
        if (!target) return;
        void scanHealth([target], true);
    }, [scanHealth]);

    // Chip counts (computed once from full server list, not filtered)
    const chipCounts = useMemo(() => {
        const counts: Record<MyServersFilterBy, number> = {
            all: servers.length,
            ftp: 0, s3: 0, webdav: 0, cloud: 0, media: 0, dev: 0, favorites: 0,
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
                const connectedAt = new Date().toISOString();
                const updated = servers.map(s => s.id === server.id ? { ...s, lastConnected: connectedAt, username: updatedUsername || s.username } : s);
                setServers(updated);
                await mergeSavedServerProfile(server.id, latest => ({
                    ...latest,
                    lastConnected: connectedAt,
                    username: updatedUsername || latest.username,
                }));

                await onConnect({ server: result.display_name, username: updatedUsername, password: '', protocol: server.protocol, displayName: server.name, providerId: server.providerId, savedServerId: server.id }, server.initialPath, server.localInitialPath);
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
                const connectedAt = new Date().toISOString();
                const updated = servers.map(s => s.id === server.id ? { ...s, lastConnected: connectedAt, username: updatedUsername || s.username } : s);
                setServers(updated);
                await mergeSavedServerProfile(server.id, latest => ({
                    ...latest,
                    lastConnected: connectedAt,
                    username: updatedUsername || latest.username,
                }));

                await onConnect({ server: result.display_name, username: updatedUsername, password: '', protocol: server.protocol, displayName: server.name, providerId: server.providerId, savedServerId: server.id }, server.initialPath, server.localInitialPath);
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
            const connectedAt = new Date().toISOString();
            const updated = servers.map(s => s.id === server.id ? { ...s, lastConnected: connectedAt } : s);
            setServers(updated);
            await mergeSavedServerProfile(server.id, latest => ({
                ...latest,
                lastConnected: connectedAt,
            }));

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
                savedServerId: server.id,
            }, server.initialPath, server.localInitialPath);

            if (proto === 'filen') {
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
        setDeleteTarget(server);
    }, []);

    const handleRenameStart = useCallback((server: ServerProfile) => {
        setRenamingId(server.id);
    }, []);

    const handleRenameSubmit = useCallback((server: ServerProfile, newName: string) => {
        const trimmed = newName.trim();
        if (trimmed && trimmed !== server.name) {
            const updated = servers.map(s => s.id === server.id ? { ...s, name: trimmed } : s);
            setServers(updated);
            secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});
        }
        setRenamingId(null);
    }, [servers]);

    const handleRenameCancel = useCallback(() => {
        setRenamingId(null);
    }, []);

    const handleHoverChange = useCallback((server: ServerProfile | null) => {
        hoveredServerRef.current = server;
    }, []);

    // F2 hotkey: rename hovered server (skip if renaming or focus is in an input/textarea)
    useEffect(() => {
        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key !== 'F2') return;
            if (renamingId) return;
            const target = e.target as HTMLElement | null;
            const tag = target?.tagName?.toLowerCase();
            if (tag === 'input' || tag === 'textarea' || target?.isContentEditable) return;
            const hovered = hoveredServerRef.current;
            if (!hovered) return;
            e.preventDefault();
            setRenamingId(hovered.id);
        };
        window.addEventListener('keydown', onKeyDown);
        return () => window.removeEventListener('keydown', onKeyDown);
    }, [renamingId]);

    // Esc: clear active narrowing on the My Servers grid (search query + filter chip).
    // Mirrors the v3.6.6 Esc gesture that clears file selections in AeroFile panels.
    // Skipped when an input/textarea is focused so native Esc-to-clear behavior wins.
    useEffect(() => {
        const onKeyDown = (e: KeyboardEvent) => {
            if (e.key !== 'Escape') return;
            const target = e.target as HTMLElement | null;
            const tag = target?.tagName?.toLowerCase();
            if (tag === 'input' || tag === 'textarea' || target?.isContentEditable) return;
            if (renamingId) return;
            if (deleteTarget || healthCheckTarget !== false || speedTestTarget !== false) return;
            const hasNarrowing = searchQuery !== '' || activeFilter !== 'all';
            if (!hasNarrowing) return;
            e.preventDefault();
            if (searchQuery) setSearchQuery('');
            if (activeFilter !== 'all') {
                setActiveFilter('all');
                localStorage.setItem('aeroftp_myservers_filter', 'all');
            }
        };
        window.addEventListener('keydown', onKeyDown);
        return () => window.removeEventListener('keydown', onKeyDown);
    }, [searchQuery, activeFilter, renamingId, deleteTarget, healthCheckTarget, speedTestTarget]);

    const confirmDelete = useCallback(() => {
        if (!deleteTarget) return;
        const updated = servers.filter(s => s.id !== deleteTarget.id);
        setServers(updated);
        secureStoreAndClean('server_profiles', STORAGE_KEY, updated).catch(() => {});
        // Clean up orphaned vault credential
        invoke('delete_credential', { account: `server_${deleteTarget.id}` }).catch(() => {});
        onServersChange?.(updated.length);
        setDeleteTarget(null);
    }, [deleteTarget, servers, onServersChange]);

    const handleContextMenu = useCallback((e: React.MouseEvent, server: ServerProfile) => {
        const isFav = favorites.has(server.id);
        const items: ContextMenuItem[] = [
            { label: t('common.connect'), icon: <Play size={14} />, action: () => handleConnect(server) },
            { label: t('common.edit'), icon: <Edit2 size={14} />, action: () => onEdit(server) },
            { label: t('introHub.renameWithHotkey'), icon: <PencilLine size={14} />, action: () => handleRenameStart(server) },
            { label: t('common.duplicate'), icon: <Copy size={14} />, action: () => handleDuplicate(server) },
            { label: isFav ? t('introHub.removeFavorite') : t('introHub.addFavorite'), icon: <Star size={14} />, action: () => toggleFavorite(server.id) },
        ];
        if (onOpenCrossProfile && servers.length > 1) {
            items.push({
                label: t('introHub.setAsCrossProfileSource'),
                icon: <ArrowUpRight size={14} className="text-indigo-500" />,
                action: () => setAsCrossProfileSource(server.id),
                divider: true,
            });
            items.push({
                label: t('introHub.setAsCrossProfileDestination'),
                icon: <ArrowDownLeft size={14} className="text-emerald-500" />,
                action: () => setAsCrossProfileDestination(server.id),
            });
        }
        items.push(
            { label: t('healthCheck.title'), icon: <Activity size={14} />, action: () => setHealthCheckTarget(server.id), divider: true },
            {
                label: t('speedTest.title'),
                icon: <Gauge size={14} />,
                action: () => setSpeedTestTarget(server.id),
                disabled: !supportsSpeedTest(server),
            },
            { label: t('common.delete'), icon: <Trash2 size={14} />, action: () => handleDelete(server), danger: true },
        );
        showContextMenu(e, items);
    }, [t, handleConnect, onEdit, handleDuplicate, handleDelete, handleRenameStart, toggleFavorite, favorites, showContextMenu, onOpenCrossProfile, setAsCrossProfileSource, setAsCrossProfileDestination, servers.length]);

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
                hideUsername={hideUsername}
                onToggleHideUsername={toggleHideUsername}
                serverCount={servers.length}
                filteredCount={filteredServers.length}
                chipCounts={chipCounts}
                onOpenExportImport={onOpenExportImport}
                onHealthCheck={() => setHealthCheckTarget('all')}
                onSpeedTest={() => setSpeedTestTarget(undefined)}
                onOpenCrossProfile={onOpenCrossProfile && servers.length > 1 ? handleOpenCrossProfile : undefined}
                crossProfileSelectionCount={crossProfileSelection.length}
                cardLayout={cardLayout}
                onToggleCardLayout={handleToggleCardLayout}
                listDensity={density}
                onToggleListDensity={() => setDensity(density === 'compact' ? 'comfortable' : 'compact')}
            />

            {filteredServers.length === 0 ? (
                <div className="flex-1 flex flex-col items-center justify-center py-10 px-4">
                    {servers.length === 0 ? (
                        <div className="w-full max-w-2xl flex flex-col items-center text-center">
                            <ServerIcon size={48} className="text-gray-300 dark:text-gray-600 mb-4" />
                            <h2 className="text-xl font-semibold text-gray-800 dark:text-gray-100 mb-1">
                                {t('introHub.getStarted')}
                            </h2>
                            <p className="text-sm text-gray-500 dark:text-gray-400 mb-6">
                                {t('introHub.noServersHint')}
                            </p>
                            <button
                                onClick={onQuickConnect}
                                className="flex items-center gap-2 px-5 py-2.5 bg-blue-600 hover:bg-blue-500 text-white rounded-lg text-sm font-semibold shadow-sm transition-colors mb-8"
                            >
                                <Plus size={18} />
                                {t('introHub.addFirstServer')}
                            </button>

                            {onJumpToCategory && (
                                <>
                                    <div className="w-full flex items-center gap-3 mb-4">
                                        <div className="flex-1 h-px bg-gray-200 dark:bg-gray-700" />
                                        <span className="text-xs uppercase tracking-wider text-gray-400 dark:text-gray-500 font-medium">
                                            {t('introHub.browseCategory')}
                                        </span>
                                        <div className="flex-1 h-px bg-gray-200 dark:bg-gray-700" />
                                    </div>
                                    <div className="grid grid-cols-2 sm:grid-cols-3 gap-2.5 w-full">
                                        {EMPTY_STATE_CATEGORIES.map((cat) => (
                                            <button
                                                key={cat.id}
                                                onClick={() => onJumpToCategory(cat.id)}
                                                className="flex items-center gap-2.5 px-3.5 py-2.5 bg-white dark:bg-gray-800 hover:bg-blue-50 dark:hover:bg-blue-900/20 border border-gray-200 dark:border-gray-700 hover:border-blue-300 dark:hover:border-blue-500/50 rounded-lg text-sm text-left transition-colors"
                                            >
                                                <span className={`shrink-0 ${cat.iconColor}`}>{cat.icon}</span>
                                                <span className="text-gray-700 dark:text-gray-200 font-medium truncate">
                                                    {t(cat.labelKey)}
                                                </span>
                                            </button>
                                        ))}
                                    </div>
                                </>
                            )}
                        </div>
                    ) : (
                        <div className="text-center">
                            <ServerIcon size={48} className="text-gray-300 dark:text-gray-600 mb-4 mx-auto" />
                            <p className="text-gray-500 dark:text-gray-400 mb-1">{t('introHub.noResults')}</p>
                            <p className="text-sm text-gray-400 dark:text-gray-500">
                                {t('introHub.noResultsHint', { query: searchQuery })}
                            </p>
                        </div>
                    )}
                </div>
            ) : viewMode === 'grid' ? (
                <div
                    ref={scrollContainerRef}
                    className="flex-1 overflow-y-auto pr-3 custom-scroll-area"
                    style={{ willChange: 'scroll-position', transform: 'translateZ(0)' }}
                >
                    <div
                        className="grid grid-cols-2 md:grid-cols-3 lg:grid-cols-4 xl:grid-cols-5 2xl:grid-cols-6 gap-3 p-1"
                        style={{ contain: 'layout style' }}
                    >
                        {filteredServers.map((server) => {
                            // Resolve the real index in the full `servers` array so drag-reorder
                            // works correctly even with a filter or search applied.
                            const realIdx = servers.findIndex(s => s.id === server.id);
                            const selectionIndex = crossProfileSelection.indexOf(server.id);
                            const selectionRole: 'source' | 'destination' | null =
                                selectionIndex === 0 ? 'source' : selectionIndex === 1 ? 'destination' : null;
                            const health = cardLayout === 'detailed' ? getHealthStatus(server.id) : undefined;
                            return (
                                <ServerCard
                                    key={server.id}
                                    server={server}
                                    isConnecting={connectingId === server.id || oauthConnecting === server.id}
                                    credentialsMasked={credentialsMasked}
                                    hideUsername={hideUsername}
                                    isFavorite={favorites.has(server.id)}
                                    onConnect={handleConnect}
                                    onEdit={onEdit}
                                    onDuplicate={handleDuplicate}
                                    onDelete={handleDelete}
                                    onToggleFavorite={handleToggleFavorite}
                                    onContextMenu={handleContextMenu}
                                    onHoverChange={handleHoverChange}
                                    isRenaming={renamingId === server.id}
                                    onRenameSubmit={handleRenameSubmit}
                                    onRenameCancel={handleRenameCancel}
                                    isDraggable={canDrag}
                                    isDragging={dragIdx === realIdx}
                                    isDragTarget={overIdx === realIdx && dragIdx !== null && dragIdx !== realIdx}
                                    onDragStart={canDrag ? handleDragStart(realIdx) : undefined}
                                    onDragEnter={canDrag ? handleDragEnter(realIdx) : undefined}
                                    onDragOver={canDrag ? handleDragOver(realIdx) : undefined}
                                    onDrop={canDrag ? handleDrop(realIdx) : undefined}
                                    onDragEnd={canDrag ? handleDragEnd : undefined}
                                    selectionRole={selectionRole}
                                    onSelect={servers.length > 1 ? handleSelectServer : undefined}
                                    healthStatus={health?.status}
                                    healthLatencyMs={health?.latencyMs}
                                    onRetryHealth={cardLayout === 'detailed' ? handleRetryHealth : undefined}
                                    thresholds={thresholds}
                                />
                            );
                        })}
                    </div>
                </div>
            ) : (
                <div className="flex-1 flex flex-col bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700 overflow-hidden">
                    <div
                        ref={scrollContainerRef}
                        className="flex-1 overflow-auto custom-scroll-area"
                    >
                    <MyServersTable
                        servers={filteredServers}
                        allServers={servers}
                        columns={tableColumns}
                        favorites={favorites}
                        connectingId={connectingId}
                        oauthConnecting={oauthConnecting}
                        credentialsMasked={credentialsMasked}
                        hideUsername={hideUsername}
                        onConnect={handleConnect}
                        onEdit={onEdit}
                        onDuplicate={handleDuplicate}
                        onDelete={handleDelete}
                        onToggleFavorite={handleToggleFavorite}
                        onContextMenu={handleContextMenu}
                        onHoverChange={handleHoverChange}
                        renamingId={renamingId}
                        onRenameSubmit={handleRenameSubmit}
                        onRenameCancel={handleRenameCancel}
                        canDrag={canDrag}
                        dragIdx={dragIdx}
                        overIdx={overIdx}
                        onDragStart={handleDragStart}
                        onDragEnter={handleDragEnter}
                        onDragOver={handleDragOver}
                        onDrop={handleDrop}
                        onDragEnd={handleDragEnd}
                        crossProfileSelection={crossProfileSelection}
                        onSelect={handleSelectServer}
                        cardLayout={cardLayout}
                        getHealthStatus={getHealthStatus}
                        onRetryHealth={handleRetryHealth}
                        thresholds={thresholds}
                        density={density}
                    />
                    {breakdownOpen && (
                        <div className="px-3 pb-4">
                            <MyServersProtocolBreakdown servers={filteredServers} thresholds={thresholds} />
                        </div>
                    )}
                    </div>
                    <MyServersTableFooter
                        servers={filteredServers}
                        breakdownAvailable={breakdownIsAvailable(filteredServers)}
                        breakdownOpen={breakdownOpen}
                        onToggleBreakdown={() => setBreakdownOpen(prev => !prev)}
                    />
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
            {speedTestTarget !== false && (
                <SpeedTestDialog
                    servers={servers}
                    initialServerId={speedTestTarget || undefined}
                    onClose={() => setSpeedTestTarget(false)}
                />
            )}
            {deleteTarget && (
                <AlertDialog
                    title={t('common.delete')}
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
}
