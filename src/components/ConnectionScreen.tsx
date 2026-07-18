// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * ConnectionScreen Component
 * Initial connection form with Quick Connect and Saved Servers
 */

import React, { useState, useEffect, useRef, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { readFile } from '@tauri-apps/plugin-fs';
import { FolderOpen, HardDrive, ChevronRight, ChevronDown, Save, Copy, Cloud, Check, Settings, Clock, Folder, X, Lock, ArrowLeft, Eye, EyeOff, ExternalLink, Shield, ShieldCheck, KeyRound, Loader2, Image, Info, Pencil, Link2, ArrowRightLeft, RefreshCw, Usb } from 'lucide-react';
import { ConnectionParams, ProviderType, ProviderOptions, DeviceFingerprint, isOAuthProvider, isAeroCloudProvider, isFourSharedProvider, isNativeApiProtocol, isNonFtpProvider, providerServesQuota, providerSupportsCryptOverlay, ServerProfile } from '../types';
import type { MtpDeviceInfo } from '../types/aerofile';
import { deviceFingerprintFromMtpInfo, matchLiveDevice } from '../utils/mtpFingerprint';
import { listMtpDevices } from '../utils/mtpListDevices';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { PasswordStrengthBar } from './vault/PasswordStrengthBar';
import { PasswordMatchHint } from './common/PasswordMatchHint';
import { SavedServers } from './SavedServers';
import { ExportImportDialog } from './ExportImportDialog';
import { useTranslation } from '../i18n';
import { ProtocolSelector, ProtocolFields, getDefaultPort } from './ProtocolSelector';
import { UnstableProviderNotice } from './UnstableProviderNotice';
import { ProviderModeTabs } from './ProviderModeTabs';
import { CollapsibleSetupBox } from './CollapsibleSetupBox';
import { AeroShareHandshakeBody } from './AeroShare/AeroShareHandshakeBody';
import { TotpCodeInput } from './TotpCodeInput';
import { StoredTotpSecretDisclosure } from './StoredTotpSecretDisclosure';
import { InlinePasswordGenerator } from './common/InlinePasswordGenerator';
import { findActiveMode, findActiveModeGroup, modeGroupProviderIds, resolveModeHeader, resolveModeSwitchCredentials } from './providerModeGroups';
import { loadModeCredentials, storeModeCredentials, deleteModeCredentials, type ModeCredentialMap } from '../utils/modeCredentialStore';
import { openUrl } from '../utils/openUrl';
import { safePickerStartDir } from '../utils/safePickerDir';
import { isValidOverlayScope, resolveOverlayScope } from '../utils/overlayScope';
import { OAuthConnect } from './OAuthConnect';
import { ProviderSelector } from './ProviderSelector';
import { AlertDialog } from './Dialogs';
import { IconPickerDialog } from './IconPickerDialog';
import { getProviderById, resolveS3Endpoint, ProviderConfig } from '../providers';
import { getProviderDocsUrl, PROVIDER_DOCS_INDEX } from '../providers/docsLinks';
import { getMegaConnectionMode, normalizeMegaOptions } from '../utils/providerConnectionMeta';
import { loadSavedServerProfiles, storeSavedServerProfiles } from '../utils/serverProfileStore';
import { carryFavoriteServer } from '../utils/favoriteServers';
import { carryServerGroups } from '../utils/serverGroups';
import { getStorageDedupKey } from '../utils/storageDedup';
import { formatBytes, parseHumanSize } from '../utils/formatters';
import { useActivityLog } from '../hooks/useActivityLog';
import { logger } from '../utils/logger';
import { Checkbox } from './ui/Checkbox';
import { FILEN_BRIDGE_MAX_LENGTH, FILEN_BRIDGE_REJECTED_CHARACTERS, filenBridgeCredentialError } from '../utils/passwordForge';

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
    onConnect: (overrideParams?: ConnectionParams) => void;
    onSavedServerConnect: (params: ConnectionParams, initialPath?: string, localInitialPath?: string) => Promise<void>;
    onSkipToFileManager: () => void;
    onAeroFile?: () => void;
    onAeroCloud?: () => void;
    isAeroCloudConfigured?: boolean;
    isAeroCloudConnected?: boolean;
    onOpenCloudPanel?: () => void;
    hasExistingSessions?: boolean;  // Show active sessions badge next to QuickConnect
    sessionCount?: number;  // Number of open session tabs, shown as a count chip on the badge (#128-C)
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
            // Sanitize the starting dir so a stale local path cannot crash the
            // native folder chooser (Fix G).
            const selected = await open({ directory: true, multiple: false, defaultPath: await safePickerStartDir(localPath), title: t('connection.fourshared.selectLocalFolder') });
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

    // Active state: already authenticated
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
                            onClick={() => openUrl('https://www.4shared.com/developer/docs/app/')}
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
                            <button tabIndex={-1} type="button" onClick={() => setShowSecret(!showSecret)} className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
    sessionCount = 0,
    serversRefreshKey = 0,
    formOnly = false,
    editingProfile,
    onFormSaved,
    onTabLabelChange,
}) => {
    const t = useTranslation();
    const { log: logActivity } = useActivityLog();
    const protocol = connectionParams.protocol; // Can be undefined
    // AeroShare friend (protocol "peer"): editing one must NOT show the FTP
    // credential layout. The identity (AeroFTP-ID) + drive binding are fixed by
    // the handshake; the only editable attribute is the friend's display name.
    // So we relabel Server -> AeroFTP-ID (read-only), Username -> friend name,
    // and hide Port / Password / Remote Path / storage fields below.
    const isPeer = protocol === 'peer';
    // Portable MTP device profile (APPENDIX-DEVICE-PROFILES Phase 2): no host/password;
    // detect attached devices, pick one, store deviceFingerprint + default paths.
    const isMtp = protocol === 'mtp';

    // Connections are always saved (the legacy "Save this connection" checkbox
    // was removed: the user can still delete a profile from the list afterwards).
    const [saveConnection, setSaveConnection] = useState(true);
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
    const [showIconPicker, setShowIconPicker] = useState(false);

    // MTP device-profile form state (list_mtp_devices → fingerprint → save)
    const [mtpDevices, setMtpDevices] = useState<MtpDeviceInfo[]>([]);
    const [mtpDetecting, setMtpDetecting] = useState(false);
    const [mtpDetectError, setMtpDetectError] = useState<string | null>(null);
    const [mtpSelectedDeviceId, setMtpSelectedDeviceId] = useState<string | null>(null);
    const [mtpFingerprint, setMtpFingerprint] = useState<DeviceFingerprint | undefined>(undefined);

    // AeroCloud state
    const [aeroCloudConfig, setAeroCloudConfig] = useState<AeroCloudConfig | null>(null);
    const [aeroCloudLoading, setAeroCloudLoading] = useState(false);

    // Edit state
    const [editingProfileId, setEditingProfileId] = useState<string | null>(null);
    const editingProfileIdRef = useRef<string | null>(null);
    // Snapshot of the (protocol, providerId) pair captured when the user
    // entered edit mode. Used by `modeChanged` to detect when the operator
    // has switched to a different mode of the SAME provider group (issue
    // #215). When that happens the footer offers "Save as new" + "Convert"
    // instead of the standard in-place Save.
    const [originalEditMode, setOriginalEditMode] = useState<{
        protocol: ProviderType;
        providerId?: string;
    } | null>(null);
    const [savedServersUpdate, setSavedServersUpdate] = useState(0);
    const [showPassword, setShowPassword] = useState(false);
    // Reveal toggle for the optional Filen CLI API Key field.
    const [showFilenApiKey, setShowFilenApiKey] = useState(false);

    // Provider selection state (for S3/WebDAV)
    const [showAdvanced, setShowAdvanced] = useState(false);
    const [advancedUnlocked, setAdvancedUnlocked] = useState(false);
    const [showAdvancedWarning, setShowAdvancedWarning] = useState(false);
    const [selectedProviderId, setSelectedProviderId] = useState<string | null>(
        formOnly && connectionParams.providerId ? connectionParams.providerId : null
    );
    const selectedProvider = selectedProviderId ? getProviderById(selectedProviderId) : null;
    // Group-wide account links: when the active config belongs to a provider
    // mode group (Koofr, OpenDrive, Filen, FileLu), the "Create Account" /
    // "Generate password" buttons resolve from any preset in the group, not
    // just the active mode's own preset. Preset-less native modes (Koofr API,
    // OpenDrive API) otherwise carried no provider and the buttons vanished
    // when switching to them (#215). Every surface in a group hits the same
    // account, so the same signup / password page is correct for all tabs.
    const groupAccountProviders = modeGroupProviderIds(connectionParams.providerId, connectionParams.protocol)
        .map(getProviderById);
    const accountSignupUrl =
        selectedProvider?.signupUrl
        || groupAccountProviders.find((p) => p?.signupUrl)?.signupUrl;
    const accountPasswordGenUrl =
        selectedProvider?.passwordGenUrl
        || groupAccountProviders.find((p) => p?.passwordGenUrl)?.passwordGenUrl;
    const megaMode = getMegaConnectionMode(connectionParams.options);
    const isMegaCmdMode = megaMode === 'megacmd';
    const activeProviderId = connectionParams.providerId || selectedProviderId || undefined;
    const isFilenDesktopBridge = activeProviderId === 'filen-desktop-webdav' || activeProviderId === 'filen-desktop-s3';
    const filenBridgeCredentialIssue = isFilenDesktopBridge
        ? filenBridgeCredentialError(connectionParams.username) || filenBridgeCredentialError(connectionParams.password)
        : null;
    // Resolve the provider currently targeted by the form so we can warn when
    // it is flagged stable:false. The dev-only protocol grid hides these, but
    // the Add Service list view leaks them into production (#308); swift with no
    // explicit providerId means Blomp, mirroring the save/connect fallbacks below.
    const formProvider = selectedProvider
        || (activeProviderId ? getProviderById(activeProviderId) : null)
        || (protocol === 'swift' ? getProviderById('blomp') : null);

    // Protocol selector open state (to hide form when selector is open)
    const [isProtocolSelectorOpen, setIsProtocolSelectorOpen] = useState(false);

    // Track which preset fields have been unlocked for editing
    const [presetUnlocked, setPresetUnlocked] = useState<Record<string, boolean>>({});

    // Track previous protocol for switch detection in handleProtocolChange
    const previousProtocolRef = React.useRef<ProviderType | undefined>(undefined);

    // Issue #215: per-mode credential snapshots, in-memory for the lifetime
    // of one edit session. When a saved profile is switched between modes of
    // the same provider group (Filen API <-> Local WebDAV <-> Local S3, etc.)
    // each mode's typed credentials, including options-level secrets like
    // totp_secret / filen_api_key / S3 keys, are stashed under the mode key
    // (providerId || protocol) and restored on return, so switching back no
    // longer wipes the API key and 2FA secret. Cleared when a different
    // profile starts editing to avoid cross-profile leakage.
    const modeCredentialSnapshotsRef = React.useRef<Record<string, {
        username: string;
        password: string;
        server: string;
        port?: number;
        options?: ConnectionParams['options'];
    }>>({});

    // Issue #215: opt-in to PERSIST the per-mode snapshots to the encrypted
    // vault so they survive a restart (one profile per account, switch protocol
    // freely). Mirrors profile.persistModeCredentials; only meaningful for
    // profiles whose provider/protocol belongs to a mode group.
    const [persistModeCredentials, setPersistModeCredentials] = useState(false);
    // P3: AeroCrypt Profile binding (transparent encrypted overlay on the dual-panel).
    // Ehud #276 (17324431): a collapsible "Wrappers / Overlays" parent keeps the Quick
    // Connect page tidy while staying collapsible. Expanded by default (Ehud 2026-06-28):
    // the Crypt enable checkbox is the only thing it reveals when closed, and you have to
    // tick it to configure anything, so opening the section by default removes a click and
    // surfaces the option instead of burying it one level down.
    const [overlaysExpanded, setOverlaysExpanded] = useState(true);
    const [aeroCryptEnabled, setAeroCryptEnabled] = useState(false);
    // No default crypt kind: the user must actively pick aerocrypt vs rclone-crypt
    // (Ehud #276, 2026-06-13: both opt-in, no tap-Enter default). null until chosen.
    const [aeroCryptKind, setAeroCryptKind] = useState<'aerocrypt' | 'rclone-crypt' | null>(null);
    // C-EDIT-GUARD: true when editing a profile that ALREADY has an overlay binding.
    // The remote already holds blobs whose keys derive directly from kind + salt +
    // password, so changing any of them would orphan that data. In that case the
    // kind switch and the credential fields are shown but DISABLED (enable/disable
    // stays active). Changing the password safely needs a re-encrypt / format
    // rewrap, tracked as a separate feature.
    const [overlayBindingLocked, setOverlayBindingLocked] = useState(false);
    const [aeroCryptPassword, setAeroCryptPassword] = useState('');
    // Confirm field for the set-once overlay password: a typo here permanently
    // locks the encrypted blobs, so a live match check is worth the extra field.
    const [aeroCryptConfirm, setAeroCryptConfirm] = useState('');
    const [showAeroCryptPassword, setShowAeroCryptPassword] = useState(false);
    const [aeroCryptWithHeader, setAeroCryptWithHeader] = useState(false);
    const [aeroCryptDefaultSalt, setAeroCryptDefaultSalt] = useState(false);
    const [aeroCryptDefaultSaltStrength, setAeroCryptDefaultSaltStrength] = useState<'128' | '256'>('128');
    const [aeroCryptDefaultSaltAttested, setAeroCryptDefaultSaltAttested] = useState(false);
    // P3.3b: rclone-crypt interop needs salt (password2) + filename/dir-name
    // encryption mode to auto-unlock on connect, mirroring the RcloneCryptUnlock
    // modal. Native AeroCrypt ignores these (config lives in its marker).
    const [aeroCryptSalt, setAeroCryptSalt] = useState('');
    const [showAeroCryptSalt, setShowAeroCryptSalt] = useState(false);
    // AeroCrypt Tier 1 keyfile second factor: the PATH is a pointer, not a
    // secret, so unlike the password it is hydrated into the form on edit and
    // stays editable even when the binding is locked (re-pointing after an
    // import moves where the file lives, not the factor itself; a mismatched
    // keyfile fails closed at unlock, the remote config decides).
    const [aeroCryptKeyfilePath, setAeroCryptKeyfilePath] = useState('');
    const [keyfileJustGenerated, setKeyfileJustGenerated] = useState(false);
    const [keyfileError, setKeyfileError] = useState<string | null>(null);
    const [aeroCryptFilenameEnc, setAeroCryptFilenameEnc] = useState<'standard' | 'obfuscate' | 'off'>('standard');
    const [aeroCryptDirNameEnc, setAeroCryptDirNameEnc] = useState(true);
    // P3 follow-up (#369): optional user-pinned remote anchor for the overlay.
    // Empty means "use the profile Remote Path" (preserves today's behavior and
    // all existing profiles). A non-empty value is interpreted RELATIVE to the
    // Remote Path (#369): a bare subfolder name is nested under it, so the prefix
    // is never re-typed and the anchor can never escape the Remote Path. Resolved
    // via resolveOverlayScope() and persisted into aeroCryptOverlay.remoteScope.
    const [overlaysRemotePath, setOverlaysRemotePath] = useState('');
    const [overlaysRemotePathError, setOverlaysRemotePathError] = useState<string | null>(null);

    // Re-validate overlays remote path whenever the baseline remoteDir changes
    // (user edits Remote Path after typing a scope, or on hydration).
    useEffect(() => {
        if (!overlaysRemotePath && !overlaysRemotePathError) return;
        // Validate the RESOLVED scope: a bare subfolder name is nested under the
        // Remote Path (#369 relative UX), so it is always in scope and the error
        // never fires for normal input; the guard only survives as a safety net.
        const valid = isValidOverlayScope(resolveOverlayScope(overlaysRemotePath, quickConnectDirs.remoteDir), quickConnectDirs.remoteDir);
        setOverlaysRemotePathError(valid ? null : t('aerocryptProfile.overlaysRemotePathInvalid'));
        // eslint-disable-next-line react-hooks/exhaustive-deps
    }, [overlaysRemotePath, quickConnectDirs.remoteDir, t]);

    // Issue #215: MEGAcmd WebDAV endpoint auto-fetch state. Running
    // `mega-webdav /` (same idempotent call that warms the bridge for the
    // quota probe) prints the served URL, so the operator no longer has to
    // copy it from the MEGAcmd terminal.
    const [megaWebdavFetching, setMegaWebdavFetching] = useState(false);
    const [megaWebdavError, setMegaWebdavError] = useState<string | null>(null);
    // #215 follow-up: true when the active local-bridge mode's helper app
    // (Filen Desktop / MEGAcmd) is confidently not installed (🔴). Disables the
    // connect/save button. Reported by ProviderModeTabs -> BridgeStatusBanner.
    const [bridgeSaveBlocked, setBridgeSaveBlocked] = useState(false);
    // Active local-bridge 🔴/🟠/🟢 state (idea D): collapses the "Setup … first"
    // box once the bridge is active. undefined for non-bridge providers.
    const [bridgeUiState, setBridgeUiState] = useState<'red' | 'amber' | 'green' | undefined>(undefined);
    // Whether the active mode is a local-bridge mode (Filen Desktop / MEGAcmd),
    // computed synchronously so the collapsible setup box knows at mount and does
    // not flash open before an effect reports it (#215 idea D flash fix).
    const activeBridgeKind = (() => {
        const pid = selectedProviderId || connectionParams.providerId;
        const g = findActiveModeGroup(pid, protocol);
        return g ? findActiveMode(g, pid, protocol)?.bridgeKind : undefined;
    })();
    const isBridgeMode = !!activeBridgeKind;

    // When re-opening dropdown with a protocol already selected, clear the selection.
    // In formOnly (IntroHub edit), keep everything: just open the dropdown overlay.
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
    const [savedProfilesForNaming, setSavedProfilesForNaming] = useState<ServerProfile[]>([]);

    // Load servers when opening export/import dialog
    useEffect(() => {
        if (!showExportImport) return;
        let cancelled = false;
        (async () => {
            const vaultServers = await loadSavedServerProfiles();
            if (!cancelled) setServers(vaultServers);
        })();
        return () => { cancelled = true; };
    }, [showExportImport]);

    useEffect(() => {
        let cancelled = false;
        (async () => {
            const loaded = await loadSavedServerProfiles();
            if (!cancelled) setSavedProfilesForNaming(loaded);
        })();
        return () => { cancelled = true; };
    }, [savedServersUpdate, serversRefreshKey]);
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

    // Hydrate MTP fingerprint when editing a saved device profile (or clear when leaving mtp).
    // On edit: auto-detect attached devices and preselect the fingerprint match so
    // the user does not need a manual Detect click (live-test LT5).
    useEffect(() => {
        if (!isMtp) {
            setMtpDevices([]);
            setMtpDetectError(null);
            setMtpSelectedDeviceId(null);
            setMtpFingerprint(undefined);
            return;
        }
        if (!editingProfile?.deviceFingerprint) {
            return;
        }
        setMtpFingerprint(editingProfile.deviceFingerprint);
        setMtpSelectedDeviceId(null);
        let cancelled = false;
        setMtpDetecting(true);
        setMtpDetectError(null);
        void (async () => {
            try {
                const devices = await listMtpDevices();
                if (cancelled) return;
                setMtpDevices(devices || []);
                if (!devices || devices.length === 0) {
                    setMtpDetectError(t('connection.mtpNoDevices'));
                    return;
                }
                const live = matchLiveDevice(
                    editingProfile.deviceFingerprint?.canonical,
                    devices,
                );
                if (live) {
                    setMtpSelectedDeviceId(live.deviceId);
                    // Refresh fingerprint from live row; keep saved profile name.
                    const fp = deviceFingerprintFromMtpInfo(live);
                    if (fp) setMtpFingerprint(fp);
                }
            } catch (e) {
                if (cancelled) return;
                const msg = e instanceof Error ? e.message : String(e);
                setMtpDevices([]);
                setMtpDetectError(msg || t('connection.mtpDetectFailed'));
            } finally {
                if (!cancelled) setMtpDetecting(false);
            }
        })();
        return () => {
            cancelled = true;
        };
    }, [isMtp, editingProfile?.id, editingProfile?.deviceFingerprint, t]);

    const detectMtpDevices = async () => {
        setMtpDetecting(true);
        setMtpDetectError(null);
        try {
            const devices = await listMtpDevices();
            setMtpDevices(devices || []);
            if (!devices || devices.length === 0) {
                setMtpDetectError(t('connection.mtpNoDevices'));
            } else if (editingProfile?.deviceFingerprint?.canonical) {
                // Manual Detect while editing: re-preselect the saved device.
                const live = matchLiveDevice(
                    editingProfile.deviceFingerprint.canonical,
                    devices,
                );
                if (live) {
                    setMtpSelectedDeviceId(live.deviceId);
                    const fp = deviceFingerprintFromMtpInfo(live);
                    if (fp) setMtpFingerprint(fp);
                }
            }
        } catch (e) {
            const msg = e instanceof Error ? e.message : String(e);
            setMtpDevices([]);
            setMtpDetectError(msg || t('connection.mtpDetectFailed'));
        } finally {
            setMtpDetecting(false);
        }
    };

    const selectMtpDevice = (deviceId: string) => {
        setMtpSelectedDeviceId(deviceId);
        const device = mtpDevices.find((d) => d.deviceId === deviceId);
        if (!device) {
            setMtpFingerprint(undefined);
            return;
        }
        const fp = deviceFingerprintFromMtpInfo(device);
        setMtpFingerprint(fp);
        // Suggest profile name from device display name when the field is empty
        // or still the previous device name.
        const suggestion = device.displayName?.trim() || fp?.model || 'MTP device';
        if (!connectionName.trim() || connectionName === connectionParams.server) {
            setConnectionName(suggestion);
        }
        onConnectionParamsChange({
            ...connectionParams,
            server: suggestion,
            port: 0,
            username: '',
            password: '',
            protocol: 'mtp',
            providerId: connectionParams.providerId || 'mtp-portable',
        });
    };

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

    // Issue #230: the Filen CLI API key is a long-lived secret, so it must live
    // in the secure vault (exactly like passwords), never inside the saved
    // profile's options (which get serialised to localStorage / the profile
    // store). Strip it from `options`, persist it under filen_api_key_<id>, and
    // return whether a key is now stored so the caller can set the profile flag.
    // `hadStored` preserves an existing vault key when the form field is left
    // blank on edit, mirroring how a blank password keeps the stored credential.
    const stashFilenApiKey = async (
        profileId: string,
        options: ProviderOptions,
        hadStored?: boolean,
    ): Promise<boolean> => {
        if (!('filen_api_key' in options)) return !!hadStored;
        const key = options.filen_api_key;
        delete options.filen_api_key;
        if (key && key.trim()) {
            return tryStoreCredential(`filen_api_key_${profileId}`, key);
        }
        // The field was present but cleared. On edit the key is hydrated into the
        // form (the saved profile options never carry it), so a blank value here
        // is an explicit "remove the key", not the password-style "left blank to
        // keep". Delete the vaulted key (idempotent) and report none stored, so
        // the green Save persists the removal exactly like "Convert to <mode>"
        // does. Previously this returned `hadStored`, silently keeping the vault
        // key so reopening re-hydrated the "deleted" key (issues #128 / #215).
        if (hadStored) {
            try {
                await invoke('delete_credential', { account: `filen_api_key_${profileId}` });
            } catch (err) {
                console.error('Failed to delete Filen API key credential:', err);
            }
        }
        return false;
    };

    // Issue #215: Detect when the operator has switched to a different
    // mode of the SAME provider group while editing a saved profile (e.g.
    // Koofr WebDAV -> Koofr Native API, FileLu API -> FileLu S3). When
    // true, the footer offers "Save as new" + "Convert to <mode>" instead
    // of the standard in-place Save, to avoid silently mutating a saved
    // profile across structurally different credential shapes.
    const modeChanged = useMemo(() => {
        if (!editingProfileId || !originalEditMode || !protocol) return false;
        const currentProviderId = selectedProviderId || connectionParams.providerId || undefined;
        const origProviderId = originalEditMode.providerId || undefined;
        if (protocol === originalEditMode.protocol && currentProviderId === origProviderId) {
            return false;
        }
        const oldGroup = findActiveModeGroup(origProviderId, originalEditMode.protocol);
        const newGroup = findActiveModeGroup(currentProviderId, protocol);
        return !!oldGroup && oldGroup === newGroup;
    }, [editingProfileId, originalEditMode, protocol, selectedProviderId, connectionParams.providerId]);

    // Issue #215: the persist-credentials opt-in is only meaningful when the
    // active provider/protocol is part of a mode group (one account, several
    // protocols). Outside a group there is a single credential set, already
    // handled by the standard per-profile vault entry.
    const inModeGroup = useMemo(
        () => !!protocol && !!findActiveModeGroup(activeProviderId, protocol),
        [activeProviderId, protocol],
    );

    // Issue #215 (Ehud): hide the opt-in for groups whose modes share one
    // credential set (Koofr, OpenDrive). There is nothing to remember per
    // protocol because both surfaces use the same email + password, already
    // saved with the profile. With the checkbox hidden, `persistModeCredentials`
    // stays at its default (false) for new profiles, so no per-mode snapshots
    // are written; the save logic continues to key off `inModeGroup`.
    const showPersistCheckbox = useMemo(() => {
        if (!protocol) return false;
        const group = findActiveModeGroup(activeProviderId, protocol);
        return !!group && !group.sharedCredentials;
    }, [activeProviderId, protocol]);

    // Mode-stash key: uniquely identifies a mode within a group. It MUST include
    // the protocol, not just the providerId. Koofr's native API is preset-less
    // (its providerId falls back to the protocol 'koofr') while its WebDAV
    // preset's providerId is ALSO 'koofr', so a providerId-only key collided:
    // switching API->WebDAV in edit stashed the API creds under 'koofr' and then
    // restored them as the WebDAV mode, leaking the bare app.koofr.net endpoint
    // over the preset's https://app.koofr.net/dav/Koofr -> 404 on every switch
    // (#385). Normalizing a legacy native whose providerId === protocol back to
    // preset-less keeps the native key stable however the profile persisted it.
    const modeStashKey = (providerId: string | null | undefined, proto: string): string => {
        const pid = providerId && providerId !== proto ? providerId : '';
        return `${pid}|${proto}`;
    };

    // Legacy persisted snapshots (pre collision-free keys) used
    // `providerId || protocol` as the map key. Re-key them to the current
    // `pid|proto` form at hydrate time, so per-mode credentials saved before
    // the rekey keep restoring instead of coming up blank, and the old-format
    // entries do not linger in the vault map as dead secrets. For the one
    // historic collision (Koofr: both modes hashed to 'koofr') the surviving
    // entry deterministically lands on the group's first matching mode.
    const migrateLegacyModeKeys = (
        persisted: ModeCredentialMap,
        group: ReturnType<typeof findActiveModeGroup>
    ): ModeCredentialMap => {
        if (!group) return persisted;
        const out: ModeCredentialMap = { ...persisted };
        for (const mode of group.modes) {
            const proto = mode.protocol as string;
            const legacyKey = mode.providerId || proto;
            const newKey = modeStashKey(mode.providerId, proto);
            if (legacyKey !== newKey && out[legacyKey] && !out[newKey]) {
                out[newKey] = out[legacyKey];
                delete out[legacyKey];
            }
        }
        return out;
    };

    // The stash key of the currently-active mode, matching the oldKey/newKey
    // convention in handleProtocolChange.
    const computeActiveModeKey = (): string => {
        const pid = selectedProviderId || connectionParams.providerId || undefined;
        return modeStashKey(pid, protocol as string);
    };

    // The full per-mode map to persist: every stashed mode plus the live one.
    const buildModeCredentialMap = (): ModeCredentialMap => {
        const map: ModeCredentialMap = { ...modeCredentialSnapshotsRef.current };
        map[computeActiveModeKey()] = {
            username: connectionParams.username,
            password: connectionParams.password,
            server: connectionParams.server,
            port: connectionParams.port,
            options: connectionParams.options ? { ...connectionParams.options } : undefined,
        };
        return map;
    };

    // Write or clear the persisted per-mode snapshots for a profile depending on
    // the opt-in. Called from saveToServers for both the edit and new branches.
    const syncPersistedModeCredentials = async (profileId: string): Promise<void> => {
        if (persistModeCredentials && inModeGroup) {
            await storeModeCredentials(profileId, buildModeCredentialMap());
        } else {
            await deleteModeCredentials(profileId);
        }
    };

    // P3: the AeroCrypt overlay binding is offered only on backends where a
    // transparent crypt overlay actually applies (shared predicate with the
    // runtime context-menu entries in App.tsx). Media-only and repo APIs are
    // excluded: an encrypted overlay there is confusing and corrupts uploads.
    const overlayEligible = providerSupportsCryptOverlay(protocol);
    // C-EDIT-GUARD: kind + credential fields are read-only when editing a profile
    // that already carries an overlay binding (its remote holds keyed blobs).
    const overlayFieldsLocked = overlayBindingLocked && !!editingProfileId;
    // Node 2: enabling an overlay on an EXISTING profile that had none means its
    // remote may already hold plaintext files; those stay unencrypted and render
    // as an undecryptable mix under the overlay. Warn (do not block: a brand-new
    // empty remote is a legitimate case).
    const overlayNewlyBound = !!editingProfileId && aeroCryptEnabled && !overlayBindingLocked;
    // #322: when binding a new crypt overlay, the set-once password's confirm
    // must match before the profile can be saved (a typo locks the blobs forever).
    const aeroCryptConfirmMismatch = aeroCryptEnabled && overlayEligible && !overlayFieldsLocked && !!aeroCryptPassword && aeroCryptConfirm !== aeroCryptPassword;

    // D1-D3 entropy gate (reuse PasswordStrengthBar visual signal + length floor).
    // 128-bit recommended tier: level 4 + ~20 chars minimum.
    // 256-bit stricter: level 4 + ~39 chars.
    // Plus explicit attestation checkbox.
    const pwLen = aeroCryptPassword.length;
    const strengthLevel = useMemo(() => {
        // Lightweight mirror of the bar's internal compute for gating (level 0-4).
        if (!aeroCryptPassword) return 0;
        let sc = Math.min(pwLen * 4, 40);
        const variety = [/[a-z]/.test(aeroCryptPassword), /[A-Z]/.test(aeroCryptPassword), /[0-9]/.test(aeroCryptPassword), /[^a-zA-Z0-9]/.test(aeroCryptPassword)].filter(Boolean).length;
        sc += variety * 10;
        if (variety >= 3 && pwLen >= 12) sc += 10;
        if (variety >= 4 && pwLen >= 16) sc += 10;
        sc = Math.max(0, Math.min(100, sc));
        return sc < 20 ? 0 : sc < 40 ? 1 : sc < 60 ? 2 : sc < 80 ? 3 : 4;
    }, [aeroCryptPassword, pwLen]);

    const requiredLen = aeroCryptDefaultSaltStrength === '256' ? 39 : 20;
    const meetsEntropy = strengthLevel === 4 && pwLen >= requiredLen;
    // Effective flag only when user explicitly opts in + attests + passes gate.
    const effectiveUseDefaultSalt = aeroCryptDefaultSalt && aeroCryptDefaultSaltAttested && meetsEntropy;
    // Checkbox itself enabled as soon as entropy floor is met (attestation revealed after).
    const canToggleDefaultSalt = meetsEntropy;

    // P3: build the overlay-binding profile fields + stash the overlay password
    // in the vault under aerocrypt_overlay_pw_<id> (mirrors stashFilenApiKey).
    // Always returns explicit values so disabling the toggle on an existing
    // profile clears the binding. The password is never written to the JSON.
    const aeroCryptOverlayFields = async (profileId: string, hadStored?: boolean, hadStoredSalt?: boolean, hadStoredKeyfile?: boolean): Promise<Partial<ServerProfile>> => {
        // No kind chosen yet means the overlay was enabled but not actively
        // configured: build no binding (fail to plaintext, never a default cipher).
        if (!aeroCryptEnabled || !overlayEligible || !aeroCryptKind) {
            return { aeroCryptOverlay: undefined, hasStoredAeroCryptPassword: false, hasStoredAeroCryptSalt: false, hasStoredAeroCryptKeyfilePath: false };
        }
        const isRclone = aeroCryptKind === 'rclone-crypt';
        let pwStored = !!hadStored;
        if (aeroCryptPassword && aeroCryptPassword.trim()) {
            pwStored = await tryStoreCredential(`aerocrypt_overlay_pw_${profileId}`, aeroCryptPassword);
        }
        // rclone-crypt salt (password2): stored in the vault like the password, never
        // in the JSON. Native AeroCrypt has no salt field (config carries the salt).
        let saltStored = isRclone ? !!hadStoredSalt : false;
        if (isRclone && aeroCryptSalt && aeroCryptSalt.trim()) {
            saltStored = await tryStoreCredential(`aerocrypt_overlay_salt_${profileId}`, aeroCryptSalt);
        }
        // AeroCrypt Tier 1 keyfile: only the PATH is stored (pointer, not secret),
        // under aerocrypt_overlay_keyfile_path_<id>. A blank field keeps the
        // stored path (same convention as the password field).
        let keyfileStored = !isRclone ? !!hadStoredKeyfile : false;
        if (!isRclone && aeroCryptKeyfilePath.trim()) {
            keyfileStored = await tryStoreCredential(`aerocrypt_overlay_keyfile_path_${profileId}`, aeroCryptKeyfilePath.trim());
        }
        return {
            aeroCryptOverlay: {
                enabled: true,
                kind: aeroCryptKind,
                withHeader: aeroCryptWithHeader,
                useDefaultSalt: effectiveUseDefaultSalt,
                // Normalize the pinned anchor (strip trailing/duplicate slashes)
                // so it persists identically to how the backend reads it. A blank
                // or "/" scope collapses to '' and falls back to the Remote Path,
                // preserving today's behavior and every existing profile (#369).
                remoteScope: resolveOverlayScope(overlaysRemotePath, quickConnectDirs.remoteDir),
                localScope: quickConnectDirs.localDir || '',
                filenameEncryption: isRclone ? aeroCryptFilenameEnc : 'standard',
                ...(isRclone ? { directoryNameEncryption: aeroCryptDirNameEnc } : {}),
                aead: 'auto',
            },
            hasStoredAeroCryptPassword: pwStored,
            hasStoredAeroCryptSalt: saltStored,
            hasStoredAeroCryptKeyfilePath: keyfileStored,
        };
    };

    // #385 follow-up: Convert and Save-as-new mint a NEW profile id, but the
    // crypt-overlay password (and rclone salt) live in the vault under the OLD
    // id and the edit form shows them blank ("leave blank to keep"). Without
    // carrying them over, the converted/duplicated profile has no stored crypt
    // secret, so maybeAutoUnlockProfileOverlay bails and the transparent overlay
    // never auto-unlocks on the next connect. Copy the secrets old id -> new id
    // and report what was carried so aeroCryptOverlayFields marks hasStored*
    // correctly. Best-effort: a re-typed password in the form still wins (it
    // overwrites the migrated one under the new id in aeroCryptOverlayFields).
    const migrateCryptCredentials = async (
        oldId: string | null | undefined,
        newId: string,
    ): Promise<{ hadStored: boolean; hadStoredSalt: boolean; hadStoredKeyfile: boolean }> => {
        let hadStored = false;
        let hadStoredSalt = false;
        let hadStoredKeyfile = false;
        if (!oldId || oldId === newId) return { hadStored, hadStoredSalt, hadStoredKeyfile };
        try {
            const pw = await invoke<string>('get_credential', { account: `aerocrypt_overlay_pw_${oldId}` }).catch(() => '');
            if (pw) hadStored = await tryStoreCredential(`aerocrypt_overlay_pw_${newId}`, pw);
            const salt = await invoke<string>('get_credential', { account: `aerocrypt_overlay_salt_${oldId}` }).catch(() => '');
            if (salt) hadStoredSalt = await tryStoreCredential(`aerocrypt_overlay_salt_${newId}`, salt);
            const kf = await invoke<string>('get_credential', { account: `aerocrypt_overlay_keyfile_path_${oldId}` }).catch(() => '');
            if (kf) hadStoredKeyfile = await tryStoreCredential(`aerocrypt_overlay_keyfile_path_${newId}`, kf);
        } catch { /* best-effort; a missing secret just leaves the field to re-enter */ }
        return { hadStored, hadStoredSalt, hadStoredKeyfile };
    };

    // Tier 1 keyfile pickers. Choose re-points to an existing file; Generate
    // creates a fresh transfer-safe keyfile via the crypt_generate_keyfile
    // command (refuses overwrite, 0600 on unix) and shows the back-it-up
    // warning: losing the keyfile makes its vaults unopenable.
    const handleChooseKeyfile = async () => {
        try {
            const picked = await open({ multiple: false });
            if (typeof picked === 'string' && picked) {
                setAeroCryptKeyfilePath(picked);
                setKeyfileJustGenerated(false);
                setKeyfileError(null);
            }
        } catch {
            // Dialog cancelled or unavailable: keep the current selection.
        }
    };
    const handleGenerateKeyfile = async () => {
        try {
            const target = await save({ defaultPath: 'aeroftp.keyfile' });
            if (!target) return;
            await invoke('crypt_generate_keyfile', { path: target });
            setAeroCryptKeyfilePath(target);
            setKeyfileJustGenerated(true);
            setKeyfileError(null);
        } catch (err) {
            setKeyfileError(String(err));
        }
    };

    // Resolved label of the active target mode (for the "Convert to X"
    // button). Falls back to the protocol string when the active mode
    // cannot be resolved.
    const targetModeLabel = useMemo(() => {
        if (!modeChanged || !protocol) return '';
        const currentProviderId = selectedProviderId || connectionParams.providerId || undefined;
        const group = findActiveModeGroup(currentProviderId, protocol);
        if (!group) return protocol;
        const mode = findActiveMode(group, currentProviderId, protocol);
        return mode?.label || protocol;
    }, [modeChanged, protocol, selectedProviderId, connectionParams.providerId]);

    const normalizeEndpointForDuplicate = (value?: string) => {
        const raw = (value || '').trim();
        if (!raw) return '';
        try {
            const parsed = new URL(raw.includes('://') ? raw : `https://${raw}`);
            const pathname = parsed.pathname.replace(/\/+$/, '').toLowerCase();
            return `${parsed.protocol.toLowerCase()}//${parsed.host.toLowerCase()}${pathname}`;
        } catch {
            return raw.replace(/\/+$/, '').toLowerCase();
        }
    };

    const findDuplicateProfile = (
        profiles: ServerProfile[],
        candidateName: string,
        candidateHost: string,
        candidateUsername?: string,
        excludeId?: string | null,
    ) => {
        const nameKey = candidateName.trim().toLowerCase();
        const hostKey = normalizeEndpointForDuplicate(candidateHost);
        const userKey = (candidateUsername || '').trim().toLowerCase();
        return profiles.find((profile) => {
            if (excludeId && profile.id === excludeId) return false;
            const profileName = (profile.name || '').trim().toLowerCase();
            const profileHost = normalizeEndpointForDuplicate(profile.host);
            const profileUser = (profile.username || '').trim().toLowerCase();
            return profileName === nameKey || (profileHost === hostKey && profileUser === userKey);
        });
    };

    // Save the current connection to saved servers (or update existing)
    const saveToServers = async () => {
        // If editing an existing profile (and not creating a copy), name/saveConnection might be implicit
        if (!protocol) return;

        // #322: the crypt-overlay password is set-once; block a save where the
        // freshly-typed password and its confirm disagree (a typo would lock the
        // encrypted blobs forever). The save button is already disabled on
        // mismatch (aeroCryptConfirmMismatch); this is defense-in-depth.
        if (aeroCryptConfirmMismatch) return;

        // #369: block save if the overlays remote path is invalid. The field
        // already shows the translated error and the button is disabled while
        // invalid; this is defense-in-depth (same pattern as the password confirm).
        if (overlaysRemotePathError) return;

        // #128: saving a Quick Connect profile (typically to add the Filen API
        // key so a 2FA login is no longer needed) must cancel any pending
        // saved-secret 2FA auto-retry, otherwise the green bottom-right countdown
        // popup keeps ticking and fires a stale TOTP reconnect. The countdown
        // state lives in App; signal it to clear (connectToFtp already does the
        // same for a manual connect, #128 item E).
        window.dispatchEvent(new CustomEvent('aeroftp-cancel-totp-autoretry'));

        const normalizedParams = protocol === 'mtp'
            ? {
                ...connectionParams,
                // host is a human label (model); connect uses deviceFingerprint, not host.
                server: connectionParams.server || mtpFingerprint?.model || connectionName || 'MTP device',
                port: 0,
                username: '',
                password: '',
                providerId: connectionParams.providerId || selectedProviderId || 'mtp-portable',
            }
            : protocol === 'uploadcare'
            ? {
                ...connectionParams,
                server: connectionParams.server || 'api.uploadcare.com',
                port: connectionParams.port || 443,
                providerId: connectionParams.providerId || 'uploadcare',
            }
            : protocol === 'imagekit'
            ? {
                ...connectionParams,
                server: connectionParams.server || 'api.imagekit.io',
                port: connectionParams.port || 443,
                providerId: connectionParams.providerId || 'imagekit',
            }
            : protocol === 'cloudinary'
            ? {
                ...connectionParams,
                server: connectionParams.server || 'api.cloudinary.com',
                port: connectionParams.port || 443,
                providerId: connectionParams.providerId || 'cloudinary',
            }
            : protocol === 'filelu'
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
            : protocol === 'backblaze'
                ? {
                    ...connectionParams,
                    server: connectionParams.server || 'api.backblazeb2.com',
                    port: connectionParams.port || 443,
                }
            : selectedProvider?.defaults?.server && !connectionParams.server
                ? {
                    ...connectionParams,
                    server: selectedProvider.defaults.server,
                    port: connectionParams.port || selectedProvider.defaults.port || getDefaultPort(protocol),
                }
            : connectionParams;

        // MTP save requires a fingerprint (from Detect or hydrated edit profile).
        if (protocol === 'mtp' && !mtpFingerprint?.canonical) return;

        const optionsToSave = protocol === 'mega'
            ? normalizeMegaOptions(connectionParams.options)
            : { ...connectionParams.options };
        // Persist default tlsMode for FTP/FTPS so saved servers show correct badge
        if ((protocol === 'ftp' || protocol === 'ftps') && !optionsToSave.tlsMode) {
            optionsToSave.tlsMode = protocol === 'ftps' ? 'implicit' : 'explicit';
        }
        // TOTP 2FA codes are single-use and rotate every 30 seconds, so they
        // must never be stored in the saved profile. Re-using yesterday's code
        // tomorrow would always fail with "Wrong Two Factor Authentication
        // code" (issue #128). The user re-enters it on every reconnect, the
        // same way Filen / Internxt / MEGA web clients ask for it.
        if ('two_factor_code' in optionsToSave) {
            delete optionsToSave.two_factor_code;
        }
        // The one-time STS MFA token code is single-use too (issue #301): never
        // persist it; the user re-enters it on every reconnect.
        if ('roleMfaTokenCode' in optionsToSave) {
            delete optionsToSave.roleMfaTokenCode;
        }

        const existingServers = await loadSavedServerProfiles();

        if (editingProfileId) {
            const credentialStored = await tryStoreCredential(`server_${editingProfileId}`, connectionParams.password);
            const prevProfile = existingServers.find((s) => s.id === editingProfileId);
            const filenKeyStored = await stashFilenApiKey(editingProfileId, optionsToSave, prevProfile?.hasStoredFilenApiKey);
            const editedName = connectionName || prevProfile?.name || normalizedParams.server || protocol;
            const duplicate = findDuplicateProfile(
                existingServers,
                editedName,
                normalizedParams.server,
                normalizedParams.username,
                editingProfileId,
            );
            if (duplicate) {
                // The duplicate is recorded in the Activity Log only. A blocking/
                // flashing alert on save was pure friction (it appeared for a flash
                // during a legitimate save, e.g. duplicating + re-pathing a profile).
                const dedupKey = getStorageDedupKey({
                    id: editingProfileId,
                    name: editedName,
                    host: normalizedParams.server,
                    port: normalizedParams.port || getDefaultPort(protocol),
                    username: normalizedParams.username,
                    protocol: protocol as ProviderType,
                    providerId: selectedProviderId,
                } as ServerProfile);
                logActivity(
                    'PROFILE_DUPLICATE',
                    `Duplicate profile detected: "${editedName}" overlaps with "${duplicate.name}"`,
                    'success',
                    `dedupKey=${dedupKey}`,
                );
            }

            const aeroFieldsEdit = await aeroCryptOverlayFields(editingProfileId, prevProfile?.hasStoredAeroCryptPassword, prevProfile?.hasStoredAeroCryptSalt, prevProfile?.hasStoredAeroCryptKeyfilePath);
            const savedPortEdit = protocol === 'mtp' ? 0 : (normalizedParams.port || getDefaultPort(protocol));
            const updatedServers = existingServers.map((s: ServerProfile) => {
                if (s.id === editingProfileId) {
                    return {
                        ...s,
                        name: editedName,
                        host: normalizedParams.server,
                        port: savedPortEdit,
                        username: protocol === 'mtp' ? '' : normalizedParams.username,
                        hasStoredCredential: protocol === 'mtp'
                            ? false
                            : (credentialStored || (s.hasStoredCredential && !connectionParams.password)),
                        hasStoredFilenApiKey: filenKeyStored,
                        protocol: protocol as ProviderType,
                        options: protocol === 'mtp' ? undefined : optionsToSave,
                        initialPath: quickConnectDirs.remoteDir,
                        localInitialPath: quickConnectDirs.localDir,
                        persistModeCredentials: protocol === 'mtp' ? false : (persistModeCredentials && inModeGroup),
                        providerId: selectedProviderId
                            || s.providerId
                            || (protocol === 'mtp' ? (connectionParams.providerId || 'mtp-portable')
                                : protocol === 'swift' ? 'blomp'
                                : protocol === 'mega' ? 'mega'
                                : undefined),
                        customIconUrl: customIconForSave !== undefined ? customIconForSave : s.customIconUrl,
                        ...(protocol === 'mtp' && mtpFingerprint ? { deviceFingerprint: mtpFingerprint } : {}),
                        ...(protocol === 'mtp' ? {} : aeroFieldsEdit),
                    };
                }
                return s;
            });

            await storeSavedServerProfiles(updatedServers).catch(() => { });
            await syncPersistedModeCredentials(editingProfileId);
            setSavedServersUpdate(Date.now());
            const savedServer = updatedServers.find((s) => s.id === editingProfileId);
            if (savedServer) {
                logActivity(
                    'PROFILE_SAVE',
                    `Profile updated: "${savedServer.name}"`,
                    'success',
                    `dedupKey=${getStorageDedupKey(savedServer)}`,
                );
            }
        } else if (saveConnection) {
            const newId = `srv_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
            const credentialStored = await tryStoreCredential(`server_${newId}`, connectionParams.password);
            const filenKeyStored = await stashFilenApiKey(newId, optionsToSave);
            const newName = connectionName || normalizedParams.server || protocol;
            const duplicate = findDuplicateProfile(
                existingServers,
                newName,
                normalizedParams.server,
                normalizedParams.username,
                null,
            );
            if (duplicate) {
                // Activity-Log only (no flashing alert on save); see the edit path.
                const dedupKey = getStorageDedupKey({
                    id: newId,
                    name: newName,
                    host: normalizedParams.server,
                    port: normalizedParams.port || getDefaultPort(protocol),
                    username: normalizedParams.username,
                    protocol: protocol as ProviderType,
                    providerId: selectedProviderId,
                } as ServerProfile);
                logActivity(
                    'PROFILE_DUPLICATE',
                    `Duplicate profile detected: "${newName}" overlaps with "${duplicate.name}"`,
                    'success',
                    `dedupKey=${dedupKey}`,
                );
            }

            const aeroFieldsNew = protocol === 'mtp' ? {} : await aeroCryptOverlayFields(newId);
            const newServer: ServerProfile = {
                id: newId,
                name: newName,
                host: normalizedParams.server,
                port: protocol === 'mtp' ? 0 : (normalizedParams.port || getDefaultPort(protocol)),
                username: protocol === 'mtp' ? '' : normalizedParams.username,
                hasStoredCredential: protocol === 'mtp' ? false : credentialStored,
                hasStoredFilenApiKey: filenKeyStored,
                protocol: protocol as ProviderType,
                initialPath: quickConnectDirs.remoteDir,
                localInitialPath: quickConnectDirs.localDir,
                options: protocol === 'mtp' ? undefined : optionsToSave,
                persistModeCredentials: protocol === 'mtp' ? false : (persistModeCredentials && inModeGroup),
                providerId: selectedProviderId
                    || (protocol === 'mtp' ? (connectionParams.providerId || 'mtp-portable')
                        : protocol === 'swift' ? 'blomp'
                        : protocol === 'mega' ? 'mega'
                        : undefined),
                customIconUrl: customIconForSave,
                ...(protocol === 'mtp' && mtpFingerprint ? { deviceFingerprint: mtpFingerprint } : {}),
                ...aeroFieldsNew,
            };

            const newServers = [...existingServers, newServer];
            await storeSavedServerProfiles(newServers).catch(() => { });
            await syncPersistedModeCredentials(newId);
            setSavedServersUpdate(Date.now());
            logActivity(
                'PROFILE_SAVE',
                `Profile saved: "${newServer.name}"`,
                'success',
                `dedupKey=${getStorageDedupKey(newServer)}`,
            );
        }
    };

    // True when this provider/protocol ships a built-in preset logo (Filen,
    // OneDrive, ...). The icon picker is shown for every Quick Connect page now,
    // but preset providers are restricted to the custom-icons library so they
    // cannot be assigned a *different* provider's logo (#270).
    const hasProviderLogoForSave = !!PROVIDER_LOGOS[selectedProviderId || connectionParams.protocol || ''];

    const renderIconPicker = () => {
        const proto = connectionParams.protocol || 'ftp';
        const PresetLogo = PROVIDER_LOGOS[selectedProviderId || connectionParams.protocol || ''];
        const hasIcon = !!customIconForSave || !!faviconForSave || !!PresetLogo;
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
                            ) : PresetLogo ? (
                                <PresetLogo size={24} />
                            ) : (
                                <span className="font-bold text-sm">{letter}</span>
                            )}
                        </div>
                        <button
                            type="button"
                            onClick={() => setShowIconPicker(true)}
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

    // #215 (Ehud redesign): compact profile icon as a clickable avatar, used in
    // the two-column forms where the icon sits immediately left of the name.
    const renderProfileIconButton = () => {
        const proto = connectionParams.protocol || 'ftp';
        const PresetLogo = PROVIDER_LOGOS[selectedProviderId || connectionParams.protocol || ''];
        const hasIcon = !!customIconForSave || !!faviconForSave || !!PresetLogo;
        const letter = (connectionName || connectionParams.server || '?').charAt(0).toUpperCase();
        return (
            <button
                type="button"
                onClick={() => setShowIconPicker(true)}
                title={t('settings.chooseIcon')}
                className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center transition-colors hover:ring-2 hover:ring-blue-500/40 ${hasIcon ? 'bg-white dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${PROTOCOL_COLORS[proto] || PROTOCOL_COLORS.ftp} text-white`}`}
            >
                {customIconForSave ? (
                    <img src={customIconForSave} alt="" className="w-6 h-6 rounded object-contain" />
                ) : faviconForSave ? (
                    <img src={faviconForSave} alt="" className="w-6 h-6 rounded object-contain" />
                ) : PresetLogo ? (
                    <PresetLogo size={24} />
                ) : (
                    <span className="font-bold text-sm">{letter}</span>
                )}
            </button>
        );
    };

    // Handle the main action button
    const handleConnectAndSave = async () => {
        // #215 follow-up: a local-bridge mode whose helper app is confidently
        // not installed (🔴) cannot connect. Defense-in-depth beyond the
        // disabled button.
        if (bridgeSaveBlocked) return;
        // Store PAT in vault for future connections (if GitHub PAT mode)
        if (connectionParams.options?.githubAuthMode === 'pat' && connectionParams.password) {
            invoke('github_store_pat', { pat: connectionParams.password }).catch(() => {});
        }

        if (editingProfileId) {
            // Edit mode: save changes and reset form
            await saveToServers();
            setEditingProfileId(null);
            editingProfileIdRef.current = null;
            setOriginalEditMode(null);
            setConnectionName('');
            setSaveConnection(false);
            setPersistModeCredentials(false);
            modeCredentialSnapshotsRef.current = {};
            setMtpDevices([]);
            setMtpSelectedDeviceId(null);
            setMtpFingerprint(undefined);
            setMtpDetectError(null);
            onConnectionParamsChange({ server: '', username: '', password: '' });
            onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
            onFormSaved?.();
        } else if (saveConnection) {
            // Save mode: only save, user connects from saved servers list
            await saveToServers();
            setConnectionName('');
            setSaveConnection(false);
            setPersistModeCredentials(false);
            modeCredentialSnapshotsRef.current = {};
            setMtpDevices([]);
            setMtpSelectedDeviceId(null);
            setMtpFingerprint(undefined);
            setMtpDetectError(null);
            onConnectionParamsChange({ server: '', username: '', password: '' });
            onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
            onFormSaved?.();
        } else {
            // Connect mode: just connect without saving
            onConnect();
        }
    };

    /**
     * Edit mode helper for 2FA-aware providers (MEGA, Filen, Internxt).
     * Saves the profile (TOTP stripped from the persisted options because it
     * is single-use and rotates every 30s) and immediately triggers a connect
     * with `connectionParams` still in memory, so the freshly-typed TOTP
     * reaches the backend on this attempt and the server validates 2FA
     * properly. Without this, "Save" + click on the saved card connects
     * without the TOTP and either resumes the old session or fails with
     * E_MFAREQUIRED. Issue #128.
     */
    const handleSaveAndConnect = async () => {
        if (editingProfileId) {
            await saveToServers();
            // Don't reset the form: onConnect drives the route change that
            // closes the panel; resetting here would race connectionParams
            // away before the connect call can read the TOTP.
        }
        onConnect();
    };

    // Persist a name / local-path (and remote path / icon) edit to an existing
    // OAuth or API profile WITHOUT re-running the OAuth sign-in. The shared footer
    // Save is hidden for OAuth providers (renderRightColumn hideSaveButton), so
    // without this the only way to save a rename was to sign in again. Mirrors the
    // OAuth-shaped merge in the sign-in success handler (keeps host/username from
    // the stored profile, never clobbers them from empty connectionParams) but
    // does not connect.
    const handleOAuthMetadataSave = async () => {
        if (!editingProfileId) return;
        const existingServers = await loadSavedServerProfiles();
        const prevProfile = existingServers.find((s) => s.id === editingProfileId);
        if (!prevProfile) return;
        const saveName = connectionName || prevProfile.name;
        const overlayFields = await aeroCryptOverlayFields(
            editingProfileId,
            prevProfile.hasStoredAeroCryptPassword,
            prevProfile.hasStoredAeroCryptSalt,
            prevProfile.hasStoredAeroCryptKeyfilePath,
        );
        const updated = existingServers.map((s) =>
            s.id === editingProfileId
                ? {
                    ...s,
                    name: saveName || s.name,
                    initialPath: quickConnectDirs.remoteDir || s.initialPath,
                    localInitialPath: quickConnectDirs.localDir,
                    customIconUrl: customIconForSave !== undefined ? customIconForSave : s.customIconUrl,
                    ...overlayFields,
                }
                : s,
        );
        await storeSavedServerProfiles(updated).catch(() => { });
        setSavedServersUpdate(Date.now());
        const savedServer = updated.find((s) => s.id === editingProfileId);
        if (savedServer) {
            logActivity(
                'PROFILE_SAVE',
                `Profile updated: "${savedServer.name}"`,
                'success',
                `dedupKey=${getStorageDedupKey(savedServer)}`,
            );
        }
        // Close the IntroHub form tab after saving (the renamed profile shows in My
        // Servers); on the main screen there is no tab, so exit edit and confirm.
        if (onFormSaved) {
            onFormSaved();
        } else {
            handleCancelEdit();
            setGitHubAlert({
                title: t('common.save'),
                message: saveName,
                type: 'info',
            });
        }
    };

    // Cancel the OAuth/API edit: discard form state and close the IntroHub form
    // tab (or just exit edit mode on the main screen). Paired with the edit-mode
    // Save so the footer is not a lone Save button.
    const handleOAuthCancel = () => {
        handleCancelEdit();
        onFormSaved?.();
    };

    // Whether the OAuth/API edit form has an unsaved change to a metadata field
    // (name, local path, remote path, icon) relative to the stored profile. Used
    // to keep the edit-mode Save button disabled until something actually changed.
    // An empty connection name means "keep the stored name", so it is not a change.
    const oauthEditHasChanges = (): boolean => {
        if (!editingProfileId) return false;
        const ep = servers.find((s) => s.id === editingProfileId);
        if (!ep) return false;
        return (
            (connectionName || ep.name) !== ep.name ||
            (quickConnectDirs.localDir || '') !== (ep.localInitialPath || '') ||
            (quickConnectDirs.remoteDir || '') !== (ep.initialPath || '') ||
            (customIconForSave !== undefined && customIconForSave !== ep.customIconUrl)
        );
    };

    const handleSaveAsNew = async () => {
        if (!protocol || !editingProfileId) return;
        // Validate name is different
        const existingServers = await loadSavedServerProfiles();
        const originalServer = existingServers.find((s: ServerProfile) => s.id === editingProfileId);
        const newName = connectionName || connectionParams.server || protocol;
        // When the operator switched mode in edit (issue #215), the new
        // profile is materially different (Native API vs WebDAV vs ...),
        // so reusing the same display name is fine: skip the "(Copy)" auto
        // suffix to keep the list readable. Otherwise (plain duplicate of
        // the SAME mode), append "(Copy)" so the two rows are
        // distinguishable.
        const targetMode = modeChanged ? targetModeLabel : '';
        const shouldSuffixCopy = !!originalServer && newName === originalServer.name && !modeChanged;
        if (shouldSuffixCopy) {
            // Auto-append "(Copy)" if user didn't change the name
            setConnectionName(`${newName} (${t('common.copy')})`);
        }
        const finalName = shouldSuffixCopy
            ? `${newName} (${t('common.copy')})`
            : (modeChanged && originalServer && newName === originalServer.name && targetMode
                ? `${newName} (${targetMode})`
                : newName);

        const normalizedParams = protocol === 'uploadcare'
            ? { ...connectionParams, server: connectionParams.server || 'api.uploadcare.com', port: connectionParams.port || 443, providerId: connectionParams.providerId || 'uploadcare' }
            : protocol === 'imagekit'
            ? { ...connectionParams, server: connectionParams.server || 'api.imagekit.io', port: connectionParams.port || 443, providerId: connectionParams.providerId || 'imagekit' }
            : protocol === 'cloudinary'
            ? { ...connectionParams, server: connectionParams.server || 'api.cloudinary.com', port: connectionParams.port || 443, providerId: connectionParams.providerId || 'cloudinary' }
            : protocol === 'filelu'
            ? { ...connectionParams, server: connectionParams.server || 'filelu.com', username: connectionParams.username || 'api-key', port: connectionParams.port || 443 }
            : protocol === 'opendrive'
                ? { ...connectionParams, server: connectionParams.server || 'dev.opendrive.com', port: connectionParams.port || 443 }
            : protocol === 'github' || protocol === 'gitlab'
                ? { ...connectionParams, server: connectionParams.server || '', port: connectionParams.port || 443 }
            : protocol === 'backblaze'
                ? { ...connectionParams, server: connectionParams.server || 'api.backblazeb2.com', port: connectionParams.port || 443 }
            : selectedProvider?.defaults?.server && !connectionParams.server
                ? { ...connectionParams, server: selectedProvider.defaults.server, port: connectionParams.port || selectedProvider.defaults.port || getDefaultPort(protocol) }
            : connectionParams;

        const optionsToSave = protocol === 'mega'
            ? normalizeMegaOptions(connectionParams.options)
            : { ...connectionParams.options };
        // Same TOTP-strip rule as the primary save path: never persist the
        // single-use 2FA code (issue #128).
        if ('two_factor_code' in optionsToSave) {
            delete optionsToSave.two_factor_code;
        }
        // Single-use STS MFA token code is never persisted (issue #301).
        if ('roleMfaTokenCode' in optionsToSave) {
            delete optionsToSave.roleMfaTokenCode;
        }

        const newId = `srv_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
        const credentialStored = await tryStoreCredential(`server_${newId}`, connectionParams.password);
        const filenKeyStored = await stashFilenApiKey(newId, optionsToSave);
        // #385: carry the crypt overlay secrets from the edited profile to the
        // new id so the converted/duplicated profile can auto-unlock its overlay.
        const migratedCrypt = aeroCryptEnabled
            ? await migrateCryptCredentials(editingProfileId, newId)
            : { hadStored: false, hadStoredSalt: false, hadStoredKeyfile: false };
        const aeroFields = await aeroCryptOverlayFields(newId, migratedCrypt.hadStored, migratedCrypt.hadStoredSalt, migratedCrypt.hadStoredKeyfile);
        // Carry-over from the original profile when present: visual color
        // tag + favicon (sort position is handled by the insert index
        // below). Custom icon URL is already in component state via
        // `customIconForSave`, set in handleEdit.
        const newServer: ServerProfile = {
            id: newId,
            name: finalName,
            host: normalizedParams.server,
            port: normalizedParams.port || getDefaultPort(protocol),
            username: normalizedParams.username,
            hasStoredCredential: credentialStored,
            hasStoredFilenApiKey: filenKeyStored,
            protocol: protocol as ProviderType,
            initialPath: quickConnectDirs.remoteDir,
            localInitialPath: quickConnectDirs.localDir,
            options: optionsToSave,
            providerId: selectedProviderId || undefined,
            customIconUrl: customIconForSave,
            color: originalServer?.color,
            faviconUrl: faviconForSave || originalServer?.faviconUrl,
            // Carry the cached storage quota across a mode switch (issue #215):
            // every mode of a provider group points at the same account/storage,
            // so the usage + capacity stats stay valid until the next refresh.
            lastQuota: originalServer?.lastQuota,
            // Keep the per-protocol credential opt-in on the duplicate too, so
            // the checkbox state survives Save-as-new (issue #215, Ehud).
            persistModeCredentials: persistModeCredentials && inModeGroup,
            ...aeroFields,
        };

        // Issue #215: when the user switched mode in edit, insert
        // immediately AFTER the original profile (visual grouping). For a
        // plain duplicate of the same mode, append at the end (legacy
        // behavior).
        const newServers = [...existingServers];
        if (modeChanged) {
            const originalIdx = existingServers.findIndex(s => s.id === editingProfileId);
            const insertIdx = originalIdx >= 0 ? originalIdx + 1 : existingServers.length;
            newServers.splice(insertIdx, 0, newServer);
        } else {
            newServers.push(newServer);
        }
        await storeSavedServerProfiles(newServers).catch(() => { });
        // Persist the duplicate's own per-mode credential snapshots (issue
        // #215): the original keeps its own, so this only writes the new id.
        await syncPersistedModeCredentials(newId);
        // On a mode switch the copy is the same account, so carry the ⭐
        // favourite flag onto it too (issue #215). Keep the original starred as
        // well (removeOld=false): a duplicate leaves both rows in the list.
        if (modeChanged && originalServer) {
            await carryFavoriteServer(originalServer.id, newId, false);
            await carryServerGroups(originalServer.id, newId, false);
        }
        setSavedServersUpdate(Date.now());

        // Reset form
        setEditingProfileId(null);
        editingProfileIdRef.current = null;
        setOriginalEditMode(null);
        setConnectionName('');
        setSaveConnection(false);
        setOverlaysRemotePath('');
        setOverlaysRemotePathError(null);
        onConnectionParamsChange({ server: '', username: '', password: '' });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
        onFormSaved?.();
    };

    // Issue #215: "Convert profile to <mode>" — when the user is editing
    // a saved profile and switched to a different mode of the SAME
    // provider group (e.g. Koofr WebDAV -> Koofr Native API), this
    // creates the new-mode profile and removes the original, keeping the
    // original's sort position. An Undo toast restores the snapshot for
    // 10 seconds.
    const handleConvertMode = async () => {
        if (!protocol || !editingProfileId || !modeChanged) return;

        const existingServers = await loadSavedServerProfiles();
        const originalIdx = existingServers.findIndex(s => s.id === editingProfileId);
        const originalServer = originalIdx >= 0 ? existingServers[originalIdx] : null;
        if (!originalServer) return;

        // Snapshot for Undo
        const snapshotServers = existingServers.map(s => ({ ...s }));
        const previousMode = originalEditMode?.protocol || originalServer.protocol;

        // Same normalization rules as handleSaveAsNew so converted
        // profiles end up with sensible host/port defaults for providers
        // that fill them implicitly (Uploadcare / ImageKit / Cloudinary /
        // FileLu / OpenDrive / GitHub / Backblaze) or via the registry
        // preset defaults.
        const normalizedParams = protocol === 'uploadcare'
            ? { ...connectionParams, server: connectionParams.server || 'api.uploadcare.com', port: connectionParams.port || 443, providerId: connectionParams.providerId || 'uploadcare' }
            : protocol === 'imagekit'
            ? { ...connectionParams, server: connectionParams.server || 'api.imagekit.io', port: connectionParams.port || 443, providerId: connectionParams.providerId || 'imagekit' }
            : protocol === 'cloudinary'
            ? { ...connectionParams, server: connectionParams.server || 'api.cloudinary.com', port: connectionParams.port || 443, providerId: connectionParams.providerId || 'cloudinary' }
            : protocol === 'filelu'
            ? { ...connectionParams, server: connectionParams.server || 'filelu.com', username: connectionParams.username || 'api-key', port: connectionParams.port || 443 }
            : protocol === 'opendrive'
                ? { ...connectionParams, server: connectionParams.server || 'dev.opendrive.com', port: connectionParams.port || 443 }
            : protocol === 'github' || protocol === 'gitlab'
                ? { ...connectionParams, server: connectionParams.server || '', port: connectionParams.port || 443 }
            : protocol === 'backblaze'
                ? { ...connectionParams, server: connectionParams.server || 'api.backblazeb2.com', port: connectionParams.port || 443 }
            : selectedProvider?.defaults?.server && !connectionParams.server
                ? { ...connectionParams, server: selectedProvider.defaults.server, port: connectionParams.port || selectedProvider.defaults.port || getDefaultPort(protocol) }
            : connectionParams;

        const optionsToSave = protocol === 'mega'
            ? normalizeMegaOptions(connectionParams.options)
            : { ...connectionParams.options };
        if ('two_factor_code' in optionsToSave) {
            delete optionsToSave.two_factor_code;
        }
        // Single-use STS MFA token code is never persisted (issue #301).
        if ('roleMfaTokenCode' in optionsToSave) {
            delete optionsToSave.roleMfaTokenCode;
        }

        const finalName = (connectionName || originalServer.name).trim() || originalServer.name;

        const newId = `srv_${Date.now()}_${Math.random().toString(36).slice(2, 11)}`;
        const credentialStored = await tryStoreCredential(`server_${newId}`, connectionParams.password);
        const filenKeyStored = await stashFilenApiKey(newId, optionsToSave);
        // #385: carry the crypt overlay secrets from the edited profile to the
        // new id so the converted/duplicated profile can auto-unlock its overlay.
        const migratedCrypt = aeroCryptEnabled
            ? await migrateCryptCredentials(editingProfileId, newId)
            : { hadStored: false, hadStoredSalt: false, hadStoredKeyfile: false };
        const aeroFields = await aeroCryptOverlayFields(newId, migratedCrypt.hadStored, migratedCrypt.hadStoredSalt, migratedCrypt.hadStoredKeyfile);
        const newServer: ServerProfile = {
            id: newId,
            name: finalName,
            host: normalizedParams.server,
            port: normalizedParams.port || getDefaultPort(protocol),
            username: normalizedParams.username,
            hasStoredCredential: credentialStored,
            hasStoredFilenApiKey: filenKeyStored,
            protocol: protocol as ProviderType,
            initialPath: quickConnectDirs.remoteDir,
            localInitialPath: quickConnectDirs.localDir,
            options: optionsToSave,
            providerId: selectedProviderId || undefined,
            customIconUrl: customIconForSave || originalServer.customIconUrl,
            color: originalServer.color,
            faviconUrl: faviconForSave || originalServer.faviconUrl,
            // Carry the cached storage quota: a convert stays on the same
            // account/storage, so usage + capacity remain valid (issue #215).
            lastQuota: originalServer.lastQuota,
            // Carry the per-protocol credential opt-in across the convert
            // (issue #215, Ehud): the checkbox stayed ticked, so the converted
            // profile must keep remembering every protocol's credentials
            // instead of silently dropping the flag.
            persistModeCredentials: persistModeCredentials && inModeGroup,
            ...aeroFields,
        };

        // Replace in slot: remove original, insert new at the same index
        const newServers = [...existingServers];
        newServers.splice(originalIdx, 1, newServer);
        await storeSavedServerProfiles(newServers).catch(() => { });
        // Migrate the persisted per-mode credential snapshots from the old id
        // to the converted one (or clear them when the opt-in is off), so a
        // later mode switch on the converted profile restores without asking.
        await syncPersistedModeCredentials(newId);
        if (newId !== originalServer.id) {
            await deleteModeCredentials(originalServer.id);
        }
        // Move the ⭐ favourite flag from the deleted original to the new
        // profile (issue #215): convert replaces in place, so remove the old id.
        await carryFavoriteServer(originalServer.id, newId, true);
        await carryServerGroups(originalServer.id, newId, true);
        setSavedServersUpdate(Date.now());

        // 10s Undo toast (via window event so we don't need to plumb a
        // toast handle through props; App.tsx listens for
        // `aeroftp-toast`).
        window.dispatchEvent(new CustomEvent('aeroftp-toast', {
            detail: {
                type: 'success',
                title: t('connection.profileConverted'),
                message: `${originalServer.name}: ${previousMode} → ${targetModeLabel}`,
                duration: 10000,
                action: {
                    label: t('connection.undo'),
                    onClick: async () => {
                        await storeSavedServerProfiles(snapshotServers).catch(() => { });
                        // Drop the converted profile's per-mode credential
                        // snapshots so the discarded new id leaves nothing in
                        // the vault (issue #215).
                        await deleteModeCredentials(newId);
                        // Reverse the favourite move so the restored original
                        // keeps its ⭐ and the discarded new id doesn't dangle.
                        await carryFavoriteServer(newId, originalServer.id, true);
                        await carryServerGroups(newId, originalServer.id, true);
                        setSavedServersUpdate(Date.now());
                        window.dispatchEvent(new CustomEvent('aeroftp-toast', {
                            detail: {
                                type: 'info',
                                title: t('connection.convertUndone'),
                                duration: 3000,
                            },
                        }));
                    },
                },
            },
        }));

        // Reset form
        setEditingProfileId(null);
        editingProfileIdRef.current = null;
        setOriginalEditMode(null);
        setConnectionName('');
        setSaveConnection(false);
        setOverlaysRemotePath('');
        setOverlaysRemotePathError(null);
        onConnectionParamsChange({ server: '', username: '', password: '' });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
        onFormSaved?.();
    };

    const handleEdit = async (profile: ServerProfile) => {
        // Close protocol selector dropdown so the form becomes visible
        setIsProtocolSelectorOpen(false);

        // Reset form FIRST to clear previous server's data immediately
        // This prevents stale data from showing when switching between servers
        // Drop any per-mode credential snapshots from a previous edit (#215).
        modeCredentialSnapshotsRef.current = {};
        setPersistModeCredentials(!!profile.persistModeCredentials);
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

        // Legacy-profile migration: when the saved profile has a
        // providerId pointing at a known registry preset whose protocol
        // value has since changed, trust the registry. This makes
        // ProtocolFields render the right fields and lets
        // ProviderModeTabs surface the provider chip strip in edit mode.
        // Filen native profiles have no providerId so they fall through
        // and keep their saved protocol as-is.
        // Native-API profiles keep their saved protocol: a WebDAV registry
        // preset sharing the providerId (Koofr/OpenDrive) must not flip the
        // edit form to WebDAV fields (issue #213).
        let effectiveProtocol = profile.protocol || 'ftp';
        if (profile.providerId && !isNativeApiProtocol(profile.protocol)) {
            const presetProtocol = getProviderById(profile.providerId)?.protocol;
            if (presetProtocol && presetProtocol !== effectiveProtocol) {
                effectiveProtocol = presetProtocol;
            }
        }

        // Snapshot the mode at edit-open time so we can detect mode-group
        // switches in the footer (issue #215).
        setOriginalEditMode({
            protocol: effectiveProtocol,
            providerId: profile.providerId,
        });

        // Immediately update form with new profile data (password empty initially)
        onConnectionParamsChange({
            server: profile.host,
            port: profile.port,
            username: profile.username,
            password: profile.password || '', // Set immediately, will be updated if stored
            protocol: effectiveProtocol,
            providerId: profile.providerId,
            options: profileOptions
        });

        onQuickConnectDirsChange({
            remoteDir: profile.initialPath || '',
            localDir: profile.localInitialPath || ''
        });

        // P3: hydrate the AeroCrypt overlay binding into the form. The password
        // is never prefilled (it lives in the vault under aerocrypt_overlay_pw_<id>).
        const overlayBinding = profile.aeroCryptOverlay;
        setAeroCryptEnabled(!!overlayBinding?.enabled);
        // Keep the overlays section expanded on edit too (default-open, Ehud 2026-06-28).
        setOverlaysExpanded(true);
        // C-EDIT-GUARD: lock kind + credential edits when a binding already exists.
        setOverlayBindingLocked(!!overlayBinding?.enabled);
        // Only seed a kind when a binding already exists; a binding-less profile
        // starts unselected so the user must actively choose on enable.
        setAeroCryptKind(overlayBinding?.enabled ? (overlayBinding.kind === 'rclone-crypt' ? 'rclone-crypt' : 'aerocrypt') : null);
        setAeroCryptPassword('');
        setAeroCryptConfirm('');
        // rclone-crypt interop options (P3.3b). Salt is never prefilled (vault).
        setAeroCryptSalt('');
        setAeroCryptFilenameEnc(overlayBinding?.filenameEncryption || 'standard');
        setAeroCryptDirNameEnc(overlayBinding?.directoryNameEncryption ?? true);
        setAeroCryptWithHeader(!!overlayBinding?.withHeader);
        setAeroCryptDefaultSalt(!!overlayBinding?.useDefaultSalt);
        // P3 follow-up (#369): hydrate the pinned overlaysRemotePath, but only if
        // it differs from the profile's Remote Path; otherwise keep '' which means
        // "same as Remote Path" (so an unchanged profile round-trips unchanged).
        {
            const savedScope = overlayBinding?.enabled ? (overlayBinding.remoteScope || '') : '';
            const profileRemote = profile.initialPath || '';
            setOverlaysRemotePath(savedScope && savedScope !== profileRemote ? savedScope : '');
            setOverlaysRemotePathError(null);
        }
        // Tier 1 keyfile PATH: a pointer, not a secret, so unlike the password
        // it IS hydrated for display and stays re-pointable (hydrated async
        // below, race-guarded like the other vault reads).
        setAeroCryptKeyfilePath('');
        setKeyfileJustGenerated(false);
        setKeyfileError(null);

        // Then hydrate vaulted secrets (password + Filen API key) asynchronously.
        // Both reads target the same profile id; we resolve them up front and apply
        // ONE combined snapshot. onConnectionParamsChange is `setConnectionParams`
        // (a full-snapshot setter, not a functional updater), so two independent
        // setter calls would clobber each other — the second to resolve would drop
        // the first's value (issue #215).
        const targetProfileId = profile.id;
        let hydratedPassword = profile.password || '';
        let hydratedOptions = profileOptions;

        if (!profile.password && profile.hasStoredCredential) {
            try {
                const storedPassword = await invoke<string>('get_credential', { account: `server_${targetProfileId}` });
                if (storedPassword) hydratedPassword = storedPassword;
            } catch {
                // Credential not found, password stays empty
            }
        }

        // #215: reload the vaulted Filen API key into the form on edit so it
        // behaves like the password and the 2FA secret and survives switching to
        // WebDAV/S3 and back. #230 moved the key to filen_api_key_<id> and it was
        // previously only read back at connect time, never on edit — so the field
        // opened blank and the protocol-switch stash carried nothing.
        if (profile.hasStoredFilenApiKey) {
            try {
                const storedFilenKey = await invoke<string>('get_credential', { account: `filen_api_key_${targetProfileId}` });
                if (storedFilenKey) {
                    hydratedOptions = { ...hydratedOptions, filen_api_key: storedFilenKey };
                }
            } catch {
                // Key not retrievable: field stays blank, the stored-key hint still applies.
            }
        }

        // Apply once, only if a secret actually hydrated and we're still editing
        // the same profile (guards the same race the password load always guarded:
        // the user may switch to another server mid-fetch).
        if (editingProfileIdRef.current === targetProfileId
            && (hydratedPassword !== (profile.password || '') || hydratedOptions !== profileOptions)) {
            onConnectionParamsChange({
                server: profile.host,
                port: profile.port,
                username: profile.username,
                password: hydratedPassword,
                protocol: effectiveProtocol,
                providerId: profile.providerId,
                options: hydratedOptions,
            });
        }

        // Tier 1: hydrate the stored keyfile PATH for display (it is a pointer,
        // not a secret), so the operator can see and re-point it after an
        // import. Race-guarded like the password load above.
        if (profile.hasStoredAeroCryptKeyfilePath) {
            try {
                const storedKeyfile = await invoke<string>('get_credential', { account: `aerocrypt_overlay_keyfile_path_${targetProfileId}` });
                if (storedKeyfile && editingProfileIdRef.current === targetProfileId) {
                    setAeroCryptKeyfilePath(storedKeyfile);
                }
            } catch {
                // Path not retrievable: field stays blank, re-point as needed.
            }
        }

        // Issue #215: when the profile opted into persistent per-mode
        // credentials, hydrate the in-memory snapshot map from the vault so a
        // tab switch in handleProtocolChange restores each mode's saved
        // credentials. Race-guarded like the password load above.
        if (profile.persistModeCredentials) {
            try {
                const persisted = await loadModeCredentials(targetProfileId);
                if (editingProfileIdRef.current === targetProfileId) {
                    // Re-key any legacy-format snapshot so pre-rekey saves
                    // still restore on the first mode switch.
                    const group = findActiveModeGroup(profile.providerId, profile.protocol);
                    modeCredentialSnapshotsRef.current = migrateLegacyModeKeys(persisted, group);
                }
            } catch {
                // No persisted modes: in-session snapshots only.
            }
        }
    };

    const handleCancelEdit = () => {
        setEditingProfileId(null);
        editingProfileIdRef.current = null;
        setOriginalEditMode(null);
        setConnectionName('');
        setCustomIconForSave(undefined);
        setFaviconForSave(undefined);
        setSaveConnection(false);
        setPersistModeCredentials(false);
        setAeroCryptEnabled(false);
        setOverlayBindingLocked(false);
        setAeroCryptKind(null);
        setAeroCryptPassword('');
        setAeroCryptConfirm('');
        setAeroCryptSalt('');
        setAeroCryptFilenameEnc('standard');
        setAeroCryptDirNameEnc(true);
        setAeroCryptWithHeader(false);
        setAeroCryptDefaultSalt(false);
        setAeroCryptDefaultSaltStrength('128');
        setAeroCryptDefaultSaltAttested(false);
        setAeroCryptKeyfilePath('');
        setKeyfileJustGenerated(false);
        setKeyfileError(null);
        setOverlaysRemotePath('');
        setOverlaysRemotePathError(null);
        modeCredentialSnapshotsRef.current = {};
        // Reset params
        onConnectionParamsChange({ ...connectionParams, server: '', username: '', password: '', options: {} });
        onQuickConnectDirsChange({ remoteDir: '', localDir: '' });
    };

    const handleBrowseLocalDir = async () => {
        try {
            // Seed the chooser at the current local dir when it exists, or the
            // nearest existing ancestor: an imported/stale path must never reach
            // the native dialog (Fix G).
            const selected = await open({ directory: true, multiple: false, defaultPath: await safePickerStartDir(quickConnectDirs.localDir), title: t('browser.local') });
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

        // Issue #215: when editing a saved profile and switching to a
        // different mode of the SAME provider group (e.g. Koofr WebDAV ->
        // Koofr Native API, FileLu API -> FileLu S3), keep edit mode and
        // update protocol/providerId/options so the form re-renders with
        // mode-specific fields. The user then chooses Save-as-new or
        // Convert in the footer (modeChanged === true).
        if (editingProfileId && effectiveOldProtocol) {
            const oldProviderId = selectedProviderId || connectionParams.providerId || undefined;
            const oldGroup = findActiveModeGroup(oldProviderId, effectiveOldProtocol);
            const newGroup = findActiveModeGroup(providerId, newProtocol);
            if (oldGroup && oldGroup === newGroup) {
                // Stash the credentials of the mode we are leaving, then look
                // up the target mode's stash so a return visit restores exactly
                // what was typed there (incl. options-level secrets), instead
                // of wiping the API key / 2FA secret (#215). On a first visit
                // there is no stash, so the original carry-over behaviour holds.
                const oldKey = modeStashKey(oldProviderId, effectiveOldProtocol);
                modeCredentialSnapshotsRef.current[oldKey] = {
                    username: connectionParams.username,
                    password: connectionParams.password,
                    server: connectionParams.server,
                    port: connectionParams.port,
                    options: connectionParams.options ? { ...connectionParams.options } : undefined,
                };
                const newKey = modeStashKey(providerId, newProtocol);
                const restored = modeCredentialSnapshotsRef.current[newKey];
                // Issue #215 (Ehud, security): the S3 "Secret Access Key" field
                // is the shared `password` slot relabeled, and the API "password"
                // is the same slot. Carrying it across modes leaked the Filen/
                // MEGA/FileLu account password into the S3 Secret Access Key on
                // the FIRST visit to a tab (no stash yet). Only groups that
                // authenticate every mode with the SAME credentials may carry
                // creds over; groups with structurally different secrets must
                // start blank on a never-visited tab. A restored stash (return
                // visit) still wins, so the user's own typed keys come back.
                const switchCreds = resolveModeSwitchCredentials(newGroup, restored, {
                    username: connectionParams.username,
                    password: connectionParams.password,
                });
                if (providerId) {
                    const provider = getProviderById(providerId);
                    if (provider) {
                        setSelectedProviderId(providerId);
                        onConnectionParamsChange({
                            ...connectionParams,
                            protocol: newProtocol,
                            port: restored?.port ?? (provider.defaults?.port || getDefaultPort(newProtocol)),
                            providerId: provider.id,
                            // Adopt the target preset's canonical endpoint. The
                            // modes in a group share an account but have
                            // DIFFERENT hosts (OpenDrive API dev.opendrive.com
                            // vs WebDAV webdav.opendrive.com), so carrying over
                            // the old server connected WebDAV to the API host
                            // and returned 404. Prefer the preset default; fall
                            // back to the current server only when the preset
                            // has none. A restored stash already holds this
                            // mode's own host, so it wins.
                            server: restored ? restored.server : (provider.defaults?.server || connectionParams.server || ''),
                            username: switchCreds.username,
                            password: switchCreds.password,
                            options: restored ? (restored.options ?? {}) : {
                                pathStyle: provider.defaults?.pathStyle,
                                region: provider.defaults?.region,
                                endpoint: provider.defaults?.endpoint,
                                anonymous: provider.defaults?.anonymous,
                                webdavScheme: provider.defaults?.webdavScheme,
                                bucket: provider.defaults?.bucket,
                                verifyCert: provider.defaults?.verifyCert,
                            },
                        });
                        return;
                    }
                }
                // Preset-less native mode (Koofr native, OpenDrive native,
                // Filen native): drop the providerId and switch protocol.
                setSelectedProviderId(null);
                onConnectionParamsChange({
                    ...connectionParams,
                    protocol: newProtocol,
                    providerId: undefined,
                    port: restored?.port ?? getDefaultPort(newProtocol),
                    // Reset to the protocol's canonical native host so a switch
                    // from a sibling preset (OpenDrive WebDAV at
                    // webdav.opendrive.com) does not carry a wrong host into the
                    // native API mode, which would hit the WebDAV frontend (#368).
                    // A restored stash already holds this mode's own host, so it
                    // wins; other protocols keep the current server as before.
                    server: restored
                        ? restored.server
                        : newProtocol === 'opendrive'
                            ? 'dev.opendrive.com'
                            : connectionParams.server,
                    username: switchCreds.username,
                    password: switchCreds.password,
                    options: restored ? (restored.options ?? {}) : {},
                });
                return;
            }
        }

        // Exit edit mode when changing to an incompatible protocol
        if (editingProfileId) {
            setEditingProfileId(null);
            editingProfileIdRef.current = null;
            setOriginalEditMode(null);
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
                    options: newProtocol === 'mega' ? normalizeMegaOptions() : {
                        pathStyle: provider.defaults?.pathStyle,
                        region: provider.defaults?.region,
                        endpoint: provider.defaults?.endpoint,
                        anonymous: provider.defaults?.anonymous,
                        // Propagate WebDAV scheme override (Filen Desktop, MEGAcmd, etc.)
                        // so the backend builds http://... instead of https://...
                        webdavScheme: provider.defaults?.webdavScheme,
                        bucket: provider.defaults?.bucket,
                        // Local HTTPS bridges (Filen Desktop S3) use self-signed certs
                        verifyCert: provider.defaults?.verifyCert,
                    },
                });
                onQuickConnectDirsChange({
                    remoteDir: provider.defaults?.basePath || '',
                    localDir: '',
                });
                return;
            }
        }

        const protocolDefaults: Partial<ConnectionParams> = newProtocol === 'uploadcare'
            ? { server: 'api.uploadcare.com', port: 443, providerId: 'uploadcare' }
            : newProtocol === 'imagekit'
            ? { server: 'api.imagekit.io', port: 443, providerId: 'imagekit' }
            : newProtocol === 'cloudinary'
            ? { server: 'api.cloudinary.com', port: 443, providerId: 'cloudinary' }
            : newProtocol === 'filelu'
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
            providerId: protocolDefaults.providerId,
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
                anonymous: provider.defaults?.anonymous,
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
        if (protocol === 'peer') return t('aeroShare.dialog.aliasLabel');
        if (protocol === 's3') return t('connection.accessKeyId');
        if (protocol === 'azure') return t('connection.azureAccountName');
        if (protocol === 'github') return t('github.ownerRepo');
        if (protocol === 'gitlab') return 'Project Path';
        // A selected preset can relabel the shared username field (Koofr,
        // OpenDrive, pCloud, Yandex WebDAV all declare "Email"), so a provider's
        // WebDAV mode reads the same label as its dedicated native form instead
        // of the generic "Username" (#369 label consistency). Mirrors how the
        // placeholder already prefers the preset field below.
        const presetLabel = selectedProvider?.fields?.find((f) => f.key === 'username')?.label;
        if (presetLabel) return presetLabel;
        return t('connection.username');
    };

    const getUsernamePlaceholder = () => {
        // A selected preset's own field placeholder wins over the protocol
        // default: the Filen Desktop local WebDAV bridge declares "admin"
        // (issue #215), which the generic WebDAV/opendrive default below would
        // otherwise mask, hiding from the user that AeroFTP tries "admin".
        const usernameField = selectedProvider?.fields?.find((f) => f.key === 'username');
        if (usernameField?.placeholder) return usernameField.placeholder;
        if (protocol === 's3') return 'AKIAIOSFODNN7EXAMPLE';
        if (protocol === 'azure') return 'aeroftp2026';
        if (protocol === 'opendrive' || protocol === 'webdav') return 'email@example.com';
        if ((usernameField?.label || '').toLowerCase().includes('email')) return 'email@example.com';
        return t('connection.usernamePlaceholder');
    };

    const getSuggestedConnectionName = () => {
        const baseRaw = (selectedProvider?.isGeneric && protocol)
            ? protocol
            : (selectedProviderId || protocol || 'connection');
        const base = baseRaw.replace(/[_\-]+/g, ' ').trim().toLowerCase();
        const taken = new Set(
            savedProfilesForNaming
                .filter((p) => p.id !== editingProfileId)
                .map((p) => (p.name || '').trim().toLowerCase())
                .filter(Boolean)
        );
        if (!taken.has(base)) return base;
        for (let i = 2; i < 100; i += 1) {
            const candidate = `${base} ${i}`;
            if (!taken.has(candidate)) return candidate;
        }
        return base;
    };

    // Dynamic server label based on protocol.
    // For URL-based protocols (WebDAV, S3, Azure) we use "Endpoint URL" with a
    // Link2 icon; for hostname-based protocols (FTP/SFTP/FTPS) the simple
    // "Server" label fits.
    const getServerLabel = (): React.ReactNode => {
        if (protocol === 'peer') return 'AeroFTP-ID';
        if (protocol === 's3') {
            return <span className="inline-flex items-center gap-1.5"><Link2 size={12} className="text-gray-400" />{t('protocol.s3Endpoint')}</span>;
        }
        if (protocol === 'azure') {
            return <span className="inline-flex items-center gap-1.5"><Link2 size={12} className="text-gray-400" />{t('connection.azureEndpoint')}</span>;
        }
        if (protocol === 'webdav') {
            return <span className="inline-flex items-center gap-1.5"><Link2 size={12} className="text-gray-400" />{t('connection.endpointUrl')}</span>;
        }
        return t('connection.server');
    };

    // Dynamic password label based on protocol
    const getPasswordLabel = () => {
        if (protocol === 's3') return t('connection.secretAccessKey');
        if (protocol === 'azure') return t('connection.azureAccessKey');
        if (protocol === 'github') return t('github.personalAccessToken');
        if (protocol === 'gitlab') return 'Access Token';
        // Same as getUsernameLabel: honour a preset's own password label so the
        // WebDAV mode matches the native form ("App Password" for Koofr/Yandex,
        // "Password" for OpenDrive/pCloud) instead of the generic "Password".
        const presetLabel = selectedProvider?.fields?.find((f) => f.key === 'password')?.label;
        if (presetLabel) return presetLabel;
        return t('connection.password');
    };

    // Username/Password label rows with contextual signup / password-gen links inline.
    // The links live next to the field they help with (more discoverable than a footer row).
    const renderUsernameLabel = (overrideText?: string) => (
        <div className="flex items-center justify-between gap-2 mb-1.5">
            <label className="block text-sm font-medium">{overrideText ?? getUsernameLabel()}</label>
            {accountSignupUrl && (
                <a
                    href={`${accountSignupUrl}${accountSignupUrl.includes('?') ? '&' : '?'}utm_source=aeroftp`}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="inline-flex items-center gap-1 text-xs text-emerald-500 hover:text-emerald-600 dark:text-emerald-400 dark:hover:text-emerald-300"
                >
                    <ExternalLink size={10} />
                    {t('connection.createAccount')}
                </a>
            )}
        </div>
    );

    const renderPasswordLabel = (overrideText?: string) => (
        <div className="flex items-center justify-between gap-2 mb-1.5">
            <label className="block text-sm font-medium">{overrideText ?? getPasswordLabel()}</label>
            {accountPasswordGenUrl && (
                <a
                    href={accountPasswordGenUrl}
                    target="_blank"
                    rel="noopener noreferrer"
                    className="inline-flex items-center gap-1 text-xs text-amber-500 hover:text-amber-600 dark:text-amber-400 dark:hover:text-amber-300"
                >
                    <KeyRound size={10} />
                    {t('connection.generatePassword')}
                </a>
            )}
        </div>
    );

    // #369: the 2FA code fields (Filen / MEGA / Internxt) accept digits only,
    // so a stray letter or space can never make a TOTP silently wrong. Shared
    // by all three forms so the behaviour stays identical. The 6-digit clamp
    // lives HERE, not in a maxLength attribute: the DOM truncates a paste
    // BEFORE the change event, so "123 456" under maxLength arrived as
    // "123 45" and stripped to five digits, silently corrupting the code.
    const handleTotpCodeChange = (raw: string) => {
        const digits = raw.replace(/\D/g, '').slice(0, 6);
        onConnectionParamsChange({
            ...connectionParams,
            options: { ...connectionParams.options, two_factor_code: digits || undefined },
        });
    };

    const parseEndpointPort = (value: string, fallback: number) => {
        try {
            const url = new URL(value);
            if (url.port) return parseInt(url.port, 10) || fallback;
            return url.protocol === 'http:' ? 80 : url.protocol === 'https:' ? 443 : fallback;
        } catch {
            return fallback;
        }
    };

    // Issue #215: ask the backend to run `mega-webdav /` and fill the Endpoint
    // URL from its output, mirroring the mega-df quota probe. Requires an active
    // MEGAcmd login; the typed error from the backend is surfaced inline.
    const handleFetchMegaWebdavUrl = async () => {
        setMegaWebdavError(null);
        setMegaWebdavFetching(true);
        try {
            const url = await invoke<string>('mega_webdav_url');
            if (url) {
                onConnectionParamsChange({
                    ...connectionParams,
                    server: url,
                    username: '',
                    password: '',
                    port: parseEndpointPort(url, connectionParams.port || 4443),
                    options: { ...(connectionParams.options || {}), anonymous: true },
                });
            }
        } catch (e) {
            setMegaWebdavError(typeof e === 'string' ? e : String(e));
        } finally {
            setMegaWebdavFetching(false);
        }
    };

    // Provider logo for connect buttons (OAuth/API providers show their logo instead of Cloud icon)
    const ConnectIcon = (() => {
        const logoId = selectedProviderId || protocol || '';
        const Logo = PROVIDER_LOGOS[logoId];
        if (Logo) return <Logo size={18} />;
        return <Cloud size={18} />;
    })();

    /**
     * Issue #215: shared footer rendered whenever the operator switched
     * to a different mode of the SAME provider group while editing a
     * saved profile. Three explicit choices: Cancel, Save-as-new (the
     * original is preserved), Convert (the original is replaced in slot
     * with a 10s Undo toast).
     */
    const renderModeChangedFooter = () => (
        <>
            <div className="flex flex-wrap gap-2 pt-2">
                <button
                    onClick={handleCancelEdit}
                    className="px-4 py-3 bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 font-medium rounded-lg hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors"
                    title={t('connection.cancelEditing')}
                >
                    <X size={20} />
                </button>
                <button
                    onClick={handleSaveAsNew}
                    disabled={loading}
                    className="flex-1 min-w-[140px] py-3 px-4 rounded-lg font-medium text-white bg-green-600 hover:bg-green-700 transition-colors flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
                    title={t('connection.saveAsNewProfileTitle')}
                >
                    <Copy size={18} />
                    <span className="truncate">{t('connection.saveAsNewProfile')}</span>
                </button>
                <button
                    onClick={handleConvertMode}
                    disabled={loading}
                    className="flex-1 min-w-[140px] py-3 px-4 rounded-lg font-medium text-white bg-orange-600 hover:bg-orange-700 transition-colors flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed"
                    title={t('connection.convertReplacesOriginal')}
                >
                    <ArrowRightLeft size={18} />
                    <span className="truncate">{t('connection.convertToMode', { mode: targetModeLabel })}</span>
                </button>
            </div>
            <p className="text-[11px] text-orange-700 dark:text-orange-300 text-center pt-1 leading-snug">
                {t('connection.convertReplacesOriginal')}
            </p>
        </>
    );

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
        hideSaveButton?: boolean;
        // When set, the footer button persists metadata via this handler instead of
        // connecting (used to save a name / local-path edit on an OAuth profile
        // without re-running the sign-in). The button honours the `disabled` opt so
        // callers can gate it on a dirty check.
        saveOverride?: () => void | Promise<void>;
        // Paired Cancel next to a `saveOverride` Save, so the footer is not a lone
        // Save button (closes the form tab / exits edit mode).
        cancelOverride?: () => void;
    }) => {
        const {
            disabled: btnDisabled,
            buttonColorClass,
            buttonText,
            remotePathPlaceholder = t('connection.initialRemotePath'),
            connectionNameKey = getSuggestedConnectionName(),
            showE2ENote,
            showIconPicker: showIcon = true,
            showCancelSaveAsNew = false,
            hideSaveButton = false,
            saveOverride,
            cancelOverride,
        } = opts;
        const isSourceForge = selectedProviderId === 'sourceforge';
        const sfPrefix = '/home/frs/project/';
        return (
            <div className="space-y-3">
                {/* #215 (Ehud redesign): profile name + inline icon block, moved to
                    the top of the column. The icon sits Google-Docs style,
                    immediately to the left of the name input. */}
                <div className="pb-3 border-b border-gray-200 dark:border-gray-700/50">
                    <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                        <Save size={14} />
                        {t('connection.connectionNameOptional')}
                    </label>
                    <div className="flex items-center gap-2">
                        {showIcon && renderProfileIconButton()}
                        <input
                            type="text"
                            value={connectionName}
                            onChange={(e) => setConnectionName(e.target.value)}
                            placeholder={connectionNameKey}
                            className="flex-1 px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                        />
                        {showIcon && customIconForSave && (
                            <button
                                type="button"
                                onClick={() => setCustomIconForSave(undefined)}
                                title={t('settings.removeIcon')}
                                className="p-1.5 shrink-0 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-red-500 transition-colors"
                            >
                                <X size={14} />
                            </button>
                        )}
                    </div>
                </div>
                {/* Local Path first: a local folder is set in almost every case, so
                    it is the more important field. Remote Path follows (#215). */}
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
                {/* Remote Path: optional for most providers (writable root), so it sits
                    below Local Path. Hidden for an AeroShare friend (a read-only replica
                    has no remote-path concept). SourceForge: prefix + project name. */}
                {!isPeer && (
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
                            disabled={overlayFieldsLocked}
                            onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm disabled:opacity-60 disabled:cursor-not-allowed"
                            placeholder={remotePathPlaceholder}
                        />
                    )}
                    {overlayFieldsLocked && (
                        <p className="text-xs text-amber-600 dark:text-amber-400 mt-1">{t('aerocryptProfile.remotePathLockedNote')}</p>
                    )}
                </div>
                )}
                {/* OpenDrive (#252): default privacy applied to newly created
                    folders and uploaded files. Only on the OpenDrive native API
                    form (absent on WebDAV -> WebDAV uses OpenDrive's defaults). */}
                {protocol === 'opendrive' && (
                    <div>
                        <label className="block text-sm font-medium mb-1.5">
                            {t('connection.opendriveDefaultPrivacy')}
                        </label>
                        <select
                            value={connectionParams.options?.opendriveDefaultPrivacy || 'private'}
                            onChange={(e) => onConnectionParamsChange({
                                ...connectionParams,
                                options: {
                                    ...connectionParams.options,
                                    opendriveDefaultPrivacy: e.target.value as 'private' | 'public' | 'hidden',
                                },
                            })}
                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-cyan-500 focus:border-cyan-500"
                        >
                            <option value="private">{t('properties.privacyPrivate')}</option>
                            <option value="public">{t('properties.privacyPublic')}</option>
                            <option value="hidden">{t('properties.privacyHidden')}</option>
                        </select>
                        <p className="text-xs text-gray-400 mt-1.5">{t('connection.opendriveDefaultPrivacyHelp')}</p>
                    </div>
                )}
                {/* Profile name + icon block relocated to the top of this column
                    (see the #215 redesign block above). */}
                {/* P3: AeroCrypt Profile. Bind an encrypted overlay to this
                    profile so the standard dual-panel renders transparently
                    decrypted (Filen/MEGA-style). Remote/local scope come from the
                    paths above. Offered on every provider-API backend. */}
                {overlayEligible && (
                    <div className="rounded-lg border border-gray-300 dark:border-gray-600 overflow-hidden">
                        {/* Ehud #276 (17324431): collapsible "Wrappers / Overlays" parent so
                            the Quick Connect page stays uncluttered. Crypt is the first
                            sub-section; future overlays slot in alongside it. */}
                        <button
                            type="button"
                            onClick={() => setOverlaysExpanded((v) => !v)}
                            className="w-full flex items-center gap-2 px-3 py-2.5 text-sm font-medium text-gray-700 dark:text-gray-200 hover:bg-gray-50 dark:hover:bg-gray-700/40"
                        >
                            <Shield size={15} className="text-emerald-600 dark:text-emerald-400" />
                            {t('aerocryptProfile.overlaysSection')}
                            {aeroCryptEnabled && (
                                <span className="text-[10px] uppercase tracking-wide px-1.5 py-0.5 rounded bg-emerald-500/15 text-emerald-700 dark:text-emerald-300">{t('aerocryptProfile.overlaysActiveBadge')}</span>
                            )}
                            <ChevronDown size={14} className={`ml-auto transition-transform duration-200 ${overlaysExpanded ? 'rotate-180' : ''}`} />
                        </button>
                        {overlaysExpanded && (
                        <div className="p-3 border-t border-gray-200 dark:border-gray-700">
                        {/* #369: Overlays Remote Path field (one for all overlays).
                            Placed outside the emerald box per design. Empty value means
                            "same as Remote Path" (no schema change; re-uses remoteScope). */}
                        <div className="mb-3">
                            <label className="block text-xs font-medium text-gray-600 dark:text-gray-400 mb-1">
                                {t('aerocryptProfile.overlaysRemotePath')}
                            </label>
                            <input
                                type="text"
                                value={overlaysRemotePath}
                                disabled={overlayFieldsLocked}
                                onChange={(e) => {
                                    const v = e.target.value;
                                    setOverlaysRemotePath(v);
                                    const valid = isValidOverlayScope(resolveOverlayScope(v, quickConnectDirs.remoteDir), quickConnectDirs.remoteDir);
                                    setOverlaysRemotePathError(valid ? null : t('aerocryptProfile.overlaysRemotePathInvalid'));
                                }}
                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm disabled:opacity-60 disabled:cursor-not-allowed"
                                placeholder="/"
                            />
                            <p className="mt-1 text-xs text-gray-400 dark:text-gray-500">
                                {t('aerocryptProfile.overlaysRemotePathHint')}
                            </p>
                            {/* #369 F5: the scope is the encryption anchor, immutable once the
                                overlay is bound (like the Remote Path and the other crypt fields);
                                editing it on an existing vault would orphan the encrypted data. */}
                            {overlayFieldsLocked && (
                                <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">{t('aerocryptProfile.remotePathLockedNote')}</p>
                            )}
                            {overlaysRemotePathError && (
                                <p className="mt-1 text-xs text-red-600 dark:text-red-400">{overlaysRemotePathError}</p>
                            )}
                        </div>
                        <div className="rounded-lg border border-emerald-300/60 dark:border-emerald-700/50 bg-emerald-50/50 dark:bg-emerald-900/20 p-3 space-y-2">
                        <Checkbox
                            checked={aeroCryptEnabled}
                            onChange={setAeroCryptEnabled}
                            label={t('aerocryptProfile.enable')}
                            labelClassName="text-sm font-medium"
                        />
                        <p className="text-xs text-gray-500 dark:text-gray-400">{t('aerocryptProfile.hint')}</p>
                        {aeroCryptEnabled && (
                            <div className="space-y-2 pt-1">
                                {/* Ehud #276 (17324431): Rclone Crypt on the left, it is the
                                    older format. AeroCrypt native sits to its right. */}
                                <div className="flex gap-2">
                                    <button
                                        type="button"
                                        disabled={overlayFieldsLocked}
                                        onClick={() => setAeroCryptKind('rclone-crypt')}
                                        className={`flex-1 px-3 py-2 rounded-lg text-xs border transition-colors ${aeroCryptKind === 'rclone-crypt' ? 'border-emerald-500 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400'} ${overlayFieldsLocked ? 'opacity-60 cursor-not-allowed' : ''}`}
                                    >
                                        {t('aerocryptProfile.kindRclone')}
                                    </button>
                                    <button
                                        type="button"
                                        disabled={overlayFieldsLocked}
                                        onClick={() => setAeroCryptKind('aerocrypt')}
                                        className={`flex-1 px-3 py-2 rounded-lg text-xs border transition-colors ${aeroCryptKind === 'aerocrypt' ? 'border-emerald-500 bg-emerald-500/10 text-emerald-700 dark:text-emerald-300' : 'border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-400'} ${overlayFieldsLocked ? 'opacity-60 cursor-not-allowed' : ''}`}
                                    >
                                        {t('aerocryptProfile.kindNative')}
                                    </button>
                                </div>
                                {!aeroCryptKind && (
                                    <p className="text-xs text-amber-600 dark:text-amber-400">{t('aerocryptProfile.chooseKind')}</p>
                                )}
                                {aeroCryptKind && (<>
                                {/* Ehud #276 (17324431): per-kind interop note. The native
                                    format is AeroFTP-only; rclone-crypt is the standard rclone
                                    format and stays decryptable by rclone itself. */}
                                <p className="text-xs text-gray-500 dark:text-gray-400">
                                    {aeroCryptKind === 'rclone-crypt' ? t('aerocryptProfile.hintRclone') : t('aerocryptProfile.hintNative')}
                                </p>
                                <div>
                                    <label className="block text-xs font-medium text-gray-600 dark:text-gray-400 mb-1">{t('aerocryptProfile.passwordLabel')}</label>
                                    <div className="relative">
                                        <input
                                            type={showAeroCryptPassword ? 'text' : 'password'}
                                            value={aeroCryptPassword}
                                            disabled={overlayFieldsLocked}
                                            onChange={(e) => setAeroCryptPassword(e.target.value)}
                                            placeholder={editingProfileId && !aeroCryptPassword ? t('aerocryptProfile.passwordStored') : t('aerocryptProfile.passwordPlaceholder')}
                                            className="w-full px-4 py-2.5 pr-20 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm disabled:opacity-60 disabled:cursor-not-allowed"
                                        />
                                        {!overlayFieldsLocked && (
                                            <InlinePasswordGenerator
                                                onGenerated={(value) => { setAeroCryptPassword(value); setAeroCryptConfirm(value); }}
                                                className="absolute right-9 top-1/2 -translate-y-1/2"
                                            />
                                        )}
                                        <button
                                            type="button"
                                            tabIndex={-1}
                                            onClick={() => setShowAeroCryptPassword((v) => !v)}
                                            className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200"
                                        >
                                            {showAeroCryptPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                        </button>
                                    </div>
                                </div>
                                {/* #322: strength meter + a set-once confirm with live match.
                                    The overlay credentials are immutable once data exists, so a
                                    confirm guards against a typo that would lock the blobs forever.
                                    Both hidden when editing a locked binding. */}
                                {!overlayFieldsLocked && aeroCryptPassword.length > 0 && (
                                    <PasswordStrengthBar password={aeroCryptPassword} />
                                )}
                                {!overlayFieldsLocked && (
                                    <div>
                                        <label className="block text-xs font-medium text-gray-600 dark:text-gray-400 mb-1">{t('password.confirm')}</label>
                                        <div className="relative">
                                            <input
                                                type={showAeroCryptPassword ? 'text' : 'password'}
                                                value={aeroCryptConfirm}
                                                onChange={(e) => setAeroCryptConfirm(e.target.value)}
                                                placeholder={t('password.confirmPlaceholder')}
                                                className="w-full px-4 py-2.5 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                            />
                                            <button
                                                type="button"
                                                tabIndex={-1}
                                                onClick={() => setShowAeroCryptPassword((v) => !v)}
                                                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200"
                                            >
                                                {showAeroCryptPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                            </button>
                                        </div>
                                        <PasswordMatchHint password={aeroCryptPassword} confirm={aeroCryptConfirm} />
                                    </div>
                                )}
                                {/* AeroCrypt Tier 1 keyfile (optional second factor). The PATH
                                    stays editable even when the binding is locked: set-once
                                    applies to the FACTOR, not to where the file lives (re-point
                                    after import needs this). A mismatch fails closed at unlock. */}
                                {aeroCryptKind === 'aerocrypt' && (
                                    <>
                                        <div className="flex flex-col gap-1.5 mt-2 mb-4">
                                            <label className="flex items-start gap-3 cursor-pointer group">
                                                <div className="relative flex items-center h-5 mt-0.5">
                                                    <input
                                                        type="checkbox"
                                                        checked={aeroCryptWithHeader}
                                                        onChange={(e) => setAeroCryptWithHeader(e.target.checked)}
                                                        disabled={overlayFieldsLocked}
                                                        className="peer sr-only"
                                                    />
                                                    <div className="w-9 h-5 bg-gray-200 dark:bg-gray-700 peer-focus:outline-none peer-focus:ring-2 peer-focus:ring-emerald-500/30 rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-emerald-500 disabled:opacity-50 transition-colors"></div>
                                                </div>
                                                <div className="flex flex-col text-sm">
                                                    <span className={`font-medium transition-colors ${overlayFieldsLocked ? 'text-gray-400 dark:text-gray-500' : 'text-gray-700 dark:text-gray-300 group-hover:text-gray-900 dark:group-hover:text-gray-100'}`}>
                                                        {t('aerocryptProfile.headerToggle')}
                                                    </span>
                                                    <span className={`text-xs mt-0.5 leading-relaxed ${overlayFieldsLocked ? 'text-gray-400/80 dark:text-gray-500/80' : 'text-gray-500 dark:text-gray-400'}`}>
                                                        {t('aerocryptProfile.headerToggleHint')}
                                                    </span>
                                                </div>
                                            </label>
                                        </div>

                                        {/* D1-D3: opt-in default-salt (public constant) for headerless password-only portability.
                                            Sibling to header toggle. Gated by entropy + explicit attestation.
                                            Two tiers: 128-bit recommended (default), 256-bit stricter. */}
                                        {aeroCryptKind === 'aerocrypt' && (
                                            <div className="flex flex-col gap-1.5 mt-2 mb-4">
                                                <label className="flex items-start gap-3 cursor-pointer group">
                                                    <div className="relative flex items-center h-5 mt-0.5">
                                                        <input
                                                            type="checkbox"
                                                            checked={aeroCryptDefaultSalt}
                                                            onChange={(e) => setAeroCryptDefaultSalt(e.target.checked)}
                                                            disabled={overlayFieldsLocked || !canToggleDefaultSalt}
                                                            className="peer sr-only"
                                                        />
                                                        <div className="w-9 h-5 bg-gray-200 dark:bg-gray-700 peer-focus:outline-none peer-focus:ring-2 peer-focus:ring-emerald-500/30 rounded-full peer peer-checked:after:translate-x-full peer-checked:after:border-white after:content-[''] after:absolute after:top-[2px] after:left-[2px] after:bg-white after:border-gray-300 after:border after:rounded-full after:h-4 after:w-4 after:transition-all peer-checked:bg-emerald-500 disabled:opacity-50 transition-colors"></div>
                                                    </div>
                                                    <div className="flex flex-col text-sm">
                                                        <span className={`font-medium transition-colors ${overlayFieldsLocked || !canToggleDefaultSalt ? 'text-gray-400 dark:text-gray-500' : 'text-gray-700 dark:text-gray-300 group-hover:text-gray-900 dark:group-hover:text-gray-100'}`}>
                                                            {t('aerocryptProfile.defaultSaltToggle') || 'Default salt (password only, no kit/keystore needed)'}
                                                        </span>
                                                        <span className={`text-xs mt-0.5 leading-relaxed ${overlayFieldsLocked || !canToggleDefaultSalt ? 'text-gray-400/80 dark:text-gray-500/80' : 'text-gray-500 dark:text-gray-400'}`}>
                                                            {t('aerocryptProfile.defaultSaltToggleHint') || 'Uses a public constant salt. Requires high-entropy generated password. Same password across vaults makes names linkable.'}
                                                        </span>
                                                    </div>
                                                </label>

                                                {/* Two-tier selector (appears when considering default salt or always for clarity) */}
                                                {aeroCryptKind === 'aerocrypt' && (
                                                    <div className="ml-12 mt-1 flex items-center gap-3 text-xs">
                                                        <label className="flex items-center gap-1.5">
                                                            <input
                                                                type="radio"
                                                                name="saltStrength"
                                                                checked={!aeroCryptDefaultSaltStrength || aeroCryptDefaultSaltStrength === '128'}
                                                                onChange={() => setAeroCryptDefaultSaltStrength('128')}
                                                                disabled={overlayFieldsLocked}
                                                            />
                                                            <span>128-bit (recommended)</span>
                                                        </label>
                                                        <label className="flex items-center gap-1.5">
                                                            <input
                                                                type="radio"
                                                                name="saltStrength"
                                                                checked={aeroCryptDefaultSaltStrength === '256'}
                                                                onChange={() => setAeroCryptDefaultSaltStrength('256')}
                                                                disabled={overlayFieldsLocked}
                                                            />
                                                            <span>256-bit (stricter)</span>
                                                        </label>
                                                    </div>
                                                )}

                                                {/* Attestation (required to enable default salt) */}
                                                {aeroCryptDefaultSalt && (
                                                    <div className="ml-12 mt-1">
                                                        <label className="flex items-start gap-2 text-xs cursor-pointer">
                                                            <input
                                                                type="checkbox"
                                                                checked={aeroCryptDefaultSaltAttested}
                                                                onChange={(e) => setAeroCryptDefaultSaltAttested(e.target.checked)}
                                                                className="mt-0.5"
                                                            />
                                                            <span className="text-gray-600 dark:text-gray-400">
                                                                {t('aerocryptProfile.defaultSaltAttestation') || 'I generated this password with a password manager and understand the linkability tradeoff (identical passwords produce linkable encrypted names).'}
                                                            </span>
                                                        </label>
                                                    </div>
                                                )}
                                            </div>
                                        )}

                                        <div>
                                        <label className="block text-xs font-medium text-gray-600 dark:text-gray-400 mb-1">{t('aerocryptProfile.keyfileLabel')}</label>
                                        <div className="flex gap-2">
                                            <input
                                                type="text"
                                                value={aeroCryptKeyfilePath}
                                                readOnly
                                                placeholder={t('aerocryptProfile.keyfilePlaceholder')}
                                                className="flex-1 min-w-0 px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm truncate"
                                                title={aeroCryptKeyfilePath || undefined}
                                                aria-label={t('aerocryptProfile.keyfileLabel')}
                                            />
                                            <button
                                                type="button"
                                                onClick={handleChooseKeyfile}
                                                className="shrink-0 whitespace-nowrap px-3 py-2 rounded-lg text-xs font-medium border border-gray-300 dark:border-gray-600 text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700"
                                            >
                                                {t('aerocryptProfile.keyfileChoose')}
                                            </button>
                                            <button
                                                type="button"
                                                onClick={handleGenerateKeyfile}
                                                title={t('aerocryptProfile.keyfileStorageHint')}
                                                className="shrink-0 whitespace-nowrap px-3 py-2 rounded-lg text-xs font-medium border border-emerald-400/60 text-emerald-700 dark:text-emerald-300 hover:bg-emerald-500/10"
                                            >
                                                {t('aerocryptProfile.keyfileGenerate')}
                                            </button>
                                        </div>
                                        {keyfileError && (
                                            <p className="mt-1 text-xs text-red-600 dark:text-red-400 break-words">{keyfileError}</p>
                                        )}
                                        {keyfileJustGenerated && (
                                            <p className="mt-1 text-xs text-amber-600 dark:text-amber-400">{t('aerocryptProfile.keyfileGeneratedBackup')}</p>
                                        )}
                                        <p className="mt-1 text-xs text-gray-500 dark:text-gray-400">
                                            {overlayFieldsLocked ? t('aerocryptProfile.keyfileRepointHint') : t('aerocryptProfile.keyfileHint')}
                                        </p>
                                        </div>
                                    </>
                                )}
                                {/* rclone-crypt interop (P3.3b): salt + filename/dir-name
                                    encryption so the bound profile auto-unlocks like native.
                                    Native AeroCrypt reads these from its marker. */}
                                {aeroCryptKind === 'rclone-crypt' && (
                                    <>
                                        <div>
                                            <label className="block text-xs font-medium text-gray-600 dark:text-gray-400 mb-1">{t('aerocryptProfile.saltLabel')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showAeroCryptSalt ? 'text' : 'password'}
                                                    value={aeroCryptSalt}
                                                    disabled={overlayFieldsLocked}
                                                    onChange={(e) => setAeroCryptSalt(e.target.value)}
                                                    placeholder={editingProfileId && !aeroCryptSalt ? t('aerocryptProfile.passwordStored') : t('aerocrypt.saltPlaceholder')}
                                                    className="w-full px-4 py-2.5 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm disabled:opacity-60 disabled:cursor-not-allowed"
                                                />
                                                <button
                                                    type="button"
                                                    tabIndex={-1}
                                                    onClick={() => setShowAeroCryptSalt((v) => !v)}
                                                    className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-200"
                                                >
                                                    {showAeroCryptSalt ? <EyeOff size={16} /> : <Eye size={16} />}
                                                </button>
                                            </div>
                                        </div>
                                        <select
                                            value={aeroCryptFilenameEnc}
                                            disabled={overlayFieldsLocked}
                                            onChange={(e) => setAeroCryptFilenameEnc(e.target.value as 'standard' | 'obfuscate' | 'off')}
                                            className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm disabled:opacity-60 disabled:cursor-not-allowed"
                                        >
                                            <option value="standard">{t('aerocrypt.filenameEncOption.standard')}</option>
                                            <option value="obfuscate">{t('aerocrypt.filenameEncOption.obfuscate')}</option>
                                            <option value="off">{t('aerocrypt.filenameEncOption.off')}</option>
                                        </select>
                                        <Checkbox
                                            checked={aeroCryptDirNameEnc}
                                            onChange={setAeroCryptDirNameEnc}
                                            disabled={overlayFieldsLocked}
                                            label={t('aerocrypt.directoryNameEncryption')}
                                            labelClassName="text-xs"
                                        />
                                    </>
                                )}
                                {/* C-EDIT-GUARD: kind + credentials derive the keys directly, so
                                    they are immutable once the overlay holds data. */}
                                <p className={`text-xs ${overlayFieldsLocked ? 'text-amber-600 dark:text-amber-400' : 'text-gray-500 dark:text-gray-400'}`}>
                                    {overlayFieldsLocked ? t('aerocryptProfile.lockedNote') : t('aerocryptProfile.immutableWarn')}
                                </p>
                                {overlayNewlyBound && (
                                    <p className="text-xs text-amber-600 dark:text-amber-400">{t('aerocryptProfile.addToExistingWarn')}</p>
                                )}
                                </>)}
                                <p className="text-xs text-gray-500 dark:text-gray-400">{t('aerocryptProfile.scopeHint')}</p>
                            </div>
                        )}
                        </div>
                        </div>
                        )}
                    </div>
                )}
                {/* Issue #215: persist credentials for every protocol of this
                    account so switching modes never asks again, even after a
                    restart. Only offered when the active provider/protocol is
                    part of a mode group (Filen, MEGA, OpenDrive, Koofr, ...). */}
                {showPersistCheckbox && (
                    <div>
                        <Checkbox
                            checked={persistModeCredentials}
                            onChange={setPersistModeCredentials}
                            label={t('connection.persistModeCredentials')}
                            labelClassName="text-sm"
                        />
                        <p className="mt-1 text-xs text-gray-400 dark:text-gray-500">
                            {t('connection.persistModeCredentialsHint')}
                        </p>
                    </div>
                )}
                {/* Total storage (manual): optional cap for backends with no
                    quota API (raw FTP/FTPS/SFTP, most S3/WebDAV) or that
                    expose USED but not TOTAL (Backblaze B2). The provider API
                    total always wins; this is only the fallback so the My
                    Servers usage bar and % can render (item 4a). Hidden for
                    backends that already report their own quota (native-API
                    providers, and Koofr even over WebDAV via its REST API):
                    the manual cap and the used-storage scan are pointless
                    noise there. */}
                {!providerServesQuota(
                    connectionParams.protocol,
                    activeProviderId,
                    connectionParams.server,
                ) && (
                <div>
                    <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                        <HardDrive size={14} />
                        {t('connection.manualTotalStorage')}
                    </label>
                    <input
                        key={`mtb-${editingProfileId || 'new'}`}
                        type="text"
                        defaultValue={connectionParams.options?.manualTotalBytes
                            ? formatBytes(connectionParams.options.manualTotalBytes)
                            : ''}
                        onChange={(e) => {
                            const raw = e.target.value.trim();
                            const opts = { ...(connectionParams.options || {}) };
                            if (!raw) {
                                delete opts.manualTotalBytes;
                            } else {
                                const bytes = parseHumanSize(raw);
                                if (bytes && bytes > 0) opts.manualTotalBytes = bytes;
                                else delete opts.manualTotalBytes;
                            }
                            onConnectionParamsChange({ ...connectionParams, options: opts });
                        }}
                        placeholder={t('connection.manualTotalStoragePlaceholder')}
                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                    />
                    <p className="mt-1 text-xs text-gray-400 dark:text-gray-500">
                        {t('connection.manualTotalStorageHint')}
                    </p>
                </div>
                )}
                {/* The opt-in used-storage scan is only meaningful for backends
                    that do NOT report their own quota. MEGAcmd now serves its
                    real quota via mega-df on every connect (run unconditionally
                    as part of the daemon warm-up), so the checkbox was inert
                    noise there and is no longer shown (#275). */}
                {!providerServesQuota(
                    connectionParams.protocol,
                    activeProviderId,
                    connectionParams.server,
                ) && (
                    <div>
                        <Checkbox
                            checked={!!connectionParams.options?.autoScanUsedOnConnect}
                            onChange={(checked) => {
                                const opts = { ...(connectionParams.options || {}) };
                                if (checked) {
                                    opts.autoScanUsedOnConnect = true;
                                } else {
                                    delete opts.autoScanUsedOnConnect;
                                }
                                onConnectionParamsChange({ ...connectionParams, options: opts });
                            }}
                            label={t('connection.autoScanUsedOnConnect')}
                            labelClassName="text-sm"
                        />
                        <p className="mt-1 text-xs text-gray-400 dark:text-gray-500">
                            {t('connection.autoScanUsedOnConnectHint')}
                        </p>
                    </div>
                )}
                {/* Action Buttons. Issue #215: mode switch in edit takes
                    over the footer with Save-as-new + Convert when the
                    operator picked a different surface of the same group. */}
                {hideSaveButton ? null : modeChanged && editingProfileId ? (
                    renderModeChangedFooter()
                ) : (
                    <div className={(showCancelSaveAsNew || cancelOverride) ? 'flex gap-2' : 'pt-2'}>
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
                        {cancelOverride && (
                            <button onClick={cancelOverride} className="px-4 py-3 bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 font-medium rounded-lg hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors flex items-center gap-2" title={t('common.cancel')}>
                                <X size={20} /> {t('common.cancel')}
                            </button>
                        )}
                        <button
                            onClick={saveOverride || handleConnectAndSave}
                            disabled={saveOverride ? (loading || btnDisabled) : (loading || btnDisabled || aeroCryptConfirmMismatch || !!overlaysRemotePathError)}
                            className={`${(showCancelSaveAsNew || cancelOverride) ? 'flex-1' : 'w-full'} py-3 rounded-lg font-medium text-white cursor-pointer active:scale-[0.98] transition-all flex items-center justify-center gap-2 shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] disabled:opacity-50 ${loading ? 'bg-gray-400 !cursor-not-allowed' : buttonColorClass}`}
                        >
                            {loading ? (
                                <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                            ) : buttonText ? buttonText : (
                                editingProfileId ? <><Save size={18} /> {t('common.save')}</> :
                                saveConnection ? <><Save size={18} /> {t('common.save')}</> :
                                t('common.connect')
                            )}
                        </button>
                    </div>
                )}
                {/* E2E note */}
                {showE2ENote && (
                    <p className="text-center text-xs text-gray-400 flex items-center justify-center gap-1.5">
                        <Lock size={12} /> {t(showE2ENote)}
                    </p>
                )}
            </div>
        );
    };

    /**
     * Persist a peer-card edit (My Servers). The shared AeroShareHandshakeBody
     * owns the EDIT form (so it stays identical to the ADD form); it hands the
     * updated friend name / local folder / icon back here, and we merge them
     * onto the existing saved profile. No drive re-import: editing a received
     * connection only changes local metadata + the replica path
     * (options.peerLocalFolder). Friend name drives both the card title and the
     * saved alias (the old redundant Connection-name field is gone).
     */
    const savePeerEditedProfile = async (v: { alias: string; localFolder: string; customIconUrl?: string }) => {
        const id = editingProfileId ?? editingProfile?.id;
        if (!id) return;
        const servers = await loadSavedServerProfiles();
        const alias = v.alias.trim();
        const updated = servers.map((s) => s.id === id ? {
            ...s,
            name: alias || s.name,
            username: alias || s.username,
            customIconUrl: v.customIconUrl,
            options: { ...(s.options || {}), peerLocalFolder: v.localFolder },
        } : s);
        await storeSavedServerProfiles(updated).catch(() => { });
        setSavedServersUpdate(Date.now());
        const saved = updated.find((s) => s.id === id);
        if (saved) {
            logActivity('PROFILE_SAVE', `Profile updated: "${saved.name}"`, 'success', `dedupKey=${getStorageDedupKey(saved)}`);
        }
    };

    // In formOnly mode: wider for 2-column protocols, narrower for single-column providers
    const twoColProtocols = ['ftp', 'ftps', 'sftp', 's3', 'webdav', 'azure', 'filen', 'internxt', 'koofr', 'opendrive', 'kdrive', 'immich', 'imagekit', 'uploadcare', 'cloudinary', 'filelu', 'drime', 'jottacloud', 'backblaze',
        // #215 harmonization: OAuth clouds are now two-column too, so they get the
        // same wide card (max-w-4xl) as the rest instead of the narrow single-column one.
        'googledrive', 'googlephotos', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive', 'yandexdisk',
        // #369: MEGA API/CMD now uses the two-column layout too, so the wide
        // card gives the MEGA MODES bar room and its S4 tab no longer wraps.
        'mega'];
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
                                {(() => {
                                    // Keep the "Connect to X" subtitle in sync
                                    // with the canonical group header so it
                                    // does not vanish on the preset-less
                                    // native tab.
                                    const mh = resolveModeHeader(connectionParams.providerId, connectionParams.protocol);
                                    const name = mh?.name
                                        || (mh?.providerId ? getProviderById(mh.providerId)?.name : undefined)
                                        || selectedProvider?.name;
                                    return name ? (
                                        <p className="text-xs text-gray-500 dark:text-gray-400 mt-1">{t('connection.connectTo', { provider: name })}</p>
                                    ) : null;
                                })()}
                            </div>
                            {(() => {
                                const PROTOCOL_DISPLAY: Record<string, { name: string; desc?: string }> = {
                                    pixelunion: { name: 'PixelUnion', desc: t('protocol.discoverPixelUnion') },
                                    immich: { name: 'Immich', desc: t('protocol.discoverImmich') },
                                };
                                // #369: single-preset OAuth/API providers launched by protocol
                                // alone (no providerId) used to fall back to protocol.toUpperCase(),
                                // rendering the header as DROPBOX / ONEDRIVE / BOX and so on. Map
                                // the protocol to its properly cased brand name so the header reads
                                // consistently (MEGA stays all-caps because that is its real style).
                                const PROTOCOL_FALLBACK_NAMES: Record<string, string> = {
                                    dropbox: 'Dropbox', onedrive: 'OneDrive', googledrive: 'Google Drive',
                                    googlephotos: 'Google Photos', box: 'Box', pcloud: 'pCloud Drive',
                                    jottacloud: 'Jottacloud', filen: 'Filen', internxt: 'Internxt',
                                    kdrive: 'kDrive', zohoworkdrive: 'Zoho WorkDrive', yandexdisk: 'Yandex Disk',
                                    drime: 'Drime', mega: 'MEGA', backblaze: 'Backblaze B2', fourshared: '4shared',
                                    imagekit: 'ImageKit', uploadcare: 'Uploadcare', cloudinary: 'Cloudinary',
                                    filelu: 'FileLu', github: 'GitHub', gitlab: 'GitLab',
                                    mtp: 'Portable device (MTP)',
                                };
                                const pid = connectionParams.providerId || '';
                                // When the active config belongs to a mode
                                // group (Koofr/OpenDrive/...), the header
                                // (logo+name+description+Docs link) is the
                                // group's canonical one for EVERY tab, so it
                                // no longer flickers between a full WebDAV
                                // preset header and a linkless native
                                // fallback. selectedProvider still drives the
                                // form fields / connection config.
                                const modeHeader = resolveModeHeader(connectionParams.providerId, connectionParams.protocol);
                                const headerProv = modeHeader?.providerId
                                    ? (getProviderById(modeHeader.providerId) || selectedProvider)
                                    : selectedProvider;
                                const logoId = modeHeader?.providerId || selectedProviderId || pid || protocol || '';
                                const LogoComponent = PROVIDER_LOGOS[logoId];
                                const display = PROTOCOL_DISPLAY[pid] || PROTOCOL_DISPLAY[protocol || ''];
                                const providerName = modeHeader?.name || headerProv?.name || display?.name || PROTOCOL_FALLBACK_NAMES[protocol || ''] || protocol?.toUpperCase() || '';
                                // Description fallback: registry > PROTOCOL_DISPLAY > i18n protocol.<protocol>Desc
                                const tryProtocolDesc = (key: string): string | undefined => {
                                    if (!key) return undefined;
                                    const i18nKey = `protocol.${key}Desc`;
                                    const v = t(i18nKey);
                                    return v && v !== i18nKey ? v : undefined;
                                };
                                // Mode-group neutral description wins: the
                                // canonical preset's text can be protocol-
                                // specific (e.g. OpenDrive WebDAV preset says
                                // "...via WebDAV"), wrong for the API tab (#270).
                                const providerDesc = modeHeader?.description
                                    || headerProv?.description
                                    || display?.desc
                                    || tryProtocolDesc(pid)
                                    || tryProtocolDesc(protocol || '');
                                if (!LogoComponent && !providerName) return null;
                                // Docs link on every Quick Connect page (#270):
                                // prefer the per-provider AeroFTP docs page, fall
                                // back to the provider's own help URL, then the
                                // providers index so a link always renders.
                                const docsUrl = getProviderDocsUrl(modeHeader?.providerId || selectedProviderId || pid, protocol)
                                    || headerProv?.helpUrl
                                    || PROVIDER_DOCS_INDEX;
                                return (
                                    <div className="flex flex-col items-end gap-0.5">
                                        <div className="flex items-center gap-2 text-sm text-gray-500 dark:text-gray-400">
                                            <a
                                                href={docsUrl}
                                                target="_blank"
                                                rel="noopener noreferrer"
                                                className="inline-flex items-center gap-1 text-xs text-blue-500 hover:text-blue-600 dark:text-blue-400 dark:hover:text-blue-300"
                                            >
                                                <ExternalLink size={10} />
                                                Docs
                                            </a>
                                            {LogoComponent && <LogoComponent size={20} />}
                                            <span className="font-medium">{providerName}</span>
                                        </div>
                                        {providerDesc && (
                                            <span className="text-[11px] text-gray-400 dark:text-gray-500 max-w-md text-right leading-tight">{providerDesc}</span>
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
                                    {sessionCount > 0 && (
                                        <span className="text-[10px] tabular-nums px-1.5 py-0.5 rounded-full bg-green-200/70 dark:bg-green-800/50 text-green-800 dark:text-green-300">{sessionCount}</span>
                                    )}
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

                        {/* Z.4.5 R2 generalized: provider mode tabs render when
                            the active preset/protocol belongs to a registered
                            mode group (FileLu, Filen, ...). Lets the operator
                            swap surfaces (Native API, Rsync, WebDAV, S3, FTP,
                            local bridges) without leaving Connect. Mode groups
                            are declared in `providerModeGroups.tsx`.

                            Issue #215: in edit mode, mode switching is now
                            ALLOWED for all groups. `handleProtocolChange`
                            keeps edit mode when both old and new belong to
                            the same group, and the footer offers Save-as-new
                            / Convert. Earlier readOnly lock removed. */}
                        <ProviderModeTabs
                            activeProviderId={selectedProviderId || connectionParams.providerId}
                            activeProtocol={protocol}
                            readOnly={false}
                            onSwitchMode={(newProtocol, newProviderId) => {
                                handleProtocolChange(newProtocol as ProviderType, newProviderId);
                            }}
                            onBridgeSaveBlockedChange={setBridgeSaveBlocked}
                            onBridgeUiStateChange={setBridgeUiState}
                        />

                        {/* Show form only when protocol is selected AND selector is closed */}
                        {!protocol || (isProtocolSelectorOpen && !formOnly) ? (
                            /* No protocol selected or selector is open - show selection prompt + security info */
                            <div className="py-6 space-y-6">
                                <p className="text-sm text-center text-gray-500 dark:text-gray-400">{t('connection.selectProtocolPrompt')}</p>
                                {/* Security Info Box: collapsible */}
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
                                        const existingServers = await loadSavedServerProfiles();
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
                                            await storeSavedServerProfiles(newServers).catch(() => { });
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
                                rightColumn={renderRightColumn(
                                    editingProfileId
                                        ? {
                                            disabled: !oauthEditHasChanges(),
                                            buttonColorClass: 'bg-green-600 hover:bg-green-700',
                                            hideSaveButton: false,
                                            saveOverride: handleOAuthMetadataSave,
                                            cancelOverride: handleOAuthCancel,
                                            buttonText: (<><Save size={18} /> {t('common.save')}</>),
                                        }
                                        : { disabled: false, buttonColorClass: '', hideSaveButton: true },
                                )}
                                saveConnection={saveConnection}
                                onSaveConnectionChange={setSaveConnection}
                                connectionName={connectionName}
                                onConnectionNameChange={setConnectionName}
                                isEditing={!!editingProfileId}
                                existingNames={servers.map(s => s.name)}
                                onConnected={async (displayName, extraOptions) => {
                                    // The saved profile id this live OAuth connection maps to. Seeded
                                    // from editingProfileId (edit mode) so the connect carries a
                                    // savedServerId even when several accounts share the provider:
                                    // connectToFtp's provider-unique linkage fallback bails with 2+
                                    // same-provider profiles, which left an OAuth crypt-overlay
                                    // profile's binding unresolved (getProfileOverlayHint = null) so
                                    // the overlay never auto-unlocked from the form (only the card
                                    // path, which always has the id, worked).
                                    let connectedSavedId: string | undefined = editingProfileId || undefined;
                                    // Save OAuth connection if requested
                                    if (saveConnection) {
                                        const existingServers = await loadSavedServerProfiles();
                                        const saveName = connectionName || displayName;
                                        // Prefer editingProfileId match to support rename (user changed the name in edit mode)
                                        const editTarget = editingProfileId ? existingServers.find(s => s.id === editingProfileId) : undefined;
                                        const duplicate = editTarget || existingServers.find(s => s.name === saveName && s.protocol === protocol);
                                        if (!duplicate) {
                                            const newId = `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;
                                            const overlayFields = await aeroCryptOverlayFields(newId);
                                            const newServer: ServerProfile = {
                                                id: newId,
                                                name: saveName,
                                                host: displayName,
                                                port: 443,
                                                username: '',
                                                password: '',
                                                protocol: protocol as ProviderType,
                                                initialPath: quickConnectDirs.remoteDir || '/',
                                                localInitialPath: quickConnectDirs.localDir,
                                                customIconUrl: customIconForSave,
                                                ...overlayFields,
                                                ...(extraOptions?.region && { options: { region: extraOptions.region } }),
                                            };
                                            const newServers = [...existingServers, newServer];
                                            await storeSavedServerProfiles(newServers).catch(() => { });
                                            connectedSavedId = newId;
                                        } else {
                                            const overlayFields = await aeroCryptOverlayFields(duplicate.id, duplicate.hasStoredAeroCryptPassword, duplicate.hasStoredAeroCryptSalt, duplicate.hasStoredAeroCryptKeyfilePath);
                                            const updated = existingServers.map(s =>
                                                s.id === duplicate.id ? {
                                                    ...s,
                                                    name: saveName || s.name,
                                                    initialPath: quickConnectDirs.remoteDir || s.initialPath,
                                                    localInitialPath: quickConnectDirs.localDir,
                                                    customIconUrl: customIconForSave ?? s.customIconUrl,
                                                    ...overlayFields,
                                                    lastConnected: new Date().toISOString(),
                                                    ...(extraOptions?.region && { options: { ...s.options, region: extraOptions.region } }),
                                                } : s
                                            );
                                            await storeSavedServerProfiles(updated).catch(() => { });
                                            connectedSavedId = duplicate.id;
                                        }
                                    }
                                    // Pass the resolved saved id so the connect links to THIS profile
                                    // (and unlocks its crypt overlay) regardless of how many accounts
                                    // share the provider. No id (fresh unsaved connect) keeps the old
                                    // no-arg behaviour + provider-unique linkage fallback.
                                    onConnect(connectedSavedId ? { ...connectionParams, protocol: protocol as ProviderType, savedServerId: connectedSavedId } : undefined);
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
                        ) : isPeer && !editingProfileId && !editingProfile ? (
                            /* AeroShare peer-ADD (reached via the Discover tile ->
                               onSelectProvider('peer')): render the SHARED handshake body
                               instead of the credential cascade, so add/edit feel like any
                               other server but reuse ONE form. -mx-6 -mb-6 cancels the card
                               padding so the body sits edge-to-edge like in the modal.
                               onClose -> onFormSaved closes the form tab, returns to My
                               Servers and refreshes the list (the saved friend appears as a
                               card). "Connect now" is intentionally omitted here: peer-add
                               behaves like every other server (save -> appears -> Connect).
                               Peer-EDIT (editingProfileId set) keeps the peer-aware form below. */
                            <div className="-mx-6 -mb-6">
                                <AeroShareHandshakeBody
                                    variant="page"
                                    initialMode="receive"
                                    receiveOnly
                                    onClose={() => { onFormSaved?.(); }}
                                />
                            </div>
                        ) : isPeer ? (
                            /* AeroShare peer-EDIT (editingProfileId / editingProfile set):
                               the ADD branch above already handled the !editing case, so
                               reaching here means we are editing a received connection.
                               Reuse the SAME AeroShareHandshakeBody in edit mode so the
                               edit form is identical to the add form (labels, violet
                               Server Icon, styling). The body owns the form; persistence
                               of the saved profile stays here via onSaveEdit. -mx-6 -mb-6
                               cancels the card padding so it sits edge-to-edge like add. */
                            <div className="-mx-6 -mb-6">
                                <AeroShareHandshakeBody
                                    variant="page"
                                    editConnection={{
                                        afid: connectionParams.server || editingProfile?.host || '',
                                        alias: connectionName || connectionParams.username || editingProfile?.name || '',
                                        localFolder: connectionParams.options?.peerLocalFolder ?? editingProfile?.options?.peerLocalFolder ?? '',
                                        customIconUrl: customIconForSave ?? editingProfile?.customIconUrl,
                                    }}
                                    onSaveEdit={savePeerEditedProfile}
                                    onClose={() => { onFormSaved?.(); }}
                                />
                            </div>
                        ) : isMtp ? (
                            /* APPENDIX-DEVICE-PROFILES Phase 2: Portable MTP device form.
                               Detect attached phones, pick one, set default paths, save
                               deviceFingerprint. No password; connect lands in Phase 3. */
                            <div className="space-y-4 pt-1">
                                <div className="flex items-start gap-2.5 p-3 bg-amber-50/70 dark:bg-amber-900/15 border border-amber-200/60 dark:border-amber-800/40 rounded-lg text-xs text-gray-700 dark:text-gray-300">
                                    <Info size={14} className="text-amber-600 dark:text-amber-400 shrink-0 mt-0.5" />
                                    <p>{t('connection.mtpHonesty')}</p>
                                </div>

                                <div>
                                    <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                        <Usb size={14} />
                                        {t('connection.mtpDevice')}
                                    </label>
                                    <div className="flex gap-2 min-w-0">
                                        <button
                                            type="button"
                                            onClick={detectMtpDevices}
                                            disabled={mtpDetecting}
                                            className="shrink-0 px-3 py-2.5 text-sm font-medium rounded-lg bg-slate-100 dark:bg-slate-700 hover:bg-slate-200 dark:hover:bg-slate-600 border border-gray-300 dark:border-gray-600 transition-colors flex items-center gap-1.5 disabled:opacity-50"
                                        >
                                            {mtpDetecting
                                                ? <Loader2 size={14} className="animate-spin" />
                                                : <RefreshCw size={14} />}
                                            {mtpDetecting ? t('connection.mtpDetecting') : t('connection.mtpDetect')}
                                        </button>
                                        <select
                                            value={mtpSelectedDeviceId || ''}
                                            onChange={(e) => selectMtpDevice(e.target.value)}
                                            disabled={mtpDevices.length === 0}
                                            className="flex-1 min-w-0 max-w-full px-3 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm disabled:opacity-60"
                                        >
                                            <option value="">
                                                {mtpDevices.length === 0
                                                    ? t('connection.mtpSelectPlaceholderEmpty')
                                                    : t('connection.mtpSelectPlaceholder')}
                                            </option>
                                            {/* Label: displayName (serial) only. VID:PID stays in the
                                                fingerprint box below so the native option popup
                                                fits the card (live-test LT6). */}
                                            {mtpDevices.map((d) => (
                                                <option key={d.deviceId} value={d.deviceId}>
                                                    {d.displayName}
                                                    {d.serial ? ` (${d.serial})` : ''}
                                                </option>
                                            ))}
                                        </select>
                                    </div>
                                    {mtpDetectError && (
                                        <p className="text-xs text-amber-600 dark:text-amber-400 mt-1.5">{mtpDetectError}</p>
                                    )}
                                    {mtpFingerprint?.canonical && (
                                        <div className="mt-2 p-2.5 rounded-lg bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 text-xs font-mono text-gray-600 dark:text-gray-300 break-all">
                                            <span className="font-sans font-medium text-gray-500 dark:text-gray-400 mr-1.5">
                                                {t('connection.mtpFingerprint')}:
                                            </span>
                                            {mtpFingerprint.canonical}
                                        </div>
                                    )}
                                    {!mtpFingerprint?.canonical && (
                                        <p className="text-xs text-gray-500 dark:text-gray-400 mt-1.5">
                                            {t('connection.mtpSaveRequiresDevice')}
                                        </p>
                                    )}
                                </div>

                                {renderRightColumn({
                                    disabled: !mtpFingerprint?.canonical,
                                    buttonColorClass: 'bg-green-600 hover:bg-green-700',
                                    buttonText: (<><Save size={18} /> {t('common.save')}</>),
                                    remotePathPlaceholder: t('connection.mtpRemotePathPlaceholder'),
                                    connectionNameKey: connectionParams.server || mtpFingerprint?.model || t('connection.mtpDefaultName'),
                                    saveOverride: handleConnectAndSave,
                                    cancelOverride: formOnly ? () => { onFormSaved?.(); } : undefined,
                                })}
                            </div>
                        ) : (
                            <>
                                {/* Unstable provider disclaimer (#308): warn before the
                                    accessible form when the target is flagged stable:false. */}
                                <UnstableProviderNotice provider={formProvider} />

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
                                {protocol === 'uploadcare' ? (
                                    /* Uploadcare Specific Form: public key + secret key */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5">Public API Key</label>
                                                <input
                                                    type="text"
                                                    value={connectionParams.username}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        username: e.target.value,
                                                        server: 'api.uploadcare.com',
                                                        port: 443,
                                                        providerId: connectionParams.providerId || selectedProviderId || 'uploadcare',
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                    placeholder="demopublickey..."
                                                    autoFocus
                                                />
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Dashboard - API Keys - Public key.
                                                </p>
                                            </div>
                                            <div>
                                                {renderPasswordLabel('Secret API Key')}
                                                <div className="relative">
                                                    <input
                                                        type={showPassword ? 'text' : 'password'}
                                                        value={connectionParams.password}
                                                        onChange={(e) => onConnectionParamsChange({
                                                            ...connectionParams,
                                                            password: e.target.value,
                                                            server: 'api.uploadcare.com',
                                                            port: 443,
                                                            providerId: connectionParams.providerId || selectedProviderId || 'uploadcare',
                                                        })}
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                        placeholder="secret_..."
                                                    />
                                                    <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                        {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                    </button>
                                                </div>
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Used only for REST file management via Uploadcare.Simple auth.
                                                </p>
                                            </div>
                                            <div className="bg-emerald-50 dark:bg-emerald-900/10 p-3 rounded-lg border border-emerald-100 dark:border-emerald-900/30 text-xs text-emerald-800 dark:text-emerald-200">
                                                <p className="font-medium mb-1">Flat media library</p>
                                                <p className="opacity-80">
                                                    Uploadcare does not expose native folders; project files are listed at root by UUID.
                                                </p>
                                            </div>
                                        </div>

                                        {renderRightColumn({
                                            disabled: !connectionParams.username || !connectionParams.password,
                                            buttonColorClass: 'bg-emerald-600 hover:bg-emerald-700',
                                            connectionNameKey: getSuggestedConnectionName()
                                        })}
                                    </div>
                                ) : protocol === 'imagekit' ? (
                                    /* ImageKit Specific Form: URL endpoint ID + private API key */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5">URL Endpoint ID</label>
                                                <input
                                                    type="text"
                                                    value={connectionParams.username}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        username: e.target.value,
                                                        server: 'api.imagekit.io',
                                                        port: 443,
                                                        providerId: connectionParams.providerId || selectedProviderId || 'imagekit',
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                    placeholder="your_imagekit_id"
                                                    autoFocus
                                                />
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Dashboard - Developer Options - URL endpoint.
                                                </p>
                                            </div>
                                            <div>
                                                {renderPasswordLabel('Private API Key')}
                                                <div className="relative">
                                                    <input
                                                        type={showPassword ? 'text' : 'password'}
                                                        value={connectionParams.password}
                                                        onChange={(e) => onConnectionParamsChange({
                                                            ...connectionParams,
                                                            password: e.target.value,
                                                            server: 'api.imagekit.io',
                                                            port: 443,
                                                            providerId: connectionParams.providerId || selectedProviderId || 'imagekit',
                                                        })}
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                        placeholder="private_..."
                                                    />
                                                    <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                        {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                    </button>
                                                </div>
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Stored in the local vault and sent only to ImageKit via Basic Auth.
                                                </p>
                                            </div>
                                        </div>

                                        {renderRightColumn({
                                            disabled: !connectionParams.username || !connectionParams.password,
                                            buttonColorClass: 'bg-blue-600 hover:bg-blue-700',
                                            connectionNameKey: getSuggestedConnectionName()
                                        })}
                                    </div>
                                ) : protocol === 'cloudinary' ? (
                                    /* Cloudinary Specific Form: cloud_name + api_key + api_secret */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5">Cloud Name</label>
                                                <input
                                                    type="text"
                                                    value={connectionParams.options?.bucket || ''}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        server: 'api.cloudinary.com',
                                                        port: 443,
                                                        providerId: connectionParams.providerId || selectedProviderId || 'cloudinary',
                                                        options: {
                                                            ...connectionParams.options,
                                                            bucket: e.target.value.trim(),
                                                        },
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
                                                    placeholder="dxz9abc12"
                                                    autoFocus
                                                />
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Found in your Cloudinary Dashboard - Account Details.
                                                </p>
                                            </div>
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5">API Key</label>
                                                <input
                                                    type="text"
                                                    value={connectionParams.username}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        username: e.target.value,
                                                        server: 'api.cloudinary.com',
                                                        port: 443,
                                                        providerId: connectionParams.providerId || selectedProviderId || 'cloudinary',
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
                                                    placeholder="API key"
                                                />
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Dashboard - API Keys - API Key.
                                                </p>
                                            </div>
                                            <div>
                                                {renderPasswordLabel('API Secret')}
                                                <div className="relative">
                                                    <input
                                                        type={showPassword ? 'text' : 'password'}
                                                        value={connectionParams.password}
                                                        onChange={(e) => onConnectionParamsChange({
                                                            ...connectionParams,
                                                            password: e.target.value,
                                                            server: 'api.cloudinary.com',
                                                            port: 443,
                                                            providerId: connectionParams.providerId || selectedProviderId || 'cloudinary',
                                                        })}
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-indigo-500"
                                                        placeholder="API secret"
                                                    />
                                                    <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                        {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                    </button>
                                                </div>
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Stored in the local vault and sent only to Cloudinary via Basic Auth.
                                                </p>
                                            </div>
                                        </div>

                                        {renderRightColumn({
                                            disabled: !connectionParams.username || !connectionParams.password || !connectionParams.options?.bucket,
                                            buttonColorClass: 'bg-indigo-600 hover:bg-indigo-700',
                                            connectionNameKey: getSuggestedConnectionName()
                                        })}
                                    </div>
                                ) : protocol === 'filelu' ? (
                                    /* FileLu Specific Form: API Key */
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
                                                    <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                                connectionNameKey: getSuggestedConnectionName()
                                            })
                                        ) : (
                                            renderRightColumn({
                                                disabled: !connectionParams.password,
                                                buttonColorClass: 'bg-sky-600 hover:bg-sky-700',
                                                connectionNameKey: getSuggestedConnectionName()
                                            })
                                        )}
                                    </div>
                                ) : protocol === 'jottacloud' ? (
                                    /* Jottacloud Specific Form: Login Token only */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                <div className="flex items-center justify-between gap-2 mb-1.5">
                                                    <label className="block text-sm font-medium">{t('connection.jottacloudToken')}</label>
                                                    <a
                                                        href="https://www.jottacloud.com/web/secure"
                                                        target="_blank"
                                                        rel="noopener noreferrer"
                                                        className="inline-flex items-center gap-1 text-xs text-amber-500 hover:text-amber-600 dark:text-amber-400 dark:hover:text-amber-300"
                                                    >
                                                        <KeyRound size={10} />
                                                        {t('connection.generatePersonalLoginToken')}
                                                    </a>
                                                </div>
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
                                                    <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                    /* Drime Cloud Specific Form: API Token only */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                {renderPasswordLabel(t('connection.drimeToken'))}
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
                                                    <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                    /* Koofr Specific Form: Email + App Password */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
                                        <div>
                                            {renderUsernameLabel(t('connection.koofrEmail'))}
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
                                            {renderPasswordLabel(t('connection.koofrAppPassword'))}
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
                                                <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={getSuggestedConnectionName()}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-green-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
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
                                            {renderUsernameLabel(t('connection.emailAccount'))}
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
                                                placeholder="email@example.com"
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            {renderPasswordLabel(t('settings.password'))}
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
                                                <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
                                        <p className="text-xs text-gray-400 mt-2">{t('protocol.opendriveAuthHelp')} (not your OpenDrive API key)</p>
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

                                        {/* OpenDrive default privacy for new items (#252) */}
                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <label className="block text-sm font-medium mb-1.5">
                                                {t('connection.opendriveDefaultPrivacy')}
                                            </label>
                                            <select
                                                value={connectionParams.options?.opendriveDefaultPrivacy || 'private'}
                                                onChange={(e) => onConnectionParamsChange({
                                                    ...connectionParams,
                                                    options: {
                                                        ...connectionParams.options,
                                                        opendriveDefaultPrivacy: e.target.value as 'private' | 'public' | 'hidden',
                                                    },
                                                })}
                                                className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-cyan-500 focus:border-cyan-500"
                                            >
                                                <option value="private">{t('properties.privacyPrivate')}</option>
                                                <option value="public">{t('properties.privacyPublic')}</option>
                                                <option value="hidden">{t('properties.privacyHidden')}</option>
                                            </select>
                                            <p className="text-xs text-gray-400 mt-1.5">{t('connection.opendriveDefaultPrivacyHelp')}</p>
                                        </div>

                                        <div className="pt-3 border-t border-gray-100 dark:border-gray-700/50">
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={t('connection.connectionNameOptional')}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-cyan-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
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
                                    /* GitHub Specific Form: Owner/Repo + PAT */
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

                                            {/* Mode buttons. Issue #215: switching auth method on a
                                                saved GitHub profile is allowed in edit. The handler
                                                clears the password so the operator supplies the new
                                                method's credential (PAT / App .pem / device-flow
                                                token) before saving; the active button stays
                                                non-clickable. */}
                                            <div className="flex gap-1.5">
                                                {(['authorize', 'pat', 'app'] as const).map((mode) => {
                                                    const isActive = connectionParams.options?.githubAuthMode === mode;
                                                    return (
                                                    <button
                                                        key={mode}
                                                        type="button"
                                                        disabled={isActive}
                                                        onClick={() => {
                                                            if (isActive) return;
                                                            onConnectionParamsChange({
                                                                ...connectionParams,
                                                                password: '',
                                                                options: { ...connectionParams.options, githubAuthMode: mode },
                                                            });
                                                        }}
                                                        className={`flex-1 px-2.5 py-2 text-xs font-medium rounded-lg border transition-colors ${
                                                            isActive
                                                                ? 'border-[var(--color-accent)] bg-[var(--color-accent)]/10 text-[var(--color-accent)]'
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
                                                        <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                                                    // PEM read securely in backend: only path crosses IPC
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
                                                            {t('github.pemVaultReady') || 'PEM key found in vault: ready to connect'}
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={t('connection.connectionNameOptional')}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-gray-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
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
                                    /* kDrive Specific Form: Drive ID + API Token (#369) */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: Credentials */}
                                        <div className="space-y-4">
                                        {/* Drive ID (numeric identifier) sits above the API Token
                                            (the secret), mirroring the id-then-secret order used for
                                            S3, per #369. The green link opens the kSuite page whose
                                            address bar shows the numeric Drive ID. */}
                                        <div>
                                            <div className="flex items-center justify-between gap-2 mb-1.5">
                                                <label className="block text-sm font-medium">{t('connection.kdriveDriveId')}</label>
                                                <a
                                                    href="https://ksuite.infomaniak.com/all/kdrive"
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    title={t('connection.kdriveFindDriveIdHint')}
                                                    className="inline-flex items-center gap-1 text-xs text-emerald-500 hover:text-emerald-600 dark:text-emerald-400 dark:hover:text-emerald-300"
                                                >
                                                    <ExternalLink size={10} />
                                                    {t('connection.kdriveFindDriveId')}
                                                </a>
                                            </div>
                                            <input
                                                type="text"
                                                value={connectionParams.options?.drive_id || connectionParams.options?.bucket || ''}
                                                onChange={(e) => {
                                                    const v = e.target.value.replace(/\D/g, '');
                                                    onConnectionParamsChange({
                                                        ...connectionParams,
                                                        options: { ...connectionParams.options, bucket: v, drive_id: v }
                                                    });
                                                }}
                                                className="w-32 px-4 py-2.5 font-mono tracking-wider bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                placeholder="1234567"
                                                inputMode="numeric"
                                                maxLength={10}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            <div className="flex items-center justify-between gap-2 mb-1.5">
                                                <label className="block text-sm font-medium">{t('connection.kdriveToken')}</label>
                                                <a
                                                    href="https://manager.infomaniak.com/v3/ng/profile/user/token/list"
                                                    target="_blank"
                                                    rel="noopener noreferrer"
                                                    className="inline-flex items-center gap-1 text-xs text-amber-500 hover:text-amber-600 dark:text-amber-400 dark:hover:text-amber-300"
                                                >
                                                    <ExternalLink size={10} />
                                                    {t('connection.kdriveCreateToken')}
                                                </a>
                                            </div>
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
                                                />
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={getSuggestedConnectionName()}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || bridgeSaveBlocked || !connectionParams.password || !connectionParams.options?.bucket}
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
                                            {renderUsernameLabel(t('connection.emailAccount'))}
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
                                            {renderPasswordLabel()}
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-blue-500"
                                                    placeholder={t('connection.internxtPasswordPlaceholder')}
                                                />
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.twoFactorCode')}</label>
                                            <TotpCodeInput
                                                value={connectionParams.options?.two_factor_code || ''}
                                                onChange={handleTotpCodeChange}
                                                autoFocus
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={getSuggestedConnectionName()}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
                                        </div>

                                        <div className="pt-2">
                                            {editingProfileId ? (
                                                (() => {
                                                    if (modeChanged) {
                                                        return renderModeChangedFooter();
                                                    }
                                                    const hasFreshTotp = !!connectionParams.options?.two_factor_code;
                                                    return (
                                                        <div className="flex gap-2">
                                                            <button
                                                                onClick={handleConnectAndSave}
                                                                disabled={loading || !connectionParams.username || !connectionParams.password || hasFreshTotp}
                                                                title={hasFreshTotp ? t('connection.saveDisabledTotp') : undefined}
                                                                className={`flex-1 py-3.5 rounded-lg font-medium cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed ${loading ? 'bg-gray-400 text-white cursor-not-allowed' : 'bg-gray-200 hover:bg-gray-300 text-gray-700 dark:bg-gray-700 dark:hover:bg-gray-600 dark:text-gray-200'}`}
                                                            >
                                                                <Save size={18} /> {t('common.save')}
                                                            </button>
                                                            <button
                                                                onClick={handleSaveAndConnect}
                                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                                className={`flex-1 py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2 disabled:opacity-50 ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-blue-600 hover:bg-blue-700'}`}
                                                            >
                                                                {loading ? (
                                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                                ) : (
                                                                    <>{ConnectIcon} {t('connection.saveAndConnect')}</>
                                                                )}
                                                            </button>
                                                        </div>
                                                    );
                                                })()
                                            ) : (
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
                                            )}
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
                                            {renderUsernameLabel(t('connection.emailAccount'))}
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
                                                placeholder={t('settings.filenEmailPlaceholder')}
                                                autoFocus
                                            />
                                        </div>
                                        <div>
                                            {renderPasswordLabel()}
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                    placeholder={t('connection.filenPasswordPlaceholder')}
                                                />
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.twoFactorCode')}</label>
                                            <TotpCodeInput
                                                value={connectionParams.options?.two_factor_code || ''}
                                                onChange={handleTotpCodeChange}
                                                autoFocus
                                            />
                                        </div>

                                        <StoredTotpSecretDisclosure
                                            value={connectionParams.options?.totp_secret || ''}
                                            onChange={(value) => onConnectionParamsChange({
                                                ...connectionParams,
                                                options: { ...connectionParams.options, totp_secret: value || undefined }
                                            })}
                                            accent="emerald"
                                        />

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.filenApiKey')}</label>
                                            <div className="relative">
                                                <input
                                                    type={showFilenApiKey ? 'text' : 'password'}
                                                    value={connectionParams.options?.filen_api_key || ''}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        options: { ...connectionParams.options, filen_api_key: e.target.value || undefined }
                                                    })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm font-mono focus:ring-2 focus:ring-emerald-500 focus:border-emerald-500"
                                                    placeholder={t('connection.filenApiKeyPlaceholder')}
                                                    autoComplete="off"
                                                    spellCheck={false}
                                                />
                                                <button type="button" tabIndex={-1} onClick={() => setShowFilenApiKey(!showFilenApiKey)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showFilenApiKey ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                            <p className="mt-1.5 text-xs text-gray-500 dark:text-gray-400">{t('connection.filenApiKeyHelp')}</p>
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={getSuggestedConnectionName()}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-emerald-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
                                        </div>

                                        <div className="pt-2">
                                            {editingProfileId ? (
                                                (() => {
                                                    if (modeChanged) {
                                                        return renderModeChangedFooter();
                                                    }
                                                    const hasFreshTotp = !!connectionParams.options?.two_factor_code;
                                                    return (
                                                        <div className="flex gap-2">
                                                            <button
                                                                onClick={handleConnectAndSave}
                                                                disabled={loading || !connectionParams.username || !connectionParams.password || hasFreshTotp}
                                                                title={hasFreshTotp ? t('connection.saveDisabledTotp') : undefined}
                                                                className={`flex-1 py-3.5 rounded-lg font-medium cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2 disabled:opacity-50 disabled:cursor-not-allowed ${loading ? 'bg-gray-400 text-white cursor-not-allowed' : 'bg-gray-200 hover:bg-gray-300 text-gray-700 dark:bg-gray-700 dark:hover:bg-gray-600 dark:text-gray-200'}`}
                                                            >
                                                                <Save size={18} /> {t('common.save')}
                                                            </button>
                                                            <button
                                                                onClick={handleSaveAndConnect}
                                                                disabled={loading || !connectionParams.username || !connectionParams.password}
                                                                className={`flex-1 py-3.5 rounded-lg font-medium text-white cursor-pointer shadow-[0_1px_3px_rgba(0,0,0,0.08)] dark:shadow-[0_1px_3px_rgba(0,0,0,0.3)] active:scale-[0.98] transition-all flex items-center justify-center gap-2 disabled:opacity-50 ${loading ? 'bg-gray-400 cursor-not-allowed' : 'bg-emerald-600 hover:bg-emerald-700'}`}
                                                            >
                                                                {loading ? (
                                                                    <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> {t('connection.connecting')}</>
                                                                ) : (
                                                                    <>{ConnectIcon} {t('connection.saveAndConnect')}</>
                                                                )}
                                                            </button>
                                                        </div>
                                                    );
                                                })()
                                            ) : (
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
                                            )}
                                            <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                                <Lock size={12} /> {t('connection.endToEndAes')}
                                            </p>
                                        </div>
                                        </>
                                        )}
                                    </div>
                                ) : protocol === 'backblaze' ? (
                                    /* Backblaze B2 native form */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <div>
                                                {renderUsernameLabel('Application Key ID')}
                                                <input
                                                    type="text"
                                                    value={connectionParams.username}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        username: e.target.value,
                                                        server: 'api.backblazeb2.com',
                                                        port: 443,
                                                        providerId: connectionParams.providerId || selectedProviderId || 'backblaze-native',
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    placeholder="003d90ca9d33900000000001"
                                                    autoFocus
                                                />
                                                <p className="text-xs text-gray-500 mt-1">
                                                    B2 Application Key ID, starts with 003. Generate one from the Backblaze App Keys page.
                                                </p>
                                            </div>
                                            <div>
                                                {renderPasswordLabel('Application Key')}
                                                <div className="relative">
                                                    <input
                                                        type={showPassword ? 'text' : 'password'}
                                                        value={connectionParams.password}
                                                        onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                        className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    />
                                                    <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                        {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                    </button>
                                                </div>
                                                <p className="text-xs text-gray-500 mt-1">Shown only once at creation. Treat it as a password.</p>
                                            </div>
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5">Bucket Name</label>
                                                <input
                                                    type="text"
                                                    value={connectionParams.options?.bucket || ''}
                                                    onChange={(e) => onConnectionParamsChange({
                                                        ...connectionParams,
                                                        server: connectionParams.server || 'api.backblazeb2.com',
                                                        port: connectionParams.port || 443,
                                                        options: { ...(connectionParams.options || {}), bucket: e.target.value },
                                                    })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    placeholder="my-b2-bucket"
                                                />
                                                <p className="text-xs text-gray-500 mt-1">Exact bucket name, case sensitive.</p>
                                            </div>

                                            <div className="bg-red-50 dark:bg-red-900/10 p-3 rounded-lg border border-red-100 dark:border-red-900/30 text-xs text-red-800 dark:text-red-200">
                                                <p className="font-medium mb-1 flex items-center gap-1.5">
                                                    <Info size={12} className="shrink-0" />
                                                    {t('connection.b2NativeAdvantagesTitle')}
                                                </p>
                                                <ul className="opacity-80 space-y-0.5 list-disc list-inside marker:text-red-400">
                                                    <li>{t('connection.b2NativeAdvLargeFiles')}</li>
                                                    <li>{t('connection.b2NativeAdvSoftDelete')}</li>
                                                    <li>{t('connection.b2NativeAdvBucketUsage')}</li>
                                                    <li>{t('connection.b2NativeAdvShareLinks')}</li>
                                                    <li>{t('connection.b2NativeAdvVersions')}</li>
                                                </ul>
                                            </div>
                                        </div>

                                        {renderRightColumn({
                                            disabled: !connectionParams.username || !connectionParams.password || !connectionParams.options?.bucket,
                                            buttonColorClass: 'bg-red-600 hover:bg-red-700',
                                            showCancelSaveAsNew: true,
                                        })}
                                    </div>
                                ) : protocol === 'immich' ? (
                                    /* Immich Specific Form: Server URL + API Key */
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
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={getSuggestedConnectionName()}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-indigo-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
                                        </div>

                                        <div className="pt-3">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || bridgeSaveBlocked || !connectionParams.server || !connectionParams.password}
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
                                    /* MEGA Specific Form (Beta v0.5.0). #369: two-column
                                       layout like every other Quick Connect page. The wide
                                       card gives the MEGA MODES bar room (S4 no longer wraps),
                                       and the shared right column brings Wrappers/Overlays
                                       (Crypt), Remember credentials and Save to API/CMD, which
                                       the legacy single-column form never exposed. */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        {/* LEFT COLUMN: MEGA credentials + connection-backend selector */}
                                        <div className="space-y-4">
                                        <div>
                                            {renderUsernameLabel(t('connection.emailAccount'))}
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
                                            {renderPasswordLabel()}
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    placeholder={t('connection.megaPasswordPlaceholder')}
                                                />
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1.5">{t('connection.twoFactorCode')}</label>
                                            <TotpCodeInput
                                                value={connectionParams.options?.two_factor_code || ''}
                                                onChange={handleTotpCodeChange}
                                                autoFocus
                                            />
                                        </div>

                                        <StoredTotpSecretDisclosure
                                            value={connectionParams.options?.totp_secret || ''}
                                            onChange={(value) => onConnectionParamsChange({
                                                ...connectionParams,
                                                options: { ...connectionParams.options, totp_secret: value || undefined }
                                            })}
                                            accent="red"
                                        />

                                        <div className="space-y-2">
                                            <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide">
                                                {t('connection.megaConnectionMode')}
                                            </label>
                                            <div className="grid grid-cols-2 gap-2">
                                                {(['native', 'megacmd'] as const).map((mode) => {
                                                    const isActive = megaMode === mode;
                                                    // Issue #215 (Ehud): native <-> MEGAcmd is a sub-mode of
                                                    // the same MEGA account on the same `mega` protocol (only
                                                    // `options.mega_mode` differs), so it can be switched and
                                                    // saved in place even while editing a saved profile. The
                                                    // previous edit-lock left the non-active button greyed out,
                                                    // so an existing MEGAcmd profile could never switch to
                                                    // Native API (and vice versa).
                                                    return (
                                                        <button
                                                            key={mode}
                                                            type="button"
                                                            onClick={() => onConnectionParamsChange({
                                                                ...connectionParams,
                                                                options: {
                                                                    ...connectionParams.options,
                                                                    mega_mode: mode,
                                                                },
                                                            })}
                                                            className={`rounded-lg border px-3 py-3 text-left transition-colors ${
                                                                isActive
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
                                        </div>
                                        {/* RIGHT COLUMN: shared block (profile name + icon,
                                            local/remote path, Wrappers/Overlays -> Crypt,
                                            Remember credentials, Save). #369: this is what
                                            brings the transparent Crypt overlay to MEGA
                                            API/CMD, which the legacy single-column form never
                                            exposed. Edit-mode Save stays blocked while a fresh
                                            one-time TOTP is typed (a code must never be
                                            persisted), matching the old custom footer. */}
                                        {renderRightColumn({
                                            disabled: !connectionParams.username || !connectionParams.password || (!!editingProfileId && !!connectionParams.options?.two_factor_code),
                                            buttonColorClass: 'bg-red-600 hover:bg-red-700',
                                            showCancelSaveAsNew: true,
                                            connectionNameKey: t('connection.megaConnectionNamePlaceholder'),
                                            remotePathPlaceholder: t('connection.initialRemotePathMega'),
                                            showE2ENote: 'connection.endToEndEncrypted',
                                        })}
                                    </div>
                                ) : protocol === 'gitlab' ? (
                                    /* GitLab Form: single-column like GitHub */
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
                                                <button type="button" tabIndex={-1} onClick={() => setShowPassword(!showPassword)} className="absolute inset-y-0 right-0 flex items-center px-3 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                                {/* Self-hosted TLS toggle: only show when host looks self-hosted */}
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={t('connection.connectionNameOptional')}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-orange-500 focus:border-transparent"
                                                />
                                                {renderIconPicker()}
                                            </div>
                                        </div>

                                        {/* Connect Button */}
                                        <div className="pt-2">
                                            <button
                                                onClick={handleConnectAndSave}
                                                disabled={loading || bridgeSaveBlocked || !connectionParams.server || !connectionParams.password}
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
                                            {renderUsernameLabel(t('connection.emailAccount'))}
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
                                            {renderPasswordLabel()}
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className="w-full px-4 py-2.5 pr-12 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-purple-500"
                                                    placeholder={t('connection.password')}
                                                />
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
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
                                            <div>
                                                <label className="block text-sm font-medium mb-1.5 flex items-center gap-1.5">
                                                    <Save size={14} />
                                                    {t('connection.connectionNameOptional')}
                                                </label>
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder={t('connection.connectionNameOptional')}
                                                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-purple-500 focus:border-transparent"
                                                />
                                            </div>
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
                                ) : protocol === 'webdav' && selectedProviderId === 'megacmd-webdav' ? (
                                    /* MEGAcmd local anonymous WebDAV */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : 'space-y-4 pt-2'}>
                                        <div className="space-y-4">
                                            <CollapsibleSetupBox
                                                key="megacmd-webdav-setup"
                                                title="Setup MEGAcmd first"
                                                tone="red"
                                                bridgeState={bridgeUiState}
                                                isBridge
                                            >
                                                <ol className="list-decimal list-inside space-y-1">
                                                    {(selectedProvider?.setupInstructions || []).map((step) => (
                                                        <li key={step}>{step}</li>
                                                    ))}
                                                </ol>
                                            </CollapsibleSetupBox>
                                            <div>
                                                <div className="flex items-center justify-between gap-2 mb-1.5">
                                                    <label className="block text-sm font-medium">{t('connection.endpointUrl')}</label>
                                                    <button
                                                        type="button"
                                                        onClick={handleFetchMegaWebdavUrl}
                                                        disabled={megaWebdavFetching}
                                                        className="inline-flex items-center gap-1 text-xs text-red-600 hover:text-red-700 dark:text-red-400 dark:hover:text-red-300 disabled:opacity-50 disabled:cursor-not-allowed"
                                                        title="Run mega-webdav / and fill the URL automatically"
                                                    >
                                                        {megaWebdavFetching ? <Loader2 size={11} className="animate-spin" /> : <RefreshCw size={11} />}
                                                        Fetch URL
                                                    </button>
                                                </div>
                                                <input
                                                    type="url"
                                                    value={connectionParams.server}
                                                    onChange={(e) => {
                                                        const endpoint = e.target.value;
                                                        onConnectionParamsChange({
                                                            ...connectionParams,
                                                            server: endpoint,
                                                            username: '',
                                                            password: '',
                                                            port: parseEndpointPort(endpoint, connectionParams.port || 4443),
                                                            options: { ...(connectionParams.options || {}), anonymous: true },
                                                        });
                                                    }}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                                    placeholder="http://127.0.0.1:4443/"
                                                    autoFocus
                                                />
                                                {megaWebdavError && (
                                                    <p className="text-xs text-red-600 dark:text-red-400 mt-1">{megaWebdavError}</p>
                                                )}
                                                <p className="text-xs text-gray-500 mt-1">
                                                    Username and Password are intentionally omitted. AeroFTP runs "mega-webdav /" itself on connect to start the bridge and read this address: leaving it blank is fine. Fetch URL is an optional manual refresh; change the port here if MEGAcmd uses a custom one.
                                                </p>
                                            </div>
                                        </div>

                                        {renderRightColumn({
                                            // Endpoint may be left blank: buildProviderParams runs
                                            // `mega-webdav /` on connect and fills it itself (#215).
                                            disabled: false,
                                            buttonColorClass: 'bg-red-600 hover:bg-red-700',
                                            showCancelSaveAsNew: true,
                                        })}
                                    </div>
                                ) : (
                                    /* Traditional connection fields (FTP/S3/WebDAV): 2-column layout in formOnly */
                                    <div className={formOnly ? 'grid grid-cols-2 gap-6 items-start' : ''}>
                                    {/* LEFT COLUMN: Connection fields */}
                                    <div className="space-y-3">
                                        {/* Provider-specific setup steps (S3Drive, Filen Desktop S3, etc.).
                                            Collapsible; for local-bridge providers it auto-collapses once
                                            the bridge is active (idea D). bridgeUiState is undefined for
                                            non-bridge providers, so those just default to open. */}
                                        {selectedProvider?.setupInstructions && selectedProvider.setupInstructions.length > 0 && !editingProfileId && (
                                            <CollapsibleSetupBox
                                                key={`setup-${selectedProviderId || selectedProvider.name}`}
                                                title={t('protocol.setupSteps', { provider: selectedProvider.name })}
                                                tone="amber"
                                                bridgeState={bridgeUiState}
                                                isBridge={isBridgeMode}
                                            >
                                                <ol className="list-decimal list-inside space-y-1">
                                                    {selectedProvider.setupInstructions.map((step) => (
                                                        <li key={step}>{step}</li>
                                                    ))}
                                                </ol>
                                                {selectedProvider.helpUrl && (
                                                    <a
                                                        href={selectedProvider.helpUrl}
                                                        target="_blank"
                                                        rel="noopener noreferrer"
                                                        className="mt-2 inline-flex items-center gap-1 underline hover:no-underline"
                                                    >
                                                        <ExternalLink size={11} />
                                                        {t('protocol.openProviderDocs', { provider: selectedProvider.name })}
                                                    </a>
                                                )}
                                            </CollapsibleSetupBox>
                                        )}
                                        {(() => {
                                            const isNonGenericS3 = protocol === 's3' && selectedProviderId && !getProviderById(selectedProviderId)?.isGeneric;
                                            const hasPresetServer = selectedProvider && selectedProvider.defaults?.server && !selectedProvider.isGeneric;
                                            // Reinforce the preset: a preset endpoint stays hidden from the
                                            // main form in EVERY mode (add and edit), so switching modes in
                                            // edit behaves like add and the managed endpoint is never shown
                                            // as an editable top-level field. It remains reachable, unlock-
                                            // gated, in Advanced Options below for the rare case the provider
                                            // changes its endpoint before the app ships an update.
                                            const hideServerField = hasPresetServer;
                                            // Z.4.5 R1: providers that mark `serverLocked: true` keep
                                            // the server/port row hidden in EVERY mode (including edit),
                                            // because their endpoint is fully managed by AeroFTP and
                                            // exposing it would confuse rather than help.
                                            const serverLocked = !!selectedProvider?.serverLocked;
                                            if (isNonGenericS3) return null;
                                            if (hideServerField) return null; // Shown in Advanced Options below
                                            if (serverLocked) return null;
                                            if (selectedProviderId === 'infinicloud') return null; // Rendered inside InfiniCloud mode selector block
                                            // AeroShare friend: the AeroFTP-ID is the identity, fixed by the
                                            // handshake and not user-editable, and there is no port. Show it
                                            // read-only on its own row (no Port column).
                                            if (isPeer) return (
                                                <div>
                                                    <label className="block text-sm font-medium mb-1.5">{getServerLabel()}</label>
                                                    <input
                                                        type="text"
                                                        value={connectionParams.server}
                                                        readOnly
                                                        className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm font-mono opacity-80 cursor-not-allowed"
                                                    />
                                                </div>
                                            );
                                            return (
                                                <div className="flex gap-2">
                                                    <div className="flex-1 min-w-0">
                                                        <div className="flex items-center gap-2 mb-1.5">
                                                            <label className="block text-sm font-medium">
                                                                {getServerLabel()}
                                                            </label>
                                                        </div>
                                                        <input
                                                            type="text"
                                                            value={connectionParams.server}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, server: e.target.value })}
                                                            className={`w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm ${hasPresetServer ? 'opacity-70 cursor-not-allowed' : ''}`}
                                                            placeholder={getServerPlaceholder()}
                                                            readOnly={!!hasPresetServer}
                                                        />
                                                    </div>
                                                    <div className="w-24">
                                                        <label className="block text-sm font-medium mb-1.5">{t('connection.port')}</label>
                                                        <input
                                                            type="number"
                                                            value={connectionParams.port || getDefaultPort(protocol)}
                                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, port: parseInt(e.target.value) || getDefaultPort(protocol) })}
                                                            className={`w-full px-3 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center ${hasPresetServer ? 'opacity-70 cursor-not-allowed' : ''}`}
                                                            min={1}
                                                            max={65535}
                                                            readOnly={!!hasPresetServer}
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
                                                        // Issue #215: InfiniCloud WebDAV <-> API is the same
                                                        // account reached two ways, like MEGA native/MEGAcmd, so
                                                        // it can be switched and saved in place while editing.
                                                        // The previous edit-lock left a saved profile stuck on
                                                        // whichever mode it was created with.
                                                        return (
                                                            <button
                                                                key={mode}
                                                                type="button"
                                                                onClick={() => onConnectionParamsChange({
                                                                    ...connectionParams,
                                                                    options: {
                                                                        ...connectionParams.options,
                                                                        infinicloud_mode: mode,
                                                                        ...(mode === 'webdav' ? { apiKey: undefined } : {}),
                                                                    },
                                                                })}
                                                                className={`rounded-lg border px-3 py-3 text-left transition-colors ${
                                                                    isActive
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
                                            {renderUsernameLabel()}
                                                <input
                                                    type="text"
                                                    value={connectionParams.username}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, username: e.target.value })}
                                                    className="w-full px-4 py-2.5 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder={getUsernamePlaceholder()}
                                                    maxLength={isFilenDesktopBridge ? FILEN_BRIDGE_MAX_LENGTH : undefined}
                                                    aria-invalid={!!filenBridgeCredentialIssue}
                                                />
                                        </div>
                                        {/* Password: not applicable to an AeroShare friend (peer carries
                                            no password; the binding rides in options.peer*). */}
                                        {!isPeer && (
                                        <div>
                                            {renderPasswordLabel()}
                                            <div className="relative">
                                                <input
                                                    type={showPassword ? 'text' : 'password'}
                                                    value={connectionParams.password}
                                                    onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                                    className={`w-full px-4 py-2.5 ${isFilenDesktopBridge ? 'pr-20' : 'pr-12'} bg-gray-50 dark:bg-gray-700 border rounded-lg text-sm ${filenBridgeCredentialIssue ? 'border-amber-500' : 'border-gray-300 dark:border-gray-600'}`}
                                                    placeholder={t('connection.passwordPlaceholder')}
                                                    maxLength={isFilenDesktopBridge ? FILEN_BRIDGE_MAX_LENGTH : undefined}
                                                    aria-invalid={!!filenBridgeCredentialIssue}
                                                />
                                                {isFilenDesktopBridge && (
                                                    <InlinePasswordGenerator
                                                        preset="compatible"
                                                        onGenerated={(value) => onConnectionParamsChange({ ...connectionParams, password: value })}
                                                        className="absolute right-9 top-1/2 -translate-y-1/2"
                                                    />
                                                )}
                                                <button tabIndex={-1} type="button" onClick={() => setShowPassword(!showPassword)} className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300">
                                                    {showPassword ? <EyeOff size={18} /> : <Eye size={18} />}
                                                </button>
                                            </div>
                                            {isFilenDesktopBridge && (
                                                <p className={`mt-1.5 text-xs ${filenBridgeCredentialIssue ? 'text-amber-600 dark:text-amber-400' : 'text-gray-500 dark:text-gray-400'}`}>
                                                    {filenBridgeCredentialIssue
                                                        ? t('connection.filenBridgeCredentialRejected')
                                                        : t('connection.filenBridgeCredentialRules', { max: FILEN_BRIDGE_MAX_LENGTH, rejected: FILEN_BRIDGE_REJECTED_CHARACTERS })}
                                                </p>
                                            )}
                                        </div>
                                        )}

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
                                                        <label className="block text-sm font-medium mb-1.5">{getServerLabel()}</label>
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
                                        {/* Advanced Options: hidden server/port for preset WebDAV, hidden endpoint for preset S3 */}
                                        {(() => {
                                            // Surface the accordion in EVERY mode (add and edit): the main
                                            // Server/Port row is hidden for a preset provider in both, so edit
                                            // must expose the managed endpoint here too (collapsed, unlock-
                                            // gated), matching add mode and keeping it overridable.
                                            const hasPresetServer = selectedProvider && selectedProvider.defaults?.server && !selectedProvider.isGeneric;
                                            // Z.4.5 R1: providers marked `serverLocked` ALWAYS surface the
                                            // accordion (even in edit mode) because the main Server/Port
                                            // row is unconditionally hidden for them. Without this branch
                                            // the operator would have no way to inspect or override the
                                            // managed endpoint at all.
                                            const serverLocked = !!selectedProvider?.serverLocked;
                                            const showForLocked = serverLocked && selectedProvider?.defaults?.server;
                                            if ((!hasPresetServer && !showForLocked) || protocol === 's3') return null;
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
                                                                    <label className="block text-xs font-medium mb-1 text-gray-500">{getServerLabel()}</label>
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
                    {/* Card footer: provider connectVia + links */}
                    {(() => {
                        if (!selectedProvider || selectedProvider.isGeneric) return null;
                        const proto = protocol === 's3' ? 'S3' : protocol === 'webdav' ? 'WebDAV' : null;
                        if (!proto) return null;
                        const footerText = t('protocol.connectVia', { name: selectedProvider.name, protocol: proto });
                        return (
                            <div className="-mx-6 -mb-6 mt-4 px-6 py-3 bg-gray-50/80 dark:bg-white/[0.02] border-t border-gray-100 dark:border-gray-700/50 rounded-b-lg">
                                <div className="flex items-center flex-wrap gap-x-2 gap-y-1 text-xs">
                                    <span className="inline-flex items-center gap-1.5 text-gray-500 dark:text-gray-400">
                                        <Cloud size={12} />
                                        {footerText}
                                    </span>
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

                {/* Skip to File Manager: accessible via status bar AeroFile button */}
            </div> {/* Close grid */}

            {/* Export/Import Dialog */}
            {showExportImport && (
                <ExportImportDialog
                    servers={servers}
                    onImport={async (newServers) => {
                        // Read ground truth from the partition-aware vault to avoid stale state
                        let currentServers = await loadSavedServerProfiles();
                        if (currentServers.length === 0) currentServers = servers;
                        const updated = [...currentServers, ...newServers];
                        setServers(updated);
                        await storeSavedServerProfiles(updated).catch(() => { });
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
            {showIconPicker && (
                <IconPickerDialog
                    onSelect={(dataUrl) => setCustomIconForSave(dataUrl)}
                    onClose={() => setShowIconPicker(false)}
                    currentIcon={customIconForSave || faviconForSave}
                    detectedFavicon={faviconForSave}
                    customIconsOnly={hasProviderLogoForSave}
                    onRescan={async () => {
                        // Live re-detection: re-runs the same Tauri commands as
                        // the auto-detection hook, so a favicon that changed on
                        // the server after the first connection shows up here.
                        // Operates on the active FTP/provider state, so this only
                        // returns a meaningful result when the editing server
                        // has a live connection: otherwise it returns null.
                        const proto = connectionParams.protocol || 'ftp';
                        const SERVER_PROTOCOLS = new Set(['ftp', 'ftps']);
                        const PROVIDER_PROTOCOLS = new Set(['sftp', 's3', 'webdav']);
                        if (!SERVER_PROTOCOLS.has(proto) && !PROVIDER_PROTOCOLS.has(proto)) return null;
                        const command = PROVIDER_PROTOCOLS.has(proto) ? 'detect_provider_favicon' : 'detect_server_favicon';
                        const searchPaths: string[] = [];
                        const initial = editingProfile?.initialPath || quickConnectDirs?.remoteDir;
                        if (initial) searchPaths.push(initial);
                        if (!searchPaths.includes('/')) searchPaths.push('/');
                        try {
                            return await invoke<string | null>(command, { searchPaths });
                        } catch (e) {
                            logger.warn('rescan-favicon failed', e);
                            return null;
                        }
                    }}
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
                                            // Password left empty: backend injects held token during connect
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
        {/* Provider independence disclaimer: outside formOnlyMaxW container */}
        {(() => {
            const disclaimerProvider = selectedProvider ?? (protocol ? getProviderById(protocol) : null);
            const nameMap: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud Drive', zohoworkdrive: 'Zoho WorkDrive', yandexdisk: 'Yandex Disk', filen: 'Filen', internxt: 'Internxt', kdrive: 'kDrive', jottacloud: 'Jottacloud', drime: 'Drime Cloud', koofr: 'Koofr', opendrive: 'OpenDrive', github: 'GitHub', gitlab: 'GitLab', pixelunion: 'PixelUnion' };
            const providerName = disclaimerProvider?.name
                || nameMap[connectionParams.providerId || ''] || nameMap[protocol || ''];
            if (!providerName || (disclaimerProvider?.isGeneric && !connectionParams.providerId)) return null;
            const contactProtocols = new Set(['zohoworkdrive', 'koofr', 'jottacloud', 'infinicloud', 'jianguoyun']);
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
