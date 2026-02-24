/**
 * ConnectionScreen Component
 * Initial connection form with Quick Connect and Saved Servers
 */

import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { readFile } from '@tauri-apps/plugin-fs';
import { FolderOpen, HardDrive, ChevronRight, ChevronDown, Save, Cloud, Check, Settings, Clock, Folder, X, Lock, ArrowLeft, Eye, EyeOff, ExternalLink, Shield, KeyRound, Loader2, Image } from 'lucide-react';
import { ConnectionParams, ProviderType, isOAuthProvider, isAeroCloudProvider, isFourSharedProvider, ServerProfile } from '../types';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { SavedServers } from './SavedServers';
import { ExportImportDialog } from './ExportImportDialog';
import { useTranslation } from '../i18n';
import { ProtocolSelector, ProtocolFields, getDefaultPort } from './ProtocolSelector';
import { OAuthConnect } from './OAuthConnect';
import { ProviderSelector } from './ProviderSelector';
import { getProviderById, ProviderConfig } from '../providers';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

// Storage key for saved servers (same as SavedServers component)
const SERVERS_STORAGE_KEY = 'aeroftp-saved-servers';

const PROTOCOL_COLORS: Record<string, string> = {
    ftp: 'from-blue-500 to-cyan-400',
    ftps: 'from-green-500 to-emerald-400',
    sftp: 'from-purple-500 to-violet-400',
    webdav: 'from-orange-500 to-amber-400',
    s3: 'from-amber-500 to-yellow-400',
    aerocloud: 'from-sky-400 to-blue-500',
    googledrive: 'from-red-500 to-red-400',
    dropbox: 'from-blue-600 to-blue-400',
    onedrive: 'from-sky-500 to-sky-400',
    mega: 'from-red-600 to-red-500',
    box: 'from-blue-500 to-blue-600',
    pcloud: 'from-green-500 to-teal-400',
    azure: 'from-blue-600 to-indigo-500',
    filen: 'from-emerald-500 to-green-400',
};

// AeroCloud config interface (matching Rust struct)
interface AeroCloudConfig {
    enabled: boolean;
    cloud_name: string;
    local_folder: string;
    remote_folder: string;
    server_profile: string;
    sync_interval_secs: number;
    sync_on_change: boolean;
    sync_on_startup: boolean;
    last_sync: string | null;
}

interface QuickConnectDirs {
    remoteDir: string;
    localDir: string;
}

interface ConnectionScreenProps {
    connectionParams: ConnectionParams;
    quickConnectDirs: QuickConnectDirs;
    loading: boolean;
    onConnectionParamsChange: (params: ConnectionParams) => void;
    onQuickConnectDirsChange: (dirs: QuickConnectDirs) => void;
    onConnect: () => void;
    onSavedServerConnect: (params: ConnectionParams, initialPath?: string, localInitialPath?: string) => Promise<void>;
    onSkipToFileManager: () => void;
    onAeroFile?: () => void;
    onOpenCloudPanel?: () => void;
    hasExistingSessions?: boolean;  // Show active sessions badge next to QuickConnect
    serversRefreshKey?: number;  // Change this to force refresh of saved servers list
}

// --- FourSharedConnect: OAuth 1.0 authentication for 4shared ---
interface FourSharedConnectProps {
    initialLocalPath?: string;
    onLocalPathChange?: (path: string) => void;
    saveConnection?: boolean;
    onSaveConnectionChange?: (save: boolean) => void;
    connectionName?: string;
    onConnectionNameChange?: (name: string) => void;
    onConnected: (displayName: string) => void;
}

