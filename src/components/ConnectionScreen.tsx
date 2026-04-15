// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

/**
 * ConnectionScreen Component
 * Initial connection form with Quick Connect and Saved Servers
 */

import React, { useState, useEffect, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { readFile } from '@tauri-apps/plugin-fs';
import { FolderOpen, HardDrive, ChevronRight, ChevronDown, Save, Copy, Cloud, Check, Settings, Clock, Folder, X, Lock, ArrowLeft, Eye, EyeOff, ExternalLink, Shield, ShieldCheck, KeyRound, Loader2, Image, Info, Pencil } from 'lucide-react';
import { ConnectionParams, ProviderType, isOAuthProvider, isAeroCloudProvider, isFourSharedProvider, ServerProfile } from '../types';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { SavedServers } from './SavedServers';
import { ExportImportDialog } from './ExportImportDialog';
import { useTranslation } from '../i18n';
import { ProtocolSelector, ProtocolFields, getDefaultPort } from './ProtocolSelector';
import { OAuthConnect } from './OAuthConnect';
import { ProviderSelector } from './ProviderSelector';
import { AlertDialog } from './Dialogs';
import { getProviderById, resolveS3Endpoint, ProviderConfig } from '../providers';
import { getMegaConnectionMode, normalizeMegaOptions } from '../utils/providerConnectionMeta';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';
import { Checkbox } from './ui/Checkbox';

// Storage key for saved servers (same as SavedServers component)
const SERVERS_STORAGE_KEY = 'aeroftp-saved-servers';

// Protocols that can be switched between when editing a saved connection
const SWITCHABLE_PROTOCOLS: ProviderType[] = ['ftp', 'ftps', 'sftp'];

const PROTOCOL_COLORS: Record<string, string> = {
    ftp: 'from-blue-500 to-cyan-400',
    ftps: 'from-green-500 to-emerald-400',
    sftp: 'from-purple-500 to-violet-400',
    webdav: 'from-orange-500 to-amber-400',
    s3: 'from-amber-500 to-yellow-400',
    aerocloud: 'from-sky-400 to-blue-500',
    googledrive: 'from-red-500 to-red-400',
    googlephotos: 'from-amber-500 to-amber-400',
    dropbox: 'from-blue-600 to-blue-400',
    onedrive: 'from-sky-500 to-sky-400',
    mega: 'from-red-600 to-red-500',
    box: 'from-blue-500 to-blue-600',
    pcloud: 'from-sky-500 to-cyan-400',
    azure: 'from-blue-600 to-indigo-500',
    filen: 'from-emerald-500 to-green-400',
    opendrive: 'from-cyan-500 to-sky-400',
    immich: 'from-indigo-500 to-violet-400',
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
    onAeroCloud?: () => void;
    isAeroCloudConfigured?: boolean;
    isAeroCloudConnected?: boolean;
    onOpenCloudPanel?: () => void;
    hasExistingSessions?: boolean;  // Show active sessions badge next to QuickConnect
    serversRefreshKey?: number;  // Change this to force refresh of saved servers list
    formOnly?: boolean;  // IntroHub: hide SavedServers panel, center form at max-w-640px
    editingProfile?: ServerProfile;  // IntroHub: auto-enter edit mode on mount for this profile
    onFormSaved?: () => void;  // IntroHub: callback after save/edit completes (to close form tab)
    onTabLabelChange?: (label: string) => void;  // IntroHub: update tab label when connection name changes
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
        invoke('store_credential', { account: 'oauth_fourshared_client_id', password: consumerKey }).catch(() => { });
        invoke('store_credential', { account: 'oauth_fourshared_client_secret', password: consumerSecret }).catch(() => { });
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
                <div className="p-4 rounded-lg border-2 border-blue-500/30 bg-blue-500/5">
                    <div className="flex items-center gap-3">
                        <div className="w-12 h-12 rounded-lg flex items-center justify-center bg-blue-500/20">
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
                {/* Local Folder */}
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('connection.fourshared.localFolderOptional')}</label>
                    <div className="flex gap-2">
                        <input
                            type="text"
                            value={localPath}
                            onChange={(e) => { setLocalPath(e.target.value); onLocalPathChange?.(e.target.value); }}
                            placeholder="~/Downloads"
                            className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                        />
                        <button type="button" onClick={browseLocalFolder} className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg" title={t('common.browse')}>
                            <FolderOpen size={18} />
                        </button>
                    </div>
                </div>
                <button
                    onClick={handleConnect}
                    disabled={isConnecting || isAuthenticating}
                    className="w-full py-3 px-4 rounded-lg text-white font-medium flex items-center justify-center gap-2 transition-colors bg-blue-500 hover:bg-blue-600 disabled:opacity-50 disabled:cursor-not-allowed"
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
                        className="flex-1 py-2 px-3 text-sm text-gray-600 dark:text-gray-400 hover:text-gray-800 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-lg flex items-center justify-center gap-2 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                    >
                        {t('connection.fourshared.useDifferentAccount')}
                    </button>
                    <button
                        onClick={handleLogout}
                        className="py-2 px-3 text-sm text-red-500 hover:text-red-600 border border-red-300 dark:border-red-600/50 rounded-lg flex items-center justify-center gap-2 hover:bg-red-50 dark:hover:bg-red-900/20 transition-colors"
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
                        className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                    />
                    <button type="button" onClick={browseLocalFolder} className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg" title={t('common.browse')}>
                        <FolderOpen size={18} />
                    </button>
                </div>
            </div>

            {/* Save Connection */}
            <div className="flex items-center gap-3 p-3 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                <Checkbox
                    checked={wantToSave}
                    onChange={(v) => { setWantToSave(v); onSaveConnectionChange?.(v); }}
                />
                <label className="flex-1">
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
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                            onClick={() => { try { invoke('open_url', { url: 'https://www.4shared.com/developer/docs/app/' }); } catch { window.open('https://www.4shared.com/developer/docs/app/', '_blank'); } }}
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
    onAeroCloud,
    isAeroCloudConfigured,
    isAeroCloudConnected,
    onOpenCloudPanel,
    hasExistingSessions = false,
    serversRefreshKey = 0,
    formOnly = false,
    editingProfile,
    onFormSaved,
    onTabLabelChange,
}) => {
    const t = useTranslation();
    const protocol = connectionParams.protocol; // Can be undefined

    // Save connection state
    const [saveConnection, setSaveConnection] = useState(false);
    const [connectionName, setConnectionName] = useState('');
    // Stable ref for onTabLabelChange to avoid re-render loops from unstable arrow functions
    const onTabLabelChangeRef = useRef(onTabLabelChange);
    onTabLabelChangeRef.current = onTabLabelChange;
    // Notify IntroHub when the user types a connection name (or clears it)
    useEffect(() => {
        onTabLabelChangeRef.current?.(connectionName.trim());
    }, [connectionName]);
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
    const [showAdvanced, setShowAdvanced] = useState(false);
    const [advancedUnlocked, setAdvancedUnlocked] = useState(false);
    const [showAdvancedWarning, setShowAdvancedWarning] = useState(false);
    const [selectedProviderId, setSelectedProviderId] = useState<string | null>(
        formOnly && connectionParams.providerId ? connectionParams.providerId : null
    );
    const selectedProvider = selectedProviderId ? getProviderById(selectedProviderId) : null;
    const megaMode = getMegaConnectionMode(connectionParams.options);
    const isMegaCmdMode = megaMode === 'megacmd';

    // Protocol selector open state (to hide form when selector is open)
    const [isProtocolSelectorOpen, setIsProtocolSelectorOpen] = useState(false);

    // Track which preset fields have been unlocked for editing
    const [presetUnlocked, setPresetUnlocked] = useState<Record<string, boolean>>({});

    // Track previous protocol for switch detection in handleProtocolChange
    const previousProtocolRef = React.useRef<ProviderType | undefined>(undefined);

    // When re-opening dropdown with a protocol already selected, clear the selection.
    // In formOnly (IntroHub edit), keep everything — just open the dropdown overlay.
    const handleProtocolSelectorOpenChange = (open: boolean) => {
        setIsProtocolSelectorOpen(open);
        if (open && protocol) {
            previousProtocolRef.current = protocol;
            if (!formOnly) {
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
    const [gitHubAlert, setGitHubAlert] = useState<{ title: string; message: string; type: 'warning' | 'error' | 'info' } | null>(null);
    const [gitHubDeviceFlow, setGitHubDeviceFlow] = useState<{ userCode: string; verificationUri: string; deviceCode: string; interval: number } | null>(null);
    const [gitHubDeviceFlowLoading, setGitHubDeviceFlowLoading] = useState(false);
    const [gitHubPemLoading, setGitHubPemLoading] = useState(false);
    const [gitHubPemInVault, setGitHubPemInVault] = useState(false);
    const [gitHubAppFieldsLocked, setGitHubAppFieldsLocked] = useState(false);

    // Auto-populate App ID + Installation ID from vault when switching to App (.pem) mode
    useEffect(() => {
        const currentMode = connectionParams.options?.githubAuthMode;
        if (currentMode !== 'app') {
            setGitHubPemInVault(false);
            return;
        }
        // Race guard: capture mode at effect start, check before applying async results
        let cancelled = false;
        const appId = connectionParams.options?.githubAppId?.trim();
        const installId = connectionParams.options?.githubInstallationId?.trim();

        if (appId && installId) {
            invoke('github_has_vault_pem', { appId, installationId: installId })
                .then((has) => {
                    if (cancelled) return;
                    setGitHubPemInVault(has as boolean);
                    setGitHubAppFieldsLocked(has as boolean);
                })
                .catch(() => { if (!cancelled) { setGitHubPemInVault(false); setGitHubAppFieldsLocked(false); } });
        } else {
            invoke('github_get_app_credentials')
                .then((result) => {
                    if (cancelled) return;
                    const creds = result as { app_id?: string; installation_id?: string } | null;
                    if (creds?.app_id && creds?.installation_id) {
                        onConnectionParamsChange({
                            ...connectionParams,
                            options: {
                                ...connectionParams.options,
                                githubAppId: creds.app_id,
                                githubInstallationId: creds.installation_id,
                            },
                        });
                        invoke('github_has_vault_pem', { appId: creds.app_id, installationId: creds.installation_id })
                            .then((has) => {
                                if (cancelled) return;
                                setGitHubPemInVault(has as boolean);
                                setGitHubAppFieldsLocked(has as boolean);
                            })
                            .catch(() => { if (!cancelled) { setGitHubPemInVault(false); setGitHubAppFieldsLocked(false); } });
                    } else {
                        setGitHubPemInVault(false);
                        setGitHubAppFieldsLocked(false);
                    }
                })
                .catch(() => { if (!cancelled) { setGitHubPemInVault(false); setGitHubAppFieldsLocked(false); } });
        }
        return () => { cancelled = true; };
    }, [connectionParams.options?.githubAuthMode]);

    // SEC-GH-001: Check if PAT/OAuth token exists in vault (token stays backend-side)
    const [hasVaultToken, setHasVaultToken] = useState(false);
    useEffect(() => {
        const mode = connectionParams.options?.githubAuthMode;
        if (mode !== 'pat' && mode !== 'authorize' && mode !== undefined) return;
        invoke('github_get_pat')
            .then(() => setHasVaultToken(true))
            .catch(() => setHasVaultToken(false));
    }, [connectionParams.options?.githubAuthMode]);

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

    // IntroHub formOnly: auto-enter edit mode when editingProfile prop is provided
    useEffect(() => {
        if (formOnly && editingProfile && editingProfile.id !== editingProfileId) {
            handleEdit(editingProfile);
        }
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [formOnly, editingProfile?.id]);

    // IntroHub formOnly: auto-select provider when providerId comes from Discover tab
    useEffect(() => {
        if (formOnly && connectionParams.providerId && !editingProfile && !selectedProviderId) {
            const provider = getProviderById(connectionParams.providerId);
            if (provider) {
                handleProviderSelect(provider);
            }
        }
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [formOnly, connectionParams.providerId]);

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

        const normalizedParams = protocol === 'filelu'
            ? {
                ...connectionParams,
                server: connectionParams.server || 'filelu.com',
                username: connectionParams.username || 'api-key',
                port: connectionParams.port || 443,
            }
            : protocol === 'opendrive'
                ? {
                    ...connectionParams,
                    server: connectionParams.server || 'dev.opendrive.com',
                    port: connectionParams.port || 443,
                }
            : protocol === 'github'
                ? {
                    ...connectionParams,
                    server: connectionParams.server || '',
                    port: connectionParams.port || 443,
                }
            : protocol === 'immich'
                ? {
                    ...connectionParams,
                    server: connectionParams.server || '',
                    username: connectionParams.username || 'api-key',
                    port: connectionParams.port || 443,
                }
            : selectedProvider?.defaults?.server && !connectionParams.server
                ? {
                    ...connectionParams,
                    server: selectedProvider.defaults.server,
                    port: connectionParams.port || selectedProvider.defaults.port || getDefaultPort(protocol),
                }
            : connectionParams;

        const optionsToSave = protocol === 'mega'
            ? normalizeMegaOptions(connectionParams.options)
            : { ...connectionParams.options };
        // Persist default tlsMode for FTP/FTPS so saved servers show correct badge
        if ((protocol === 'ftp' || protocol === 'ftps') && !optionsToSave.tlsMode) {
            optionsToSave.tlsMode = protocol === 'ftps' ? 'implicit' : 'explicit';
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
                        host: normalizedParams.server,
                        port: normalizedParams.port || getDefaultPort(protocol),
                        username: normalizedParams.username,
                        hasStoredCredential: credentialStored || (s.hasStoredCredential && !connectionParams.password),
                        protocol: protocol as ProviderType,
                        options: optionsToSave,
                        initialPath: quickConnectDirs.remoteDir,
                        localInitialPath: quickConnectDirs.localDir,
                        providerId: selectedProviderId || s.providerId || (protocol === 'swift' ? 'blomp' : protocol === 'mega' ? 'mega' : undefined),
                        customIconUrl: customIconForSave !== undefined ? customIconForSave : s.customIconUrl,
                    };
                }
                return s;
            });

            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, updatedServers).catch(() => { });
            setSavedServersUpdate(Date.now());
        } else if (saveConnection) {
            const newId = `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
            const credentialStored = await tryStoreCredential(`server_${newId}`, connectionParams.password);

            const newServer: ServerProfile = {
                id: newId,
                name: connectionName || normalizedParams.server || protocol,
                host: normalizedParams.server,
                port: normalizedParams.port || getDefaultPort(protocol),
                username: normalizedParams.username,
                hasStoredCredential: credentialStored,
                protocol: protocol as ProviderType,
                initialPath: quickConnectDirs.remoteDir,
                localInitialPath: quickConnectDirs.localDir,
                options: optionsToSave,
                providerId: selectedProviderId || (protocol === 'swift' ? 'blomp' : protocol === 'mega' ? 'mega' : undefined),
                customIconUrl: customIconForSave,
            };

            const newServers = [...existingServers, newServer];
            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => { });
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
                <div className="flex items-start gap-3">
                    <div className="flex items-center gap-3 flex-1">
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
                    <div className="flex items-start gap-1 text-gray-400 dark:text-gray-500 text-xs max-w-[180px] pt-1">
                        <Info size={12} className="shrink-0 mt-0.5" />
                        <span>{t('settings.iconAutoDetectHint')}</span>
                    </div>
                </div>
            </div>
        );
    };

    // Handle the main action button
    const handleConnectAndSave = async () => {
        // Store PAT in vault for future connections (if GitHub PAT mode)
        if (connectionParams.options?.githubAuthMode === 'pat' && connectionParams.password) {
            invoke('github_store_pat', { pat: connectionParams.password }).catch(() => {});
        }

        if (editingProfileId) {
            // Edit mode: save changes and reset form
            await saveToServers();
            setEditingProfileId(null);
            editingProfileIdRef.current = null;
            setConnectionName('');
            setSaveConnection(false);
            onConnectionParamsChange({ server: '', username: '', password: '' });
            onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
            onFormSaved?.();
        } else if (saveConnection) {
            // Save mode: only save, user connects from saved servers list
            await saveToServers();
            setConnectionName('');
            setSaveConnection(false);
            onConnectionParamsChange({ server: '', username: '', password: '' });
            onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
            onFormSaved?.();
        } else {
            // Connect mode: just connect without saving
            onConnect();
        }
    };

    const handleSaveAsNew = async () => {
        if (!protocol || !editingProfileId) return;
        // Validate name is different
        const existingServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', SERVERS_STORAGE_KEY) || [];
        const originalServer = existingServers.find((s: ServerProfile) => s.id === editingProfileId);
        const newName = connectionName || connectionParams.server || protocol;
        if (originalServer && newName === originalServer.name) {
            // Auto-append "(Copy)" if user didn't change the name
            setConnectionName(`${newName} (${t('common.copy')})`);
        }
        const finalName = (originalServer && newName === originalServer.name)
            ? `${newName} (${t('common.copy')})`
            : newName;

        const normalizedParams = protocol === 'filelu'
            ? { ...connectionParams, server: connectionParams.server || 'filelu.com', username: connectionParams.username || 'api-key', port: connectionParams.port || 443 }
            : protocol === 'opendrive'
                ? { ...connectionParams, server: connectionParams.server || 'dev.opendrive.com', port: connectionParams.port || 443 }
            : protocol === 'github' || protocol === 'gitlab'
                ? { ...connectionParams, server: connectionParams.server || '', port: connectionParams.port || 443 }
            : selectedProvider?.defaults?.server && !connectionParams.server
                ? { ...connectionParams, server: selectedProvider.defaults.server, port: connectionParams.port || selectedProvider.defaults.port || getDefaultPort(protocol) }
            : connectionParams;

        const optionsToSave = protocol === 'mega'
            ? normalizeMegaOptions(connectionParams.options)
            : { ...connectionParams.options };

        const newId = `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
        const credentialStored = await tryStoreCredential(`server_${newId}`, connectionParams.password);
        const newServer: ServerProfile = {
            id: newId,
            name: finalName,
            host: normalizedParams.server,
            port: normalizedParams.port || getDefaultPort(protocol),
            username: normalizedParams.username,
            hasStoredCredential: credentialStored,
            protocol: protocol as ProviderType,
            initialPath: quickConnectDirs.remoteDir,
            localInitialPath: quickConnectDirs.localDir,
            options: optionsToSave,
            providerId: selectedProviderId || undefined,
            customIconUrl: customIconForSave,
        };

        const newServers = [...existingServers, newServer];
        await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => { });
        setSavedServersUpdate(Date.now());

        // Reset form
        setEditingProfileId(null);
        editingProfileIdRef.current = null;
        setConnectionName('');
        setSaveConnection(false);
        onConnectionParamsChange({ server: '', username: '', password: '' });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
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

        // Resolve endpoint and accountId from registry for S3 profiles
        let profileOptions = profile.options || {};
        if (profile.protocol === 's3' && profile.providerId) {
            const provider = getProviderById(profile.providerId);
            if (provider) {
                // Extract accountId from old-format endpoint (e.g. Cloudflare R2 migration)
                const template = provider.defaults?.endpointTemplate;
                if (template?.includes('{accountId}') && profileOptions.endpoint && !profileOptions.accountId) {
                    // Reverse-extract accountId from stored endpoint using template pattern
                    const templateRegex = template.replace('{accountId}', '(.+)').replace(/\./g, '\\.');
                    const match = profileOptions.endpoint.match(new RegExp(templateRegex));
                    if (match?.[1]) {
                        profileOptions = { ...profileOptions, accountId: match[1] };
                    }
                }

                // Resolve endpoint if missing
                if (!profileOptions.endpoint) {
                    let effectiveRegion = profileOptions.region || provider.defaults?.region;
                    if (!effectiveRegion && provider.defaults?.endpointTemplate && !template?.includes('{accountId}')) {
                        const regionField = provider.fields?.find(f => f.key === 'region');
                        if (regionField?.type === 'select' && regionField.options?.length) {
                            effectiveRegion = regionField.options[0].value;
                        }
                    }
                    const extraParams = profileOptions.accountId ? { accountId: profileOptions.accountId } : undefined;
                    const resolvedEndpoint = provider.defaults?.endpoint
                        || resolveS3Endpoint(provider.id, effectiveRegion, extraParams)
                        || undefined;
                    if (resolvedEndpoint) {
                        profileOptions = { ...profileOptions, endpoint: resolvedEndpoint };
                        if (effectiveRegion && !profileOptions.region) {
                            profileOptions = { ...profileOptions, region: effectiveRegion };
                        }
                    }
                }
            }
        }

        // Immediately update form with new profile data (password empty initially)
        onConnectionParamsChange({
            server: profile.host,
            port: profile.port,
            username: profile.username,
            password: profile.password || '', // Set immediately, will be updated if stored
            protocol: profile.protocol || 'ftp',
            options: profileOptions
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
                        options: profileOptions
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

    const handleProtocolChange = (newProtocol: ProviderType, providerId?: string) => {
        // When editing a saved connection and switching between compatible protocols (FTP/FTPS/SFTP),
        // keep edit mode and only update protocol + port.
        // Use previousProtocolRef as fallback when protocol was cleared on dropdown open.
        const effectiveOldProtocol = protocol || previousProtocolRef.current;
        previousProtocolRef.current = undefined;
        if (editingProfileId
            && SWITCHABLE_PROTOCOLS.includes(newProtocol)
            && SWITCHABLE_PROTOCOLS.includes(effectiveOldProtocol as ProviderType)
        ) {
            onConnectionParamsChange({
                ...connectionParams,
                protocol: newProtocol,
                port: getDefaultPort(newProtocol),
                options: {},
            });
            return;
        }

        // Exit edit mode when changing to an incompatible protocol
        if (editingProfileId) {
            setEditingProfileId(null);
            editingProfileIdRef.current = null;
            setConnectionName('');
            setSaveConnection(false);
        }

        // Reset provider selection when protocol changes
        setSelectedProviderId(null);
        setPresetUnlocked({});
        setAdvancedUnlocked(false);

        // If a providerId was passed (e.g. SourceForge), auto-apply the preset
        if (providerId) {
            const provider = getProviderById(providerId);
            if (provider) {
                setSelectedProviderId(providerId);
                onConnectionParamsChange({
                    server: provider.defaults?.server || '',
                    username: '',
                    password: '',
                    protocol: newProtocol,
                    port: provider.defaults?.port || getDefaultPort(newProtocol),
                    providerId: provider.id,
                    options: newProtocol === 'mega' ? normalizeMegaOptions() : {},
                });
                onQuickConnectDirsChange({
                    remoteDir: provider.defaults?.basePath || '',
                    localDir: '',
                });
                return;
            }
        }

        const protocolDefaults: Partial<ConnectionParams> = newProtocol === 'filelu'
            ? { server: 'filelu.com', username: 'api-key', port: 443 }
            : newProtocol === 'opendrive'
                ? { server: 'dev.opendrive.com', port: 443 }
            : newProtocol === 'mega'
                ? { server: 'mega.nz', port: 443, options: normalizeMegaOptions() }
                : {};

        // Reset ALL form fields (clear previous server's credentials)
        onConnectionParamsChange({
            server: protocolDefaults.server || '',
            username: protocolDefaults.username || '',
            password: '',
            protocol: newProtocol,
            port: protocolDefaults.port || getDefaultPort(newProtocol),
            options: protocolDefaults.options || {},
        });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
    };

    // Handle provider selection (for S3/WebDAV)
    const handleProviderSelect = (provider: ProviderConfig) => {
        setSelectedProviderId(provider.id);
        setPresetUnlocked({});
        setAdvancedUnlocked(false);

        // For endpointTemplate providers without a default region, auto-select the first region option
        let effectiveRegion = provider.defaults?.region;
        if (!effectiveRegion && provider.defaults?.endpointTemplate) {
            const regionField = provider.fields?.find(f => f.key === 'region');
            if (regionField?.type === 'select' && regionField.options?.length) {
                effectiveRegion = regionField.options[0].value;
            }
        }

        // Resolve S3 endpoint: static defaults.endpoint OR computed from endpointTemplate + region
        const resolvedEndpoint = provider.defaults?.endpoint
            || resolveS3Endpoint(provider.id, effectiveRegion)
            || undefined;

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
                region: effectiveRegion,
                endpoint: resolvedEndpoint,
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
            case 'github':
                return t('protocol.githubOwnerRepoPlaceholder');
            case 'gitlab':
                return 'gitlab.com/owner/repo';
            default:
                return t('connection.serverPlaceholder');
        }
    };

    // Dynamic username label based on protocol
    const getUsernameLabel = () => {
        if (protocol === 's3') return t('connection.accessKeyId');
        if (protocol === 'azure') return t('connection.azureAccountName');
        if (protocol === 'github') return t('github.ownerRepo');
        if (protocol === 'gitlab') return 'Project Path';
        return t('connection.username');
    };

    // Dynamic password label based on protocol
    const getPasswordLabel = () => {
        if (protocol === 's3') return t('connection.secretAccessKey');
        if (protocol === 'azure') return t('connection.azureAccessKey');
        if (protocol === 'github') return t('github.personalAccessToken');
        if (protocol === 'gitlab') return 'Access Token';
        return t('connection.password');
    };

    // Provider logo for connect buttons (OAuth/API providers show their logo instead of Cloud icon)
    const ConnectIcon = (() => {
        const logoId = selectedProviderId || protocol || '';
        const Logo = PROVIDER_LOGOS[logoId];
        if (Logo) return <Logo size={18} />;
        return <Cloud size={18} />;
    })();

    /**
     * Renders the right column (paths + save + button) for formOnly 2-column layout.
     * Also used inline for single-column providers.
     * This replaces 9+ duplicated blocks across protocol branches.
     */
    const renderRightColumn = (opts: {
        disabled: boolean;
        buttonColorClass: string;
        buttonText?: React.ReactNode;
        remotePathPlaceholder?: string;
        connectionNameKey?: string;
        showE2ENote?: string;
        showIconPicker?: boolean;
        showCancelSaveAsNew?: boolean;
    }) => {
        const {
            disabled: btnDisabled,
            buttonColorClass,
            buttonText,
            remotePathPlaceholder = t('connection.initialRemotePath'),
            connectionNameKey = t('connection.connectionNamePlaceholder'),
            showE2ENote,
            showIconPicker: showIcon = true,
            showCancelSaveAsNew = false,
        } = opts;
        const isSourceForge = selectedProviderId === 'sourceforge';
        const sfPrefix = '/home/frs/project/';
        return (
            <div className="space-y-3">
                {/* Remote Path (SourceForge: prefix + project name) */}
                <div>
                    <label className="block text-sm font-medium mb-1.5">
                        {isSourceForge ? 'Project (Unixname)' : `${t('browser.remote')} ${t('browser.path')}`}
                    </label>
                    {isSourceForge ? (
                        <div className="flex items-center gap-0">
                            <span className="px-3 py-2.5 bg-gray-100 dark:bg-gray-600 border border-r-0 border-gray-300 dark:border-gray-600 rounded-l-lg text-sm text-gray-500 dark:text-gray-400 whitespace-nowrap select-none">
                                {sfPrefix}
                            </span>
                            <input
                                type="text"
                                value={quickConnectDirs.remoteDir.replace(sfPrefix, '')}
                                onChange={(e) => {
                                    const project = e.target.value.replace(/^\/+/, '');
                                    onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: sfPrefix + project });
                                }}
                                className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-r-lg text-sm"
                                placeholder="aeroftp"
                            />
                        </div>
                    ) : (
                        <input
                            type="text"
                            value={quickConnectDirs.remoteDir}
                            onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                            placeholder={remotePathPlaceholder}
                        />
                    )}
                </div>
                {/* Local Path */}
                <div>
                    <label className="block text-sm font-medium mb-1.5">{t('browser.local')} {t('browser.path')}</label>
                    <div className="flex gap-2">
                        <input
                            type="text"
                            value={quickConnectDirs.localDir}
                            onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                            className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                            placeholder={t('connection.initialLocalPath')}
                        />
                        <button
                            type="button"
                            onClick={handleBrowseLocalDir}
                            className="px-3 py-2.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                            title={t('common.browse')}
                        >
                            <FolderOpen size={16} />
                        </button>
                    </div>
                </div>
                {/* Save Connection */}
                <div className="pt-2 border-t border-gray-200 dark:border-gray-700/50">
                    <Checkbox
                        checked={saveConnection}
                        onChange={setSaveConnection}
                        label={
                            <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                <Save size={14} />
                                {t('connection.saveThisConnection')}
                            </span>
                        }
                    />
                    {saveConnection && (
                        <div className="mt-2 animate-fade-in-down">
                            <input
                                type="text"
                                value={connectionName}
                                onChange={(e) => setConnectionName(e.target.value)}
                                placeholder={connectionNameKey}
                                className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                            />
                            {showIcon && renderIconPicker()}
                        </div>
                    )}
                </div>
                {/* Action Buttons */}
                <div className={showCancelSaveAsNew ? 'flex gap-2' : 'pt-2'}>
                    {showCancelSaveAsNew && editingProfileId && (
                        <button onClick={handleCancelEdit} className="px-4 py-3 bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 font-medium rounded-lg hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors" title={t('connection.cancelEditing')}>
                            <X size={20} />
                        </button>
                    )}
                    {showCancelSaveAsNew && editingProfileId && (
                        <button onClick={handleSaveAsNew} className="px-4 py-3 bg-green-600 hover:bg-green-700 text-white font-medium rounded-lg transition-colors flex items-center gap-2" title={t('connection.saveAsNew')}>
                            <Copy size={18} />
                        </button>
                    )}
                    <button
                        onClick={handleConnectAndSave}
                        disabled={loading || btnDisabled}
                        className={`${showCancelSaveAsNew ? 'flex-1' : 'w-full'} py-3 rounded-lg font-medium text-white cursor-pointer active:scale-[0.98] transition-all flex items-center justify-center gap-2 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] disabled:opacity-50 ${loading ? 'bg-gray-400 !cursor-not-allowed' : buttonColorClass}`}
                    >
                        {loading ? (
                            <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                        ) : buttonText ? buttonText : (
                            editingProfileId ? <><Save size={18} /> {t('connection.saveChanges')}</> :
                            saveConnection ? <><Save size={18} /> {t('common.save')}</> :
                            t('common.connect')
                        )}
                    </button>
                </div>
                {/* E2E note */}
                {showE2ENote && (
                    <p className="text-center text-xs text-gray-400 flex items-center justify-center gap-1.5">
                        <Lock size={12} /> {t(showE2ENote)}
                    </p>
                )}
            </div>
        );
    };

    // In formOnly mode: wider for 2-column protocols, narrower for single-column providers
    const twoColProtocols = ['ftp', 'ftps', 'sftp', 's3', 'webdav', 'azure', 'filen', 'internxt', 'koofr', 'opendrive', 'kdrive', 'immich'];
    const isTwoColumnProtocol = protocol && twoColProtocols.includes(protocol);
    const formOnlyMaxW = formOnly ? (isTwoColumnProtocol ? 'max-w-4xl' : 'max-w-lg') : 'max-w-5xl';

    return (
        <>
        <div className={`w-full mx-auto relative z-10 ${formOnlyMaxW}`}>
            <div className={formOnly ? '' : 'grid md:grid-cols-2 gap-6'}>
                {/* Quick Connect */}
                <div className={`min-w-0 w-full overflow-hidden ${formOnly ? 'bg-white dark:bg-gray-800 rounded-lg border border-gray-100 dark:border-gray-700/50 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] p-6' : 'bg-white dark:bg-gray-800 rounded-lg border border-gray-100 dark:border-gray-700/50 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] p-6'}`}>
                    {/* Header: simplified in formOnly (just title, no buttons) */}
                    {formOnly ? (
                    <div className="mb-4">
                        <div className="flex items-start justify-between">
                            <div>
                                <h2 className="text-xl font-semibold">{t('connection.quickConnect')}</h2>
                                {selectedProvider && (
                                    <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">{t('connection.connectTo', { provider: selectedProvider.name })}</p>
                                )}
                            </div>
                            {(() => {
                                const PROTOCOL_DISPLAY: Record<string, { name: string; desc?: string }> = {
                                    pixelunion: { name: 'PixelUnion', desc: t('protocol.discoverPixelUnion') },
                                    immich: { name: 'Immich', desc: t('protocol.discoverImmich') },
                                };
                                const pid = connectionParams.providerId || '';
                                const logoId = selectedProviderId || pid || protocol || '';
                                const LogoComponent = PROVIDER_LOGOS[logoId];
                                const display = PROTOCOL_DISPLAY[pid] || PROTOCOL_DISPLAY[protocol || ''];
                                const providerName = selectedProvider?.name || display?.name || protocol?.toUpperCase() || '';
                                const providerDesc = selectedProvider?.description || display?.desc;
                                if (!LogoComponent && !providerName) return null;
                                return (
                                    <div className="flex flex-col items-end gap-0.5">
                                        <div className="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400">
                                            {LogoComponent && <LogoComponent size={20} />}
                                            <span className="font-medium">{providerName}</span>
                                        </div>
                                        {providerDesc && (
                                            <span className="text-[11px] text-gray-400 dark:text-gray-500 max-w-xs text-right leading-tight">{providerDesc}</span>
                                        )}
                                    </div>
                                );
                            })()}
                        </div>
                    </div>
                    ) : (
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
                        <div className="flex items-center gap-1.5">
                            {onAeroCloud && (
                                <button
                                    onClick={onAeroCloud}
                                    className={`flex items-center gap-1.5 px-2.5 py-1.5 rounded-lg transition-colors ${
                                        isAeroCloudConnected
                                            ? 'bg-sky-50 dark:bg-sky-900/30 hover:bg-sky-100 dark:hover:bg-sky-800/40 text-sky-600 dark:text-sky-400'
                                            : isAeroCloudConfigured
                                                ? 'bg-gray-50 dark:bg-gray-700 hover:bg-gray-100 dark:hover:bg-gray-600 text-gray-500 dark:text-gray-400'
                                                : 'bg-gray-50 dark:bg-gray-700 hover:bg-gray-100 dark:hover:bg-gray-600 text-gray-400 dark:text-gray-500'
                                    }`}
                                    title={isAeroCloudConfigured ? 'AeroCloud' : 'Configure AeroCloud'}
                                >
                                    <Cloud size={16} />
                                    {isAeroCloudConnected && <span className="w-1.5 h-1.5 rounded-full bg-green-500" />}
                                </button>
                            )}
                            {onAeroFile && (
                                <button
                                    onClick={onAeroFile}
                                    className="flex items-center p-1.5 bg-blue-50 dark:bg-blue-900/30 hover:bg-blue-100 dark:hover:bg-blue-800/40 text-blue-600 dark:text-blue-400 rounded-lg transition-colors"
                                    title="AeroFile"
                                >
                                    <FolderOpen size={18} />
                                </button>
                            )}
                        </div>
                    </div>
                    )}
                    <div className="space-y-3">
                        {/* Protocol Selector - hidden in formOnly unless editing a switchable protocol (FTP/FTPS/SFTP) */}
                        {(!formOnly || (editingProfileId && SWITCHABLE_PROTOCOLS.includes(protocol as ProviderType))) && (
                        <ProtocolSelector
                            value={protocol}
                            onChange={handleProtocolChange}
                            disabled={loading}
                            onOpenChange={handleProtocolSelectorOpenChange}
                            ftpTlsMode={connectionParams.options?.tlsMode}
                            allowedProtocols={editingProfileId && SWITCHABLE_PROTOCOLS.includes(protocol as ProviderType) ? SWITCHABLE_PROTOCOLS : undefined}
                        />
                        )}

                        {/* Show form only when protocol is selected AND selector is closed */}
                        {!protocol || (isProtocolSelectorOpen && !formOnly) ? (
                            /* No protocol selected or selector is open - show selection prompt + security info */
                            <div className="py-6 space-y-6">
                                <p className="text-sm text-center text-gray-500 dark:text-gray-400">{t('connection.selectProtocolPrompt')}</p>
                                {/* Security Info Box — collapsible */}
                                <div className="mx-auto max-w-sm bg-gradient-to-br from-emerald-50 to-teal-50 dark:from-emerald-900/20 dark:to-teal-900/20 border border-emerald-200 dark:border-emerald-800 rounded-lg overflow-hidden">
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
                                        <div className="flex items-center gap-3 p-3 bg-gradient-to-r from-sky-50 to-blue-50 dark:from-sky-900/30 dark:to-blue-900/30 border border-sky-200 dark:border-sky-700 rounded-lg">
                                            <div className="w-12 h-12 bg-gradient-to-br from-sky-400 to-blue-500 rounded-lg flex items-center justify-center shadow">
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
                                                className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 bg-gradient-to-r from-sky-500 to-blue-600 text-white font-medium rounded-lg hover:from-sky-600 hover:to-blue-700 transition-all"
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
                                            className="px-6 py-3 bg-gradient-to-r from-sky-500 to-blue-600 text-white font-medium rounded-lg hover:from-sky-600 hover:to-blue-700 transition-all shadow-lg hover:shadow-xl"
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
                                            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => { });
                                        }
                                    }
                                    onConnect();
                                }}
                            />
                        ) : isOAuthProvider(protocol) ? (
                            <OAuthConnect
                                provider={protocol as 'googledrive' | 'googlephotos' | 'dropbox' | 'onedrive' | 'box' | 'pcloud' | 'zohoworkdrive' | 'yandexdisk'}
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
                                            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, newServers).catch(() => { });
                                        } else {
                                            const updated = existingServers.map(s =>
                                                s.id === duplicate.id ? {
                                                    ...s,
                                                    localInitialPath: quickConnectDirs.localDir,
                                                    lastConnected: new Date().toISOString(),
                                                    ...(extraOptions?.region && { options: { ...s.options, region: extraOptions.region } }),
                                                } : s
                                            );
                                            await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, updated).catch(() => { });
                                        }
                                    }
                                    onConnect();
                                }}
                            />
                        ) : (protocol === 's3' || protocol === 'webdav') && !selectedProviderId && !editingProfileId && !formOnly ? (
                            /* Show provider selector for S3/WebDAV (skip when editing or formOnly) */
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
                                {selectedProvider && !formOnly && (
                                    <div className="flex items-center justify-between p-3 bg-gray-100 dark:bg-gray-700/50 border border-gray-100 dark:border-gray-700/50 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] rounded-lg mb-3">
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
                                                {selectedProvider.description && (
                                                    <div className="text-xs text-gray-500 dark:text-gray-400">{selectedProvider.description}</div>
                                                )}
                                            </div>
                                        </div>
                                        <button
                                            onClick={() => setSelectedProviderId(null)}
                                            className="text-xs text-blue-500 hover:text-blue-600 hover:underline"
                                        >
                                            {t('connection.change')}
                                        </button>
                                    </div>
                                )}

                                {/* Connection Fields Area */}
                                {protocol === 'filelu' ? (
                                    /* FileLu Specific Form — API Key */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5">{t('ai.settings.apiKey')}</label>
                                                <div className="relative">
                                                    <input
                                                        type={showPassword ? 'text' : 'password'}
                                                        value={connectionParams.password}
                                                        onChange={(e) => onConnectionParamsChange({
                                                            ...connectionParams,
                                                            password: e.target.value,
                                                            server: 'filelu.com',
                                                            port: 443,
                                                            username: 'api-key'
                                                        })}
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-violet-500 focus:border-violet-500"
                                                        placeholder={t('ai.settings.enterApiKey')}
                                                        autoFocus
                                                    />
                                                    <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                        {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                    </button>
                                                </div>
                                            </div>
                                            <p className="text-xs text-gray-400 mt-2 flex items-center gap-1.5">
                                                <span>{t('protocol.fileluTooltip')}</span>
                                                <a
                                                    href="https://filelu.com/5253515355.html"
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    className="text-sky-500 hover:text-sky-400 transition-colors"
                                                    title="FileLu"
                                                    aria-label="Open FileLu link"
                                                >
                                                    <ExternalLink size={12} />
                                                </a>
                                            </p>
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({
                                                disabled: !connectionParams.password,
                                                buttonColorClass: 'bg-sky-600 hover:bg-sky-700',
                                                connectionNameKey: t('filelu.connectionNamePlaceholder')
                                            })
                                        ) : (
                                            renderRightColumn({
                                                disabled: !connectionParams.password,
                                                buttonColorClass: 'bg-sky-600 hover:bg-sky-700',
                                                connectionNameKey: t('filelu.connectionNamePlaceholder')
                                            })
                                        )}
                                    </div>
                                ) : protocol === 'jottacloud' ? (
                                    /* Jottacloud Specific Form — Login Token only */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
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
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-purple-500"
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
                                        </div>

                                        {renderRightColumn({
                                            disabled: !connectionParams.password,
                                            buttonColorClass: 'bg-purple-600 hover:bg-purple-700'
                                        })}
                                    </div>
                                ) : protocol === 'drime' ? (
                                    /* Drime Cloud Specific Form — API Token only */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
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
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-green-500 focus:border-green-500"
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
                                        </div>

                                        {renderRightColumn({
                                            disabled: !connectionParams.password,
                                            buttonColorClass: 'bg-green-600 hover:bg-green-700'
                                        })}
                                    </div>
                                ) : protocol === 'koofr' ? (
                                    /* Koofr Specific Form — Email + App Password */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.koofrEmail')}</label>
                                            <input
                                                type="email"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    username: e.target.value,
                                                    server: 'app.koofr.net',
                                                    port: 443,
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-green-500 focus:border-green-500"
                                                placeholder={t('connection.koofrEmailPlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.koofrAppPassword')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        server: 'app.koofr.net',
                                                        port: 443,
                                                    })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-green-500 focus:border-green-500"
                                                    placeholder={t('connection.koofrAppPasswordPlaceholder')}
                                                />
                                                <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">
                                            {t('connection.koofrHelp')}
                                        </p>
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({ disabled: !connectionParams.username || !connectionParams.password, buttonColorClass: 'bg-green-600 hover:bg-green-700' })
                                        ) : (
                                        <>
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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-green-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-green-600 hover:bg-green-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                        </>
                                        )}
                                    </div>
                                ) : protocol === 'opendrive' ? (
                                    /* OpenDrive Specific Form - Username + Password */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('settings.username')}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    username: e.target.value,
                                                    server: 'dev.opendrive.com',
                                                    port: 443,
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-cyan-500 focus:border-cyan-500"
                                                placeholder={t('protocol.opendriveUsernamePlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('settings.password')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        server: 'dev.opendrive.com',
                                                        port: 443,
                                                    })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-cyan-500 focus:border-cyan-500"
                                                    placeholder={t('settings.passwordPlaceholder')}
                                                />
                                                <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">{t('protocol.opendriveAuthHelp')}</p>
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({ disabled: !connectionParams.username || !connectionParams.password, buttonColorClass: 'bg-cyan-600 hover:bg-cyan-700' })
                                        ) : (
                                        <>
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNameOptional')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-cyan-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-cyan-600 hover:bg-cyan-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {editingProfileId || saveConnection ? t('common.save') : t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                        </>
                                        )}
                                    </div>
                                ) : protocol === 'github' ? (
                                    /* GitHub Specific Form — Owner/Repo + PAT */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('github.ownerRepo')}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.server}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    server: e.target.value,
                                                    port: 443,
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-gray-500 focus:border-gray-500"
                                                placeholder={t('protocol.githubOwnerRepoPlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div className="flex gap-2 p-2.5 rounded-lg bg-blue-500/10 border border-blue-500/20 mt-1">
                                            <Info size={14} className="text-blue-400 flex-shrink-0 mt-0.5" />
                                            <p className="text-xs text-blue-300/80">{t('github.branchProtectionInfo')}</p>
                                        </div>
                                        {/* GitHub Auth Mode Selector */}
                                        <div className="space-y-2 mt-1">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide">Authentication</label>

                                            {/* Mode buttons — locked when editing a saved connection */}
                                            <div className="flex gap-1.5">
                                                {(['authorize', 'pat', 'app'] as const).map((mode) => {
                                                    const isActive = connectionParams.options?.githubAuthMode === mode;
                                                    const isLocked = !!editingProfileId;
                                                    return (
                                                    <button
                                                        key={mode}
                                                        type="button"
                                                        disabled={(isLocked && !isActive) || isActive}
                                                        onClick={() => {
                                                            if (isLocked || isActive) return;
                                                            onConnectionParamsChange({
                                                                ...connectionParams,
                                                                password: '',
                                                                options: { ...connectionParams.options, githubAuthMode: mode },
                                                            });
                                                        }}
                                                        className={`flex-1 px-2.5 py-2 text-xs font-medium rounded-lg border transition-colors ${
                                                            isActive
                                                                ? 'border-[var(--color-accent)] bg-[var(--color-accent)]/10 text-[var(--color-accent)]'
                                                                : isLocked
                                                                    ? 'border-gray-700 text-gray-600 cursor-not-allowed opacity-40'
                                                                    : 'border-gray-600 text-gray-400 hover:border-gray-400'
                                                        }`}
                                                    >
                                                        {mode === 'authorize' && 'Authorize'}
                                                        {mode === 'pat' && 'Access Token'}
                                                        {mode === 'app' && 'App (.pem)'}
                                                    </button>
                                                    );
                                                })}
                                            </div>

                                            {/* Mode: Authorize with GitHub (Device Flow) */}
                                            {connectionParams.options?.githubAuthMode === 'authorize' && (
                                                <div className="pt-1">
                                                    {/* Show "already authorized" if token exists in vault */}
                                                    {(connectionParams.password || hasVaultToken) && !gitHubDeviceFlow && (
                                                        <p className="text-xs text-green-500 text-center flex items-center justify-center gap-1 mb-2">
                                                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"><polyline points="20 6 9 17 4 12"/></svg>
                                                            {t('github.alreadyAuthorized')}
                                                        </p>
                                                    )}
                                                    <button
                                                        type="button"
                                                        onClick={async () => {
                                                            try {
                                                                const result = await invoke('github_device_flow_start') as { user_code: string; verification_uri: string; device_code: string; interval: number };
                                                                setGitHubDeviceFlow({
                                                                    userCode: result.user_code,
                                                                    verificationUri: result.verification_uri,
                                                                    deviceCode: result.device_code,
                                                                    interval: result.interval,
                                                                });
                                                            } catch (err) {
                                                                console.error('Device Flow failed:', err);
                                                                setGitHubAlert({
                                                                    title: t('github.authTitle'),
                                                                    message: t('github.authorizationFailed', { error: String(err) }),
                                                                    type: 'error',
                                                                });
                                                            }
                                                        }}
                                                        className="w-full flex items-center justify-center gap-2 px-4 py-3 text-sm font-medium rounded-lg border border-gray-600 hover:border-gray-400 hover:bg-gray-700 transition-colors"
                                                    >
                                                        <svg width="16" height="16" viewBox="0 0 24 24" fill="currentColor"><path d="M12 0c-6.626 0-12 5.373-12 12 0 5.302 3.438 9.8 8.207 11.387.599.111.793-.261.793-.577v-2.234c-3.338.726-4.033-1.416-4.033-1.416-.546-1.387-1.333-1.756-1.333-1.756-1.089-.745.083-.729.083-.729 1.205.084 1.839 1.237 1.839 1.237 1.07 1.834 2.807 1.304 3.492.997.107-.775.418-1.305.762-1.604-2.665-.305-5.467-1.334-5.467-5.931 0-1.311.469-2.381 1.236-3.221-.124-.303-.535-1.524.117-3.176 0 0 1.008-.322 3.301 1.23.957-.266 1.983-.399 3.003-.404 1.02.005 2.047.138 3.006.404 2.291-1.552 3.297-1.23 3.297-1.23.653 1.653.242 2.874.118 3.176.77.84 1.235 1.911 1.235 3.221 0 4.609-2.807 5.624-5.479 5.921.43.372.823 1.102.823 2.222v3.293c0 .319.192.694.801.576 4.765-1.589 8.199-6.086 8.199-11.386 0-6.627-5.373-12-12-12z"/></svg>
                                                        {t('github.authorizeWithGitHub')}
                                                    </button>
                                                    <p className="text-xs text-gray-500 mt-1.5 text-center">{t('github.authorizeBrowserHint')}</p>
                                                </div>
                                            )}

                                            {/* Mode: Personal Access Token */}
                                            {connectionParams.options?.githubAuthMode === 'pat' && (
                                                <div className="pt-1">
                                                    <div className="relative">
                                                        <input
                                                            type={showPassword ? 'text' : 'password'}
                                                            value={connectionParams.password}
                                                            onChange={(e) => onConnectionParamsChange({
                                                                ...connectionParams,
                                                                password: e.target.value,
                                                                port: 443,
                                                            })}
                                                            className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-gray-500 focus:border-gray-500"
                                                            placeholder="github_pat_xxxxxxxxxxxx"
                                                        />
                                                        <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                            {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                        </button>
                                                    </div>
                                                    <p className="text-xs text-gray-400 mt-1.5">
                                                        Fine-grained PAT with Contents (Read & Write).{' '}
                                                        <a href="https://github.com/settings/personal-access-tokens/new" target="_blank" rel="noopener noreferrer" className="text-[var(--color-accent)] hover:underline">
                                                            Generate token
                                                        </a>
                                                    </p>
                                                </div>
                                            )}

                                            {/* Mode: App Installation (Bot mode with .pem) */}
                                            {connectionParams.options?.githubAuthMode === 'app' && (
                                                <div className="pt-1 space-y-2">
                                                    <p className="text-xs text-gray-400">{t('github.appModeHint')}</p>
                                                    <p className="text-xs text-gray-500">{t('github.appTokenDuration')}</p>
                                                    <div className="relative">
                                                        <input
                                                            type="text"
                                                            value={connectionParams.options?.githubAppId || ''}
                                                            onChange={(e) => onConnectionParamsChange({
                                                                ...connectionParams,
                                                                options: { ...connectionParams.options, githubAppId: e.target.value },
                                                            })}
                                                            disabled={gitHubAppFieldsLocked}
                                                            className={`w-full px-3 py-2 text-sm border rounded-lg ${gitHubAppFieldsLocked ? 'bg-gray-100 dark:bg-gray-800 text-gray-500 dark:text-gray-400 border-gray-200 dark:border-gray-700' : 'bg-gray-50 dark:bg-gray-700 border-gray-300 dark:border-gray-600'}`}
                                                            placeholder={t('github.appIdPlaceholder')}
                                                        />
                                                        {gitHubAppFieldsLocked && (
                                                            <button type="button" onClick={() => setGitHubAppFieldsLocked(false)} className="absolute right-2 top-1/2 -translate-y-1/2 text-xs text-[var(--color-accent)] hover:underline">
                                                                Edit
                                                            </button>
                                                        )}
                                                    </div>
                                                    <div className="relative">
                                                        <input
                                                            type="text"
                                                            value={connectionParams.options?.githubInstallationId || ''}
                                                            onChange={(e) => onConnectionParamsChange({
                                                                ...connectionParams,
                                                            options: { ...connectionParams.options, githubInstallationId: e.target.value },
                                                        })}
                                                        disabled={gitHubAppFieldsLocked}
                                                        className={`w-full px-3 py-2 text-sm border rounded-lg ${gitHubAppFieldsLocked ? 'bg-gray-100 dark:bg-gray-800 text-gray-500 dark:text-gray-400 border-gray-200 dark:border-gray-700' : 'bg-gray-50 dark:bg-gray-700 border-gray-300 dark:border-gray-600'}`}
                                                        placeholder={t('github.installationIdPlaceholder')}
                                                        />
                                                    </div>
                                                    <button
                                                        type="button"
                                                        onClick={async () => {
                                                            try {
                                                                const selected = await open({
                                                                    title: t('github.selectPemTitle'),
                                                                    filters: [{ name: t('github.pemKeyLabel'), extensions: ['pem'] }],
                                                                    multiple: false,
                                                                });
                                                                if (selected) {
                                                                    const appId = connectionParams.options?.githubAppId || '';
                                                                    const installId = connectionParams.options?.githubInstallationId || '';
                                                                    if (!appId || !installId) {
                                                                        setGitHubAlert({
                                                                            title: t('github.appTitle'),
                                                                            message: t('github.appMissingIds'),
                                                                            type: 'warning',
                                                                        });
                                                                        return;
                                                                    }
                                                                    setGitHubPemLoading(true);
                                                                    // PEM read securely in backend — only path crosses IPC
                                                                    // SEC-GH-001: Token held backend-side, never returned via IPC
                                                                    const result = await invoke('github_app_token_from_pem', {
                                                                        pemPath: selected as string,
                                                                        appId,
                                                                        installationId: installId,
                                                                    }) as { success: boolean; expires_at: string };
                                                                    onConnectionParamsChange({
                                                                        ...connectionParams,
                                                                        password: '',
                                                                        options: {
                                                                            ...connectionParams.options,
                                                                            githubPemPath: selected as string,
                                                                            githubPemStored: true,
                                                                            githubTokenExpiresAt: result.expires_at,
                                                                        },
                                                                    });
                                                                    setGitHubAlert({
                                                                        title: t('github.appTitle'),
                                                                        message: t('github.pemStoredInVault'),
                                                                        type: 'warning',
                                                                    });
                                                                }
                                                            } catch (err) {
                                                                console.error('PEM auth failed:', err);
                                                                const errStr = String(err);
                                                                let message: string;
                                                                if (errStr.includes('not found') || errStr.includes('No such file')) {
                                                                    message = t('github.pemNotFound');
                                                                } else if (errStr.includes('Invalid PEM') || errStr.includes('InvalidKeyFormat') || errStr.includes('does not contain')) {
                                                                    message = t('github.pemInvalidFormat');
                                                                } else if (errStr.includes('empty')) {
                                                                    message = t('github.pemEmpty');
                                                                } else {
                                                                    message = t('github.operationFailed', { error: errStr });
                                                                }
                                                                setGitHubAlert({
                                                                    title: t('github.appTitle'),
                                                                    message,
                                                                    type: 'error',
                                                                });
                                                            } finally {
                                                                setGitHubPemLoading(false);
                                                            }
                                                        }}
                                                        className="w-full flex items-center justify-center gap-2 px-4 py-2.5 text-sm font-medium rounded-lg border border-gray-600 hover:border-gray-400 hover:bg-gray-700 transition-colors"
                                                    >
                                                        {gitHubPemLoading ? <Loader2 size={14} className="animate-spin" /> : <KeyRound size={14} />}
                                                        {gitHubPemLoading ? t('github.appTokenGenerating') : t('github.appImportPem')}
                                                    </button>
                                                    {gitHubPemInVault && !connectionParams.password && (
                                                        <p className="text-xs text-green-500 text-center flex items-center justify-center gap-1">
                                                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
                                                            {t('github.pemVaultReady') || 'PEM key found in vault — ready to connect'}
                                                        </p>
                                                    )}
                                                    {connectionParams.password && (() => {
                                                        const expiresAt = connectionParams.options?.githubTokenExpiresAt;
                                                        const expiresMs = expiresAt ? Date.parse(expiresAt) : NaN;
                                                        const isExpired = Number.isFinite(expiresMs) && expiresMs <= Date.now();
                                                        const isExpiringSoon = Number.isFinite(expiresMs) && !isExpired && expiresMs <= Date.now() + 5 * 60 * 1000;
                                                        if (isExpired) {
                                                            return (
                                                                <p className="text-xs text-amber-500 text-center flex items-center justify-center gap-1">
                                                                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"><circle cx="12" cy="12" r="10"/><line x1="12" y1="8" x2="12" y2="12"/><line x1="12" y1="16" x2="12.01" y2="16"/></svg>
                                                                    {t('github.appTokenExpired')}
                                                                </p>
                                                            );
                                                        }
                                                        if (isExpiringSoon) {
                                                            return (
                                                                <p className="text-xs text-amber-400 text-center flex items-center justify-center gap-1">
                                                                    <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"><circle cx="12" cy="12" r="10"/><polyline points="12 6 12 12 16 14"/></svg>
                                                                    {t('github.appTokenExpiringSoon')}
                                                                </p>
                                                            );
                                                        }
                                                        const expiresDate = Number.isFinite(expiresMs) ? new Date(expiresMs).toLocaleTimeString() : '';
                                                        return (
                                                            <p className="text-xs text-green-500 text-center flex items-center justify-center gap-1">
                                                                <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="3"><polyline points="20 6 9 17 4 12"/></svg>
                                                                {expiresDate ? t('github.appTokenReady', { expiresAt: expiresDate }) : t('github.appTokenReadyShort')}
                                                            </p>
                                                        );
                                                    })()}
                                                    {connectionParams.options?.githubPemStored && (
                                                        <p className="text-xs text-blue-400 text-center flex items-center justify-center gap-1">
                                                            <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2"><rect x="3" y="11" width="18" height="11" rx="2" ry="2"/><path d="M7 11V7a5 5 0 0 1 10 0v4"/></svg>
                                                            {t('github.pemVaultBadge')}
                                                        </p>
                                                    )}
                                                    <p className="text-xs text-gray-500">
                                                        <a href="https://github.com/settings/apps" target="_blank" rel="noopener noreferrer" className="text-[var(--color-accent)] hover:underline">
                                                            {t('github.manageApps')}
                                                        </a>
                                                    </p>
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                            </div>
                                        </div>

                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNameOptional')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-gray-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.server || (!connectionParams.password && !gitHubPemInVault && connectionParams.options?.githubAuthMode !== 'authorize')}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-gray-600 hover:bg-gray-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {editingProfileId || saveConnection ? t('common.save') : t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                ) : protocol === 'kdrive' ? (
                                    /* kDrive Specific Form — API Token + Drive ID */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
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
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
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
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                placeholder={t('connection.kdriveDriveIdPlaceholder')}
                                                inputMode="numeric"
                                            />
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">
                                            {t('connection.kdriveTokenHelp')}
                                        </p>
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({ disabled: !connectionParams.password || !connectionParams.options?.bucket, buttonColorClass: 'bg-blue-600 hover:bg-blue-700' })
                                        ) : (
                                        <>
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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.password || !connectionParams.options?.bucket}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-blue-600 hover:bg-blue-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                        </>
                                        )}
                                    </div>
                                ) : protocol === 'internxt' ? (
                                    /* Internxt Specific Form */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
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
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
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
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
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
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
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
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({ disabled: !connectionParams.username || !connectionParams.password, buttonColorClass: 'bg-blue-600 hover:bg-blue-700', showE2ENote: 'connection.endToEndAes' })
                                        ) : (
                                        <>
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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-blue-600 hover:bg-blue-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndAes')}
                                            </p>
                                        </div>
                                        </>
                                        )}
                                    </div>
                                ) : protocol === 'filen' ? (
                                    /* Filen Specific Form */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
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
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                placeholder={t('connection.filenEmailPlaceholder')}
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
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
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
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
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
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({ disabled: !connectionParams.username || !connectionParams.password, buttonColorClass: 'bg-emerald-600 hover:bg-emerald-700', showE2ENote: 'connection.endToEndAes' })
                                        ) : (
                                        <>
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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-emerald-600 hover:bg-emerald-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndAes')}
                                            </p>
                                        </div>
                                        </>
                                        )}
                                    </div>
                                ) : protocol === 'immich' ? (
                                    /* Immich Specific Form — Server URL + API Key */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.immichServerUrl')}</label>
                                            <input
                                                type="url"
                                                value={connectionParams.server}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    server: e.target.value,
                                                    port: 443,
                                                    username: 'api-key'
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
                                                placeholder={connectionParams.providerId === 'pixelunion' ? 'https://yourname.pixelunion.eu' : 'https://immich.example.com'}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('ai.settings.apiKey')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        server: connectionParams.server || '',
                                                        port: 443,
                                                        username: 'api-key'
                                                    })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
                                                    placeholder={t('connection.immichApiKeyPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">
                                            {t('connection.immichApiKeyHelp')}
                                        </p>
                                        <p className="text-xs text-gray-400/70 mt-1.5">
                                            {t('connection.immichOps')}
                                        </p>
                                        </div>

                                        {formOnly ? (
                                            renderRightColumn({ disabled: !connectionParams.server || !connectionParams.password, buttonColorClass: 'bg-indigo-600 hover:bg-indigo-700' })
                                        ) : (
                                        <>
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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNamePlaceholder')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.server || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-indigo-600 hover:bg-indigo-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                        </>
                                        )}
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
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
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
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    placeholder={t('connection.megaPasswordPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div className="space-y-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide">
                                                {t('connection.megaConnectionMode')}
                                            </label>
                                            <div className="grid grid-cols-2 gap-2">
                                                {(['native', 'megacmd'] as const).map((mode) => {
                                                    const isActive = megaMode === mode;
                                                    const isLocked = !!editingProfileId;
                                                    return (
                                                        <button
                                                            key={mode}
                                                            type="button"
                                                            disabled={isLocked && !isActive}
                                                            onClick={() => !isLocked && onConnectionParamsChange({
                                                                ...connectionParams,
                                                                options: {
                                                                    ...connectionParams.options,
                                                                    mega_mode: mode,
                                                                },
                                                            })}
                                                            className={`rounded-lg border px-3 py-3 text-left transition-colors ${
                                                                isLocked && !isActive
                                                                    ? 'opacity-40 cursor-not-allowed border-gray-200 dark:border-gray-700'
                                                                    : isActive
                                                                        ? 'border-red-500 bg-red-500/10 text-red-700 dark:text-red-300'
                                                                        : 'border-gray-300 bg-white text-gray-700 hover:border-red-300 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-300 dark:hover:border-red-500/60'
                                                            }`}
                                                        >
                                                            <div className="text-sm font-medium">
                                                                {mode === 'native'
                                                                    ? t('connection.megaModeNative')
                                                                    : t('connection.megaModeCmd')}
                                                            </div>
                                                            <p className="mt-1 text-xs opacity-80">
                                                                {mode === 'native'
                                                                    ? t('connection.megaModeNativeDesc')
                                                                    : t('connection.megaModeCmdDesc')}
                                                            </p>
                                                        </button>
                                                    );
                                                })}
                                            </div>
                                        </div>

                                        <div className="bg-blue-50 dark:bg-blue-900/10 p-3 rounded-lg border border-blue-100 dark:border-blue-900/30 text-xs text-blue-800 dark:text-blue-200">
                                            <p className="font-medium mb-1">
                                                {isMegaCmdMode ? t('connection.megaRequirement') : t('connection.megaNativeNotice')}
                                            </p>
                                            <p className="opacity-80">
                                                {isMegaCmdMode ? t('connection.megaRequirementDesc') : t('connection.megaNativeNoticeDesc')}
                                                {isMegaCmdMode && (
                                                    <a
                                                        href="https://mega.io/cmd"
                                                        target="_blank"
                                                        rel="noopener noreferrer"
                                                        className="block mt-1 underline hover:text-blue-600 dark:hover:text-blue-300"
                                                    >
                                                        {t('connection.downloadMegacmd')}
                                                    </a>
                                                )}
                                            </p>
                                        </div>

                                        <div className="bg-red-50 dark:bg-red-900/10 p-3 rounded-lg border border-red-100 dark:border-red-900/30">
                                            <Checkbox
                                                checked={connectionParams.options?.save_session !== false}
                                                onChange={(v) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    options: { ...connectionParams.options, save_session: v }
                                                })}
                                                label={
                                                    <div>
                                                        <span className="text-sm font-medium text-gray-900 dark:text-gray-200">{t('connection.rememberSession')}</span>
                                                        <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                            {t('connection.sessionKeysStored')}
                                                        </p>
                                                    </div>
                                                }
                                            />

                                            {isMegaCmdMode && (
                                                <div className="mt-3 pt-3 border-t border-red-200 dark:border-red-900/30">
                                                    <Checkbox
                                                        checked={!!connectionParams.options?.logout_on_disconnect}
                                                        onChange={(v) => onConnectionParamsChange({
                                                            ...connectionParams,
                                                            options: { ...connectionParams.options, logout_on_disconnect: v }
                                                        })}
                                                        label={
                                                            <div>
                                                                <span className="text-sm font-medium text-gray-900 dark:text-gray-200">{t('connection.logoutOnDisconnect')}</span>
                                                                <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                                    {t('connection.logoutOnDisconnectDesc')}
                                                                </p>
                                                            </div>
                                                        }
                                                    />
                                                </div>
                                            )}
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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePathMega')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.megaConnectionNamePlaceholder')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-red-600 hover:bg-red-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : saveConnection ? (
                                                    <><Save size={18} /> {t('common.save')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndEncrypted')}
                                            </p>
                                        </div>
                                    </div>
                                ) : protocol === 'gitlab' ? (
                                    /* GitLab Form — single-column like GitHub */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('gitlab.projectPath')}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.server}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    server: e.target.value,
                                                    port: 443,
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-orange-500 focus:border-orange-500"
                                                placeholder={t('gitlab.projectPathPlaceholder')}
                                                autoFocus
                                            />
                                            <p className="text-xs text-gray-400 mt-1.5">
                                                {t('gitlab.projectPathHint')}
                                            </p>
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('gitlab.accessToken')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        password: e.target.value,
                                                        port: 443,
                                                    })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-orange-500 focus:border-orange-500"
                                                    placeholder="glpat-xxxxxxxxxxxx"
                                                />
                                                <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                            <p className="text-xs text-gray-400 mt-1.5">
                                                {t('gitlab.tokenHint')}{' '}
                                                <a href="https://gitlab.com/-/user_settings/personal_access_tokens" target="_blank" rel="noopener noreferrer" className="underline hover:text-orange-400">{t('gitlab.createToken')}</a>
                                            </p>
                                        </div>

                                        {/* Optional: Branch + Remote/Local Path + Self-hosted TLS */}
                                        <div className="pt-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                                {t('connection.optionalSettings')}
                                            </label>
                                            <div className="space-y-2">
                                                <input
                                                    type="text"
                                                    value={connectionParams.options?.githubBranch || ''}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        options: { ...connectionParams.options, githubBranch: e.target.value },
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('gitlab.branchPlaceholder')}
                                                />
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.remoteDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('connection.initialLocalPath')}
                                                    />
                                                    <button
                                                        type="button"
                                                        onClick={handleBrowseLocalDir}
                                                        className="px-3 py-2.5 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                                                        title={t('common.browse')}
                                                    >
                                                        <FolderOpen size={16} />
                                                    </button>
                                                </div>
                                                {/* Self-hosted TLS toggle — only show when host looks self-hosted */}
                                                {connectionParams.server && !connectionParams.server.includes('gitlab.com') && connectionParams.server.includes('.') && (
                                                    <label className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400 cursor-pointer">
                                                        <input
                                                            type="checkbox"
                                                            checked={connectionParams.options?.verifyCert === false}
                                                            onChange={(e) => onConnectionParamsChange({
                                                                ...connectionParams,
                                                                options: { ...connectionParams.options, verifyCert: e.target.checked ? false : undefined },
                                                            })}
                                                            className="rounded border-gray-300 dark:border-gray-600"
                                                        />
                                                        {t('gitlab.acceptSelfSignedCerts')}
                                                    </label>
                                                )}
                                            </div>
                                        </div>

                                        {/* Save Connection */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />
                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNameOptional')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-orange-500 focus:border-transparent"
                                                    />
                                                    {renderIconPicker()}
                                                </div>
                                            )}
                                        </div>

                                        {/* Connect Button */}
                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.server || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-orange-600 hover:bg-orange-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : (
                                                    <>{ConnectIcon} {editingProfileId || saveConnection ? t('common.save') : t('connection.connect')}</>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                ) : protocol === 'swift' ? (
                                    /* Blomp / OpenStack Swift Form */
                                    <div className="space-y-4 pt-2">
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.emailAccount')}</label>
                                            <input
                                                type="email"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    username: e.target.value,
                                                    server: 'https://authenticate.blomp.com',
                                                    port: 443
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-purple-500"
                                                placeholder="your@blomp.com"
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
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-purple-500"
                                                    placeholder={t('connection.password')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

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
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.initialRemotePath')}
                                                />
                                                <div className="flex gap-2">
                                                    <input
                                                        type="text"
                                                        value={quickConnectDirs.localDir}
                                                        onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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

                                        {/* Save Connection */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <Checkbox
                                                checked={saveConnection}
                                                onChange={setSaveConnection}
                                                label={
                                                    <span className="text-sm flex items-center gap-1.5 font-medium text-gray-700 dark:text-gray-300">
                                                        <Save size={14} />
                                                        {t('connection.saveToServers')}
                                                    </span>
                                                }
                                            />

                                            {saveConnection && (
                                                <div className="mt-2 animate-fade-in-down">
                                                    <input
                                                        type="text"
                                                        value={connectionName}
                                                        onChange={(e) => setConnectionName(e.target.value)}
                                                        placeholder={t('connection.connectionNameOptional')}
                                                        className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-transparent"
                                                    />
                                                </div>
                                            )}
                                        </div>

                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                className={`w-full py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2
                                                ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-purple-600 hover:bg-purple-700'}`}
                                            >
                                                {loading ? (
                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                ) : saveConnection ? (
                                                    <><Save size={18} /> {t('common.save')}</>
                                                ) : (
                                                    <>{ConnectIcon} {t('connection.secureLogin')}</>
                                                )}
                                            </button>
                                        </div>
                                    </div>
                                ) : (
                                    /* Traditional connection fields (FTP/S3/WebDAV) — 2-column layout in formOnly */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : ''}>
                                    {/* LEFT COLUMN: Connection fields */}
                                    <div className="space-y-3">
                                        {(() => {
                                            const isNonGenericS3 = protocol === 's3' && selectedProviderId && !getProviderById(selectedProviderId)?.isGeneric;
                                            const hasPresetServer = selectedProvider && selectedProvider.defaults?.server && !selectedProvider.isGeneric;
                                            const hideServerField = hasPresetServer && !editingProfileId;
                                            if (isNonGenericS3) return null;
                                            if (hideServerField) return null; // Shown in Advanced Options below
                                            if (selectedProviderId === 'infinicloud') return null; // Rendered inside InfiniCloud mode selector block
                                            return (
                                                <div className="flex gap-2">
                                                    <div className="flex-1 min-w-0">
                                                        <div className="flex items-center gap-2 mb-1.5">
                                                            <label className="block text-sm font-medium">
                                                                {protocol === 's3' ? t('protocol.s3Endpoint') : protocol === 'azure' ? t('connection.azureEndpoint') : t('connection.server')}
                                                            </label>
                                                        </div>
                                                        <input
                                                            type="text"
                                                            value={connectionParams.server}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, server: e.target.value })}
                                                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                            placeholder={getServerPlaceholder()}
                                                        />
                                                    </div>
                                                    <div className="w-24">
                                                        <label className="block text-sm font-medium mb-1.5">{t('connection.port')}</label>
                                                        <input
                                                            type="number"
                                                            value={connectionParams.port || getDefaultPort(protocol)}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, port: parseInt(e.target.value) || getDefaultPort(protocol) })}
                                                            className="w-full px-3 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center"
                                                            min={1}
                                                            max={65535}
                                                        />
                                                    </div>
                                                </div>
                                            );
                                        })()}
                                        {/* InfiniCloud: connection mode selector (before credentials) */}
                                        {selectedProviderId === 'infinicloud' && (
                                            <div className="space-y-2">
                                                <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide">
                                                    {t('protocol.infinicloudConnectionMode')}
                                                </label>
                                                <div className="grid grid-cols-2 gap-2">
                                                    {(['webdav', 'api'] as const).map((mode) => {
                                                        const isActive = (connectionParams.options?.infinicloud_mode || 'webdav') === mode;
                                                        const isLocked = !!editingProfileId;
                                                        return (
                                                            <button
                                                                key={mode}
                                                                type="button"
                                                                disabled={isLocked && !isActive}
                                                                onClick={() => !isLocked && onConnectionParamsChange({
                                                                    ...connectionParams,
                                                                    options: {
                                                                        ...connectionParams.options,
                                                                        infinicloud_mode: mode,
                                                                        ...(mode === 'webdav' ? { apiKey: undefined } : {}),
                                                                    },
                                                                })}
                                                                className={`rounded-lg border px-3 py-3 text-left transition-colors ${
                                                                    isLocked && !isActive
                                                                        ? 'opacity-40 cursor-not-allowed border-gray-200 dark:border-gray-700'
                                                                        : isActive
                                                                            ? 'border-blue-500 bg-blue-500/10 text-blue-700 dark:text-blue-300'
                                                                            : 'border-gray-300 bg-white text-gray-700 hover:border-blue-300 dark:border-gray-600 dark:bg-gray-800 dark:text-gray-300 dark:hover:border-blue-500/60'
                                                                }`}
                                                            >
                                                                <div className="text-sm font-medium">
                                                                    {mode === 'webdav'
                                                                        ? t('protocol.infinicloudModeWebdav')
                                                                        : t('protocol.infinicloudModeApi')}
                                                                </div>
                                                                <p className="mt-1 text-xs opacity-80">
                                                                    {mode === 'webdav'
                                                                        ? t('protocol.infinicloudModeWebdavDesc')
                                                                        : t('protocol.infinicloudModeApiDesc')}
                                                                </p>
                                                            </button>
                                                        );
                                                    })}
                                                </div>
                                            </div>
                                        )}
                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{getUsernameLabel()}</label>
                                            <input
                                                type="text"
                                                value={connectionParams.username}
                                                onChange={(e) => onConnectionParamsChange({ ...connectionParams, username: e.target.value })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
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
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={t('connection.passwordPlaceholder')}
                                                />
                                                <button type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        {/* InfiniCloud: mode-dependent fields (server+port for WebDAV, API key for REST API) */}
                                        {selectedProviderId === 'infinicloud' && (
                                            connectionParams.options?.infinicloud_mode === 'api' ? (
                                                <div>
                                                    <label className="block text-sm font-medium mb-1.5">API Key</label>
                                                    <input
                                                        type="text"
                                                        value={connectionParams.options?.apiKey || ''}
                                                        onChange={(e) => onConnectionParamsChange({
                                                            ...connectionParams,
                                                            options: { ...connectionParams.options, apiKey: e.target.value.trim() },
                                                        })}
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm font-mono"
                                                        placeholder="FEF5078EA41D182EEF89A21E034BD680"
                                                    />
                                                    <p className="text-xs text-gray-400 dark:text-gray-500 mt-1">
                                                        {t('protocol.infinicloudApiKeyHint')}
                                                    </p>
                                                    <div className="mt-2 bg-blue-50 dark:bg-blue-900/10 p-3 rounded-lg border border-blue-100 dark:border-blue-900/30 text-xs text-blue-800 dark:text-blue-200">
                                                        <p className="font-medium mb-1">{t('protocol.infinicloudApiInfoTitle')}</p>
                                                        <p className="opacity-80">{t('protocol.infinicloudApiInfoDesc')}</p>
                                                    </div>
                                                </div>
                                            ) : (
                                                <div className="flex gap-2">
                                                    <div className="flex-1 min-w-0">
                                                        <label className="block text-sm font-medium mb-1.5">{t('connection.server')}</label>
                                                        <input
                                                            type="text"
                                                            value={connectionParams.server}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, server: e.target.value })}
                                                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                            placeholder="https://davXXX.teracloud.jp/dav/"
                                                        />
                                                    </div>
                                                    <div className="w-24">
                                                        <label className="block text-sm font-medium mb-1.5">{t('connection.port')}</label>
                                                        <input
                                                            type="number"
                                                            value={connectionParams.port || 443}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, port: parseInt(e.target.value) || 443 })}
                                                            className="w-full px-3 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center"
                                                            min={1}
                                                            max={65535}
                                                        />
                                                    </div>
                                                </div>
                                            )
                                        )}

                                        {/* Protocol-specific fields */}
                                        <ProtocolFields
                                            protocol={protocol || 'ftp'}
                                            options={connectionParams.options || {}}
                                            onChange={(options) => onConnectionParamsChange({ ...connectionParams, options })}
                                            disabled={loading}
                                            onBrowseKeyFile={protocol === 'sftp' ? handleBrowseSshKey : undefined}
                                            selectedProviderId={selectedProviderId}
                                            isEditing={!!editingProfileId}
                                            presetUnlocked={presetUnlocked}
                                            onPresetUnlock={(field) => setPresetUnlocked(prev => ({ ...prev, [field]: true }))}
                                        />
                                        {/* Advanced Options — hidden server/port for preset WebDAV, hidden endpoint for preset S3 */}
                                        {(() => {
                                            const hasPresetServer = selectedProvider && selectedProvider.defaults?.server && !selectedProvider.isGeneric && !editingProfileId;
                                            if (!hasPresetServer || protocol === 's3') return null;
                                            return (
                                                <div className="pt-1">
                                                    <button
                                                        type="button"
                                                        onClick={() => setShowAdvanced(!showAdvanced)}
                                                        className="flex items-center gap-1.5 text-xs text-gray-400 hover:text-gray-500 dark:text-gray-500 dark:hover:text-gray-400 transition-colors"
                                                    >
                                                        <Settings size={12} />
                                                        <span>{t('protocol.advanced')}</span>
                                                        <ChevronDown size={12} className={`transition-transform duration-200 ${showAdvanced ? 'rotate-180' : ''}`} />
                                                    </button>
                                                    {showAdvanced && (
                                                        <div className="mt-2 space-y-2 pl-0.5">
                                                            <div className="flex gap-2">
                                                                <div className="flex-1 min-w-0">
                                                                    <label className="block text-xs font-medium mb-1 text-gray-500">{t('connection.server')}</label>
                                                                    <input
                                                                        type="text"
                                                                        value={connectionParams.server || selectedProvider?.defaults?.server || ''}
                                                                        onChange={(e) => onConnectionParamsChange({ ...connectionParams, server: e.target.value })}
                                                                        disabled={!advancedUnlocked}
                                                                        className={`w-full px-3 py-2 border rounded-lg text-sm ${advancedUnlocked ? 'bg-gray-50 dark:bg-gray-700 border-gray-300 dark:border-gray-600' : 'bg-gray-100 dark:bg-gray-800 border-gray-200 dark:border-gray-700 text-gray-400 dark:text-gray-500 cursor-not-allowed'}`}
                                                                        placeholder={selectedProvider?.defaults?.server || ''}
                                                                    />
                                                                </div>
                                                                <div className="w-20">
                                                                    <label className="block text-xs font-medium mb-1 text-gray-500">{t('connection.port')}</label>
                                                                    <input
                                                                        type="number"
                                                                        value={connectionParams.port || selectedProvider?.defaults?.port || getDefaultPort(protocol)}
                                                                        onChange={(e) => onConnectionParamsChange({ ...connectionParams, port: parseInt(e.target.value) || getDefaultPort(protocol) })}
                                                                        disabled={!advancedUnlocked}
                                                                        className={`w-full px-2 py-2 border rounded-lg text-sm text-center ${advancedUnlocked ? 'bg-gray-50 dark:bg-gray-700 border-gray-300 dark:border-gray-600' : 'bg-gray-100 dark:bg-gray-800 border-gray-200 dark:border-gray-700 text-gray-400 dark:text-gray-500 cursor-not-allowed'}`}
                                                                        min={1}
                                                                        max={65535}
                                                                    />
                                                                </div>
                                                            </div>
                                                            {!advancedUnlocked && (
                                                                <button
                                                                    type="button"
                                                                    onClick={() => setShowAdvancedWarning(true)}
                                                                    className="inline-flex items-center gap-1 text-xs text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
                                                                >
                                                                    <Pencil size={10} />
                                                                    {t('common.edit')}
                                                                </button>
                                                            )}
                                                            {/* Warning mini-modal */}
                                                            {showAdvancedWarning && (
                                                                <div className="p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-700/50 rounded-lg">
                                                                    <p className="text-xs text-amber-700 dark:text-amber-300 mb-2">
                                                                        <Shield size={12} className="inline mr-1 -mt-0.5" />
                                                                        {t('protocol.advancedWarning')}
                                                                    </p>
                                                                    <div className="flex gap-2">
                                                                        <button
                                                                            type="button"
                                                                            onClick={() => { setAdvancedUnlocked(true); setShowAdvancedWarning(false); }}
                                                                            className="px-3 py-1 text-xs bg-amber-500 hover:bg-amber-600 text-white rounded-md transition-colors"
                                                                        >
                                                                            {t('protocol.advancedUnlock')}
                                                                        </button>
                                                                        <button
                                                                            type="button"
                                                                            onClick={() => setShowAdvancedWarning(false)}
                                                                            className="px-3 py-1 text-xs text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-300"
                                                                        >
                                                                            {t('common.cancel')}
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                            )}
                                                        </div>
                                                    )}
                                                </div>
                                            );
                                        })()}
                                    </div>
                                    {/* RIGHT COLUMN: Paths, Save, Buttons (shared renderRightColumn) */}
                                    {renderRightColumn({
                                        disabled: ((protocol === 's3' || protocol === 'azure') && !connectionParams.options?.bucket),
                                        buttonColorClass: 'bg-blue-600 hover:bg-blue-700',
                                        remotePathPlaceholder: selectedProviderId === 'sourceforge' ? '/home/frs/project/your-project/' : protocol === 's3' ? '/remote-folder' : protocol === 'azure' ? '/remote-folder' : '/remote-folder',
                                        showCancelSaveAsNew: true,
                                    })}
                                    </div>
                                )}
                            </>
                        )}
                    </div>
                    {/* Card footer — provider connectVia + links */}
                    {(() => {
                        if (!selectedProvider || selectedProvider.isGeneric || editingProfileId) return null;
                        const proto = protocol === 's3' ? 'S3' : protocol === 'webdav' ? 'WebDAV' : null;
                        if (!proto) return null;
                        const footerText = selectedProvider.defaults?.basePath && protocol === 'webdav'
                            ? t('protocol.webdavBasePath', { path: selectedProvider.defaults.basePath })
                            : t('protocol.connectVia', { name: selectedProvider.name, protocol: proto });
                        return (
                            <div className="-mx-6 -mb-6 mt-4 px-6 py-3 bg-gray-50/80 dark:bg-white/[0.02] border-t border-gray-100 dark:border-gray-700/50 rounded-b-lg">
                                <div className="flex items-center flex-wrap gap-x-2 gap-y-1 text-xs">
                                    <span className="inline-flex items-center gap-1.5 text-gray-500 dark:text-gray-400">
                                        <Cloud size={12} />
                                        {footerText}
                                    </span>
                                    {selectedProvider.signupUrl && (
                                        <>
                                            <span className="text-gray-300 dark:text-gray-600">&middot;</span>
                                            <a
                                                href={`${selectedProvider.signupUrl}${selectedProvider.signupUrl.includes('?') ? '&' : '?'}utm_source=aeroftp`}
                                                target="_blank"
                                                rel="noopener noreferrer"
                                                className="inline-flex items-center gap-1 text-emerald-500 hover:text-emerald-600 dark:text-emerald-400 dark:hover:text-emerald-300"
                                            >
                                                <ExternalLink size={10} />
                                                {t('connection.createAccount')}
                                            </a>
                                        </>
                                    )}
                                    {selectedProvider.helpUrl && (
                                        <>
                                            <span className="text-gray-300 dark:text-gray-600">&middot;</span>
                                            <a
                                                href={selectedProvider.helpUrl}
                                                target="_blank"
                                                rel="noopener noreferrer"
                                                className="inline-flex items-center gap-1 text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
                                            >
                                                <ExternalLink size={10} />
                                                Docs
                                            </a>
                                        </>
                                    )}
                                </div>
                            </div>
                        );
                    })()}
                </div>

                {/* Saved Servers (hidden in formOnly mode) */}
                {!formOnly && (
                <div className="min-w-0 w-full overflow-hidden bg-white dark:bg-gray-800 rounded-lg shadow-xl p-6">
                    <SavedServers
                        onConnect={onSavedServerConnect}
                        onEdit={handleEdit}
                        lastUpdate={savedServersUpdate + serversRefreshKey}
                        onOpenExportImport={() => setShowExportImport(true)}
                    />
                </div>
                )}

                {/* Skip to File Manager — accessible via status bar AeroFile button */}
            </div> {/* Close grid */}

            {/* Export/Import Dialog */}
            {showExportImport && (
                <ExportImportDialog
                    servers={servers}
                    onImport={async (newServers) => {
                        // Read ground truth from localStorage to avoid stale state
                        let currentServers: ServerProfile[] = [];
                        try {
                            const stored = localStorage.getItem(SERVERS_STORAGE_KEY);
                            if (stored) currentServers = JSON.parse(stored);
                        } catch { /* fallback */ }
                        if (currentServers.length === 0) currentServers = servers;
                        const updated = [...currentServers, ...newServers];
                        setServers(updated);
                        await secureStoreAndClean('server_profiles', SERVERS_STORAGE_KEY, updated).catch(() => { });
                        setShowExportImport(false);
                        setSavedServersUpdate(Date.now());
                    }}
                    onClose={() => setShowExportImport(false)}
                />
            )}
            {gitHubAlert && (
                <AlertDialog
                    title={gitHubAlert.title}
                    message={gitHubAlert.message}
                    type={gitHubAlert.type}
                    onClose={() => setGitHubAlert(null)}
                />
            )}
            {gitHubDeviceFlow && (
                <div className="fixed inset-0 bg-black/50 backdrop-blur-sm flex items-center justify-center z-50" role="dialog" aria-modal="true" aria-label={t('github.authTitle')}>
                    <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl max-w-md w-full mx-4 overflow-hidden animate-scale-in">
                        <div className="p-5 border-b border-gray-200 dark:border-gray-700">
                            <h3 className="text-base font-semibold text-gray-900 dark:text-gray-100">{t('github.authTitle')}</h3>
                            <p className="text-sm text-gray-600 dark:text-gray-400 mt-1">
                                {t('github.deviceFlowHint')}
                            </p>
                        </div>
                        <div className="p-5 space-y-4">
                            <div>
                                <div className="text-xs uppercase tracking-wide text-gray-500 mb-1">{t('github.deviceCode')}</div>
                                <div className="px-4 py-3 rounded-lg bg-gray-100 dark:bg-gray-700 text-lg font-mono tracking-[0.3em] text-center text-gray-900 dark:text-gray-100">
                                    {gitHubDeviceFlow.userCode}
                                </div>
                            </div>
                            <a href={gitHubDeviceFlow.verificationUri} target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-2 text-sm text-[var(--color-accent)] hover:underline">
                                <ExternalLink size={14} />
                                {gitHubDeviceFlow.verificationUri}
                            </a>
                        </div>
                        <div className="flex justify-end gap-2 px-5 py-3 bg-gray-50 dark:bg-gray-800/50 border-t border-gray-200 dark:border-gray-700">
                            <button
                                onClick={() => {
                                    setGitHubDeviceFlow(null);
                                    setGitHubDeviceFlowLoading(false);
                                }}
                                className="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                            >
                                {t('common.cancel')}
                            </button>
                            <button
                                onClick={async () => {
                                    try {
                                        setGitHubDeviceFlowLoading(true);
                                        // SEC-GH-001: Token held backend-side, never returned to frontend
                                        const result = await invoke<{ success: boolean }>('github_device_flow_complete', {
                                            deviceCode: gitHubDeviceFlow.deviceCode,
                                            interval: gitHubDeviceFlow.interval,
                                        });
                                        if (result.success) {
                                            // Store token in vault for multi-repo reuse (backend already holds it)
                                            await invoke('github_store_pat_from_held').catch((e: unknown) => console.error('Failed to store token in vault:', e));
                                            setHasVaultToken(true);
                                            // Password left empty — backend injects held token during connect
                                            onConnectionParamsChange({ ...connectionParams, password: '' });
                                        }
                                        setGitHubDeviceFlow(null);
                                        setGitHubAlert({
                                            title: t('github.authTitle'),
                                            message: t('github.alreadyAuthorized'),
                                            type: 'info',
                                        });
                                    } catch (err) {
                                        console.error('Device Flow completion failed:', err);
                                        setGitHubAlert({
                                            title: t('github.authTitle'),
                                            message: t('github.authorizationFailed', { error: String(err) }),
                                            type: 'error',
                                        });
                                    } finally {
                                        setGitHubDeviceFlowLoading(false);
                                    }
                                }}
                                className="px-4 py-2 text-sm text-white bg-[var(--color-accent)] rounded-lg hover:opacity-90 transition-colors inline-flex items-center gap-2"
                            >
                                {gitHubDeviceFlowLoading && <Loader2 size={14} className="animate-spin" />}
                                {t('github.confirmAuthorized')}
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </div>
        {/* Rebex demo server disclaimer */}
        {connectionParams.server === 'test.rebex.net' && (
            <div className="mt-3">
                <p className="text-center text-xs text-gray-400 dark:text-gray-500 flex items-center justify-center gap-1.5 flex-wrap">
                    <Info size={12} className="shrink-0" />
                    <span>{t('protocol.rebexDemoDisclaimer')}</span>
                    <a href="https://www.rebex.net" target="_blank" rel="noopener noreferrer" className="inline-flex items-center gap-1 text-blue-400 hover:text-blue-300">
                        <ExternalLink size={10} />
                        rebex.net
                    </a>
                </p>
            </div>
        )}
        {/* Provider independence disclaimer — outside formOnlyMaxW container */}
        {(() => {
            const disclaimerProvider = selectedProvider ?? (protocol ? getProviderById(protocol) : null);
            const nameMap: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', zohoworkdrive: 'Zoho WorkDrive', yandexdisk: 'Yandex Disk', filen: 'Filen', internxt: 'Internxt', kdrive: 'kDrive', jottacloud: 'Jottacloud', drime: 'Drime Cloud', koofr: 'Koofr', opendrive: 'OpenDrive', github: 'GitHub', gitlab: 'GitLab', pixelunion: 'PixelUnion' };
            const providerName = disclaimerProvider?.name
                || nameMap[connectionParams.providerId || ''] || nameMap[protocol || ''];
            if (!providerName || (disclaimerProvider?.isGeneric && !connectionParams.providerId)) return null;
            const contactProtocols = new Set(['zohoworkdrive', 'koofr', 'jottacloud', 'infinicloud']);
            const isContact = disclaimerProvider?.contactVerified || contactProtocols.has(protocol || '');
            return (
                <div className="mt-3 space-y-1">
                    <p className="text-center text-xs text-gray-400 dark:text-gray-500 flex items-center justify-center gap-1.5">
                        <Info size={12} className="shrink-0" />
                        <span>{t('protocol.independentProject', { provider: providerName })}</span>
                    </p>
                    {isContact && (
                        <p className="text-center text-xs text-gray-400 dark:text-gray-500 flex items-center justify-center gap-1.5">
                            <ShieldCheck size={12} className="shrink-0 text-emerald-500" />
                            <span>{t('protocol.directContact', { provider: providerName })}</span>
                        </p>
                    )}
                </div>
            );
        })()}
        </>
    );
};

export default ConnectionScreen;