const FourSharedConnect: React.FC<FourSharedConnectProps> = ({
    initialLocalPath = '',
    onLocalPathChange,
    saveConnection = false,
    onSaveConnectionChange,
    connectionName = '',
    onConnectionNameChange,
    onConnected,
}) => {
    const t = useTranslation();
    const [hasExistingTokens, setHasExistingTokens] = useState(false);
    const [isChecking, setIsChecking] = useState(true);
    const [isAuthenticating, setIsAuthenticating] = useState(false);
    const [isConnecting, setIsConnecting] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [localPath, setLocalPath] = useState(initialLocalPath);
    const [wantToSave, setWantToSave] = useState(saveConnection);
    const [saveName, setSaveName] = useState(connectionName);
    const [consumerKey, setConsumerKey] = useState('');
    const [consumerSecret, setConsumerSecret] = useState('');
    const [showCredentialsForm, setShowCredentialsForm] = useState(false);
    const [wantsNewAccount, setWantsNewAccount] = useState(false);
    const [showSecret, setShowSecret] = useState(false);

    // Load consumer key/secret from credential store
    useEffect(() => {
        const load = async () => {
            try {
                const key = await invoke<string>('get_credential', { account: 'oauth_fourshared_client_id' });
                if (key) setConsumerKey(key);
            } catch { /* no stored key */ }
            try {
                const secret = await invoke<string>('get_credential', { account: 'oauth_fourshared_client_secret' });
                if (secret) setConsumerSecret(secret);
            } catch { /* no stored secret */ }
        };
        load();
    }, []);

    // Check for existing tokens
    useEffect(() => {
        const check = async () => {
            setIsChecking(true);
            try {
                const exists = await invoke<boolean>('fourshared_has_tokens');
                setHasExistingTokens(exists);
            } catch {
                setHasExistingTokens(false);
            }
            setIsChecking(false);
        };
        check();
    }, []);

    const browseLocalFolder = async () => {
        try {
            const selected = await open({ directory: true, multiple: false, title: t('connection.fourshared.selectLocalFolder') });
            if (selected && typeof selected === 'string') {
                setLocalPath(selected);
                onLocalPathChange?.(selected);
            }
        } catch { /* cancelled */ }
    };

    const handleSignIn = async () => {
        if (!consumerKey || !consumerSecret) {
            setShowCredentialsForm(true);
            return;
        }
        setIsAuthenticating(true);
        setError(null);
        // Save credentials to vault
        invoke('store_credential', { account: 'oauth_fourshared_client_id', password: consumerKey }).catch(() => {});
        invoke('store_credential', { account: 'oauth_fourshared_client_secret', password: consumerSecret }).catch(() => {});
        try {
            await invoke<string>('fourshared_full_auth', { params: { consumer_key: consumerKey, consumer_secret: consumerSecret } });
            setHasExistingTokens(true);
            // Now connect
            await handleConnect();
        } catch (e) {
            setError(String(e));
        } finally {
            setIsAuthenticating(false);
        }
    };

    const handleConnect = async () => {
        if (!consumerKey || !consumerSecret) {
            setShowCredentialsForm(true);
            return;
        }
        setIsConnecting(true);
        setError(null);
        try {
            const result = await invoke<{ display_name: string; account_email: string | null }>('fourshared_connect', { params: { consumer_key: consumerKey, consumer_secret: consumerSecret } });
            onConnected(result.display_name || '4shared');
        } catch (e) {
            setError(String(e));
        } finally {
            setIsConnecting(false);
        }
    };

    const handleLogout = async () => {
        try {
            await invoke('fourshared_logout');
            setHasExistingTokens(false);
            setWantsNewAccount(false);
        } catch (e) {
            setError(String(e));
        }
    };

    if (isChecking) {
        return (
            <div className="flex items-center justify-center p-4">
                <div className="w-5 h-5 border-2 border-blue-500 border-t-transparent rounded-full animate-spin" />
            </div>
        );
    }

    // Active state — already authenticated
    if (hasExistingTokens && !wantsNewAccount) {
        return (
            <div className="space-y-4">
                <div className="p-4 rounded-xl border-2 border-blue-500/30 bg-blue-500/5">
                    <div className="flex items-center gap-3">
                        <div className="w-12 h-12 rounded-xl flex items-center justify-center bg-blue-500/20">
                            <Cloud size={24} className="text-blue-500" />
                        </div>
                        <div className="flex-1">
                            <div className="flex items-center gap-2">
                                <span className="font-medium">4shared</span>
                                <span className="px-2 py-0.5 text-xs font-medium bg-green-500/20 text-green-400 rounded-full flex items-center gap-1">
                                    <Check size={12} />
                                    {t('connection.active')}
                                </span>
                            </div>
                            <span className="text-sm text-gray-500">{t('connection.fourshared.previouslyAuthenticated')}</span>
                        </div>
                    </div>
                </div>
                <button
                    onClick={handleConnect}
                    disabled={isConnecting || isAuthenticating}
                    className="w-full py-3 px-4 rounded-xl text-white font-medium flex items-center justify-center gap-2 transition-colors bg-blue-500 hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
                >
                    {isConnecting ? (
                        <>
                            <div className="w-5 h-5 border-2 border-white border-t-transparent rounded-full animate-spin" />
                            {t('connection.connecting')}
                        </>
                    ) : (
                        <>
                            <Cloud size={18} />
                            {t('connection.fourshared.connectTo4shared')}
                        </>
                    )}
                </button>
                <div className="flex gap-2">
                    <button
                        onClick={() => setWantsNewAccount(true)}
                        className="flex-1 py-2 px-3 text-sm text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-xl flex items-center justify-center gap-2 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                    >
                        {t('connection.fourshared.useDifferentAccount')}
                    </button>
                    <button
                        onClick={handleLogout}
                        className="py-2 px-3 text-sm text-red-500 hover:text-red-600 border border-red-300 dark:border-red-600/50 rounded-xl flex items-center justify-center gap-2 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors"
                        title={t('connection.fourshared.disconnectAccount')}
                    >
                        <X size={14} />
                    </button>
                </div>
                {error && (
                    <div className="p-3 bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded-lg">
                        <span className="text-sm text-red-700 dark:text-red-300">{error}</span>
                    </div>
                )}
            </div>
        );
    }

    // Sign-in state
    return (
        <div className="space-y-4">
            {/* Local Path */}
            <div>
                <label className="block text-sm font-medium mb-1.5">{t('connection.fourshared.localFolderOptional')}</label>
                <div className="flex gap-2">
                    <input
                        type="text"
                        value={localPath}
                        onChange={(e) => { setLocalPath(e.target.value); onLocalPathChange?.(e.target.value); }}
                        placeholder="~/Downloads"
                        className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-sm"
                    />
                    <button type="button" onClick={browseLocalFolder} className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-xl" title={t('common.browse')}>
                        <FolderOpen size={18} />
                    </button>
                </div>
            </div>

            {/* Save Connection */}
            <div className="flex items-center gap-3 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-xl">
                <input
                    type="checkbox"
                    id="save-fourshared"
                    checked={wantToSave}
                    onChange={(e) => { setWantToSave(e.target.checked); onSaveConnectionChange?.(e.target.checked); }}
                    className="w-4 h-4 rounded border-gray-300 text-blue-600 focus:ring-blue-500"
                />
                <label htmlFor="save-fourshared" className="flex-1">
                    <span className="text-sm font-medium">{t('connection.saveThisConnection')}</span>
                    <p className="text-xs text-gray-500">{t('connection.fourshared.quickConnectNextTime')}</p>
                </label>
                <Save size={16} className="text-gray-400" />
            </div>

            {wantToSave && (
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('connection.connectionNameOptional')}</label>
                    <input
                        type="text"
                        value={saveName}
                        onChange={(e) => { setSaveName(e.target.value); onConnectionNameChange?.(e.target.value); }}
                        placeholder={t('connection.fourshared.my4shared')}
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-sm"
                    />
                </div>
            )}

            {/* Sign In Button */}
            <button
                onClick={hasExistingTokens ? handleConnect : handleSignIn}
                disabled={isAuthenticating || isConnecting}
                className="w-full py-3 px-4 rounded-lg text-white font-medium flex items-center justify-center gap-2 transition-colors bg-blue-500 hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
            >
                {isAuthenticating || isConnecting ? (
                    <>
                        <div className="w-5 h-5 border-2 border-white border-t-transparent rounded-full animate-spin" />
                        {isAuthenticating ? t('connection.authenticating') : t('connection.connecting')}
                    </>
                ) : (
                    <>
                        <Cloud size={18} />
                        {t('connection.fourshared.signInWith4shared')}
                    </>
                )}
            </button>

            {error && (
                <div className="p-3 bg-red-100 dark:bg-red-900/30 border border-red-300 dark:border-red-700 rounded-lg">
                    <span className="text-sm text-red-700 dark:text-red-300">{error}</span>
                </div>
            )}

            {/* Credentials Form */}
            {showCredentialsForm && (
                <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                    <div className="flex items-center justify-between">
                        <h4 className="font-medium text-sm">{t('connection.fourshared.oauth1Credentials')}</h4>
                        <button
                            onClick={() => { try { invoke('open_url', { url: 'https://www.4shared.com/developer/' }); } catch { /* ignore */ } }}
                            className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                        >
                            {t('settings.getCredentials')} <ExternalLink size={12} />
                        </button>
                    </div>
                    <p className="text-xs text-gray-500 dark:text-gray-400">
                        {t('connection.fourshared.createAppInstructions')}
                    </p>
                    <div>
                        <label className="block text-xs font-medium mb-1">{t('settings.consumerKey')}</label>
                        <input
                            type="text"
                            value={consumerKey}
                            onChange={(e) => setConsumerKey(e.target.value)}
                            placeholder={t('connection.fourshared.enterConsumerKey')}
                            className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                        />
                    </div>
                    <div>
                        <label className="block text-xs font-medium mb-1">{t('settings.consumerSecret')}</label>
                        <div className="relative">
                            <input
                                type={showSecret ? 'text' : 'password'}
                                value={consumerSecret}
                                onChange={(e) => setConsumerSecret(e.target.value)}
                                placeholder={t('connection.fourshared.enterConsumerSecret')}
                                className="w-full px-3 py-2 pr-10 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                            />
                            <button type="button" onClick={() => setShowSecret(!showSecret)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                {showSecret ? <EyeOff size={16} /> : <Eye size={16} />}
                            </button>
                        </div>
                    </div>
                    <div className="flex gap-2">
                        <button onClick={() => setShowCredentialsForm(false)} className="flex-1 py-2 px-3 text-sm border rounded-lg hover:bg-gray-100 dark:hover:bg-gray-600">
                            {t('common.cancel')}
                        </button>
                        <button
                            onClick={handleSignIn}
                            disabled={!consumerKey || !consumerSecret}
                            className="flex-1 py-2 px-3 text-sm text-white rounded-lg bg-blue-500 hover:bg-blue-600 disabled:opacity-50"
                        >
                            {t('connection.fourshared.continue')}
                        </button>
                    </div>
                </div>
            )}

            {!showCredentialsForm && (
                <button
                    onClick={() => setShowCredentialsForm(true)}
                    className="w-full py-2 text-sm text-gray-500 hover:text-gray-700 dark:hover:text-gray-300 flex items-center justify-center gap-1"
                >
                    <Settings size={16} />
                    {t('connection.fourshared.configureCredentials')}
                </button>
            )}

            {wantsNewAccount && hasExistingTokens && (
                <button onClick={() => setWantsNewAccount(false)} className="w-full py-2 text-sm text-blue-500 hover:text-blue-600 flex items-center justify-center gap-1">
                    &larr; {t('connection.fourshared.backToExistingAccount')}
                </button>
            )}
        </div>
    );
};

export const ConnectionScreen: React.FC<ConnectionScreenProps> = ({
    connectionParams,
    quickConnectDirs,
    loading,
    onConnectionParamsChange,
    onQuickConnectDirsChange,
    onConnect,
    onSavedServerConnect,
    onSkipToFileManager,
    onAeroFile,
    onOpenCloudPanel,
    hasExistingSessions = false,
    serversRefreshKey = 0,
}) => {
    const t = useTranslation();
    const protocol = connectionParams.protocol; // Can be undefined

    // Save connection state
    const [saveConnection, setSaveConnection] = useState(false);
    const [connectionName, setConnectionName] = useState('');
    const [customIconForSave, setCustomIconForSave] = useState<string | undefined>(undefined);
    const [faviconForSave, setFaviconForSave] = useState<string | undefined>(undefined);

    // AeroCloud state
    const [aeroCloudConfig, setAeroCloudConfig] = useState<AeroCloudConfig | null>(null);
    const [aeroCloudLoading, setAeroCloudLoading] = useState(false);

    // Edit state
    const [editingProfileId, setEditingProfileId] = useState<string | null>(null);
    const editingProfileIdRef = useRef<string | null>(null);
    const [savedServersUpdate, setSavedServersUpdate] = useState(0);
    const [showPassword, setShowPassword] = useState(false);

    // Provider selection state (for S3/WebDAV)
    const [selectedProviderId, setSelectedProviderId] = useState<string | null>(null);
    const selectedProvider = selectedProviderId ? getProviderById(selectedProviderId) : null;

    // Protocol selector open state (to hide form when selector is open)
    const [isProtocolSelectorOpen, setIsProtocolSelectorOpen] = useState(false);

    // When re-opening dropdown with a protocol already selected, clear the selection
    const handleProtocolSelectorOpenChange = (open: boolean) => {
        setIsProtocolSelectorOpen(open);
        if (open && protocol) {
            onConnectionParamsChange({
                ...connectionParams,
                protocol: undefined,
            });
            setSelectedProviderId(null);
            if (editingProfileId) {
                setEditingProfileId(null);
                editingProfileIdRef.current = null;
                setConnectionName('');
                setCustomIconForSave(undefined);
                setFaviconForSave(undefined);
                setSaveConnection(false);
            }
        }
    };

    // Export/Import dialog state
    const [showExportImport, setShowExportImport] = useState(false);
    const [servers, setServers] = useState<ServerProfile[]>([]);

    // Load servers when opening export/import dialog
    useEffect(() => {
        if (showExportImport) {
            // Sync fallback first
            try {
                const stored = localStorage.getItem(SERVERS_STORAGE_KEY);
                if (stored) setServers(JSON.parse(stored));
            } catch { /* ignore */ }
            // Then try vault
            (async () => {
                const vaultServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', SERVERS_STORAGE_KEY);
                if (vaultServers && vaultServers.length > 0) setServers(vaultServers);
            })();
        }
    }, [showExportImport]);
    const [securityInfoOpen, setSecurityInfoOpen] = useState(false);

    // Fetch AeroCloud config when AeroCloud is selected
    useEffect(() => {
        if (protocol === 'aerocloud') {
            setAeroCloudLoading(true);
            invoke<AeroCloudConfig>('get_cloud_config')
                .then(config => {
                    setAeroCloudConfig(config);
                    setAeroCloudLoading(false);
                })
                .catch(() => {
                    setAeroCloudConfig(null);
                    setAeroCloudLoading(false);
                });
        }
    }, [protocol]);

    // Store a credential in the universal vault
    const tryStoreCredential = async (account: string, password: string | undefined): Promise<boolean> => {
        if (!password) return false;
        try {
            await invoke('store_credential', { account, password });
            return true;
        } catch (err) {
            console.error('Failed to store credential:', err);
            return false;
        }
    };

    // Save the current connection to saved servers (or update existing)
    const saveToServers = async () => {
        // If editing an existing profile (and not creating a copy), name/saveConnection might be implicit
        if (!protocol) return;

        // MEGA: Add/Update session expiry (24h)
        const optionsToSave = { ...connectionParams.options };
        if (protocol === 'mega') {
            optionsToSave.session_expires_at = Date.now() + 24 * 60 * 60 * 1000;
        }

        // Try vault first, fallback to localStorage
        const existingServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', SERVERS_STORAGE_KEY) || [];

        if (editingProfileId) {
            const credentialStored = await tryStoreCredential(`server_${editingProfileId}`, connectionParams.password);

            const updatedServers = existingServers.map((s: ServerProfile) => {
                if (s.id === editingProfileId) {
                    return {
                        ...s,
                        name: connectionName || s.name,
                        host: connectionParams.server,
                        port: connectionParams.port || getDefaultPort(protocol),
                        username: connectionParams.username,
                        hasStoredCredential: credentialStored || (s.hasStoredCredential && !connectionParams.password),
                        protocol: protocol as ProviderType,
                        options: optionsToSave,
                        initialPath: quickConnectDirs.remoteDir,
                        localInitialPath: quickConnectDirs.localDir,
                        providerId: selectedProviderId || s.providerId,
                        customIconUrl: customIconForSave !== undefined ? customIconForSave : s.customIconUrl,
                    };
                }
                return s;
            });

            localStorage.setItem(SERVERS_STORAGE_KEY, JSON.stringify(updatedServers));
            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, updatedServers).catch(() => {});
            setSavedServersUpdate(Date.now());
        } else if (saveConnection) {
            const newId = `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
            const credentialStored = await tryStoreCredential(`server_${newId}`, connectionParams.password);

            const newServer: ServerProfile = {
                id: newId,
                name: connectionName || connectionParams.server || protocol,
                host: connectionParams.server,
                port: connectionParams.port || getDefaultPort(protocol),
                username: connectionParams.username,
                hasStoredCredential: credentialStored,
                protocol: protocol as ProviderType,
                initialPath: quickConnectDirs.remoteDir,
                localInitialPath: quickConnectDirs.localDir,
                options: optionsToSave,
                providerId: selectedProviderId || undefined,
                customIconUrl: customIconForSave,
            };

            const newServers = [...existingServers, newServer];
            localStorage.setItem(SERVERS_STORAGE_KEY, JSON.stringify(newServers));
            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => {});
            setSavedServersUpdate(Date.now());
        }
    };

    // Icon picker for saved connections (no provider logo)
    const pickCustomIcon = async () => {
        try {
            const selected = await open({
                multiple: false,
                filters: [{ name: 'Images', extensions: ['png', 'jpg', 'jpeg', 'ico', 'webp', 'gif'] }],
            });
            if (!selected) return;
            const filePath = Array.isArray(selected) ? selected[0] : selected;
            const bytes = await readFile(filePath);
            const ext = filePath.split('.').pop()?.toLowerCase() || 'png';
            const mimeMap: Record<string, string> = { jpg: 'image/jpeg', jpeg: 'image/jpeg', png: 'image/png', gif: 'image/gif', webp: 'image/webp', ico: 'image/x-icon' };
            const mime = mimeMap[ext] || 'image/png';
            const blob = new Blob([bytes], { type: mime });
            const url = URL.createObjectURL(blob);
            const img = new window.Image();
            const timeout = setTimeout(() => URL.revokeObjectURL(url), 10000);
            img.onload = () => {
                clearTimeout(timeout);
                const canvas = document.createElement('canvas');
                const size = 128;
                canvas.width = size; canvas.height = size;
                const ctx = canvas.getContext('2d');
                if (!ctx) { URL.revokeObjectURL(url); return; }
                const scale = Math.min(size / img.width, size / img.height);
                const w = img.width * scale, h = img.height * scale;
                ctx.drawImage(img, (size - w) / 2, (size - h) / 2, w, h);
                setCustomIconForSave(canvas.toDataURL('image/png'));
                URL.revokeObjectURL(url);
            };
            img.onerror = () => { clearTimeout(timeout); URL.revokeObjectURL(url); };
            img.src = url;
        } catch { /* cancelled */ }
    };

    const hasProviderLogoForSave = !!PROVIDER_LOGOS[selectedProviderId || connectionParams.protocol || ''];

    const renderIconPicker = () => {
        if (hasProviderLogoForSave) return null;
        const proto = connectionParams.protocol || 'ftp';
        const hasIcon = !!customIconForSave || !!faviconForSave;
        const letter = (connectionName || connectionParams.server || '?').charAt(0).toUpperCase();
        return (
            <div className="mt-2">
                <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.serverIcon')}</label>
                <div className="flex items-center gap-3">
                    <div className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center ${hasIcon ? 'bg-white dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${PROTOCOL_COLORS[proto] || PROTOCOL_COLORS.ftp} text-white`}`}>
                        {customIconForSave ? (
                            <img src={customIconForSave} alt="" className="w-6 h-6 rounded object-contain" />
                        ) : faviconForSave ? (
                            <img src={faviconForSave} alt="" className="w-6 h-6 rounded object-contain" />
                        ) : (
                            <span className="font-bold text-sm">{letter}</span>
                        )}
                    </div>
                    <button
                        type="button"
                        onClick={pickCustomIcon}
                        className="px-3 py-1.5 text-xs font-medium rounded-lg bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 border border-gray-300 dark:border-gray-600 transition-colors flex items-center gap-1.5"
                    >
                        <Image size={12} />
                        {t('settings.chooseIcon')}
                    </button>
                    {customIconForSave && (
                        <button
                            type="button"
                            onClick={() => setCustomIconForSave(undefined)}
                            className="p-1.5 text-xs rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-red-500 transition-colors"
                            title={t('settings.removeIcon')}
                        >
                            <X size={14} />
                        </button>
                    )}
                </div>
            </div>
        );
    };

    // Handle the main action button
    const handleConnectAndSave = async () => {
        if (editingProfileId) {
            // Edit mode: save changes and reset form
            await saveToServers();
            setEditingProfileId(null);
            editingProfileIdRef.current = null;
            setConnectionName('');
            setSaveConnection(false);
            onConnectionParamsChange({ server: '', username: '', password: '' });
            onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
        } else if (saveConnection) {
            // Save mode: only save, user connects from saved servers list
            await saveToServers();
            setConnectionName('');
            setSaveConnection(false);
            onConnectionParamsChange({ server: '', username: '', password: '' });
            onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
        } else {
            // Connect mode: just connect without saving
            onConnect();
        }
    };

    const handleEdit = async (profile: ServerProfile) => {
        // Close protocol selector dropdown so the form becomes visible
        setIsProtocolSelectorOpen(false);

        // Reset form FIRST to clear previous server's data immediately
        // This prevents stale data from showing when switching between servers
        setEditingProfileId(profile.id);
        editingProfileIdRef.current = profile.id;
        setConnectionName(profile.name);
        setCustomIconForSave(profile.customIconUrl);
        setFaviconForSave(profile.faviconUrl);
        setSaveConnection(true); // Implied for editing
        setSelectedProviderId(profile.providerId || null);

        // Immediately update form with new profile data (password empty initially)
        onConnectionParamsChange({
            server: profile.host,
            port: profile.port,
            username: profile.username,
            password: profile.password || '', // Set immediately, will be updated if stored
            protocol: profile.protocol || 'ftp',
            options: profile.options || {}
        });

        onQuickConnectDirsChange({
            remoteDir: profile.initialPath || '',
            localDir: profile.localInitialPath || ''
        });

        // Then load password from OS keyring asynchronously (if stored)
        const targetProfileId = profile.id;
        if (!profile.password && profile.hasStoredCredential) {
            try {
                const storedPassword = await invoke<string>('get_credential', { account: `server_${targetProfileId}` });
                // Only update if we're still editing the same profile (prevents race condition
                // where user switches to editing a different server before credential fetch completes)
                if (storedPassword && editingProfileIdRef.current === targetProfileId) {
                    onConnectionParamsChange({
                        server: profile.host,
                        port: profile.port,
                        username: profile.username,
                        password: storedPassword,
                        protocol: profile.protocol || 'ftp',
                        options: profile.options || {}
                    });
                }
            } catch {
                // Credential not found, password stays empty
            }
        }
    };

    const handleCancelEdit = () => {
        setEditingProfileId(null);
        editingProfileIdRef.current = null;
        setConnectionName('');
        setCustomIconForSave(undefined);
        setFaviconForSave(undefined);
        setSaveConnection(false);
        // Reset params
        onConnectionParamsChange({ ...connectionParams, server: '', username: '', password: '', options: {} });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
    };

    const handleBrowseLocalDir = async () => {
        try {
            const selected = await open({ directory: true, multiple: false, title: t('browser.local') });
            if (selected && typeof selected === 'string') {
                onQuickConnectDirsChange({ ...quickConnectDirs, localDir: selected });
            }
        } catch (e) {
            console.error('Folder picker error:', e);
        }
    };

    // Browse for SSH key file (SFTP)
    const handleBrowseSshKey = async () => {
        try {
            const selected = await open({
                multiple: false,
                title: t('connection.selectSshKey'),
                filters: [
                    { name: t('connection.allFiles'), extensions: ['*'] },
                    { name: t('connection.sshKeys'), extensions: ['pem', 'key', 'ppk'] },
                ]
            });
            if (selected && typeof selected === 'string') {
                onConnectionParamsChange({
                    ...connectionParams,
                    options: { ...connectionParams.options, private_key_path: selected }
                });
            }
        } catch (e) {
            console.error('File picker error:', e);
        }
    };

    const handleProtocolChange = (newProtocol: ProviderType) => {
        // Exit edit mode when changing protocol (user wants to create new connection)
        if (editingProfileId) {
            setEditingProfileId(null);
            editingProfileIdRef.current = null;
            setConnectionName('');
            setSaveConnection(false);
        }

        // Reset provider selection when protocol changes
        setSelectedProviderId(null);

        // Reset ALL form fields (clear previous server's credentials)
        onConnectionParamsChange({
            server: '',
            username: '',
            password: '',
            protocol: newProtocol,
            port: getDefaultPort(newProtocol),
            options: {},
        });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
    };

    // Handle provider selection (for S3/WebDAV)
    const handleProviderSelect = (provider: ProviderConfig) => {
        setSelectedProviderId(provider.id);

        // Apply provider defaults
        const newParams: ConnectionParams = {
            ...connectionParams,
            protocol: provider.protocol as ProviderType,
            server: provider.defaults?.server || '',
            port: provider.defaults?.port || getDefaultPort(provider.protocol as ProviderType),
            providerId: provider.isGeneric ? undefined : provider.id,
            options: {
                ...connectionParams.options,
                pathStyle: provider.defaults?.pathStyle,
                region: provider.defaults?.region,
            },
        };
        onConnectionParamsChange(newParams);
    };

    // Dynamic server placeholder based on protocol and provider
    const getServerPlaceholder = () => {
        if (selectedProvider) {
            const serverField = selectedProvider.fields?.find(f => f.key === 'server');
            if (serverField?.placeholder) return serverField.placeholder;
            if (selectedProvider.defaults?.server) return selectedProvider.defaults.server.replace('https://', '');
        }
        switch (protocol) {
            case 'webdav':
                return 'cloud.example.com';
            case 's3':
                return 's3.amazonaws.com';
            case 'azure':
                return 'myaccount.blob.core.windows.net';
            default:
                return t('connection.serverPlaceholder');
        }
    };

    // Dynamic username label based on protocol
    const getUsernameLabel = () => {
        if (protocol === 's3') return t('connection.accessKeyId');
        if (protocol === 'azure') return t('connection.azureAccountName');
        return t('connection.username');
    };

    // Dynamic password label based on protocol
    const getPasswordLabel = () => {
        if (protocol === 's3') return t('connection.secretAccessKey');
        if (protocol === 'azure') return t('connection.azureAccessKey');
        return t('connection.password');
    };

    return (
        <div className="max-w-5xl mx-auto relative z-10">
            <div className="grid md:grid-cols-2 gap-6">
                {/* Quick Connect */}
                <div className="min-w-0 bg-white dark:bg-gray-800 rounded-xl shadow-xl p-6">
                    <div className="flex items-center justify-between mb-4">
                        <div className="flex items-center gap-3">
                            <h2 className="text-xl font-semibold">{t('connection.quickConnect')}</h2>
                            {hasExistingSessions && (
                                <button
                                    onClick={onSkipToFileManager}
                                    className="flex items-center gap-1.5 px-2.5 py-1 rounded-full bg-green-50 dark:bg-green-900/30 border border-green-200 dark:border-green-800 text-green-700 dark:text-green-400 hover:bg-green-100 dark:hover:bg-green-800/40 transition-colors"
                                    title={t('connection.activeSessions')}
                                >
                                    <span className="w-2 h-2 rounded-full bg-green-500 animate-pulse" />
                                    <span className="text-xs font-medium">{t('connection.activeSessions')}</span>
                                </button>
                            )}
                        </div>
                        {onAeroFile && (
                            <button
                                onClick={onAeroFile}
                                className="flex items-center gap-1.5 px-3 py-1.5 bg-blue-50 dark:bg-blue-900/30 hover:bg-blue-100 dark:hover:bg-blue-800/40 text-blue-600 dark:text-blue-400 rounded-lg transition-colors text-sm font-medium"
                                title={t('statusBar.aerofileTitle')}
                            >
                                <FolderOpen size={16} />
                                <span>AeroFile</span>
                            </button>
                        )}
                    </div>
                    <div className="space-y-3">
                        {/* Protocol Selector - always shown */}
                        <ProtocolSelector
                            value={protocol}
                            onChange={handleProtocolChange}
                            disabled={loading}
                            onOpenChange={handleProtocolSelectorOpenChange}
                        />

                        {/* Show form only when protocol is selected AND selector is closed */}
                        {!protocol || isProtocolSelectorOpen ? (
                            /* No protocol selected or selector is open - show selection prompt + security info */
                            <div className="py-6 space-y-6">
                                <p className="text-sm text-center text-gray-500 dark:text-gray-400">{t('connection.selectProtocolPrompt')}</p>

                                {/* Security Info Box — collapsible */}
                                <div className="mx-auto max-w-sm bg-gradient-to-br from-emerald-50 to-teal-50 dark:from-emerald-900/20 dark:to-teal-900/20 border border-emerald-200 dark:border-emerald-800 rounded-xl overflow-hidden">
                                    <button
                                        type="button"
                                        onClick={() => setSecurityInfoOpen(!securityInfoOpen)}
                                        className="w-full flex items-center gap-2 p-3 hover:bg-emerald-100/50 dark:hover:bg-emerald-800/20 transition-colors"
                                    >
                                        <Shield className="w-4 h-4 text-emerald-600 dark:text-emerald-400" />
                                        <h4 className="font-semibold text-emerald-800 dark:text-emerald-300 text-xs">{t('connection.securityTitle')}</h4>
                                        <ChevronDown size={14} className={`ml-auto text-emerald-600 dark:text-emerald-400 transition-transform duration-200 ${securityInfoOpen ? 'rotate-180' : ''}`} />
                                    </button>
                                    <div className={`grid transition-all duration-200 ${securityInfoOpen ? 'grid-rows-[1fr]' : 'grid-rows-[0fr]'}`}>
                                        <div className="overflow-hidden">
                                            <ul className="space-y-1.5 text-xs text-emerald-700 dark:text-emerald-300 px-3 pb-3">
                                                <li className="flex items-start gap-2">
                                                    <Check size={12} className="mt-0.5 flex-shrink-0 text-emerald-500" />
                                                    <span>{t('connection.securityKeyring')}</span>
                                                </li>
                                                <li className="flex items-start gap-2">
                                                    <Check size={12} className="mt-0.5 flex-shrink-0 text-emerald-500" />
                                                    <span>{t('connection.securityNoSend')}</span>
                                                </li>
                                                <li className="flex items-start gap-2">
                                                    <Check size={12} className="mt-0.5 flex-shrink-0 text-emerald-500" />
                                                    <span>{t('connection.securityTLS')}</span>
                                                </li>
                                            </ul>
                                        </div>
                                    </div>
                                </div>
                            </div>
                        ) : isAeroCloudProvider(protocol) ? (
                            /* AeroCloud - show status or setup */
                            <div className="py-4 space-y-4">
                                {aeroCloudLoading ? (
                                    <div className="text-center py-8">
                                        <div className="animate-spin w-8 h-8 border-2 border-sky-500 border-t-transparent rounded-full mx-auto"></div>
                                        <p className="text-sm text-gray-500 mt-2">{t('connection.loadingAerocloud')}</p>
                                    </div>
                                ) : aeroCloudConfig?.enabled ? (
                                    /* Already configured - show status */
                                    <div className="space-y-4">
                                        <div className="flex items-center gap-3 p-3 bg-gradient-to-r from-sky-50 to-blue-50 dark:from-sky-900/30 dark:to-blue-900/30 border border-sky-200 dark:border-sky-700 rounded-xl">
                                            <div className="w-12 h-12 bg-gradient-to-br from-sky-400 to-blue-500 rounded-xl flex items-center justify-center shadow">
                                                <Cloud className="w-6 h-6 text-white" />
                                            </div>
                                            <div className="flex-1 min-w-0">
                                                <div className="flex items-center gap-2">
                                                    <h3 className="font-semibold">{aeroCloudConfig.cloud_name || 'AeroCloud'}</h3>
                                                    <span className="flex items-center gap-1 text-xs bg-green-100 dark:bg-green-900 text-green-700 dark:text-green-300 px-2 py-0.5 rounded-full">
                                                        <Check size={10} /> {t('connection.active')}
                                                    </span>
                                                </div>
                                                <p className="text-xs text-gray-500 truncate">{aeroCloudConfig.server_profile}</p>
                                            </div>
                                        </div>

                                        {/* Quick info */}
                                        <div className="grid grid-cols-2 gap-3 text-sm">
                                            <div className="p-2 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                                                <div className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400 text-xs mb-1">
                                                    <Folder size={12} /> {t('connection.localFolder')}
                                                </div>
                                                <p className="truncate text-xs font-medium" title={aeroCloudConfig.local_folder}>
                                                    {aeroCloudConfig.local_folder.split(/[\\/]/).pop() || aeroCloudConfig.local_folder}
                                                </p>
                                            </div>
                                            <div className="p-2 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                                                <div className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400 text-xs mb-1">
                                                    <Clock size={12} /> {t('connection.syncInterval')}
                                                </div>
                                                <p className="text-xs font-medium">{Math.round(aeroCloudConfig.sync_interval_secs / 60)} {t('connection.minutes')}</p>
                                            </div>
                                        </div>

                                        {/* Actions */}
                                        <div className="flex gap-2">
                                            <button
                                                onClick={onOpenCloudPanel}
                                                className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 bg-gradient-to-r from-sky-500 to-blue-600 text-white font-medium rounded-xl hover:from-sky-600 hover:to-blue-700 transition-all"
                                            >
                                                <Settings size={16} /> {t('connection.manageAerocloud')}
                                            </button>
                                        </div>

                                        <p className="text-xs text-center text-gray-400">
                                            {t('connection.aerocloudConfigured')}
                                        </p>
                                    </div>
                                ) : (
                                    /* Not configured - show setup prompt */
                                    <div className="text-center space-y-4">
                                        <div className="w-16 h-16 mx-auto bg-gradient-to-br from-sky-400 to-blue-500 rounded-2xl flex items-center justify-center shadow-lg">
                                            <Cloud className="w-8 h-8 text-white" />
                                        </div>
                                        <div>
                                            <h3 className="font-semibold text-lg">{t('connection.aerocloudTitle')}</h3>
                                            <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
                                                {t('connection.aerocloudDesc')}
                                            </p>
                                        </div>
                                        <button
                                            onClick={onOpenCloudPanel}
                                            className="px-6 py-3 bg-gradient-to-r from-sky-500 to-blue-600 text-white font-medium rounded-xl hover:from-sky-600 hover:to-blue-700 transition-all shadow-lg hover:shadow-xl"
                                        >
                                            {t('connection.configureAerocloud')}
                                        </button>
                                        <p className="text-xs text-gray-400">
                                            {t('connection.aerocloudHelp')}
                                        </p>
                                    </div>
                                )}
                            </div>
                        ) : isFourSharedProvider(protocol) ? (
                            <FourSharedConnect
                                initialLocalPath={quickConnectDirs.localDir}
                                onLocalPathChange={(path) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: path })}
                                saveConnection={saveConnection}
                                onSaveConnectionChange={setSaveConnection}
                                connectionName={connectionName}
                                onConnectionNameChange={setConnectionName}
                                onConnected={async (displayName) => {
                                    if (saveConnection) {
                                        const existingServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', SERVERS_STORAGE_KEY) || [];
                                        const saveName = connectionName || displayName;
                                        const duplicate = existingServers.find(s => s.name === saveName && s.protocol === protocol);
                                        if (!duplicate) {
                                            const newServer: ServerProfile = {
                                                id: `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
                                                name: saveName,
                                                host: displayName,
                                                port: 443,
                                                username: '',
                                                password: '',
                                                protocol: protocol as ProviderType,
                                                initialPath: '/',
                                                localInitialPath: quickConnectDirs.localDir,
                                            };
                                            const newServers = [...existingServers, newServer];
                                            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => {});
                                        }
                                    }
                                    onConnect();
                                }}
                            />
                        ) : isOAuthProvider(protocol) ? (
                            <OAuthConnect
                                provider={protocol as 'googledrive' | 'dropbox' | 'onedrive' | 'box' | 'pcloud' | 'zohoworkdrive'}
                                initialLocalPath={quickConnectDirs.localDir}
                                onLocalPathChange={(path) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: path })}
                                saveConnection={saveConnection}
                                onSaveConnectionChange={setSaveConnection}
                                connectionName={connectionName}
                                onConnectionNameChange={setConnectionName}
                                onConnected={async (displayName, extraOptions) => {
                                    // Save OAuth connection if requested
                                    if (saveConnection) {
                                        const existingServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', SERVERS_STORAGE_KEY) || [];
                                        const saveName = connectionName || displayName;
                                        const duplicate = existingServers.find(s => s.name === saveName && s.protocol === protocol);
                                        if (!duplicate) {
                                            const newServer: ServerProfile = {
                                                id: `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
                                                name: saveName,
                                                host: displayName,
                                                port: 443,
                                                username: '',
                                                password: '',
                                                protocol: protocol as ProviderType,
                                                initialPath: '/',
                                                localInitialPath: quickConnectDirs.localDir,
                                                ...(extraOptions?.region && { options: { region: extraOptions.region } }),
                                            };
                                            const newServers = [...existingServers, newServer];
                                            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => {});
                                        } else {
                                            const updated = existingServers.map(s =>
                                                s.id === duplicate.id ? {
                                                    ...s,
                                                    localInitialPath: quickConnectDirs.localDir,
                                                    lastConnected: new Date().toISOString(),
                                                    ...(extraOptions?.region && { options: { ...s.options, region: extraOptions.region } }),
                                                } : s
                                            );
                                            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, updated).catch(() => {});
                                        }
                                    }
                                    onConnect();
                                }}
                            />
                        ) : (protocol === 's3' || protocol === 'webdav') && !selectedProviderId && !editingProfileId ? (
                            /* Show provider selector for S3/WebDAV (skip when editing) */
                            <div className="py-2">
                                <ProviderSelector
                                    selectedProvider={selectedProviderId || undefined}
                                    onSelect={handleProviderSelect}
                                    category={protocol as any}
                                    stableOnly={false}
                                    compact={false}
                                />
                                <p className="text-xs text-gray-500 text-center mt-3">
                                    {t('connection.selectProviderPrompt')}
                                </p>
                            </div>
                        ) : (
                            <>
                                {/* Selected Provider Header (for S3/WebDAV) */}
                                {selectedProvider && (
                                    <div className="flex items-center justify-between p-3 bg-gray-100 dark:bg-gray-700/50 rounded-xl mb-3">
                                        <div className="flex items-center gap-2">
                                            <div className="w-8 h-8 bg-gray-200 dark:bg-gray-600 rounded-lg flex items-center justify-center">
                                                {selectedProvider.id && PROVIDER_LOGOS[selectedProvider.id]
                                                    ? React.createElement(PROVIDER_LOGOS[selectedProvider.id], { size: 20 })
                                                    : <Cloud size={16} style={{ color: selectedProvider.color }} />
                                                }
                                            </div>
                                            <div>
                                                <span className="font-medium text-sm">{selectedProvider.name}</span>
                                                {selectedProvider.isGeneric && (
                                                    <span className="text-xs text-gray-500 ml-2">({t('connection.custom')})</span>
                                                )}
                                            </div>
                                        </div>
                                        <div className="flex items-center gap-3">
                                            {selectedProvider.helpUrl && (
                                                <a
                                                    href={selectedProvider.helpUrl}
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    className="text-xs text-gray-400 hover:text-blue-500 flex items-center gap-1 transition-colors"
                                                    title={t('connection.docs')}
                                                >
                                                    <ExternalLink size={12} />
                                                    {t('connection.docs')}
                                                </a>
                                            )}
                                            <button
                                                onClick={() => setSelectedProviderId(null)}
                                                className="text-xs text-blue-500 hover:text-blue-600 hover:underline"
                                            >
                                                {t('connection.change')}
                                            </button>
                                        </div>
                                    </div>
                                )}

                                {/* Connection Fields Area */}
                                {protocol === 'jottacloud' ? (
                                    /* Jottacloud Specific Form — Login Token only */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.jottacloudToken')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        server: 'jfs.jottacloud.com',
                                                        port: 443,
                                                        username: 'token'
                                                    })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-purple-500 focus:border-purple-500"
                                                    placeholder={t('connection.jottacloudTokenPlaceholder')}
                                                    autoFocus
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">
                                            {t('connection.jottacloudTokenHelp')}
                                        </p>

                                        {/* Optional Remote/Local Path */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        {/* Save Connection Option */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-purple-600 focus:ring-purple-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                    <Save size={14} />
                                                    {t('connection.saveToServers')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-xl font-medium text-white shadow-lg shadow-purple-500/20 active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-gradient-to-r from-purple-500 to-violet-400 hover:from-purple-600 hover:to-violet-500'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <><Cloud size={20} /> {t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                ) : protocol === 'drime' ? (
                                    /* Drime Cloud Specific Form — API Token only */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.drimeToken')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        server: 'app.drime.cloud',
                                                        port: 443,
                                                        username: 'api-token'
                                                    })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-green-500 focus:border-green-500"
                                                    placeholder={t('connection.drimeTokenPlaceholder')}
                                                    autoFocus
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">
                                            {t('connection.drimeTokenHelp')}
                                        </p>

                                        {/* Optional Remote/Local Path */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        {/* Save Connection Option */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-green-600 focus:ring-green-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                    <Save size={14} />
                                                    {t('connection.saveToServers')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-green-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-xl font-medium text-white shadow-lg shadow-green-500/20 active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-gradient-to-r from-green-500 to-emerald-400 hover:from-green-600 hover:to-emerald-500'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <><Cloud size={20} /> {t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                ) : protocol === 'kdrive' ? (
                                    /* kDrive Specific Form — API Token + Drive ID */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.kdriveToken')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        server: 'api.infomaniak.com',
                                                        port: 443,
                                                        username: 'api-token'
                                                    })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                    placeholder={t('connection.kdriveTokenPlaceholder')}
                                                    autoFocus
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.kdriveDriveId')}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.options?.drive_id || connectionParams.options?.bucket || ''}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    options: { ...connectionParams.options, bucket: e.target.value, drive_id: e.target.value }
                                                })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                placeholder={t('connection.kdriveDriveIdPlaceholder')}
                                                inputMode="numeric"
                                            />
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">
                                            {t('connection.kdriveTokenHelp')}
                                        </p>

                                        {/* Optional Remote/Local Path */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        {/* Save Connection Option */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-blue-600 focus:ring-blue-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                    <Save size={14} />
                                                    {t('connection.saveToServers')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.password || !connectionParams.options?.bucket}
                                                className={`w-full py-3.5 rounded-xl font-medium text-white shadow-lg shadow-blue-500/20 active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-gradient-to-r from-blue-500 to-sky-400 hover:from-blue-600 hover:to-sky-500'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <><Cloud size={20} /> {t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                ) : protocol === 'internxt' ? (
                                    /* Internxt Specific Form */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.emailAccount')}</label>
                                            <input
                                                type="email"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    username: e.target.value,
                                                    server: 'gateway.internxt.com',
                                                    port: 443
                                                })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                placeholder={t('connection.internxtEmailPlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.password')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                    placeholder={t('connection.internxtPasswordPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.twoFactorCode')}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.options?.two_factor_code || ''}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    options: { ...connectionParams.options, two_factor_code: e.target.value || undefined }
                                                })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                placeholder={t('connection.twoFactorOptional')}
                                                maxLength={6}
                                                inputMode="numeric"
                                                autoComplete="one-time-code"
                                            />
                                        </div>

                                        <div className="bg-blue-50 dark:bg-blue-900/10 p-3 rounded-lg border border-blue-100 dark:border-blue-900/30 text-xs text-blue-800 dark:text-blue-200">
                                            <p className="font-medium mb-1">{t('connection.internxtEncryptionTitle')}</p>
                                            <p className="opacity-80">
                                                {t('connection.internxtEncryptionDesc')}
                                            </p>
                                        </div>

                                        {/* Optional Remote Path */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        {/* Save Connection Option */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-blue-600 focus:ring-blue-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                    <Save size={14} />
                                                    {t('connection.saveToServers')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-xl font-medium text-white shadow-lg shadow-blue-500/20 active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-blue-600 hover:bg-blue-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <><Cloud size={20} /> {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndAes')}
                                            </p>
                                        </div>
                                    </div>
                                ) : protocol === 'filen' ? (
                                    /* Filen Specific Form */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.emailAccount')}</label>
                                            <input
                                                type="email"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    username: e.target.value,
                                                    server: 'filen.io',
                                                    port: 443
                                                })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                placeholder={t('connection.megaEmailPlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.password')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                    placeholder={t('connection.filenPasswordPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.twoFactorCode')}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.options?.two_factor_code || ''}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    options: { ...connectionParams.options, two_factor_code: e.target.value || undefined }
                                                })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                placeholder={t('connection.twoFactorOptional')}
                                                maxLength={6}
                                                inputMode="numeric"
                                                autoComplete="one-time-code"
                                            />
                                        </div>

                                        <div className="bg-emerald-50 dark:bg-emerald-900/10 p-3 rounded-lg border border-emerald-100 dark:border-emerald-900/30 text-xs text-emerald-800 dark:text-emerald-200">
                                            <p className="font-medium mb-1">{t('connection.filenEncryptionTitle')}</p>
                                            <p className="opacity-80">
                                                {t('connection.filenEncryptionDesc')}
                                            </p>
                                        </div>

                                        {/* Optional Remote Path */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        {/* Save Connection Option */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-emerald-600 focus:ring-emerald-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                    <Save size={14} />
                                                    {t('connection.saveToServers')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-xl font-medium text-white shadow-lg shadow-emerald-500/20 active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-emerald-600 hover:bg-emerald-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <><Cloud size={20} /> {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndAes')}
                                            </p>
                                        </div>
                                    </div>
                                ) : protocol === 'mega' ? (
                                    /* MEGA Specific Form (Beta v0.5.0) */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.emailAccount')}</label>
                                            <input
                                                type="email"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    username: e.target.value,
                                                    server: 'mega.nz', // Force dummy server for internal logic
                                                    port: 443
                                                })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                placeholder={t('connection.megaEmailPlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.password')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    placeholder={t('connection.megaPasswordPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div className="bg-blue-50 dark:bg-blue-900/10 p-3 rounded-lg border border-blue-100 dark:border-blue-900/30 text-xs text-blue-800 dark:text-blue-200">
                                            <p className="font-medium mb-1">{t('connection.megaRequirement')}</p>
                                            <p className="opacity-80">
                                                {t('connection.megaRequirementDesc')}
                                                <a
                                                    href="https://mega.io/cmd"
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    className="block mt-1 underline hover:text-blue-600 dark:hover:text-blue-300"
                                                >
                                                    {t('connection.downloadMegacmd')}
                                                </a>
                                            </p>
                                        </div>

                                        <div className="bg-red-50 dark:bg-red-900/10 p-3 rounded-lg border border-red-100 dark:border-red-900/30">
                                            <label className="flex items-center gap-3 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={connectionParams.options?.save_session !== false} // Default true
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        options: { ...connectionParams.options, save_session: e.target.checked }
                                                    })}
                                                    className="w-5 h-5 rounded text-red-600 focus:ring-red-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <div>
                                                    <span className="text-sm font-medium text-gray-900 dark:text-gray-200">{t('connection.rememberSession')}</span>
                                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                        {t('connection.sessionKeysStored')}
                                                    </p>
                                                </div>
                                            </label>

                                            <label className="flex items-center gap-3 cursor-pointer mt-3 pt-3 border-t border-red-200 dark:border-red-900/30">
                                                <input
                                                    type="checkbox"
                                                    checked={!!connectionParams.options?.logout_on_disconnect} // Default false
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        options: { ...connectionParams.options, logout_on_disconnect: e.target.checked }
                                                    })}
                                                    className="w-5 h-5 rounded text-red-600 focus:ring-red-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <div>
                                                    <span className="text-sm font-medium text-gray-900 dark:text-gray-200">{t('connection.logoutOnDisconnect')}</span>
                                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                        {t('connection.logoutOnDisconnectDesc')}
                                                    </p>
                                                </div>
                                            </label>
                                        </div>

                                        {/* Optional Remote Path */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePathMega')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        {/* Save Connection Option (re-added) */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-red-600 focus:ring-red-500 border-gray-300 dark:border-gray-600"
                                                />
                                                <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                    <Save size={14} />
                                                    {t('connection.saveToServers')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.megaConnectionNamePlaceholder')}
                                                        className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-xl font-medium text-white shadow-lg shadow-red-500/20 active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-red-600 hover:bg-red-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : saveConnection ? (
                                                    <><Save size={18} /> {t('common.save')}</>
                                                ) : (
                                                    <><Cloud size={20} /> {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndEncrypted')}
                                            </p>
                                        </div>
                                    </div>
                                ) : (
                                    /* Traditional connection fields (FTP/S3/WebDAV) */
                                    <>
                                        {(() => {
                                            const providerHasNoEndpoint = protocol === 's3' && selectedProviderId && !getProviderById(selectedProviderId)?.fields?.find(f => f.key === 'endpoint');
                                            return providerHasNoEndpoint ? null : (
                                                <div className="flex gap-2">
                                                    <div className="flex-1 min-w-0">
                                                        <label className="block text-sm font-medium mb-1.5">
                                                            {protocol === 's3' ? t('protocol.s3Endpoint') : protocol === 'azure' ? t('connection.azureEndpoint') : t('connection.server')}
                                                        </label>
                                                        <input
                                                            type="text"
                                                            value={connectionParams.server}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, server: e.target.value })}
                                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                                            placeholder={getServerPlaceholder()}
                                                        />
                                                    </div>
                                                    <div className="w-24">
                                                        <label className="block text-sm font-medium mb-1.5">{t('connection.port')}</label>
                                                        <input
                                                            type="number"
                                                            value={connectionParams.port || getDefaultPort(protocol)}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, port: parseInt(e.target.value) || getDefaultPort(protocol) })}
                                                            className="w-full px-3 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl text-center"
                                                            min={1}
                                                            max={65535}
                                                        />
                                                    </div>
                                                </div>
                                            );
                                        })()}
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{getUsernameLabel()}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({ ...connectionParams, username: e.target.value })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                                placeholder={protocol === 's3' ? 'AKIAIOSFODNN7EXAMPLE' : protocol === 'azure' ? 'aeroftp2026' : t('connection.usernamePlaceholder')}
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{getPasswordLabel()}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-3 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                                    placeholder={t('connection.passwordPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        {/* Protocol-specific fields */}
                                        <ProtocolFields
                                            protocol={protocol}
                                            options={connectionParams.options || {}}
                                            onChange={(options) => onConnectionParamsChange({ ...connectionParams, options })}
                                            disabled={loading}
                                            onBrowseKeyFile={protocol === 'sftp' ? handleBrowseSshKey : undefined}
                                            selectedProviderId={selectedProviderId}
                                            isEditing={!!editingProfileId}
                                        />

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('browser.remote')} {t('browser.path')}</label>
                                            <input
                                                type="text"
                                                value={quickConnectDirs.remoteDir}
                                                onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                                placeholder={protocol === 's3' ? '/prefix/' : protocol === 'azure' ? '/virtual-folder/' : '/www'}
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('browser.local')} {t('browser.path')}</label>
                                            <div className="flex gap-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.localDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                    className="flex-1 px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                                    placeholder={t('connection.localPathPlaceholder')}
                                                />
                                                <button
                                                    type="button"
                                                    onClick={handleBrowseLocalDir}
                                                    className="px-4 py-3 bg-gray-100 dark:bg-gray-600 hover:bg-gray-200 dark:hover:bg-gray-500 rounded-xl transition-colors"
                                                    title={t('common.browse')}
                                                >
                                                    <FolderOpen size={18} />
                                                </button>
                                            </div>
                                        </div>

                                        {/* Save Connection Option */}
                                        <div className="pt-2 border-t border-gray-200 dark:border-gray-600">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="checkbox"
                                                    checked={saveConnection}
                                                    onChange={(e) => setSaveConnection(e.target.checked)}
                                                    className="w-4 h-4 rounded text-blue-500"
                                                />
                                                <span className="text-sm flex items-center gap-1">
                                                    <Save size={14} />
                                                    {t('connection.saveThisConnection')}
                                                </span>
                                            </label>

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNameOptional')}
                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="flex gap-2">
                                            {editingProfileId && (
                                                <button
                                                    onClick={handleCancelEdit}
                                                    className="px-4 py-3 bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 font-medium rounded-xl hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors"
                                                    title={t('connection.cancelEditing')}
                                                >
                                                    <X size={20} />
                                                </button>
                                            )}
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || ((protocol === 's3' || protocol === 'azure') && !connectionParams.options?.bucket)}
                                                className={`flex-1 text-white font-medium py-3 rounded-xl disabled:opacity-50 flex items-center justify-center gap-2 ${editingProfileId
                                                    ? 'bg-blue-600 hover:bg-blue-700'
                                                    : 'bg-gradient-to-r from-blue-500 to-cyan-500'
                                                    }`}
                                            >
                                                {loading ? (
                                                    t('connection.connecting')
                                                ) : editingProfileId ? (
                                                    <>
                                                        <Save size={18} />
                                                        {t('connection.saveChanges')}
                                                    </>
                                                ) : (
                                                    saveConnection ? <><Save size={18} /> {t('common.save')}</> : t('common.connect')
                                                )}
                                            </button>
                                        </div>
                                    </>
                                )}
                            </>
                        )}
                    </div>
                </div>

                {/* Saved Servers */}
                <div className="min-w-0 bg-white dark:bg-gray-800 rounded-xl shadow-xl p-6">
                    <SavedServers
                        onConnect={onSavedServerConnect}
                        onEdit={handleEdit}
                        lastUpdate={savedServersUpdate + serversRefreshKey}
                        onOpenExportImport={() => setShowExportImport(true)}
                    />
                </div>

                {/* Skip to File Manager — accessible via status bar AeroFile button */}
            </div> {/* Close grid */}

            {/* Export/Import Dialog */}
            {showExportImport && (
                <ExportImportDialog
                    servers={servers}
                    onImport={async (newServers) => {
                        const updated = [...servers, ...newServers];
                        setServers(updated);
                        localStorage.setItem(SERVERS_STORAGE_KEY, JSON.stringify(updated));
                        await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, updated).catch(() => {});
                        setShowExportImport(false);
                        setSavedServersUpdate(Date.now());
                    }}
                    onClose={() => setShowExportImport(false)}
                />
            )}
        </div>
    );
};

export default ConnectionScreen;
