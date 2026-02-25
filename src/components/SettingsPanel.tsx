import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { sendNotification } from '@tauri-apps/plugin-notification';
import { readFile } from '@tauri-apps/plugin-fs';
import { X, Settings, Server, Upload, Download, Palette, Trash2, Edit, Plus, FolderOpen, Wifi, FileCheck, Cloud, ExternalLink, Key, Clock, Shield, Lock, Eye, EyeOff, ShieldCheck, AlertCircle, CheckCircle2, MonitorCheck, Power, Sun, Moon, Monitor, Image, Shapes } from 'lucide-react';
import type { Theme } from '../hooks/useTheme';
import { getEffectiveTheme } from '../hooks/useTheme';
import { useIconTheme } from '../hooks/useIconTheme';
import { getIconThemeProvider, type IconTheme } from '../utils/iconThemes';
import { enable as enableAutostart, disable as disableAutostart, isEnabled as isAutostartEnabled } from '@tauri-apps/plugin-autostart';
import { ServerProfile, isOAuthProvider, isFourSharedProvider, ProviderType } from '../types';
import { LanguageSelector } from './LanguageSelector';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { ExportImportDialog } from './ExportImportDialog';
import { ConfirmDialog } from './Dialogs';
import { ImportExportIcon } from './icons/ImportExportIcon';
import { LOCK_SCREEN_PATTERNS } from './LockScreen';
import { APP_BACKGROUND_PATTERNS, APP_BACKGROUND_KEY, DEFAULT_APP_BACKGROUND } from '../utils/appBackgroundPatterns';
import { TotpSetup } from './TotpSetup';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';

// Protocol colors for avatar (same as SavedServers)
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
    fourshared: 'from-blue-500 to-blue-400',
};

// Get display info for a server (matches SavedServers sidebar schema)
const getServerDisplayInfo = (server: ServerProfile) => {
    const protocol = server.protocol || 'ftp';
    const isOAuth = isOAuthProvider(protocol as ProviderType) || isFourSharedProvider(protocol as ProviderType);

    if (isOAuth) {
        const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', fourshared: '4shared', zohoworkdrive: 'Zoho WorkDrive' };
        return `OAuth — ${server.username || providerNames[protocol] || protocol}`;
    }

    if (protocol === 'filen') {
        return `E2E AES-256 — ${server.username}`;
    }

    if (protocol === 'mega') {
        return `E2E AES-128 — ${server.username}`;
    }

    if (protocol === 's3') {
        const bucket = server.options?.bucket || 'S3';
        const host = server.host?.replace(/^https?:\/\//, '') || '';
        const provider = host.includes('cloudflarestorage') ? 'Cloudflare R2'
            : host.includes('backblazeb2') ? 'Backblaze B2'
                : host.includes('amazonaws') ? 'AWS S3'
                    : host.includes('wasabisys') ? 'Wasabi'
                        : host.includes('digitaloceanspaces') ? 'DigitalOcean'
                            : host.split('.')[0];
        return `${bucket} — ${provider}`;
    }

    if (protocol === 'webdav') {
        return `${server.username}@${server.host?.replace(/^https?:\/\//, '')}`;
    }

    return `${server.username}@${server.host}:${server.port}`;
};

import type { UpdateInfo } from '../hooks/useAutoUpdate';
import { useI18n, Language, AVAILABLE_LANGUAGES } from '../i18n';
import { openUrl } from '../utils/openUrl';

// Operation types for activity log - must match useActivityLog.ts
type ActivityLogOperation = 'CONNECT' | 'DISCONNECT' | 'UPLOAD' | 'DOWNLOAD' | 'DELETE' | 'RENAME' | 'MOVE' | 'MKDIR' | 'NAVIGATE' | 'UPDATE' | 'ERROR' | 'INFO' | 'SUCCESS';

interface ActivityLogCallback {
    logRaw: (messageKey: string, operation: ActivityLogOperation, params: Record<string, string | number>, status: 'running' | 'success' | 'error') => void;
}

interface SettingsPanelProps {
    isOpen: boolean;
    onClose: () => void;
    onOpenCloudPanel?: () => void;
    onActivityLog?: ActivityLogCallback;
    initialTab?: TabId;
    onServersChanged?: () => void;
    theme?: Theme;
    setTheme?: (t: Theme) => void;
}

// Settings storage key
const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';
const SERVERS_KEY = 'aeroftp-saved-servers';
const OAUTH_SETTINGS_KEY = 'aeroftp_oauth_settings';

interface OAuthSettings {
    googledrive: { clientId: string; clientSecret: string };
    dropbox: { clientId: string; clientSecret: string };
    onedrive: { clientId: string; clientSecret: string };
    box: { clientId: string; clientSecret: string };
    pcloud: { clientId: string; clientSecret: string };
    fourshared: { clientId: string; clientSecret: string };
}

const defaultOAuthSettings: OAuthSettings = {
    googledrive: { clientId: '', clientSecret: '' },
    dropbox: { clientId: '', clientSecret: '' },
    onedrive: { clientId: '', clientSecret: '' },
    box: { clientId: '', clientSecret: '' },
    pcloud: { clientId: '', clientSecret: '' },
    fourshared: { clientId: '', clientSecret: '' },
};

interface AppSettings {
    // General
    defaultLocalPath: string;
    showHiddenFiles: boolean;
    confirmBeforeDelete: boolean;
    rememberLastFolder: boolean;
    doubleClickAction: 'preview' | 'download';
    // Connection
    timeoutSeconds: number;
    tlsVersion: 'auto' | '1.2' | '1.3';
    reconnectAttempts: number;
    reconnectDelay: number;
    ftpMode: 'passive' | 'active';
    // Transfers
    maxConcurrentTransfers: number;
    retryCount: number;
    // File Handling
    fileExistsAction: 'ask' | 'overwrite' | 'skip' | 'rename' | 'resume' | 'overwrite_if_newer' | 'overwrite_if_different' | 'skip_if_identical';
    preserveTimestamps: boolean;
    transferMode: 'auto' | 'ascii' | 'binary';
    // UI
    showStatusBar: boolean;
    compactMode: boolean;
    showSystemMenu: boolean;
    fontSize: 'small' | 'medium' | 'large';
    // Columns
    visibleColumns: string[];
    // File browser
    sortFoldersFirst: boolean;
    showFileExtensions: boolean;
    // Notifications
    showToastNotifications: boolean;
    // Privacy
    analyticsEnabled: boolean;
    // Startup
    launchOnStartup: boolean;
}

const defaultSettings: AppSettings = {
    defaultLocalPath: '',
    showHiddenFiles: true,  // Developer-first: show all files by default
    confirmBeforeDelete: true,
    rememberLastFolder: true,
    doubleClickAction: 'preview',
    timeoutSeconds: 30,
    tlsVersion: 'auto',
    reconnectAttempts: 3,
    reconnectDelay: 5,
    ftpMode: 'passive',
    maxConcurrentTransfers: 5,
    retryCount: 3,
    fileExistsAction: 'ask',
    preserveTimestamps: true,
    transferMode: 'auto',
    showStatusBar: true,
    compactMode: false,
    showSystemMenu: false,
    fontSize: 'medium',
    visibleColumns: ['name', 'size', 'type', 'permissions', 'modified'],
    sortFoldersFirst: true,
    showFileExtensions: true,
    showToastNotifications: false,  // Default off - use Activity Log instead
    analyticsEnabled: false,
    launchOnStartup: false,
};

type TabId = 'general' | 'connection' | 'servers' | 'transfers' | 'filehandling' | 'cloudproviders' | 'ui' | 'security' | 'backup' | 'privacy';

// Check Update Button with loading animation and Activity Log support
interface CheckUpdateButtonProps {
    onActivityLog?: ActivityLogCallback;
}

const CheckUpdateButton: React.FC<CheckUpdateButtonProps> = ({ onActivityLog }) => {
    const t = useTranslation();
    const [isChecking, setIsChecking] = useState(false);

    const handleCheck = async () => {
        logger.debug('[CheckUpdateButton] handleCheck called');
        setIsChecking(true);

        // Log start of update check to Activity Log
        onActivityLog?.logRaw('activity.update_checking', 'UPDATE', { action: 'manual_check' }, 'running');

        try {
            logger.debug('[CheckUpdateButton] Invoking check_update...');
            const info = await invoke<UpdateInfo>('check_update');
            logger.debug('[CheckUpdateButton] Result:', info);

            if (info.has_update) {
                // Log update available to Activity Log
                onActivityLog?.logRaw('activity.update_available', 'UPDATE', {
                    version: info.latest_version || 'unknown',
                    format: info.install_format
                }, 'success');

                try {
                    await sendNotification({
                        title: t('settings.updateAvailable'),
                        body: `AeroFTP v${info.latest_version} is ready (.${info.install_format})`
                    });
                } catch {
                    alert(t('settings.updateAvailableAlert', {
                        version: info.latest_version || '',
                        format: info.install_format || '',
                        url: info.download_url || 'https://github.com/axpnet/aeroftp/releases/latest'
                    }));
                }
            } else {
                // Log no update available to Activity Log
                onActivityLog?.logRaw('activity.update_uptodate', 'UPDATE', {
                    version: info.current_version
                }, 'success');

                try {
                    await sendNotification({
                        title: t('settings.upToDate'),
                        body: t('settings.runningLatest', { version: info.current_version })
                    });
                } catch {
                    alert(t('settings.upToDateAlert', { version: info.current_version }));
                }
            }
        } catch (err) {
            console.error('[CheckUpdateButton] Update check failed:', err);
            // Log error to Activity Log
            onActivityLog?.logRaw('activity.update_error', 'UPDATE', { error: String(err) }, 'error');
            alert(t('settings.updateCheckFailedAlert', { error: String(err) }));
        } finally {
            setIsChecking(false);
        }
    };

    return (
        <button
            type="button"
            onClick={handleCheck}
            disabled={isChecking}
            className="px-4 py-2 bg-blue-100 dark:bg-blue-900 text-blue-700 dark:text-blue-300 rounded-lg text-sm hover:bg-blue-200 dark:hover:bg-blue-800 transition-colors flex items-center gap-2 disabled:opacity-50"
        >
            <svg className={`w-4 h-4 ${isChecking ? 'animate-spin' : ''}`} fill="none" stroke="currentColor" viewBox="0 0 24 24">
                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2} d="M4 4v5h.582m15.356 2A8.001 8.001 0 004.582 9m0 0H9m11 11v-5h-.581m0 0a8.003 8.003 0 01-15.357-2m15.357 2H15" />
            </svg>
            {isChecking ? t('settings.checking') : t('settings.checkForUpdates')}
        </button>
    );
};

export const SettingsPanel: React.FC<SettingsPanelProps> = ({ isOpen, onClose, onOpenCloudPanel, onActivityLog, initialTab, onServersChanged, theme: appThemeProp = 'auto', setTheme: setAppTheme }) => {
    const [activeTab, setActiveTab] = useState<TabId>(initialTab || 'general');
    const [appearanceSubTab, setAppearanceSubTab] = useState<'theme' | 'icons' | 'interface' | 'backgrounds'>('theme');
    const { iconTheme, setIconTheme } = useIconTheme();

    // Reset to initialTab when panel opens with a specific tab
    useEffect(() => {
        if (isOpen && initialTab) {
            setActiveTab(initialTab);
        }
    }, [isOpen, initialTab]);
    const [settings, setSettings] = useState<AppSettings>(defaultSettings);
    const [oauthSettings, setOauthSettings] = useState<OAuthSettings>(defaultOAuthSettings);
    const [servers, setServers] = useState<ServerProfile[]>([]);
    const [editingServer, setEditingServer] = useState<ServerProfile | null>(null);
    const [showEditPassword, setShowEditPassword] = useState(false);
    const [showExportImport, setShowExportImport] = useState(false);
    const [hasChanges, setHasChanges] = useState(false);
    const [saveState, setSaveState] = useState<'idle' | 'saving' | 'saved'>('idle');

    // Master Password state
    const [masterPasswordStatus, setMasterPasswordStatus] = useState<{
        is_set: boolean;
        is_locked: boolean;
        timeout_seconds: number;
    } | null>(null);
    const [newMasterPassword, setNewMasterPassword] = useState('');
    const [confirmMasterPassword, setConfirmMasterPassword] = useState('');
    const [currentMasterPassword, setCurrentMasterPassword] = useState('');
    const [autoLockTimeout, setAutoLockTimeout] = useState(0); // minutes (0 = disabled)
    const [showMasterPassword, setShowMasterPassword] = useState(false);
    const [masterPasswordError, setMasterPasswordError] = useState('');
    const [showOAuthSecrets, setShowOAuthSecrets] = useState(false);

    // TOTP 2FA state
    const [showTotpSetup, setShowTotpSetup] = useState(false);
    const [totpEnabled, setTotpEnabled] = useState(false);
    const [totpDisableCode, setTotpDisableCode] = useState('');
    const [totpDisableError, setTotpDisableError] = useState('');
    const [showTotpDisable, setShowTotpDisable] = useState(false);

    // Badge extension feedback
    const [badgeFeedback, setBadgeFeedback] = useState<{ type: 'success' | 'error'; message: string } | null>(null);
    const [masterPasswordSuccess, setMasterPasswordSuccess] = useState('');
    const [isSettingPassword, setIsSettingPassword] = useState(false);
    const [passwordBtnState, setPasswordBtnState] = useState<'idle' | 'encrypting' | 'done'>('idle');

    // Vault Backup state
    const [vaultEntriesCount, setVaultEntriesCount] = useState(0);
    const [keystoreExportPassword, setKeystoreExportPassword] = useState('');
    const [keystoreExportConfirm, setKeystoreExportConfirm] = useState('');
    const [showKeystoreExportPassword, setShowKeystoreExportPassword] = useState(false);
    const [keystoreExporting, setKeystoreExporting] = useState(false);
    const [keystoreImporting, setKeystoreImporting] = useState(false);
    const [keystoreImportPassword, setKeystoreImportPassword] = useState('');
    const [showKeystoreImportPassword, setShowKeystoreImportPassword] = useState(false);
    const [keystoreImportMerge, setKeystoreImportMerge] = useState<'skip' | 'overwrite'>('skip');
    const [keystoreMetadata, setKeystoreMetadata] = useState<{
        exportDate: string;
        aeroftpVersion: string;
        entriesCount: number;
        categories: {
            serverCredentials: number;
            serverProfiles: number;
            aiKeys: number;
            oauthTokens: number;
            configEntries: number;
        };
    } | null>(null);
    const [keystoreImportFilePath, setKeystoreImportFilePath] = useState<string | null>(null);
    const [keystoreMessage, setKeystoreMessage] = useState<{ type: 'success' | 'error'; text: string } | null>(null);
    const [removePasswordConfirm, setRemovePasswordConfirm] = useState(false);

    // i18n hook
    const { language, setLanguage, t, availableLanguages } = useI18n();

    // Load settings on open
    useEffect(() => {
        if (isOpen) {
            (async () => {
                try {
                    const saved = await secureGetWithFallback<AppSettings>(SETTINGS_VAULT_KEY, SETTINGS_KEY);
                    if (saved) {
                        setSettings({ ...defaultSettings, ...saved });
                        // One-way idempotent migration to vault with plaintext cleanup
                        secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, { ...defaultSettings, ...saved }).catch(() => { });
                    }

                    // Load servers: sync fallback first, then try vault
                    const savedServers = localStorage.getItem(SERVERS_KEY);
                    if (savedServers) setServers(JSON.parse(savedServers));
                    const vaultServers = await secureGetWithFallback<ServerProfile[]>('server_profiles', SERVERS_KEY);
                    if (vaultServers && vaultServers.length > 0) setServers(vaultServers);

                    // Load OAuth settings from secure credential store (fallback: localStorage)
                    const loadOAuthFromStore = async () => {
                        const providers = ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'fourshared'] as const;
                        const loaded = { ...defaultOAuthSettings };
                        for (const p of providers) {
                            try {
                                const id = await invoke<string>('get_credential', { account: `oauth_${p}_client_id` });
                                const secret = await invoke<string>('get_credential', { account: `oauth_${p}_client_secret` });
                                loaded[p] = { clientId: id || '', clientSecret: secret || '' };
                            } catch {
                                // SEC: No localStorage fallback — credentials must be in vault.
                                // Migration wizard handles legacy data on first launch.
                            }
                        }
                        setOauthSettings(loaded);
                    };
                    await loadOAuthFromStore();

                    const savedAnalytics = localStorage.getItem('analytics_enabled');
                    if (savedAnalytics !== null) {
                        setSettings(prev => ({ ...prev, analyticsEnabled: savedAnalytics === 'true' }));
                    }

                    // Sync autostart state from OS (authoritative source)
                    isAutostartEnabled()
                        .then(enabled => setSettings(prev => ({ ...prev, launchOnStartup: enabled })))
                        .catch(e => logger.debug('Autostart status check failed:', e));

                    // Load credential store status
                    invoke<{ master_mode: boolean; is_locked: boolean; timeout_seconds: number; accounts_count?: number }>('get_credential_store_status')
                        .then(status => {
                            setMasterPasswordStatus({
                                is_set: status.master_mode,
                                is_locked: status.is_locked,
                                timeout_seconds: Number(status.timeout_seconds),
                            });
                            if (status.timeout_seconds > 0) {
                                setAutoLockTimeout(Math.floor(Number(status.timeout_seconds) / 60));
                            }
                            if (typeof status.accounts_count === 'number') {
                                setVaultEntriesCount(status.accounts_count);
                            }
                        })
                        .catch(console.error);

                    // Load TOTP 2FA status
                    invoke<boolean>('totp_status')
                        .then(enabled => setTotpEnabled(enabled))
                        .catch(() => setTotpEnabled(false));
                } catch { }
            })();
        }
    }, [isOpen]);

    const flashSaved = () => {
        setSaveState('saving');
        setTimeout(() => setSaveState('saved'), 800);
        setTimeout(() => setSaveState('idle'), 1800);
    };

    const handleSave = async () => {
        await secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, settings);
        localStorage.setItem(SERVERS_KEY, JSON.stringify(servers));
        // Persist servers to vault (async, removes localStorage copy on success)
        secureStoreAndClean('server_profiles', SERVERS_KEY, servers).catch(() => { });
        // Save OAuth secrets to secure credential store sequentially (avoid vault write races)
        const providers = ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'fourshared'] as const;
        for (const p of providers) {
            const creds = oauthSettings[p];
            if (creds.clientId) {
                await invoke('store_credential', { account: `oauth_${p}_client_id`, password: creds.clientId }).catch(console.error);
            }
            if (creds.clientSecret) {
                await invoke('store_credential', { account: `oauth_${p}_client_secret`, password: creds.clientSecret }).catch(console.error);
            }
        }
        // Remove legacy OAuth settings from localStorage
        localStorage.removeItem(OAUTH_SETTINGS_KEY);
        localStorage.setItem('analytics_enabled', settings.analyticsEnabled ? 'true' : 'false');
        // Apply system menu setting immediately
        invoke('toggle_menu_bar', { visible: settings.showSystemMenu });
        // Apply autostart setting (idempotent — no pre-check needed)
        try {
            if (settings.launchOnStartup) {
                await enableAutostart();
            } else {
                await disableAutostart();
            }
        } catch (e) {
            logger.debug('Autostart toggle failed:', e);
            // Revert UI to actual OS state on next open
            const actual = await isAutostartEnabled().catch(() => false);
            setSettings(prev => ({ ...prev, launchOnStartup: actual }));
        }
        // Notify App.tsx of settings change (for compactMode etc)
        window.dispatchEvent(new CustomEvent('aeroftp-settings-changed'));
        setHasChanges(false);
        onClose();
    };

    const updateSetting = <K extends keyof AppSettings>(key: K, value: AppSettings[K]) => {
        setSettings(prev => ({ ...prev, [key]: value }));
        setHasChanges(true);
    };

    const deleteServer = (id: string) => {
        setServers(prev => prev.filter(s => s.id !== id));
        setHasChanges(true);
        onServersChanged?.();
    };

    if (!isOpen) return null;

    const tabs: { id: TabId; label: string; icon: React.ReactNode }[] = [
        { id: 'general', label: t('settings.general'), icon: <Settings size={16} /> },
        { id: 'connection', label: t('settings.connection'), icon: <Wifi size={16} /> },
        { id: 'servers', label: t('settings.servers'), icon: <Server size={16} /> },
        { id: 'cloudproviders', label: t('settings.cloudProviders'), icon: <Cloud size={16} /> },
        { id: 'transfers', label: t('settings.transfers'), icon: <Upload size={16} /> },
        { id: 'filehandling', label: t('settings.fileHandling'), icon: <FileCheck size={16} /> },
        { id: 'ui', label: t('settings.appearance'), icon: <Palette size={16} /> },
        { id: 'security', label: t('settings.security'), icon: <Lock size={16} /> },
        { id: 'backup', label: t('settings.backup'), icon: <Key size={16} /> },
        { id: 'privacy', label: t('settings.privacy'), icon: <Shield size={16} /> },
    ];

    const updateOAuthSetting = (provider: keyof OAuthSettings, field: 'clientId' | 'clientSecret', value: string) => {
        setOauthSettings(prev => ({
            ...prev,
            [provider]: { ...prev[provider], [field]: value }
        }));
        setHasChanges(true);
    };

    return (
        <>
            <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
                {/* Backdrop */}
                <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />

                {/* Panel */}
                <div className="relative bg-white dark:bg-gray-900 rounded-xl shadow-2xl w-full max-w-3xl max-h-[85vh] overflow-hidden animate-scale-in flex flex-col">
                    {/* Header */}
                    <div className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700">
                        <div className="flex items-center gap-3">
                            <div className={`p-2 rounded-lg bg-gradient-to-br ${appThemeProp === 'tokyo' ? 'from-purple-600 to-violet-500' :
                                    appThemeProp === 'cyber' ? 'from-emerald-600 to-cyan-500' :
                                        'from-blue-500 to-cyan-500'
                                }`}>
                                <Settings size={20} className="text-white" />
                            </div>
                            <h2 className="text-lg font-semibold">{t('settings.title')}</h2>
                        </div>
                        <button onClick={onClose} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors">
                            <X size={18} />
                        </button>
                    </div>

                    {/* Content */}
                    <div className="flex flex-1 overflow-hidden">
                        {/* Sidebar */}
                        <div className="w-48 border-r border-gray-200 dark:border-gray-700 p-2 space-y-1">
                            {tabs.map(tab => (
                                <button
                                    key={tab.id}
                                    onClick={() => setActiveTab(tab.id)}
                                    className={`w-full flex items-start gap-2 px-3 py-2 rounded-lg text-sm transition-colors text-left ${activeTab === tab.id
                                        ? 'bg-blue-500 text-white'
                                        : 'hover:bg-gray-100 dark:hover:bg-gray-700'
                                        }`}
                                >
                                    {tab.icon}
                                    {tab.label}
                                </button>
                            ))}
                        </div>

                        {/* Main content */}
                        <div className="flex-1 p-6 overflow-y-auto">
                            {activeTab === 'general' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.generalSettings')}</h3>

                                    <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.defaultLocalPath')}</label>
                                            <div className="flex gap-2">
                                                <input
                                                    type="text"
                                                    value={settings.defaultLocalPath}
                                                    onChange={e => updateSetting('defaultLocalPath', e.target.value)}
                                                    placeholder={t('settings.defaultLocalPathPlaceholder')}
                                                    className="flex-1 px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                                                />
                                                <button
                                                    onClick={async () => {
                                                        try {
                                                            const selected = await open({ directory: true, multiple: false, title: t('settings.selectDefaultFolder') });
                                                            if (selected && typeof selected === 'string') {
                                                                updateSetting('defaultLocalPath', selected);
                                                            }
                                                        } catch (e) { console.error('Folder picker error:', e); }
                                                    }}
                                                    className="px-3 py-2 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                                                    title={t('common.browse')}
                                                >
                                                    <FolderOpen size={16} />
                                                </button>
                                            </div>
                                        </div>

                                        <label className="flex items-center gap-3 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={settings.showHiddenFiles}
                                                onChange={e => updateSetting('showHiddenFiles', e.target.checked)}
                                                className="w-4 h-4 rounded"
                                            />
                                            <span className="text-sm">{t('settings.showHiddenFiles')}</span>
                                        </label>

                                        <label className="flex items-center gap-3 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={settings.confirmBeforeDelete}
                                                onChange={e => updateSetting('confirmBeforeDelete', e.target.checked)}
                                                className="w-4 h-4 rounded"
                                            />
                                            <span className="text-sm">{t('settings.confirmBeforeDelete')}</span>
                                        </label>

                                        <label className="flex items-center gap-3 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={settings.sortFoldersFirst !== false}
                                                onChange={e => updateSetting('sortFoldersFirst', e.target.checked)}
                                                className="w-4 h-4 rounded"
                                            />
                                            <div>
                                                <p className="text-sm">{t('settings.sortFoldersFirst')}</p>
                                                <p className="text-xs text-gray-500">{t('settings.sortFoldersFirstDesc')}</p>
                                            </div>
                                        </label>

                                        <label className="flex items-center gap-3 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={settings.showFileExtensions !== false}
                                                onChange={e => updateSetting('showFileExtensions', e.target.checked)}
                                                className="w-4 h-4 rounded"
                                            />
                                            <div>
                                                <p className="text-sm">{t('settings.showFileExtensions')}</p>
                                                <p className="text-xs text-gray-500">{t('settings.showFileExtensionsDesc')}</p>
                                            </div>
                                        </label>

                                        <label className="flex items-center gap-3 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={settings.rememberLastFolder}
                                                onChange={e => updateSetting('rememberLastFolder', e.target.checked)}
                                                className="w-4 h-4 rounded"
                                            />
                                            <div>
                                                <p className="text-sm">{t('settings.rememberLastFolder')}</p>
                                                <p className="text-xs text-gray-500">{t('settings.rememberLastFolderDesc')}</p>
                                            </div>
                                        </label>

                                        <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
                                            <label className="block text-sm font-medium mb-2">{t('settings.doubleClickAction')}</label>
                                            <div className="flex gap-4">
                                                <label className="flex items-center gap-2 cursor-pointer">
                                                    <input
                                                        type="radio"
                                                        name="doubleClickAction"
                                                        checked={settings.doubleClickAction === 'preview'}
                                                        onChange={() => updateSetting('doubleClickAction', 'preview')}
                                                        className="w-4 h-4"
                                                    />
                                                    <span className="text-sm">{t('settings.doubleClickPreview')}</span>
                                                </label>
                                                <label className="flex items-center gap-2 cursor-pointer">
                                                    <input
                                                        type="radio"
                                                        name="doubleClickAction"
                                                        checked={settings.doubleClickAction === 'download'}
                                                        onChange={() => updateSetting('doubleClickAction', 'download')}
                                                        className="w-4 h-4"
                                                    />
                                                    <span className="text-sm">{t('settings.doubleClickDownload')}</span>
                                                </label>
                                            </div>
                                            <p className="text-xs text-gray-500 mt-1">{t('settings.doubleClickDesc')}</p>
                                        </div>

                                        <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
                                            <h4 className="text-sm font-medium mb-3 flex items-center gap-2">
                                                <Power size={14} />
                                                {t('settings.startupOptions')}
                                            </h4>
                                            <div className="space-y-3">
                                                <label className="flex items-center gap-3 cursor-pointer">
                                                    <input
                                                        type="checkbox"
                                                        checked={settings.launchOnStartup}
                                                        onChange={e => updateSetting('launchOnStartup', e.target.checked)}
                                                        className="w-4 h-4 rounded"
                                                    />
                                                    <div>
                                                        <p className="text-sm">{t('settings.launchOnStartup')}</p>
                                                        <p className="text-xs text-gray-500">{t('settings.launchOnStartupDesc')}</p>
                                                    </div>
                                                </label>
                                            </div>
                                        </div>

                                        <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
                                            <h4 className="text-sm font-medium mb-2">{t('settings.softwareUpdates')}</h4>
                                            <CheckUpdateButton onActivityLog={onActivityLog} />
                                        </div>
                                    </div>
                                </div>
                            )}

                            {activeTab === 'connection' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.connectionSettings')}</h3>

                                    <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.connectionTimeout')}</label>
                                            <div className="flex items-center gap-3">
                                                <input
                                                    type="range"
                                                    min="10"
                                                    max="120"
                                                    value={settings.timeoutSeconds}
                                                    onChange={e => updateSetting('timeoutSeconds', parseInt(e.target.value))}
                                                    className="flex-1"
                                                />
                                                <span className="text-sm w-16 text-right">{settings.timeoutSeconds}s</span>
                                            </div>
                                            <p className="text-xs text-gray-500 mt-1">{t('settings.connectionTimeoutDesc')}</p>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.tlsVersion')}</label>
                                            <select
                                                value={settings.tlsVersion}
                                                onChange={e => updateSetting('tlsVersion', e.target.value as 'auto' | '1.2' | '1.3')}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                <option value="auto">{t('settings.tlsAuto')}</option>
                                                <option value="1.2">{t('settings.tls12')}</option>
                                                <option value="1.3">{t('settings.tls13')}</option>
                                            </select>
                                        </div>

                                        <div className="grid grid-cols-2 gap-4">
                                            <div>
                                                <label className="block text-sm font-medium mb-1">{t('settings.reconnectAttempts')}</label>
                                                <select
                                                    value={settings.reconnectAttempts}
                                                    onChange={e => updateSetting('reconnectAttempts', parseInt(e.target.value))}
                                                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                                >
                                                    {[0, 1, 2, 3, 5, 10].map(n => (
                                                        <option key={n} value={n}>{n === 0 ? t('settings.disabled') : t('settings.attemptsCount', { n })}</option>
                                                    ))}
                                                </select>
                                            </div>
                                            <div>
                                                <label className="block text-sm font-medium mb-1">{t('settings.reconnectDelay')}</label>
                                                <select
                                                    value={settings.reconnectDelay}
                                                    onChange={e => updateSetting('reconnectDelay', parseInt(e.target.value))}
                                                    className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                                >
                                                    {[1, 3, 5, 10, 30].map(n => (
                                                        <option key={n} value={n}>{t('settings.nSeconds', { n })}</option>
                                                    ))}
                                                </select>
                                            </div>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-2">{t('settings.ftpMode')}</label>
                                            <div className="flex gap-4">
                                                <label className="flex items-center gap-2 cursor-pointer">
                                                    <input
                                                        type="radio"
                                                        name="ftpMode"
                                                        checked={settings.ftpMode === 'passive'}
                                                        onChange={() => updateSetting('ftpMode', 'passive')}
                                                        className="w-4 h-4"
                                                    />
                                                    <span className="text-sm">{t('settings.ftpPassive')}</span>
                                                </label>
                                                <label className="flex items-center gap-2 cursor-pointer">
                                                    <input
                                                        type="radio"
                                                        name="ftpMode"
                                                        checked={settings.ftpMode === 'active'}
                                                        onChange={() => updateSetting('ftpMode', 'active')}
                                                        className="w-4 h-4"
                                                    />
                                                    <span className="text-sm">{t('settings.ftpActive')}</span>
                                                </label>
                                            </div>
                                            <p className="text-xs text-gray-500 mt-1">{t('settings.ftpModeDesc')}</p>
                                        </div>
                                    </div>
                                </div>
                            )}

                            {activeTab === 'servers' && (
                                <div className="space-y-6">
                                    <div className="flex items-center justify-between">
                                        <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.savedServers')}</h3>
                                        <div className="flex items-center gap-2">
                                            <button
                                                onClick={() => setShowExportImport(true)}
                                                className="px-3 py-1.5 border border-gray-300 dark:border-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg text-sm flex items-center gap-1.5"
                                                title={t('settings.exportImport')}
                                            >
                                                <ImportExportIcon size={14} /> {t('settings.exportImport')}
                                            </button>
                                            <button
                                                onClick={() => setEditingServer({ id: crypto.randomUUID(), name: '', host: '', port: 21, username: '', password: '' })}
                                                className="px-3 py-1.5 bg-blue-500 hover:bg-blue-600 text-white rounded-lg text-sm flex items-center gap-1.5"
                                            >
                                                <Plus size={14} /> {t('settings.addServer')}
                                            </button>
                                        </div>
                                    </div>

                                    {servers.length === 0 ? (
                                        <div className="text-center py-8 text-gray-500">
                                            <Server size={48} className="mx-auto mb-3 opacity-30" />
                                            <p>{t('settings.noSavedServers')}</p>
                                            <p className="text-sm">{t('settings.noSavedServersDesc')}</p>
                                        </div>
                                    ) : (
                                        <div className="space-y-2">
                                            {servers.map(server => {
                                                const protocol = server.protocol || 'ftp';
                                                const isOAuth = isOAuthProvider(protocol as ProviderType) || isFourSharedProvider(protocol as ProviderType);
                                                const isExpired = protocol === 'mega' && server.options?.session_expires_at && Date.now() > server.options.session_expires_at;

                                                return (
                                                    <div
                                                        key={server.id}
                                                        className="flex items-center justify-between p-3 bg-gray-100 dark:bg-gray-700 rounded-xl hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors group"
                                                    >
                                                        <div className="flex items-center gap-3">
                                                            {/* Protocol-colored avatar */}
                                                            {(() => {
                                                                const logoKey = server.providerId || protocol;
                                                                const LogoComponent = PROVIDER_LOGOS[logoKey];
                                                                const hasLogo = !!LogoComponent;
                                                                return (
                                                                    <div className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center ${hasLogo ? 'bg-[#FFFFF0] dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${PROTOCOL_COLORS[protocol] || 'from-gray-500 to-gray-400'} text-white`}`}>
                                                                        {hasLogo ? <LogoComponent size={20} /> : <span className="font-bold">{(server.name || server.host).charAt(0).toUpperCase()}</span>}
                                                                    </div>
                                                                );
                                                            })()}
                                                            <div>
                                                                <div className="font-medium flex items-center gap-2">
                                                                    {server.name || server.host}
                                                                    {/* MEGA expiry badge */}
                                                                    {isExpired && (
                                                                        <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-red-100 dark:bg-red-900/60 text-red-600 dark:text-red-300 font-bold border border-red-200 dark:border-red-800 flex items-center gap-1" title={t('ui.sessionExpired')}>
                                                                            <Clock size={10} /> EXP
                                                                        </span>
                                                                    )}
                                                                    {/* Protocol badge */}
                                                                    <span className="text-xs px-1.5 py-0.5 rounded bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300 uppercase">
                                                                        {protocol}
                                                                    </span>
                                                                </div>
                                                                <div className="text-xs text-gray-500 dark:text-gray-400">
                                                                    {getServerDisplayInfo(server)}
                                                                </div>
                                                                {(server.initialPath || server.localInitialPath) && (
                                                                    <p className="text-xs text-gray-400 mt-1">
                                                                        {server.initialPath && <span>📁 {server.initialPath}</span>}
                                                                        {server.initialPath && server.localInitialPath && ' • '}
                                                                        {server.localInitialPath && <span>💻 {server.localInitialPath}</span>}
                                                                    </p>
                                                                )}
                                                            </div>
                                                        </div>
                                                        <div className="flex items-center gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                                                            <button
                                                                onClick={() => setEditingServer(server)}
                                                                className="p-2 text-gray-500 hover:text-blue-500 hover:bg-blue-50 dark:hover:bg-blue-900/30 rounded-lg transition-colors"
                                                                title={t('common.edit')}
                                                            >
                                                                <Edit size={14} />
                                                            </button>
                                                            <button
                                                                onClick={() => deleteServer(server.id)}
                                                                className="p-2 text-gray-500 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-900/30 rounded-lg transition-colors"
                                                                title={t('common.delete')}
                                                            >
                                                                <Trash2 size={14} />
                                                            </button>
                                                        </div>
                                                    </div>
                                                );
                                            })}
                                        </div>
                                    )}

                                    {/* Edit Server Modal */}
                                    {editingServer && (() => {
                                        const protocol = editingServer.protocol || 'ftp';
                                        const isOAuth = isOAuthProvider(protocol as ProviderType) || isFourSharedProvider(protocol as ProviderType);
                                        const isMega = protocol === 'mega';
                                        const isFilen = protocol === 'filen';
                                        const isS3 = protocol === 's3';
                                        const isAzure = protocol === 'azure';
                                        const isSftp = protocol === 'sftp';
                                        const isInternxt = protocol === 'internxt';
                                        const isKDrive = protocol === 'kdrive';
                                        const isJottacloud = protocol === 'jottacloud';
                                        const isDrime = protocol === 'drime';
                                        const needsHostPort = !isOAuth && !isMega && !isFilen && !isInternxt && !isKDrive && !isJottacloud && !isDrime;
                                        const needsPassword = !isOAuth;
                                        const isNewServer = !servers.some(s => s.id === editingServer.id);
                                        const logoKey = editingServer.providerId || protocol;
                                        const hasProviderLogo = !!PROVIDER_LOGOS[logoKey];

                                        // Protocol options for new server
                                        const protocolOptions = [
                                            { value: 'ftp', label: t('settings.protocolFtp'), port: 21 },
                                            { value: 'ftps', label: t('settings.protocolFtps'), port: 990 },
                                            { value: 'sftp', label: t('settings.protocolSftp'), port: 22 },
                                            { value: 's3', label: t('settings.protocolS3'), port: 443 },
                                            { value: 'webdav', label: t('settings.protocolWebdav'), port: 443 },
                                            { value: 'mega', label: t('settings.protocolMega'), port: 443 },
                                            { value: 'filen', label: t('settings.protocolFilen'), port: 443 },
                                            { value: 'googledrive', label: t('settings.protocolGdrive'), port: 443 },
                                            { value: 'dropbox', label: t('settings.protocolDropbox'), port: 443 },
                                            { value: 'onedrive', label: t('settings.protocolOnedrive'), port: 443 },
                                            { value: 'box', label: t('settings.protocolBox'), port: 443 },
                                            { value: 'pcloud', label: t('settings.protocolPcloud'), port: 443 },
                                            { value: 'fourshared', label: t('settings.protocolFourshared'), port: 443 },
                                            { value: 'zohoworkdrive', label: t('settings.protocolZohoworkdrive'), port: 443 },
                                            { value: 'internxt', label: 'Internxt Drive', port: 443 },
                                            { value: 'kdrive', label: 'kDrive', port: 443 },
                                            { value: 'jottacloud', label: 'Jottacloud', port: 443 },
                                            { value: 'drime', label: 'Drime Cloud', port: 443 },
                                            { value: 'azure', label: t('settings.protocolAzure'), port: 443 },
                                        ];

                                        const handleProtocolChange = (newProtocol: string) => {
                                            const opt = protocolOptions.find(p => p.value === newProtocol);
                                            setEditingServer({
                                                ...editingServer,
                                                protocol: newProtocol as ProviderType,
                                                port: opt?.port || 21,
                                                // Clear fields that don't apply
                                                host: (isOAuthProvider(newProtocol as ProviderType) || isFourSharedProvider(newProtocol as ProviderType)) ? '' : editingServer.host,
                                                username: '',
                                                password: '',
                                                options: {}
                                            });
                                        };

                                        return (
                                            <div className="fixed inset-0 z-60 flex items-center justify-center">
                                                <div className="absolute inset-0 bg-black/30" onClick={() => setEditingServer(null)} />
                                                <div className="relative bg-white dark:bg-gray-800 rounded-xl shadow-2xl p-6 w-full max-w-md space-y-4 max-h-[90vh] overflow-y-auto">
                                                    <div className="flex items-center gap-3">
                                                        {(() => {
                                                            const logoKey = editingServer.providerId || protocol;
                                                            const LogoComponent = PROVIDER_LOGOS[logoKey];
                                                            const hasLogo = !!LogoComponent;
                                                            return (
                                                                <div className={`w-10 h-10 rounded-lg flex items-center justify-center ${hasLogo ? 'bg-[#FFFFF0] dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${PROTOCOL_COLORS[protocol] || 'from-gray-500 to-gray-400'} text-white`}`}>
                                                                    {hasLogo ? <LogoComponent size={20} /> : isOAuth ? <Cloud size={18} /> : <Server size={18} />}
                                                                </div>
                                                            );
                                                        })()}
                                                        <div>
                                                            <h3 className="text-lg font-semibold">{isNewServer ? t('settings.addServer') : t('settings.editServerTitle')}</h3>
                                                            <span className="text-xs px-1.5 py-0.5 rounded bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300 uppercase">
                                                                {protocol}
                                                            </span>
                                                        </div>
                                                    </div>

                                                    <div className="space-y-3">
                                                        {/* Protocol Selector - only for new servers */}
                                                        {isNewServer && (
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.protocol')}</label>
                                                                <select
                                                                    value={protocol}
                                                                    onChange={e => handleProtocolChange(e.target.value)}
                                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                                >
                                                                    {protocolOptions.map(opt => (
                                                                        <option key={opt.value} value={opt.value}>{opt.label}</option>
                                                                    ))}
                                                                </select>
                                                            </div>
                                                        )}

                                                        {/* Server Name */}
                                                        <div>
                                                            <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.displayName')}</label>
                                                            <input
                                                                type="text"
                                                                placeholder={t('settings.serverNamePlaceholder')}
                                                                value={editingServer.name}
                                                                onChange={e => setEditingServer({ ...editingServer, name: e.target.value })}
                                                                className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                            />
                                                        </div>

                                                        {/* Custom Icon picker — only for servers without a dedicated provider logo */}
                                                        {!hasProviderLogo && (
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.serverIcon')}</label>
                                                                <div className="flex items-center gap-3">
                                                                    <div className={`w-10 h-10 shrink-0 rounded-lg flex items-center justify-center ${editingServer.customIconUrl || editingServer.faviconUrl ? 'bg-white dark:bg-gray-600 border border-gray-200 dark:border-gray-500' : `bg-gradient-to-br ${PROTOCOL_COLORS[protocol] || PROTOCOL_COLORS.ftp} text-white`}`}>
                                                                        {editingServer.customIconUrl ? (
                                                                            <img src={editingServer.customIconUrl} alt="" className="w-6 h-6 rounded object-contain" />
                                                                        ) : editingServer.faviconUrl ? (
                                                                            <img src={editingServer.faviconUrl} alt="" className="w-6 h-6 rounded object-contain" />
                                                                        ) : (
                                                                            <span className="font-bold text-sm">{(editingServer.name || editingServer.host || '?').charAt(0).toUpperCase()}</span>
                                                                        )}
                                                                    </div>
                                                                    <button
                                                                        type="button"
                                                                        onClick={async () => {
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
                                                                                // Resize to 128x128 via canvas
                                                                                const blob = new Blob([bytes], { type: mime });
                                                                                const url = URL.createObjectURL(blob);
                                                                                const img = new window.Image();
                                                                                const timeout = setTimeout(() => URL.revokeObjectURL(url), 10000);
                                                                                img.onload = () => {
                                                                                    clearTimeout(timeout);
                                                                                    const canvas = document.createElement('canvas');
                                                                                    const size = 128;
                                                                                    canvas.width = size;
                                                                                    canvas.height = size;
                                                                                    const ctx = canvas.getContext('2d');
                                                                                    if (!ctx) { URL.revokeObjectURL(url); return; }
                                                                                    const scale = Math.min(size / img.width, size / img.height);
                                                                                    const w = img.width * scale;
                                                                                    const h = img.height * scale;
                                                                                    ctx.drawImage(img, (size - w) / 2, (size - h) / 2, w, h);
                                                                                    const dataUrl = canvas.toDataURL('image/png');
                                                                                    URL.revokeObjectURL(url);
                                                                                    setEditingServer(prev => prev ? { ...prev, customIconUrl: dataUrl } : prev);
                                                                                };
                                                                                img.onerror = () => { clearTimeout(timeout); URL.revokeObjectURL(url); };
                                                                                img.src = url;
                                                                            } catch { /* user cancelled or read error */ }
                                                                        }}
                                                                        className="px-3 py-1.5 text-xs font-medium rounded-lg bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 border border-gray-300 dark:border-gray-600 transition-colors flex items-center gap-1.5"
                                                                    >
                                                                        <Image size={12} />
                                                                        {t('settings.chooseIcon')}
                                                                    </button>
                                                                    {editingServer.customIconUrl && (
                                                                        <button
                                                                            type="button"
                                                                            onClick={() => setEditingServer(prev => prev ? { ...prev, customIconUrl: undefined } : prev)}
                                                                            className="p-1.5 text-xs rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-red-500 transition-colors"
                                                                            title={t('settings.removeIcon')}
                                                                        >
                                                                            <X size={14} />
                                                                        </button>
                                                                    )}
                                                                </div>
                                                            </div>
                                                        )}

                                                        {/* OAuth providers - read-only info */}
                                                        {isOAuth && (
                                                            <div className="p-3 bg-blue-50 dark:bg-blue-900/30 rounded-lg border border-blue-200 dark:border-blue-700">
                                                                <p className="text-sm text-blue-700 dark:text-blue-300">
                                                                    <strong>{t('settings.oauthConnection')}</strong>
                                                                </p>
                                                                <p className="text-xs text-blue-600 dark:text-blue-400 mt-1">
                                                                    {t('settings.oauthConnectionDesc')}
                                                                </p>
                                                            </div>
                                                        )}

                                                        {/* MEGA - email only */}
                                                        {isMega && (
                                                            <>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.megaEmail')}</label>
                                                                    <input
                                                                        type="email"
                                                                        placeholder={t('settings.megaEmailPlaceholder')}
                                                                        value={editingServer.username}
                                                                        onChange={e => setEditingServer({ ...editingServer, username: e.target.value })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                </div>
                                                                {editingServer.options?.session_expires_at && (
                                                                    <div className={`p-2 rounded-lg text-xs ${Date.now() > editingServer.options.session_expires_at
                                                                        ? 'bg-red-50 dark:bg-red-900/30 text-red-600 dark:text-red-300 border border-red-200 dark:border-red-700'
                                                                        : 'bg-green-50 dark:bg-green-900/30 text-green-600 dark:text-green-300 border border-green-200 dark:border-green-700'
                                                                        }`}>
                                                                        <Clock size={12} className="inline mr-1" />
                                                                        {t('settings.session')} {Date.now() > editingServer.options.session_expires_at ? t('settings.sessionExpired') : t('settings.sessionExpires')}: {new Date(editingServer.options.session_expires_at).toLocaleString()}
                                                                    </div>
                                                                )}
                                                            </>
                                                        )}

                                                        {/* Filen - email + password, no host */}
                                                        {isFilen && (
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.filenEmail')}</label>
                                                                <input
                                                                    type="email"
                                                                    placeholder={t('settings.filenEmailPlaceholder')}
                                                                    value={editingServer.username}
                                                                    onChange={e => setEditingServer({ ...editingServer, username: e.target.value, host: 'filen.io', port: 443 })}
                                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                />
                                                            </div>
                                                        )}

                                                        {/* Internxt - email + password + optional 2FA */}
                                                        {isInternxt && (
                                                            <>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.emailAccount')}</label>
                                                                    <input
                                                                        type="email"
                                                                        placeholder={t('connection.internxtEmailPlaceholder')}
                                                                        value={editingServer.username}
                                                                        onChange={e => setEditingServer({ ...editingServer, username: e.target.value, host: 'gateway.internxt.com', port: 443 })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                </div>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.password')}</label>
                                                                    <div className="relative">
                                                                        <input
                                                                            type={showEditPassword ? 'text' : 'password'}
                                                                            placeholder={t('connection.internxtPasswordPlaceholder')}
                                                                            value={editingServer.password || ''}
                                                                            onChange={e => setEditingServer({ ...editingServer, password: e.target.value })}
                                                                            className="w-full px-3 py-2 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                        />
                                                                        <button type="button" tabIndex={-1} onClick={() => setShowEditPassword(!showEditPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600">
                                                                            {showEditPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.twoFactorCode')}</label>
                                                                    <input
                                                                        type="text"
                                                                        placeholder={t('connection.twoFactorOptional')}
                                                                        value={editingServer.options?.two_factor_code || ''}
                                                                        onChange={e => setEditingServer({
                                                                            ...editingServer,
                                                                            options: { ...editingServer.options, two_factor_code: e.target.value || undefined }
                                                                        })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                        maxLength={6}
                                                                        inputMode="numeric"
                                                                    />
                                                                </div>
                                                            </>
                                                        )}

                                                        {/* kDrive - API Token + Drive ID */}
                                                        {isKDrive && (
                                                            <>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.kdriveToken')}</label>
                                                                    <div className="relative">
                                                                        <input
                                                                            type={showEditPassword ? 'text' : 'password'}
                                                                            placeholder={t('connection.kdriveTokenPlaceholder')}
                                                                            value={editingServer.password || ''}
                                                                            onChange={e => setEditingServer({ ...editingServer, password: e.target.value, host: 'api.infomaniak.com', port: 443, username: 'api-token' })}
                                                                            className="w-full px-3 py-2 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                        />
                                                                        <button type="button" tabIndex={-1} onClick={() => setShowEditPassword(!showEditPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600">
                                                                            {showEditPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.kdriveDriveId')}</label>
                                                                    <input
                                                                        type="text"
                                                                        placeholder={t('connection.kdriveDriveIdPlaceholder')}
                                                                        value={editingServer.options?.drive_id || editingServer.options?.bucket || ''}
                                                                        onChange={e => setEditingServer({
                                                                            ...editingServer,
                                                                            options: { ...editingServer.options, bucket: e.target.value, drive_id: e.target.value }
                                                                        })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                        inputMode="numeric"
                                                                    />
                                                                </div>
                                                                <p className="text-xs text-gray-400">{t('connection.kdriveTokenHelp')}</p>
                                                            </>
                                                        )}

                                                        {/* Jottacloud - Login Token only */}
                                                        {isJottacloud && (
                                                            <>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.jottacloudToken')}</label>
                                                                    <div className="relative">
                                                                        <input
                                                                            type={showEditPassword ? 'text' : 'password'}
                                                                            placeholder={t('connection.jottacloudTokenPlaceholder')}
                                                                            value={editingServer.password || ''}
                                                                            onChange={e => setEditingServer({ ...editingServer, password: e.target.value, host: 'jfs.jottacloud.com', port: 443, username: 'token' })}
                                                                            className="w-full px-3 py-2 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                        />
                                                                        <button type="button" tabIndex={-1} onClick={() => setShowEditPassword(!showEditPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600">
                                                                            {showEditPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                                <p className="text-xs text-gray-400">{t('connection.jottacloudTokenHelp')}</p>
                                                            </>
                                                        )}

                                                        {/* Drime Cloud - API Token only */}
                                                        {isDrime && (
                                                            <>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('connection.drimeToken')}</label>
                                                                    <div className="relative">
                                                                        <input
                                                                            type={showEditPassword ? 'text' : 'password'}
                                                                            placeholder={t('connection.drimeTokenPlaceholder')}
                                                                            value={editingServer.password || ''}
                                                                            onChange={e => setEditingServer({ ...editingServer, password: e.target.value, host: 'app.drime.cloud', port: 443, username: 'api-token' })}
                                                                            className="w-full px-3 py-2 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                        />
                                                                        <button type="button" tabIndex={-1} onClick={() => setShowEditPassword(!showEditPassword)} className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600">
                                                                            {showEditPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                                        </button>
                                                                    </div>
                                                                </div>
                                                                <p className="text-xs text-gray-400">{t('connection.drimeTokenHelp')}</p>
                                                            </>
                                                        )}

                                                        {/* Host and Port - for FTP/FTPS/SFTP/WebDAV/S3/Azure */}
                                                        {needsHostPort && (
                                                            <div className="flex gap-2">
                                                                <div className="flex-1">
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">
                                                                        {isS3 ? t('settings.endpointUrl') : isAzure ? t('connection.azureEndpoint') : t('settings.host')}
                                                                    </label>
                                                                    <input
                                                                        type="text"
                                                                        placeholder={isS3 ? 's3.amazonaws.com' : isAzure ? 'myaccount.blob.core.windows.net' : 'ftp.example.com'}
                                                                        value={editingServer.host}
                                                                        onChange={e => setEditingServer({ ...editingServer, host: e.target.value })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                </div>
                                                                <div className="w-24">
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.port')}</label>
                                                                    <input
                                                                        type="number"
                                                                        placeholder={t('settings.portPlaceholder')}
                                                                        value={editingServer.port}
                                                                        onChange={e => setEditingServer({ ...editingServer, port: parseInt(e.target.value) || 21 })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                </div>
                                                            </div>
                                                        )}

                                                        {/* Username - for non-OAuth, non-MEGA */}
                                                        {needsHostPort && (
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">
                                                                    {isS3 ? t('settings.accessKeyId') : isAzure ? t('connection.azureAccountName') : t('settings.username')}
                                                                </label>
                                                                <input
                                                                    type="text"
                                                                    placeholder={isS3 ? 'AKIAIOSFODNN7EXAMPLE' : isAzure ? 'aeroftp2026' : 'username'}
                                                                    value={editingServer.username}
                                                                    onChange={e => setEditingServer({ ...editingServer, username: e.target.value })}
                                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                />
                                                            </div>
                                                        )}

                                                        {/* Password - for non-OAuth */}
                                                        {needsPassword && !isMega && !isInternxt && !isKDrive && !isDrime && (
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">
                                                                    {isS3 ? t('settings.secretAccessKey') : isAzure ? t('connection.azureAccessKey') : t('settings.password')}
                                                                </label>
                                                                <div className="relative">
                                                                    <input
                                                                        type={showEditPassword ? 'text' : 'password'}
                                                                        placeholder={t('settings.passwordPlaceholder')}
                                                                        value={editingServer.password || ''}
                                                                        onChange={e => setEditingServer({ ...editingServer, password: e.target.value })}
                                                                        className="w-full px-3 py-2 pr-10 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                    <button
                                                                        type="button"
                                                                        tabIndex={-1}
                                                                        onClick={() => setShowEditPassword(!showEditPassword)}
                                                                        className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600"
                                                                    >
                                                                        {showEditPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                                    </button>
                                                                </div>
                                                            </div>
                                                        )}

                                                        {/* S3 Specific Fields */}
                                                        {isS3 && (
                                                            <div className="grid grid-cols-2 gap-2">
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.bucket')}</label>
                                                                    <input
                                                                        type="text"
                                                                        placeholder={t('settings.s3BucketPlaceholder')}
                                                                        value={editingServer.options?.bucket || ''}
                                                                        onChange={e => setEditingServer({
                                                                            ...editingServer,
                                                                            options: { ...editingServer.options, bucket: e.target.value }
                                                                        })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                </div>
                                                                <div>
                                                                    <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.region')}</label>
                                                                    <input
                                                                        type="text"
                                                                        placeholder={t('settings.s3RegionPlaceholder')}
                                                                        value={editingServer.options?.region || ''}
                                                                        onChange={e => setEditingServer({
                                                                            ...editingServer,
                                                                            options: { ...editingServer.options, region: e.target.value }
                                                                        })}
                                                                        className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                    />
                                                                </div>
                                                            </div>
                                                        )}

                                                        {/* Azure Specific Fields */}
                                                        {isAzure && (
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">{t('protocol.azureContainerName')}</label>
                                                                <input
                                                                    type="text"
                                                                    placeholder={t('protocol.azureContainerPlaceholder')}
                                                                    value={editingServer.options?.bucket || ''}
                                                                    onChange={e => setEditingServer({
                                                                        ...editingServer,
                                                                        options: { ...editingServer.options, bucket: e.target.value }
                                                                    })}
                                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                />
                                                            </div>
                                                        )}

                                                        {/* Paths - common for all */}
                                                        <div className="pt-2 border-t border-gray-200 dark:border-gray-700">
                                                            <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.remotePath')}</label>
                                                            <input
                                                                type="text"
                                                                placeholder={t('settings.remotePathPlaceholder')}
                                                                value={editingServer.initialPath || ''}
                                                                onChange={e => setEditingServer({ ...editingServer, initialPath: e.target.value })}
                                                                className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                            />
                                                        </div>
                                                        <div className="flex gap-2">
                                                            <div className="flex-1">
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">{t('settings.localPath')}</label>
                                                                <input
                                                                    type="text"
                                                                    placeholder={t('settings.localPathPlaceholder')}
                                                                    value={editingServer.localInitialPath || ''}
                                                                    onChange={e => setEditingServer({ ...editingServer, localInitialPath: e.target.value })}
                                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                                />
                                                            </div>
                                                            <button
                                                                type="button"
                                                                onClick={async () => {
                                                                    try {
                                                                        const selected = await open({ directory: true, multiple: false, title: t('settings.selectLocalFolder') });
                                                                        if (selected && typeof selected === 'string') {
                                                                            setEditingServer({ ...editingServer, localInitialPath: selected });
                                                                        }
                                                                    } catch (e) { console.error('Folder picker error:', e); }
                                                                }}
                                                                className="mt-5 px-3 py-2 bg-gray-100 dark:bg-gray-600 hover:bg-gray-200 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                                title={t('common.browse')}
                                                            >
                                                                <FolderOpen size={16} />
                                                            </button>
                                                        </div>
                                                    </div>
                                                    <div className="flex gap-2 justify-end">
                                                        <button
                                                            onClick={() => { setEditingServer(null); setShowEditPassword(false); }}
                                                            className="px-4 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg"
                                                        >
                                                            {t('common.cancel')}
                                                        </button>
                                                        <button
                                                            onClick={async () => {
                                                                const exists = servers.some(s => s.id === editingServer.id);
                                                                let updatedServers: ServerProfile[];

                                                                // Store password in credential vault if provided
                                                                let hasStoredCredential = editingServer.hasStoredCredential || false;
                                                                if (editingServer.password) {
                                                                    try {
                                                                        await invoke('store_credential', {
                                                                            account: `server_${editingServer.id}`,
                                                                            password: editingServer.password
                                                                        });
                                                                        hasStoredCredential = true;
                                                                    } catch (err) {
                                                                        console.error('Failed to store credential:', err);
                                                                    }
                                                                }

                                                                // Update server profile with hasStoredCredential flag, without storing password in localStorage
                                                                const serverToSave = {
                                                                    ...editingServer,
                                                                    password: undefined, // Don't store password in localStorage
                                                                    hasStoredCredential,
                                                                };

                                                                if (exists) {
                                                                    updatedServers = servers.map(s => s.id === editingServer.id ? serverToSave : s);
                                                                } else {
                                                                    updatedServers = [...servers, serverToSave];
                                                                }
                                                                setServers(updatedServers);
                                                                // Persist immediately so changes aren't lost if user closes without Save Changes
                                                                localStorage.setItem(SERVERS_KEY, JSON.stringify(updatedServers));
                                                                secureStoreAndClean('server_profiles', SERVERS_KEY, updatedServers).catch(() => { });
                                                                setEditingServer(null);
                                                                setShowEditPassword(false);
                                                                setHasChanges(true);
                                                                onServersChanged?.();
                                                            }}
                                                            className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg"
                                                        >
                                                            {t('common.save')}
                                                        </button>
                                                    </div>
                                                </div>
                                            </div>
                                        );
                                    })()}
                                </div>
                            )}
                            {activeTab === 'filehandling' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.fileHandlingTitle')}</h3>

                                    <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.whenFileExists')}</label>
                                            <select
                                                value={settings.fileExistsAction}
                                                onChange={e => updateSetting('fileExistsAction', e.target.value as AppSettings['fileExistsAction'])}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                <option value="ask">{t('settings.fileExistsAsk')}</option>
                                                <option value="overwrite">{t('settings.fileExistsOverwrite')}</option>
                                                <option value="skip">{t('settings.fileExistsSkip')}</option>
                                                <option value="rename">{t('settings.fileExistsRename')}</option>
                                                <option value="resume">{t('settings.fileExistsResume')}</option>
                                                <option disabled>── {t('settings.smartSync')} ──</option>
                                                <option value="overwrite_if_newer">{t('settings.overwriteIfNewer')}</option>
                                                <option value="overwrite_if_different">{t('settings.overwriteIfDifferent')}</option>
                                                <option value="skip_if_identical">{t('settings.skipIfIdentical')}</option>
                                            </select>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.transferModeLabel')}</label>
                                            <select
                                                value={settings.transferMode}
                                                onChange={e => updateSetting('transferMode', e.target.value as 'auto' | 'ascii' | 'binary')}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                <option value="auto">{t('settings.transferModeAuto')}</option>
                                                <option value="binary">{t('settings.transferModeBinary')}</option>
                                                <option value="ascii">{t('settings.transferModeAscii')}</option>
                                            </select>
                                            <p className="text-xs text-gray-500 mt-1">{t('settings.transferModeDesc')}</p>
                                        </div>

                                        <label className="flex items-center gap-3 cursor-pointer">
                                            <input
                                                type="checkbox"
                                                checked={settings.preserveTimestamps}
                                                onChange={e => updateSetting('preserveTimestamps', e.target.checked)}
                                                className="w-4 h-4 rounded"
                                            />
                                            <div>
                                                <p className="font-medium">{t('settings.preserveTimestampsLabel')}</p>
                                                <p className="text-sm text-gray-500">{t('settings.preserveTimestampsDesc')}</p>
                                            </div>
                                        </label>
                                    </div>
                                </div>
                            )}

                            {activeTab === 'transfers' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.transferSettings')}</h3>

                                    <div className="space-y-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.concurrentTransfers')}</label>
                                            <select
                                                value={settings.maxConcurrentTransfers}
                                                onChange={e => updateSetting('maxConcurrentTransfers', parseInt(e.target.value))}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                {[1, 2, 3, 4, 5, 6, 8].map(n => (
                                                    <option key={n} value={n}>{n === 1 ? t('settings.oneFileAtATime') : t('settings.nFilesAtATime', { n })}</option>
                                                ))}
                                            </select>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.retryCountOnError')}</label>
                                            <select
                                                value={settings.retryCount}
                                                onChange={e => updateSetting('retryCount', parseInt(e.target.value))}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                {[0, 1, 2, 3, 5].map(n => (
                                                    <option key={n} value={n}>{n === 0 ? t('settings.noRetries') : t('settings.retriesCount', { n })}</option>
                                                ))}
                                            </select>
                                        </div>

                                        <div>
                                            <label className="block text-sm font-medium mb-1">{t('settings.connectionTimeout')}</label>
                                            <select
                                                value={settings.timeoutSeconds}
                                                onChange={e => updateSetting('timeoutSeconds', parseInt(e.target.value))}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                {[10, 30, 60, 120].map(n => (
                                                    <option key={n} value={n}>{t('settings.nSeconds', { n })}</option>
                                                ))}
                                            </select>
                                        </div>
                                    </div>
                                </div>
                            )}

                            {activeTab === 'cloudproviders' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.cloudProviderSettings')}</h3>
                                    <div className="flex items-center justify-between">
                                        <p className="text-sm text-gray-500">{t('settings.cloudProviderDesc')}</p>
                                        <button
                                            type="button"
                                            onClick={() => setShowOAuthSecrets(!showOAuthSecrets)}
                                            className="flex items-center gap-1.5 px-3 py-1.5 text-xs font-medium text-gray-500 hover:text-gray-700 dark:text-gray-400 dark:hover:text-gray-200 border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors flex-shrink-0"
                                        >
                                            {showOAuthSecrets ? <EyeOff size={14} /> : <Eye size={14} />}
                                            {showOAuthSecrets ? t('settings.hideSecrets') : t('settings.showSecrets')}
                                        </button>
                                    </div>

                                    {/* AeroCloud - First! */}
                                    <div className="p-4 bg-gradient-to-r from-sky-50 to-blue-50 dark:from-sky-900/30 dark:to-blue-900/30 border border-sky-200 dark:border-sky-700 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-gradient-to-br from-sky-400 to-blue-500 rounded-lg flex items-center justify-center shadow">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">{t('settings.aerocloudName')}</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.aerocloudDesc')}</p>
                                                </div>
                                            </div>
                                            <span className="text-xs bg-sky-100 dark:bg-sky-800 text-sky-700 dark:text-sky-300 px-2 py-0.5 rounded-full">
                                                {t('settings.noApiKeysNeeded')}
                                            </span>
                                        </div>
                                        <p className="text-sm text-gray-600 dark:text-gray-400">
                                            {t('settings.aerocloudInfo')}
                                        </p>
                                        <button
                                            onClick={() => {
                                                onClose();
                                                onOpenCloudPanel?.();
                                            }}
                                            className="w-full py-2 bg-gradient-to-r from-sky-500 to-blue-600 text-white rounded-lg text-sm font-medium hover:from-sky-600 hover:to-blue-700 transition-all"
                                        >
                                            {t('settings.configureAerocloud')} →
                                        </button>

                                        {/* File Manager Badge Integration */}
                                        <div className="pt-3 border-t border-sky-200 dark:border-sky-700 space-y-2">
                                            <div className="flex items-center gap-2 text-sm font-medium text-sky-700 dark:text-sky-300">
                                                <MonitorCheck size={14} />
                                                {t('settings.fileManagerIntegration')}
                                            </div>
                                            {navigator.platform.startsWith('Win') ? (
                                                <p className="text-xs text-gray-500 dark:text-gray-400">
                                                    {t('settings.windowsBadgeDesc')}
                                                </p>
                                            ) : (
                                                <>
                                                    <p className="text-xs text-gray-500 dark:text-gray-400">
                                                        {t('settings.linuxBadgeDesc')}
                                                    </p>
                                                    <div className="flex gap-2">
                                                        <button
                                                            onClick={async () => {
                                                                setBadgeFeedback(null);
                                                                try {
                                                                    const result = await invoke<string>('install_shell_extension_cmd');
                                                                    setBadgeFeedback({ type: 'success', message: result });
                                                                } catch (e) {
                                                                    setBadgeFeedback({ type: 'error', message: String(e) });
                                                                }
                                                                setTimeout(() => setBadgeFeedback(null), 8000);
                                                            }}
                                                            className="flex-1 py-1.5 bg-emerald-500 hover:bg-emerald-600 text-white rounded text-xs font-medium transition-colors flex items-center justify-center gap-1"
                                                        >
                                                            <CheckCircle2 size={12} />
                                                            {t('settings.installBadges')}
                                                        </button>
                                                        <button
                                                            onClick={async () => {
                                                                setBadgeFeedback(null);
                                                                try {
                                                                    const result = await invoke<string>('uninstall_shell_extension_cmd');
                                                                    setBadgeFeedback({ type: 'success', message: result });
                                                                } catch (e) {
                                                                    setBadgeFeedback({ type: 'error', message: String(e) });
                                                                }
                                                                setTimeout(() => setBadgeFeedback(null), 8000);
                                                            }}
                                                            className="py-1.5 px-3 bg-gray-200 hover:bg-gray-300 dark:bg-gray-600 dark:hover:bg-gray-500 text-gray-700 dark:text-gray-200 rounded text-xs font-medium transition-colors"
                                                        >
                                                            {t('settings.uninstallBadges')}
                                                        </button>
                                                    </div>
                                                    {badgeFeedback && (
                                                        <div className={`p-2.5 rounded-lg text-xs space-y-2 ${badgeFeedback.type === 'success'
                                                                ? 'bg-emerald-50 dark:bg-emerald-900/30 border border-emerald-200 dark:border-emerald-700 text-emerald-700 dark:text-emerald-300'
                                                                : 'bg-red-50 dark:bg-red-900/30 border border-red-200 dark:border-red-700 text-red-700 dark:text-red-300'
                                                            }`}>
                                                            <div className="flex items-start gap-2">
                                                                {badgeFeedback.type === 'success'
                                                                    ? <CheckCircle2 size={14} className="shrink-0 mt-0.5" />
                                                                    : <AlertCircle size={14} className="shrink-0 mt-0.5" />
                                                                }
                                                                <span className="leading-relaxed flex-1">{badgeFeedback.message}</span>
                                                                <button
                                                                    onClick={() => setBadgeFeedback(null)}
                                                                    className="shrink-0 p-0.5 rounded hover:bg-black/10 dark:hover:bg-white/10 transition-colors"
                                                                >
                                                                    <X size={12} />
                                                                </button>
                                                            </div>
                                                            {badgeFeedback.type === 'success' && (
                                                                <button
                                                                    onClick={async () => {
                                                                        try {
                                                                            const result = await invoke<string>('restart_file_manager_cmd');
                                                                            setBadgeFeedback({ type: 'success', message: result });
                                                                            setTimeout(() => setBadgeFeedback(null), 5000);
                                                                        } catch (e) {
                                                                            setBadgeFeedback({ type: 'error', message: String(e) });
                                                                        }
                                                                    }}
                                                                    className="w-full py-1.5 bg-sky-500 hover:bg-sky-600 text-white rounded text-xs font-medium transition-colors flex items-center justify-center gap-1.5"
                                                                >
                                                                    <MonitorCheck size={12} />
                                                                    {t('settings.restartFileManager')}
                                                                </button>
                                                            )}
                                                        </div>
                                                    )}
                                                </>
                                            )}
                                        </div>
                                    </div>

                                    {/* Google Drive */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-red-500 rounded-lg flex items-center justify-center">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">Google Drive</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.connectWithGoogle')}</p>
                                                </div>
                                            </div>
                                            <button
                                                onClick={() => openUrl('https://console.cloud.google.com/apis/credentials')}
                                                className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                            >
                                                {t('settings.getCredentials')} <ExternalLink size={12} />
                                            </button>
                                        </div>
                                        <div className="grid grid-cols-2 gap-3">
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientId')}</label>
                                                <input
                                                    type="text"
                                                    value={oauthSettings.googledrive.clientId}
                                                    onChange={e => updateOAuthSetting('googledrive', 'clientId', e.target.value)}
                                                    placeholder={t('settings.googleClientIdPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientSecret')}</label>
                                                <input
                                                    type={showOAuthSecrets ? 'text' : 'password'}
                                                    value={oauthSettings.googledrive.clientSecret}
                                                    onChange={e => updateOAuthSetting('googledrive', 'clientSecret', e.target.value)}
                                                    placeholder={t('settings.googleClientSecretPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                        </div>
                                    </div>

                                    {/* Dropbox */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-blue-600 rounded-lg flex items-center justify-center">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">Dropbox</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.connectWithDropbox')}</p>
                                                </div>
                                            </div>
                                            <button
                                                onClick={() => openUrl('https://www.dropbox.com/developers/apps')}
                                                className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                            >
                                                {t('settings.getCredentials')} <ExternalLink size={12} />
                                            </button>
                                        </div>
                                        <div className="grid grid-cols-2 gap-3">
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.appKey')}</label>
                                                <input
                                                    type="text"
                                                    value={oauthSettings.dropbox.clientId}
                                                    onChange={e => updateOAuthSetting('dropbox', 'clientId', e.target.value)}
                                                    placeholder={t('settings.dropboxClientIdPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.appSecret')}</label>
                                                <input
                                                    type="password"
                                                    value={oauthSettings.dropbox.clientSecret}
                                                    onChange={e => updateOAuthSetting('dropbox', 'clientSecret', e.target.value)}
                                                    placeholder={t('settings.dropboxClientSecretPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                        </div>
                                    </div>

                                    {/* OneDrive */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-sky-500 rounded-lg flex items-center justify-center">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">OneDrive</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.connectWithMicrosoft')}</p>
                                                </div>
                                            </div>
                                            <button
                                                onClick={() => openUrl('https://portal.azure.com/#blade/Microsoft_AAD_RegisteredApps')}
                                                className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                            >
                                                {t('settings.getCredentials')} <ExternalLink size={12} />
                                            </button>
                                        </div>
                                        <div className="grid grid-cols-2 gap-3">
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.applicationId')}</label>
                                                <input
                                                    type="text"
                                                    value={oauthSettings.onedrive.clientId}
                                                    onChange={e => updateOAuthSetting('onedrive', 'clientId', e.target.value)}
                                                    placeholder={t('settings.onedriveClientIdPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientSecret')}</label>
                                                <input
                                                    type={showOAuthSecrets ? 'text' : 'password'}
                                                    value={oauthSettings.onedrive.clientSecret}
                                                    onChange={e => updateOAuthSetting('onedrive', 'clientSecret', e.target.value)}
                                                    placeholder={t('settings.onedriveClientSecretPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                        </div>
                                    </div>

                                    {/* Box */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-blue-500 rounded-lg flex items-center justify-center">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">Box</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.connectWithBox')}</p>
                                                </div>
                                            </div>
                                            <button
                                                onClick={() => openUrl('https://app.box.com/developers/console')}
                                                className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                            >
                                                {t('settings.getCredentials')} <ExternalLink size={12} />
                                            </button>
                                        </div>
                                        <div className="grid grid-cols-2 gap-3">
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientId')}</label>
                                                <input
                                                    type="text"
                                                    value={oauthSettings.box.clientId}
                                                    onChange={e => updateOAuthSetting('box', 'clientId', e.target.value)}
                                                    placeholder={t('settings.boxClientIdPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientSecret')}</label>
                                                <input
                                                    type={showOAuthSecrets ? 'text' : 'password'}
                                                    value={oauthSettings.box.clientSecret}
                                                    onChange={e => updateOAuthSetting('box', 'clientSecret', e.target.value)}
                                                    placeholder={t('settings.boxClientSecretPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                        </div>
                                    </div>

                                    {/* pCloud */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-green-500 rounded-lg flex items-center justify-center">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">pCloud</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.connectWithPcloud')}</p>
                                                </div>
                                            </div>
                                            <button
                                                onClick={() => openUrl('https://docs.pcloud.com/methods/oauth_2.0/authorize.html')}
                                                className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                            >
                                                {t('settings.getCredentials')} <ExternalLink size={12} />
                                            </button>
                                        </div>
                                        <div className="grid grid-cols-2 gap-3">
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientId')}</label>
                                                <input
                                                    type="text"
                                                    value={oauthSettings.pcloud.clientId}
                                                    onChange={e => updateOAuthSetting('pcloud', 'clientId', e.target.value)}
                                                    placeholder={t('settings.pcloudClientIdPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.clientSecret')}</label>
                                                <input
                                                    type={showOAuthSecrets ? 'text' : 'password'}
                                                    value={oauthSettings.pcloud.clientSecret}
                                                    onChange={e => updateOAuthSetting('pcloud', 'clientSecret', e.target.value)}
                                                    placeholder={t('settings.pcloudClientSecretPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                        </div>
                                    </div>

                                    {/* 4shared (OAuth 1.0) */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                                        <div className="flex items-center justify-between">
                                            <div className="flex items-center gap-2">
                                                <div className="w-8 h-8 bg-blue-500 rounded-lg flex items-center justify-center">
                                                    <Cloud size={16} className="text-white" />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium">4shared</h4>
                                                    <p className="text-xs text-gray-500">{t('settings.foursharedDesc')}</p>
                                                </div>
                                            </div>
                                            <button
                                                onClick={() => openUrl('https://www.4shared.com/developer/')}
                                                className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                            >
                                                {t('settings.getCredentials')} <ExternalLink size={12} />
                                            </button>
                                        </div>
                                        <div className="grid grid-cols-2 gap-3">
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.consumerKey')}</label>
                                                <input
                                                    type="text"
                                                    value={oauthSettings.fourshared.clientId}
                                                    onChange={e => updateOAuthSetting('fourshared', 'clientId', e.target.value)}
                                                    placeholder={t('settings.foursharedClientIdPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                            <div>
                                                <label className="block text-xs font-medium mb-1">{t('settings.consumerSecret')}</label>
                                                <input
                                                    type={showOAuthSecrets ? 'text' : 'password'}
                                                    value={oauthSettings.fourshared.clientSecret}
                                                    onChange={e => updateOAuthSetting('fourshared', 'clientSecret', e.target.value)}
                                                    placeholder={t('settings.foursharedClientSecretPlaceholder')}
                                                    className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                                />
                                            </div>
                                        </div>
                                    </div>

                                    <div className="p-3 bg-blue-50 dark:bg-blue-900/30 rounded-lg border border-blue-200 dark:border-blue-700 flex items-start gap-2">
                                        <Shield size={16} className="mt-0.5 flex-shrink-0 text-blue-500 dark:text-blue-400" />
                                        <p className="text-sm text-blue-700 dark:text-blue-300">
                                            {t('settings.oauthNote')}
                                        </p>
                                    </div>
                                </div>
                            )}

                            {activeTab === 'ui' && (
                                <div className="space-y-4">
                                    {/* Appearance Sub-tabs */}
                                    <div className="flex gap-1 border-b border-gray-200 dark:border-gray-700">
                                        {([
                                            { id: 'theme' as const, label: t('settings.themeTab'), icon: <Palette size={13} /> },
                                            { id: 'icons' as const, label: t('settings.iconsTab'), icon: <Shapes size={13} /> },
                                            { id: 'interface' as const, label: t('settings.interfaceTab'), icon: <Monitor size={13} /> },
                                            { id: 'backgrounds' as const, label: t('settings.backgroundsTab'), icon: <Image size={13} /> },
                                        ]).map(sub => (
                                            <button
                                                key={sub.id}
                                                onClick={() => setAppearanceSubTab(sub.id)}
                                                className={`flex items-center gap-1.5 px-3 py-2 text-xs font-medium rounded-t-lg transition-colors ${appearanceSubTab === sub.id
                                                        ? 'bg-blue-500/10 text-blue-600 dark:text-blue-400 border-b-2 border-blue-500'
                                                        : 'text-gray-500 hover:text-gray-700 dark:hover:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/50'
                                                    }`}
                                            >
                                                {sub.icon}
                                                {sub.label}
                                            </button>
                                        ))}
                                    </div>

                                    {/* Sub-tab: Theme */}
                                    {appearanceSubTab === 'theme' && (
                                        <div className="space-y-4">
                                            <p className="text-xs text-gray-500">{t('settings.themeDesc')}</p>
                                            <div className="grid grid-cols-2 gap-3">
                                                {([
                                                    { id: 'light' as Theme, label: t('settings.themeLightLabel'), icon: <Sun size={20} />, colors: ['#ffffff', '#f3f4f6', '#3b82f6'], selectedBorder: 'border-blue-500 ring-1 ring-blue-500/30 bg-blue-500/5', selectedIcon: 'text-blue-500', checkBg: 'bg-blue-500', desc: t('settings.themeLightDesc') },
                                                    { id: 'dark' as Theme, label: t('settings.themeDarkLabel'), icon: <Moon size={20} />, colors: ['#111827', '#1f2937', '#3b82f6'], selectedBorder: 'border-blue-500 ring-1 ring-blue-500/30 bg-blue-500/5', selectedIcon: 'text-blue-500', checkBg: 'bg-blue-500', desc: t('settings.themeDarkDesc') },
                                                    { id: 'tokyo' as Theme, label: t('settings.themeTokyoLabel'), icon: <svg viewBox="0 0 32 32" width={20} height={20} fill="currentColor"><path d="M30.43,12.124c-0.441-1.356-1.289-2.518-2.454-3.358c-1.157-0.835-2.514-1.276-3.924-1.276c-0.44,0.001-0.898,0.055-1.368,0.158c-0.048-0.492-0.145-0.96-0.288-1.395c-0.442-1.345-1.287-2.498-2.442-3.335c-1.111-0.805-2.44-1.212-3.776-1.242C16.054,1.64,16,1.64,16,1.64s-0.105,0.014-0.151,0.036c-1.336,0.029-2.664,0.437-3.776,1.241c-1.155,0.837-2,1.991-2.442,3.335C9.488,6.686,9.392,7.154,9.343,7.648C8.859,7.542,8.415,7.462,7.926,7.491C6.511,7.496,5.153,7.942,4,8.783c-1.151,0.839-1.99,1.994-2.428,3.34s-0.437,2.774,0.001,4.129c0.439,1.358,1.275,2.518,2.417,3.353c0.369,0.271,0.785,0.507,1.239,0.706c-0.251,0.428-0.448,0.863-0.588,1.298c-0.432,1.349-0.427,2.778,0.016,4.135c0.443,1.354,1.282,2.51,2.427,3.341c1.145,0.832,2.503,1.272,3.927,1.275c1.422,0,2.78-0.437,3.926-1.263c0.371-0.268,0.724-0.589,1.053-0.96c0.319,0.36,0.659,0.673,1.013,0.932c1.145,0.839,2.509,1.285,3.946,1.291c1.428,0,2.789-0.441,3.938-1.275c1.153-0.838,1.995-2.004,2.435-3.37c0.439-1.368,0.438-2.804-0.007-4.152c-0.137-0.417-0.329-0.837-0.573-1.251c0.44-0.192,0.842-0.418,1.199-0.675c1.151-0.831,1.998-1.991,2.446-3.355C30.865,14.918,30.869,13.48,30.43,12.124z" /></svg>, colors: ['#1a1b26', '#16161e', '#9d7cd8'], selectedBorder: 'border-purple-500 ring-1 ring-purple-500/30 bg-purple-500/5', selectedIcon: 'text-purple-500', checkBg: 'bg-purple-500', desc: t('settings.themeTokyoDesc') },
                                                    { id: 'cyber' as Theme, label: t('settings.themeCyberLabel'), icon: <svg viewBox="0 0 100 100" width={20} height={20} fill="currentColor"><path d="M73.142 41.007c0-.084.003-.166.003-.25 0-14.843-6.384-26.875-14.259-26.875-2.438 0-4.733 1.156-6.739 3.191a2.997 2.997 0 01-4.294 0c-2.007-2.035-4.301-3.191-6.739-3.191-7.875 0-14.26 12.032-14.26 26.875 0 .084.003.166.003.25C15.209 44.052 7.5 49.325 7.5 55.324 7.5 64.752 26.528 69.8 50 69.8s42.5-5.047 42.5-14.476c0-5.999-7.709-11.272-19.358-14.317z" /><path d="M76.908 69.209c-17.939 3.926-35.878 3.926-53.817 0-1.505 0-2.611 1.508-2.249 3.068l2.776 10.722c.256 1.104 1.184 1.88 2.249 1.88l12.475 1.185c.665.063 1.337.083 1.999-.011 2.005-.285 3.887-1.279 5.227-2.917 1.309-1.601 2.82-2.517 4.431-2.517s3.122.916 4.431 2.517c1.34 1.639 3.222 2.632 5.227 2.917.662.094 1.334.075 1.999.011l12.475-1.185c1.065 0 1.994-.776 2.249-1.88l2.776-10.722c.363-1.56-.742-3.068-2.248-3.068zM42.99 79.172c-.299 2.048-3.989 3.134-8.243 2.427-4.254-.707-7.461-2.94-7.162-4.988.299-2.048 3.989-3.135 8.243-2.427s7.461 2.94 7.162 4.988zm22.263 2.427c-4.254.707-7.945-.38-8.243-2.427-.299-2.048 2.908-4.281 7.162-4.988s7.945.38 8.243 2.427c.298 2.048-2.908 4.281-7.162 4.988z" /></svg>, colors: ['#0a0e17', '#0d1117', '#10b981'], selectedBorder: 'border-emerald-500 ring-1 ring-emerald-500/30 bg-emerald-500/5', selectedIcon: 'text-emerald-500', checkBg: 'bg-emerald-500', desc: t('settings.themeCyberDesc') },
                                                ]).map(themeOption => {
                                                    const isSelected = appThemeProp === themeOption.id;
                                                    return (
                                                        <button
                                                            key={themeOption.id}
                                                            onClick={() => {
                                                                setAppTheme?.(themeOption.id);
                                                                // Icon theme auto-syncs via useEffect in App.tsx
                                                            }}
                                                            className={`relative flex flex-col items-center gap-2 p-4 rounded-xl border-2 transition-all ${isSelected
                                                                    ? themeOption.selectedBorder
                                                                    : 'border-gray-200 dark:border-gray-600 hover:border-gray-400 dark:hover:border-gray-500'
                                                                }`}
                                                        >
                                                            {/* Color preview strip */}
                                                            <div className="flex w-full h-8 rounded-lg overflow-hidden shadow-inner">
                                                                {themeOption.colors.map((color, i) => (
                                                                    <div key={i} className="flex-1" style={{ backgroundColor: color }} />
                                                                ))}
                                                            </div>
                                                            {/* Icon + Label */}
                                                            <div className="flex items-center gap-2">
                                                                <span className={isSelected ? themeOption.selectedIcon : 'text-gray-500'}>{themeOption.icon}</span>
                                                                <span className="text-sm font-medium">{themeOption.label}</span>
                                                            </div>
                                                            <span className="text-[10px] text-gray-500 text-center leading-tight">{themeOption.desc}</span>
                                                            {/* Selected check */}
                                                            {isSelected && (
                                                                <div className={`absolute top-2 right-2 w-5 h-5 ${themeOption.checkBg} rounded-full flex items-center justify-center`}>
                                                                    <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                                    </svg>
                                                                </div>
                                                            )}
                                                        </button>
                                                    );
                                                })}
                                            </div>

                                            {/* Auto mode toggle */}
                                            <div className="border-t border-gray-200 dark:border-gray-700 pt-4">
                                                <label className="flex items-center gap-3 cursor-pointer">
                                                    <input
                                                        type="checkbox"
                                                        checked={appThemeProp === 'auto'}
                                                        onChange={e => {
                                                            const prefersDark = window.matchMedia('(prefers-color-scheme: dark)').matches;
                                                            if (e.target.checked) {
                                                                setAppTheme?.('auto');
                                                            } else {
                                                                setAppTheme?.(prefersDark ? 'dark' : 'light');
                                                            }
                                                            // Icon theme auto-syncs via useEffect in App.tsx
                                                        }}
                                                        className="w-4 h-4 rounded"
                                                    />
                                                    <div>
                                                        <p className="font-medium flex items-center gap-2">
                                                            <Monitor size={14} />
                                                            {t('settings.autoTheme')}
                                                        </p>
                                                        <p className="text-sm text-gray-500">{t('settings.autoThemeDesc')}</p>
                                                    </div>
                                                </label>
                                            </div>
                                        </div>
                                    )}

                                    {/* Sub-tab: Icons */}
                                    {appearanceSubTab === 'icons' && (
                                        <div className="space-y-4">
                                            <p className="text-xs text-gray-500">{t('settings.iconThemeDesc')}</p>
                                            <div className="grid grid-cols-3 gap-3">
                                                {([
                                                    { id: 'outline' as IconTheme, label: t('settings.iconThemeOutline'), desc: t('settings.iconThemeOutlineDesc') },
                                                    { id: 'filled' as IconTheme, label: t('settings.iconThemeFilled'), desc: t('settings.iconThemeFilledDesc') },
                                                    { id: 'minimal' as IconTheme, label: t('settings.iconThemeMinimal'), desc: t('settings.iconThemeMinimalDesc') },
                                                ]).map(option => {
                                                    const isDarkMode = appThemeProp === 'dark' || appThemeProp === 'tokyo' || appThemeProp === 'cyber' || (appThemeProp === 'auto' && window.matchMedia('(prefers-color-scheme: dark)').matches);
                                                    const effectiveTheme = getEffectiveTheme(appThemeProp, isDarkMode);
                                                    const isSelected = iconTheme === option.id;
                                                    const provider = getIconThemeProvider(option.id, effectiveTheme);
                                                    return (
                                                        <button key={option.id}
                                                            onClick={() => { setIconTheme(option.id); flashSaved(); }}
                                                            className={`relative flex flex-col items-center gap-2 p-4 rounded-xl border-2 transition-all ${isSelected
                                                                    ? 'border-blue-500 ring-1 ring-blue-500/30 bg-blue-500/5'
                                                                    : 'border-gray-200 dark:border-gray-600 hover:border-gray-400 dark:hover:border-gray-500'
                                                                }`}
                                                        >
                                                            <div className="flex gap-2 items-center">
                                                                {provider.getFolderIcon(24).icon}
                                                                {provider.getFileIcon('script.js', 24).icon}
                                                                {provider.getFileIcon('style.css', 24).icon}
                                                            </div>
                                                            <div className="flex gap-2 items-center">
                                                                {provider.getFileIcon('photo.png', 24).icon}
                                                                {provider.getFileIcon('report.pdf', 24).icon}
                                                                {provider.getFileIcon('readme.md', 24).icon}
                                                            </div>
                                                            <span className="text-sm font-medium mt-1">{option.label}</span>
                                                            <span className="text-[10px] text-gray-500 text-center leading-tight">{option.desc}</span>
                                                            {isSelected && (
                                                                <div className="absolute top-2 right-2 w-5 h-5 bg-blue-500 rounded-full flex items-center justify-center">
                                                                    <svg className="w-3 h-3 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                                    </svg>
                                                                </div>
                                                            )}
                                                        </button>
                                                    );
                                                })}
                                            </div>
                                        </div>
                                    )}

                                    {/* Sub-tab: Interface */}
                                    {appearanceSubTab === 'interface' && (
                                        <div className="space-y-4">
                                            {/* Language Selector */}
                                            <LanguageSelector
                                                currentLanguage={language}
                                                availableLanguages={availableLanguages}
                                                onSelect={(lang) => {
                                                    setLanguage(lang);
                                                    flashSaved();
                                                }}
                                                label={t('settings.interfaceLanguage')}
                                            />

                                            <div>
                                                <label className="block text-sm font-medium mb-2">{t('settings.fontSize')}</label>
                                                <div className="flex gap-2">
                                                    {(['small', 'medium', 'large'] as const).map(size => (
                                                        <button
                                                            key={size}
                                                            onClick={() => updateSetting('fontSize', size)}
                                                            className={`flex-1 py-2 px-3 rounded-lg text-sm font-medium transition-colors ${settings.fontSize === size
                                                                ? 'bg-blue-500 text-white'
                                                                : 'bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600'
                                                                }`}
                                                        >
                                                            {size === 'small' ? t('settings.fontSizeSmall') : size === 'medium' ? t('settings.fontSizeMedium') : t('settings.fontSizeLarge')}
                                                        </button>
                                                    ))}
                                                </div>
                                                <p className="text-xs text-gray-500 mt-1">{t('settings.fontSizeDesc')}</p>
                                            </div>

                                            <div className="border-t border-gray-200 dark:border-gray-700 my-4" />

                                            <label className="flex items-center gap-3 cursor-pointer">
                                                <input type="checkbox" checked={settings.showStatusBar} onChange={e => updateSetting('showStatusBar', e.target.checked)} className="w-4 h-4 rounded" />
                                                <div>
                                                    <p className="font-medium">{t('settings.showStatusBar')}</p>
                                                    <p className="text-sm text-gray-500">{t('settings.showStatusBarDesc')}</p>
                                                </div>
                                            </label>

                                            <label className="flex items-center gap-3 cursor-pointer">
                                                <input type="checkbox" checked={settings.showSystemMenu} onChange={e => updateSetting('showSystemMenu', e.target.checked)} className="w-4 h-4 rounded" />
                                                <div>
                                                    <p className="font-medium">{t('settings.showSystemMenuBar')}</p>
                                                    <p className="text-sm text-gray-500">{t('settings.showSystemMenuBarDesc')}</p>
                                                </div>
                                            </label>

                                            <label className="flex items-center gap-3 cursor-pointer">
                                                <input type="checkbox" checked={settings.compactMode} onChange={e => updateSetting('compactMode', e.target.checked)} className="w-4 h-4 rounded" />
                                                <div>
                                                    <p className="font-medium">{t('settings.compactMode')}</p>
                                                    <p className="text-sm text-gray-500">{t('settings.compactModeDesc')}</p>
                                                </div>
                                            </label>

                                            <label className="flex items-center gap-3 cursor-pointer">
                                                <input type="checkbox" checked={settings.showToastNotifications} onChange={e => updateSetting('showToastNotifications', e.target.checked)} className="w-4 h-4 rounded" />
                                                <div>
                                                    <p className="font-medium">{t('settings.toastNotifications')}</p>
                                                    <p className="text-sm text-gray-500">{t('settings.toastNotificationsDesc')}</p>
                                                </div>
                                            </label>

                                            {/* Visible Columns */}
                                            <div className="border-t border-gray-200 dark:border-gray-700 my-4" />
                                            <div>
                                                <label className="block text-sm font-medium mb-2">{t('settings.visibleColumns')}</label>
                                                <p className="text-xs text-gray-500 mb-3">{t('settings.visibleColumnsDesc')}</p>
                                                <div className="space-y-2">
                                                    {[
                                                        { key: 'name', label: t('settings.columnName'), disabled: true },
                                                        { key: 'size', label: t('settings.columnSize'), disabled: false },
                                                        { key: 'type', label: t('settings.columnType'), disabled: false },
                                                        { key: 'permissions', label: t('settings.columnPermissions'), disabled: false },
                                                        { key: 'modified', label: t('settings.columnModified'), disabled: false },
                                                    ].map(col => (
                                                        <label key={col.key} className={`flex items-center gap-3 ${col.disabled ? 'opacity-60' : 'cursor-pointer'}`}>
                                                            <input
                                                                type="checkbox"
                                                                checked={col.disabled || (settings.visibleColumns || []).includes(col.key)}
                                                                disabled={col.disabled}
                                                                onChange={() => {
                                                                    const current = settings.visibleColumns || ['name', 'size', 'type', 'permissions', 'modified'];
                                                                    const updated = current.includes(col.key)
                                                                        ? current.filter((c: string) => c !== col.key)
                                                                        : [...current, col.key];
                                                                    updateSetting('visibleColumns', updated);
                                                                }}
                                                                className="w-4 h-4 rounded"
                                                            />
                                                            <span className="text-sm">{col.label}</span>
                                                        </label>
                                                    ))}
                                                </div>
                                            </div>
                                        </div>
                                    )}

                                    {/* Sub-tab: Backgrounds */}
                                    {appearanceSubTab === 'backgrounds' && (
                                        <div className="space-y-6">
                                            {/* App Background Pattern */}
                                            <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                                                <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                                                    <h4 className="font-medium flex items-center gap-2 text-sm">
                                                        <Palette size={14} className="text-gray-500" />
                                                        {t('settings.appBackgroundPattern')}
                                                    </h4>
                                                </div>
                                                <div className="p-4">
                                                    <p className="text-xs text-gray-500 mb-3">{t('settings.appBackgroundPatternDesc')}</p>
                                                    <div className="grid grid-cols-4 gap-2">
                                                        {APP_BACKGROUND_PATTERNS.map(pattern => {
                                                            const currentId = localStorage.getItem(APP_BACKGROUND_KEY) || DEFAULT_APP_BACKGROUND;
                                                            const isSelected = currentId === pattern.id;
                                                            return (
                                                                <button
                                                                    key={pattern.id}
                                                                    onClick={() => {
                                                                        localStorage.setItem(APP_BACKGROUND_KEY, pattern.id);
                                                                        flashSaved();
                                                                        window.dispatchEvent(new CustomEvent('app-background-changed', { detail: pattern.id }));
                                                                    }}
                                                                    className={`relative h-16 rounded-lg border-2 overflow-hidden transition-all ${isSelected
                                                                            ? 'border-blue-500 ring-1 ring-blue-500/30'
                                                                            : 'border-gray-300 dark:border-gray-600 hover:border-gray-400 dark:hover:border-gray-500'
                                                                        }`}
                                                                    title={t(pattern.nameKey)}
                                                                >
                                                                    <div className="absolute inset-0 bg-gradient-to-br from-gray-100 to-gray-200 dark:from-gray-800 dark:to-gray-900">
                                                                        {pattern.svg && (() => {
                                                                            const lockPattern = LOCK_SCREEN_PATTERNS.find(p => p.id === pattern.id);
                                                                            return lockPattern?.svg ? (
                                                                                <div className="absolute inset-0 opacity-10 invert dark:invert-0" style={{ backgroundImage: lockPattern.svg }} />
                                                                            ) : null;
                                                                        })()}
                                                                    </div>
                                                                    <div className="absolute inset-0 flex items-end justify-center pb-1">
                                                                        <span className="text-[9px] text-gray-600 dark:text-gray-300 font-medium">{t(pattern.nameKey)}</span>
                                                                    </div>
                                                                    {isSelected && (
                                                                        <div className="absolute top-1 right-1 w-4 h-4 bg-blue-500 rounded-full flex items-center justify-center">
                                                                            <svg className="w-2.5 h-2.5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                                                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                                            </svg>
                                                                        </div>
                                                                    )}
                                                                </button>
                                                            );
                                                        })}
                                                    </div>
                                                </div>
                                            </div>

                                            {/* Lock Screen Pattern */}
                                            <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                                                <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                                                    <h4 className="font-medium flex items-center gap-2 text-sm">
                                                        <Palette size={14} className="text-gray-500" />
                                                        {t('settings.lockScreenPattern')}
                                                    </h4>
                                                </div>
                                                <div className="p-4">
                                                    <div className="grid grid-cols-4 gap-2">
                                                        {LOCK_SCREEN_PATTERNS.map(pattern => {
                                                            const currentId = localStorage.getItem('aeroftp_lock_pattern') || 'hexagon';
                                                            const isSelected = currentId === pattern.id;
                                                            return (
                                                                <button
                                                                    key={pattern.id}
                                                                    onClick={() => {
                                                                        localStorage.setItem('aeroftp_lock_pattern', pattern.id);
                                                                        flashSaved();
                                                                    }}
                                                                    className={`relative h-16 rounded-lg border-2 overflow-hidden transition-all ${isSelected
                                                                            ? 'border-emerald-500 ring-1 ring-emerald-500/30'
                                                                            : 'border-gray-300 dark:border-gray-600 hover:border-gray-400 dark:hover:border-gray-500'
                                                                        }`}
                                                                    title={t(pattern.nameKey)}
                                                                >
                                                                    <div className="absolute inset-0 bg-gradient-to-br from-gray-100 to-gray-200 dark:from-gray-800 dark:to-gray-900">
                                                                        {pattern.svg && (
                                                                            <div className="absolute inset-0 opacity-10 invert dark:invert-0" style={{ backgroundImage: pattern.svg }} />
                                                                        )}
                                                                    </div>
                                                                    <div className="absolute inset-0 flex items-end justify-center pb-1">
                                                                        <span className="text-[9px] text-gray-600 dark:text-gray-300 font-medium">{t(pattern.nameKey)}</span>
                                                                    </div>
                                                                    {isSelected && (
                                                                        <div className="absolute top-1 right-1 w-4 h-4 bg-emerald-500 rounded-full flex items-center justify-center">
                                                                            <svg className="w-2.5 h-2.5 text-white" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                                                <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                                            </svg>
                                                                        </div>
                                                                    )}
                                                                </button>
                                                            );
                                                        })}
                                                    </div>
                                                </div>
                                            </div>
                                        </div>
                                    )}
                                </div>
                            )}

                            {activeTab === 'security' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.security')}</h3>

                                    {/* Master Password Section */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-4">
                                        <div className="flex items-start gap-3">
                                            <div className="p-2 bg-emerald-100 dark:bg-emerald-900/30 text-emerald-600 dark:text-emerald-400 rounded-lg">
                                                <ShieldCheck size={24} />
                                            </div>
                                            <div>
                                                <h4 className="font-medium text-base">{t('settings.masterPassword')}</h4>
                                                <p className="text-sm text-gray-500 mt-1">{t('settings.masterPasswordDesc')}</p>
                                            </div>
                                        </div>

                                        {/* Status Badge */}
                                        <div className="flex items-center gap-2">
                                            <span className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium ${masterPasswordStatus?.is_set
                                                    ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                                                    : 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400'
                                                }`}>
                                                {masterPasswordStatus?.is_set ? (
                                                    <>
                                                        <ShieldCheck size={12} />
                                                        {t('settings.masterPasswordEnabled')}
                                                    </>
                                                ) : (
                                                    <>
                                                        <Shield size={12} />
                                                        {t('settings.masterPasswordDisabled')}
                                                    </>
                                                )}
                                            </span>
                                            {masterPasswordStatus?.is_set && masterPasswordStatus.timeout_seconds > 0 && (
                                                <span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400">
                                                    <Clock size={12} />
                                                    {t('settings.autoLockEnabled', { minutes: Math.floor(masterPasswordStatus.timeout_seconds / 60) })}
                                                </span>
                                            )}
                                        </div>

                                        {/* Error/Success Messages */}
                                        {masterPasswordError && (
                                            <div className="p-3 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg text-red-600 dark:text-red-400 text-sm">
                                                {masterPasswordError}
                                            </div>
                                        )}
                                        {masterPasswordSuccess && (
                                            <div className="p-3 bg-emerald-50 dark:bg-emerald-900/20 border border-emerald-200 dark:border-emerald-800 rounded-lg text-emerald-600 dark:text-emerald-400 text-sm">
                                                {masterPasswordSuccess}
                                            </div>
                                        )}

                                        {/* Set/Change Master Password Form */}
                                        <div className="pt-4 border-t border-gray-200 dark:border-gray-700 space-y-4">
                                            {masterPasswordStatus?.is_set && (
                                                <div>
                                                    <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                                        {t('settings.currentPassword')}
                                                    </label>
                                                    <div className="relative">
                                                        <input
                                                            type={showMasterPassword ? 'text' : 'password'}
                                                            value={currentMasterPassword}
                                                            onChange={e => setCurrentMasterPassword(e.target.value)}
                                                            className="w-full px-3 py-2 pr-10 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                            placeholder={t('settings.passwordPlaceholder')}
                                                        />
                                                        <button
                                                            type="button"
                                                            tabIndex={-1}
                                                            onClick={() => setShowMasterPassword(!showMasterPassword)}
                                                            className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
                                                        >
                                                            {showMasterPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                        </button>
                                                    </div>
                                                </div>
                                            )}

                                            <div>
                                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                                    {masterPasswordStatus?.is_set ? t('settings.newPassword') : t('settings.setPassword')}
                                                </label>
                                                <div className="relative">
                                                    <input
                                                        type={showMasterPassword ? 'text' : 'password'}
                                                        value={newMasterPassword}
                                                        onChange={e => setNewMasterPassword(e.target.value)}
                                                        className="w-full px-3 py-2 pr-10 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('settings.passwordPlaceholder')}
                                                    />
                                                    <button
                                                        type="button"
                                                        tabIndex={-1}
                                                        onClick={() => setShowMasterPassword(!showMasterPassword)}
                                                        className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
                                                    >
                                                        {showMasterPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                    </button>
                                                </div>
                                                <p className="mt-1 text-xs text-gray-500">{t('settings.passwordRequirements')}</p>
                                            </div>

                                            <div>
                                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                                    {t('settings.confirmPassword')}
                                                </label>
                                                <div className="relative">
                                                    <input
                                                        type={showMasterPassword ? 'text' : 'password'}
                                                        value={confirmMasterPassword}
                                                        onChange={e => setConfirmMasterPassword(e.target.value)}
                                                        className="w-full px-3 py-2 pr-10 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        placeholder={t('settings.confirmPasswordPlaceholder')}
                                                    />
                                                    <button
                                                        type="button"
                                                        tabIndex={-1}
                                                        onClick={() => setShowMasterPassword(!showMasterPassword)}
                                                        className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
                                                    >
                                                        {showMasterPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                    </button>
                                                </div>
                                            </div>

                                            {/* Auto-lock Timeout */}
                                            <div>
                                                <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-1">
                                                    {t('settings.autoLockTimeout')}
                                                </label>
                                                <div className="flex items-center gap-3">
                                                    <input
                                                        type="range"
                                                        min="0"
                                                        max="60"
                                                        step="5"
                                                        value={autoLockTimeout}
                                                        onChange={async (e) => {
                                                            const mins = parseInt(e.target.value);
                                                            setAutoLockTimeout(mins);
                                                            // Auto-save when master password is already set
                                                            if (masterPasswordStatus?.is_set) {
                                                                try {
                                                                    await invoke('set_auto_lock_timeout', { timeoutSeconds: mins * 60 });
                                                                    setMasterPasswordStatus(prev => prev ? { ...prev, timeout_seconds: mins * 60 } : prev);
                                                                } catch { /* best-effort */ }
                                                            }
                                                        }}
                                                        className="flex-1 accent-emerald-500"
                                                    />
                                                    <span className="w-20 text-sm font-medium text-gray-700 dark:text-gray-300 text-right">
                                                        {autoLockTimeout === 0 ? t('settings.autoLockDisabled') : `${autoLockTimeout} min`}
                                                    </span>
                                                </div>
                                                <p className="mt-1 text-xs text-gray-500">{t('settings.autoLockDesc')}</p>
                                            </div>

                                            {/* Action Buttons */}
                                            <div className="flex gap-3 pt-2">
                                                <button
                                                    onClick={async () => {
                                                        setMasterPasswordError('');
                                                        setMasterPasswordSuccess('');

                                                        // Validation
                                                        if (newMasterPassword.length < 8) {
                                                            setMasterPasswordError(t('settings.passwordTooShort'));
                                                            return;
                                                        }
                                                        if (newMasterPassword !== confirmMasterPassword) {
                                                            setMasterPasswordError(t('settings.passwordMismatch'));
                                                            return;
                                                        }

                                                        setIsSettingPassword(true);
                                                        setPasswordBtnState('encrypting');
                                                        try {
                                                            const timeoutSeconds = autoLockTimeout * 60;
                                                            if (masterPasswordStatus?.is_set) {
                                                                // Change password + sync timeout
                                                                await invoke('change_master_password', {
                                                                    oldPassword: currentMasterPassword,
                                                                    newPassword: newMasterPassword,
                                                                });
                                                                await invoke('set_auto_lock_timeout', { timeoutSeconds: timeoutSeconds });
                                                                setMasterPasswordSuccess(t('settings.passwordChanged'));
                                                            } else {
                                                                // Enable master password
                                                                await invoke('enable_master_password', {
                                                                    password: newMasterPassword,
                                                                    timeoutSeconds
                                                                });
                                                                setMasterPasswordSuccess(t('settings.passwordSet'));
                                                            }
                                                            // Refresh status
                                                            const status = await invoke<{ master_mode: boolean; is_locked: boolean; timeout_seconds: number }>('get_credential_store_status');
                                                            setMasterPasswordStatus({
                                                                is_set: status.master_mode,
                                                                is_locked: status.is_locked,
                                                                timeout_seconds: Number(status.timeout_seconds),
                                                            });
                                                            // Clear form
                                                            setNewMasterPassword('');
                                                            setConfirmMasterPassword('');
                                                            setCurrentMasterPassword('');
                                                            // Show "done" state for 1 second
                                                            setPasswordBtnState('done');
                                                            setIsSettingPassword(false);
                                                            setTimeout(() => setPasswordBtnState('idle'), 1000);
                                                        } catch (err) {
                                                            setMasterPasswordError(String(err));
                                                            setIsSettingPassword(false);
                                                            setPasswordBtnState('idle');
                                                        }
                                                    }}
                                                    disabled={isSettingPassword || passwordBtnState === 'done'}
                                                    className={`px-4 py-2 min-w-[160px] rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2 ${passwordBtnState === 'done'
                                                            ? 'bg-emerald-500 text-white'
                                                            : 'bg-emerald-600 hover:bg-emerald-700 disabled:bg-emerald-400 text-white'
                                                        }`}
                                                >
                                                    {passwordBtnState === 'encrypting' ? (
                                                        <>
                                                            <svg className="h-4 w-4" viewBox="0 0 100 100" fill="currentColor">
                                                                <path d="M31.6,3.5C5.9,13.6-6.6,42.7,3.5,68.4c10.1,25.7,39.2,38.3,64.9,28.1l-3.1-7.9c-21.3,8.4-45.4-2-53.8-23.3c-8.4-21.3,2-45.4,23.3-53.8L31.6,3.5z">
                                                                    <animateTransform attributeName="transform" type="rotate" dur="2s" from="0 50 50" to="360 50 50" repeatCount="indefinite" />
                                                                </path>
                                                                <path d="M42.3,39.6c5.7-4.3,13.9-3.1,18.1,2.7c4.3,5.7,3.1,13.9-2.7,18.1l4.1,5.5c8.8-6.5,10.6-19,4.1-27.7c-6.5-8.8-19-10.6-27.7-4.1L42.3,39.6z">
                                                                    <animateTransform attributeName="transform" type="rotate" dur="1s" from="0 50 50" to="-360 50 50" repeatCount="indefinite" />
                                                                </path>
                                                                <path d="M82,35.7C74.1,18,53.4,10.1,35.7,18S10.1,46.6,18,64.3l7.6-3.4c-6-13.5,0-29.3,13.5-35.3s29.3,0,35.3,13.5L82,35.7z">
                                                                    <animateTransform attributeName="transform" type="rotate" dur="2s" from="0 50 50" to="360 50 50" repeatCount="indefinite" />
                                                                </path>
                                                            </svg>
                                                            {t('settings.encrypting')}
                                                        </>
                                                    ) : passwordBtnState === 'done' ? (
                                                        <>
                                                            <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor"><path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d="M5 13l4 4L19 7" /></svg>
                                                            {masterPasswordStatus?.is_set ? t('settings.passwordChanged') : t('settings.passwordSet')}
                                                        </>
                                                    ) : (
                                                        masterPasswordStatus?.is_set ? t('settings.changePassword') : t('settings.setPassword')
                                                    )}
                                                </button>

                                                {masterPasswordStatus?.is_set && (
                                                    <button
                                                        onClick={() => {
                                                            if (!currentMasterPassword) {
                                                                setMasterPasswordError(t('settings.enterCurrentPassword'));
                                                                return;
                                                            }
                                                            setRemovePasswordConfirm(true);
                                                        }}
                                                        className="px-4 py-2 bg-red-100 hover:bg-red-200 text-red-700 dark:bg-red-900/30 dark:hover:bg-red-900/50 dark:text-red-400 rounded-lg text-sm font-medium transition-colors"
                                                    >
                                                        {t('settings.removePassword')}
                                                    </button>
                                                )}
                                                {removePasswordConfirm && (
                                                    <ConfirmDialog
                                                        message={t('settings.confirmRemovePassword')}
                                                        confirmLabel={t('settings.removePassword')}
                                                        confirmColor="red"
                                                        onConfirm={async () => {
                                                            setRemovePasswordConfirm(false);
                                                            try {
                                                                await invoke('disable_master_password', { password: currentMasterPassword });
                                                                const status = await invoke<{ master_mode: boolean; is_locked: boolean; timeout_seconds: number }>('get_credential_store_status');
                                                                setMasterPasswordStatus({
                                                                    is_set: status.master_mode,
                                                                    is_locked: status.is_locked,
                                                                    timeout_seconds: Number(status.timeout_seconds),
                                                                });
                                                                setCurrentMasterPassword('');
                                                                setMasterPasswordSuccess(t('settings.passwordRemoved'));
                                                            } catch (err) {
                                                                setMasterPasswordError(String(err));
                                                            }
                                                        }}
                                                        onCancel={() => setRemovePasswordConfirm(false)}
                                                    />
                                                )}
                                            </div>
                                        </div>
                                    </div>

                                    {/* Security Info */}
                                    <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                                        <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                                            <h4 className="font-medium flex items-center gap-2 text-sm">
                                                <Key size={14} className="text-gray-500" />
                                                {t('settings.encryptionDetails')}
                                            </h4>
                                        </div>
                                        <div className="p-4 space-y-3">
                                            <div className="flex items-start gap-3">
                                                <div className="w-5 h-5 rounded-full bg-emerald-100 dark:bg-emerald-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                    <svg className="w-3 h-3 text-emerald-600 dark:text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                    </svg>
                                                </div>
                                                <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityArgon2')}</span>
                                            </div>
                                            <div className="flex items-start gap-3">
                                                <div className="w-5 h-5 rounded-full bg-emerald-100 dark:bg-emerald-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                    <svg className="w-3 h-3 text-emerald-600 dark:text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                    </svg>
                                                </div>
                                                <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityAES')}</span>
                                            </div>
                                            <div className="flex items-start gap-3">
                                                <div className="w-5 h-5 rounded-full bg-emerald-100 dark:bg-emerald-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                    <svg className="w-3 h-3 text-emerald-600 dark:text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                    </svg>
                                                </div>
                                                <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityHMAC')}</span>
                                            </div>
                                            <div className="flex items-start gap-3">
                                                <div className="w-5 h-5 rounded-full bg-emerald-100 dark:bg-emerald-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                    <svg className="w-3 h-3 text-emerald-600 dark:text-emerald-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                    </svg>
                                                </div>
                                                <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityZeroize')}</span>
                                            </div>
                                        </div>
                                    </div>

                                    {/* Two-Factor Authentication (2FA) */}
                                    <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-4">
                                        <div className="flex items-start gap-3">
                                            <div className="p-2 bg-blue-100 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 rounded-lg">
                                                <Shield size={24} />
                                            </div>
                                            <div className="flex-1">
                                                <h4 className="font-medium text-base">{t('security.totp.setup')}</h4>
                                                <p className="text-sm text-gray-500 mt-1">
                                                    {totpEnabled
                                                        ? t('security.totp.enabled')
                                                        : t('security.totp.enterCode').split('.')[0] + '.'
                                                    }
                                                </p>
                                            </div>
                                        </div>

                                        {/* Status Badge */}
                                        <div className="flex items-center gap-2">
                                            <span className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium ${totpEnabled
                                                    ? 'bg-emerald-100 text-emerald-700 dark:bg-emerald-900/30 dark:text-emerald-400'
                                                    : 'bg-gray-100 text-gray-500 dark:bg-gray-800 dark:text-gray-400'
                                                }`}>
                                                <Shield size={12} />
                                                {totpEnabled ? '2FA Active' : '2FA Inactive'}
                                            </span>
                                        </div>

                                        {!totpEnabled ? (
                                            <button
                                                onClick={() => setShowTotpSetup(true)}
                                                className="flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium
                                                bg-blue-600 text-white hover:bg-blue-700 transition-colors"
                                            >
                                                <Shield size={14} />
                                                {t('security.totp.enable')}
                                            </button>
                                        ) : (
                                            <div className="space-y-3">
                                                {!showTotpDisable ? (
                                                    <button
                                                        onClick={() => setShowTotpDisable(true)}
                                                        className="flex items-center gap-2 px-4 py-2 rounded-lg text-sm font-medium
                                                        bg-red-600/10 text-red-500 hover:bg-red-600/20 transition-colors"
                                                    >
                                                        {t('security.totp.disable')}
                                                    </button>
                                                ) : (
                                                    <div className="space-y-2 p-3 rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/10">
                                                        <p className="text-sm text-red-600 dark:text-red-400">
                                                            {t('security.totp.disableConfirm')}
                                                        </p>
                                                        <input
                                                            type="text"
                                                            value={totpDisableCode}
                                                            onChange={e => setTotpDisableCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                                                            placeholder="000000"
                                                            maxLength={6}
                                                            className="w-full text-center text-xl font-mono tracking-[0.3em] py-2 rounded-lg
                                                            bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 text-sm"
                                                        />
                                                        {totpDisableError && (
                                                            <p className="text-xs text-red-500">{totpDisableError}</p>
                                                        )}
                                                        <div className="flex gap-2">
                                                            <button
                                                                onClick={() => { setShowTotpDisable(false); setTotpDisableCode(''); setTotpDisableError(''); }}
                                                                className="flex-1 py-1.5 rounded text-sm bg-gray-200 dark:bg-gray-700 text-gray-600 dark:text-gray-300"
                                                            >
                                                                {t('security.totp.back')}
                                                            </button>
                                                            <button
                                                                onClick={async () => {
                                                                    try {
                                                                        const ok = await invoke<boolean>('totp_disable', { code: totpDisableCode });
                                                                        if (ok) {
                                                                            // Remove TOTP secret from credential vault
                                                                            invoke('delete_credential', { account: 'totp_secret' })
                                                                                .catch((err) => console.error('Failed to remove TOTP secret from vault:', err));
                                                                            setTotpEnabled(false);
                                                                            setShowTotpDisable(false);
                                                                            setTotpDisableCode('');
                                                                            setTotpDisableError('');
                                                                        } else {
                                                                            setTotpDisableError(t('security.totp.invalidCode'));
                                                                        }
                                                                    } catch (e) {
                                                                        setTotpDisableError(String(e));
                                                                    }
                                                                }}
                                                                disabled={totpDisableCode.length !== 6}
                                                                className="flex-1 py-1.5 rounded text-sm bg-red-600 text-white disabled:opacity-50"
                                                            >
                                                                {t('security.totp.disable')}
                                                            </button>
                                                        </div>
                                                    </div>
                                                )}
                                            </div>
                                        )}
                                    </div>
                                </div>
                            )}

                            {/* TOTP Setup Modal */}
                            <TotpSetup
                                isOpen={showTotpSetup}
                                onClose={() => setShowTotpSetup(false)}
                                onEnabled={(secret) => {
                                    setTotpEnabled(true);
                                    setShowTotpSetup(false);
                                    // Store TOTP secret in credential vault for persistence across restarts
                                    invoke('store_credential', { account: 'totp_secret', password: secret })
                                        .catch((err) => {
                                            console.error('Failed to persist TOTP secret to vault:', err);
                                        });
                                }}
                            />

                            {activeTab === 'backup' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.backup')}</h3>

                                    {/* Vault Backup */}
                                    <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                                        <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                                            <div className="flex items-center justify-between">
                                                <h4 className="font-medium flex items-center gap-2 text-sm">
                                                    <Key size={14} className="text-blue-500" />
                                                    {t('settings.keystoreBackup')}
                                                </h4>
                                                <span className="inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400">
                                                    <Key size={12} />
                                                    {t('settings.entriesInVault', { count: vaultEntriesCount })}
                                                </span>
                                            </div>
                                        </div>
                                        <div className="p-4 space-y-4">
                                            {/* Keystore message */}
                                            {keystoreMessage && (
                                                <div className={`p-3 rounded-lg text-sm flex items-center gap-2 ${keystoreMessage.type === 'success'
                                                        ? 'bg-emerald-50 dark:bg-emerald-900/20 border border-emerald-200 dark:border-emerald-800 text-emerald-600 dark:text-emerald-400'
                                                        : 'bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 text-red-600 dark:text-red-400'
                                                    }`}>
                                                    {keystoreMessage.type === 'success' ? <CheckCircle2 size={16} /> : <AlertCircle size={16} />}
                                                    {keystoreMessage.text}
                                                </div>
                                            )}

                                            {/* Export */}
                                            <div className="space-y-3">
                                                <div className="flex items-start gap-3">
                                                    <div className="w-10 h-10 rounded-lg bg-green-100 dark:bg-green-900/30 flex items-center justify-center shrink-0">
                                                        <Download size={20} className="text-green-600 dark:text-green-400" />
                                                    </div>
                                                    <div className="flex-1">
                                                        <div className="font-medium text-sm">{t('settings.exportKeystore')}</div>
                                                        <div className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">{t('settings.exportKeystoreDesc')}</div>
                                                    </div>
                                                </div>
                                                <div className="flex gap-2">
                                                    <div className="relative flex-1">
                                                        <input
                                                            type={showKeystoreExportPassword ? 'text' : 'password'}
                                                            placeholder={t('settings.keystorePassword')}
                                                            value={keystoreExportPassword}
                                                            onChange={e => setKeystoreExportPassword(e.target.value)}
                                                            className="w-full px-3 py-2 pr-10 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        />
                                                        <button
                                                            type="button"
                                                            tabIndex={-1}
                                                            onClick={() => setShowKeystoreExportPassword(!showKeystoreExportPassword)}
                                                            className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
                                                        >
                                                            {showKeystoreExportPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                        </button>
                                                    </div>
                                                    <div className="relative flex-1">
                                                        <input
                                                            type={showKeystoreExportPassword ? 'text' : 'password'}
                                                            placeholder={t('settings.confirmPassword')}
                                                            value={keystoreExportConfirm}
                                                            onChange={e => setKeystoreExportConfirm(e.target.value)}
                                                            className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                        />
                                                    </div>
                                                </div>
                                                <button
                                                    onClick={async () => {
                                                        setKeystoreMessage(null);
                                                        if (keystoreExportPassword.length < 8) {
                                                            setKeystoreMessage({ type: 'error', text: t('settings.passwordTooShort') });
                                                            return;
                                                        }
                                                        if (keystoreExportPassword !== keystoreExportConfirm) {
                                                            setKeystoreMessage({ type: 'error', text: t('settings.passwordMismatch') });
                                                            return;
                                                        }
                                                        const filePath = await save({
                                                            title: t('settings.exportKeystore'),
                                                            filters: [{ name: 'AeroFTP Keystore', extensions: ['aeroftp-keystore'] }],
                                                            defaultPath: `aeroftp_keystore_${new Date().toISOString().slice(0, 10)}.aeroftp-keystore`,
                                                        });
                                                        if (!filePath) return;
                                                        setKeystoreExporting(true);
                                                        try {
                                                            const result = await invoke<{ entriesCount: number }>('export_keystore', {
                                                                password: keystoreExportPassword,
                                                                filePath,
                                                            });
                                                            setKeystoreMessage({
                                                                type: 'success',
                                                                text: t('settings.keystoreExported', { count: String(result.entriesCount) }),
                                                            });
                                                            setKeystoreExportPassword('');
                                                            setKeystoreExportConfirm('');
                                                        } catch (err) {
                                                            setKeystoreMessage({ type: 'error', text: String(err) });
                                                        } finally {
                                                            setKeystoreExporting(false);
                                                        }
                                                    }}
                                                    disabled={keystoreExporting || keystoreExportPassword.length < 8}
                                                    className="w-full px-4 py-2 bg-green-600 hover:bg-green-700 disabled:bg-green-400 text-white rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2"
                                                >
                                                    {keystoreExporting ? (
                                                        <>
                                                            <svg className="animate-spin h-4 w-4" viewBox="0 0 24 24"><circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" fill="none" /><path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" /></svg>
                                                            {t('common.loading')}
                                                        </>
                                                    ) : (
                                                        <>
                                                            <Download size={16} />
                                                            {t('settings.exportKeystore')}
                                                        </>
                                                    )}
                                                </button>
                                            </div>

                                            {/* Divider */}
                                            <div className="border-t border-gray-200 dark:border-gray-700" />

                                            {/* Import */}
                                            <div className="space-y-3">
                                                <div className="flex items-start gap-3">
                                                    <div className="w-10 h-10 rounded-lg bg-blue-100 dark:bg-blue-900/30 flex items-center justify-center shrink-0">
                                                        <Upload size={20} className="text-blue-600 dark:text-blue-400" />
                                                    </div>
                                                    <div className="flex-1">
                                                        <div className="font-medium text-sm">{t('settings.importKeystore')}</div>
                                                        <div className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">{t('settings.importKeystoreDesc')}</div>
                                                    </div>
                                                </div>

                                                {/* Step 1: Select file and show metadata */}
                                                {!keystoreMetadata ? (
                                                    <button
                                                        onClick={async () => {
                                                            setKeystoreMessage(null);
                                                            const filePath = await open({
                                                                title: t('settings.importKeystore'),
                                                                filters: [{ name: 'AeroFTP Keystore', extensions: ['aeroftp-keystore'] }],
                                                                multiple: false,
                                                            });
                                                            if (!filePath) return;
                                                            const path = typeof filePath === 'string' ? filePath : filePath;
                                                            try {
                                                                const meta = await invoke<{
                                                                    exportDate: string;
                                                                    aeroftpVersion: string;
                                                                    entriesCount: number;
                                                                    categories: {
                                                                        serverCredentials: number;
                                                                        serverProfiles: number;
                                                                        aiKeys: number;
                                                                        oauthTokens: number;
                                                                        configEntries: number;
                                                                    };
                                                                }>('read_keystore_metadata', { filePath: path });
                                                                setKeystoreMetadata(meta);
                                                                setKeystoreImportFilePath(path);
                                                            } catch (err) {
                                                                setKeystoreMessage({ type: 'error', text: String(err) });
                                                            }
                                                        }}
                                                        className="w-full px-4 py-2 bg-blue-100 hover:bg-blue-200 dark:bg-blue-900/30 dark:hover:bg-blue-900/50 text-blue-700 dark:text-blue-400 rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2"
                                                    >
                                                        <FolderOpen size={16} />
                                                        {t('settings.importKeystore')}
                                                    </button>
                                                ) : (
                                                    <div className="space-y-3">
                                                        {/* Metadata preview */}
                                                        <div className="p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg text-sm space-y-1.5">
                                                            <div className="flex items-center gap-2 text-blue-700 dark:text-blue-300 font-medium">
                                                                <Shield size={14} />
                                                                {t('settings.keystoreEntries', { count: keystoreMetadata.entriesCount })}
                                                            </div>
                                                            <div className="text-xs text-blue-600/70 dark:text-blue-400/70 space-y-0.5">
                                                                <div>AeroFTP {keystoreMetadata.aeroftpVersion}</div>
                                                                <div>{new Date(keystoreMetadata.exportDate).toLocaleString()}</div>
                                                                {keystoreMetadata.categories.serverCredentials > 0 && (
                                                                    <div>{keystoreMetadata.categories.serverCredentials} {t('settings.serverCredentials')}</div>
                                                                )}
                                                                {keystoreMetadata.categories.serverProfiles > 0 && (
                                                                    <div>{keystoreMetadata.categories.serverProfiles} {t('settings.serverProfilesLabel')}</div>
                                                                )}
                                                                {keystoreMetadata.categories.aiKeys > 0 && (
                                                                    <div>{keystoreMetadata.categories.aiKeys} {t('settings.aiKeysLabel')}</div>
                                                                )}
                                                                {keystoreMetadata.categories.oauthTokens > 0 && (
                                                                    <div>{keystoreMetadata.categories.oauthTokens} {t('settings.oauthTokensLabel')}</div>
                                                                )}
                                                                {keystoreMetadata.categories.configEntries > 0 && (
                                                                    <div>{keystoreMetadata.categories.configEntries} {t('settings.configEntriesLabel')}</div>
                                                                )}
                                                            </div>
                                                        </div>

                                                        {/* Password input */}
                                                        <div className="relative">
                                                            <input
                                                                type={showKeystoreImportPassword ? 'text' : 'password'}
                                                                placeholder={t('settings.keystorePassword')}
                                                                value={keystoreImportPassword}
                                                                onChange={e => setKeystoreImportPassword(e.target.value)}
                                                                className="w-full px-3 py-2 pr-10 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                            />
                                                            <button
                                                                type="button"
                                                                tabIndex={-1}
                                                                onClick={() => setShowKeystoreImportPassword(!showKeystoreImportPassword)}
                                                                className="absolute right-3 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600"
                                                            >
                                                                {showKeystoreImportPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                                            </button>
                                                        </div>

                                                        {/* Merge strategy */}
                                                        <div className="space-y-1.5">
                                                            <label className="block text-xs font-medium text-gray-600 dark:text-gray-400">
                                                                {t('settings.mergeStrategy')}
                                                            </label>
                                                            <div className="flex gap-3">
                                                                <label className="flex items-center gap-2 text-sm cursor-pointer">
                                                                    <input
                                                                        type="radio"
                                                                        name="keystoreMerge"
                                                                        checked={keystoreImportMerge === 'skip'}
                                                                        onChange={() => setKeystoreImportMerge('skip')}
                                                                        className="accent-blue-500"
                                                                    />
                                                                    {t('settings.skipExisting')}
                                                                </label>
                                                                <label className="flex items-center gap-2 text-sm cursor-pointer">
                                                                    <input
                                                                        type="radio"
                                                                        name="keystoreMerge"
                                                                        checked={keystoreImportMerge === 'overwrite'}
                                                                        onChange={() => setKeystoreImportMerge('overwrite')}
                                                                        className="accent-blue-500"
                                                                    />
                                                                    {t('settings.overwriteAll')}
                                                                </label>
                                                            </div>
                                                        </div>

                                                        {/* Import / Cancel buttons */}
                                                        <div className="flex gap-2">
                                                            <button
                                                                onClick={async () => {
                                                                    if (!keystoreImportFilePath) return;
                                                                    setKeystoreMessage(null);
                                                                    if (keystoreImportPassword.length < 8) {
                                                                        setKeystoreMessage({ type: 'error', text: t('settings.passwordTooShort') });
                                                                        return;
                                                                    }
                                                                    setKeystoreImporting(true);
                                                                    try {
                                                                        const result = await invoke<{
                                                                            imported: number;
                                                                            skipped: number;
                                                                            total: number;
                                                                        }>('import_keystore', {
                                                                            password: keystoreImportPassword,
                                                                            filePath: keystoreImportFilePath,
                                                                            mergeStrategy: keystoreImportMerge,
                                                                        });
                                                                        setKeystoreMessage({
                                                                            type: 'success',
                                                                            text: t('settings.keystoreImported', {
                                                                                imported: result.imported,
                                                                                skipped: result.skipped,
                                                                            }),
                                                                        });
                                                                        // Refresh vault count
                                                                        setVaultEntriesCount(prev => prev + result.imported);
                                                                        // Reset import state
                                                                        setKeystoreMetadata(null);
                                                                        setKeystoreImportFilePath(null);
                                                                        setKeystoreImportPassword('');
                                                                    } catch (err) {
                                                                        const errStr = String(err);
                                                                        if (errStr.includes('Invalid password') || errStr.includes('decrypt')) {
                                                                            setKeystoreMessage({ type: 'error', text: t('settings.invalidPassword') });
                                                                        } else {
                                                                            setKeystoreMessage({ type: 'error', text: errStr });
                                                                        }
                                                                    } finally {
                                                                        setKeystoreImporting(false);
                                                                    }
                                                                }}
                                                                disabled={keystoreImporting || keystoreImportPassword.length < 8}
                                                                className="flex-1 px-4 py-2 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-400 text-white rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2"
                                                            >
                                                                {keystoreImporting ? (
                                                                    <>
                                                                        <svg className="animate-spin h-4 w-4" viewBox="0 0 24 24"><circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" fill="none" /><path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" /></svg>
                                                                        {t('common.loading')}
                                                                    </>
                                                                ) : (
                                                                    <>
                                                                        <Upload size={16} />
                                                                        {t('settings.importKeystore')}
                                                                    </>
                                                                )}
                                                            </button>
                                                            <button
                                                                onClick={() => {
                                                                    setKeystoreMetadata(null);
                                                                    setKeystoreImportFilePath(null);
                                                                    setKeystoreImportPassword('');
                                                                    setKeystoreMessage(null);
                                                                }}
                                                                className="px-4 py-2 bg-gray-100 hover:bg-gray-200 dark:bg-gray-700 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-300 rounded-lg text-sm font-medium transition-colors"
                                                            >
                                                                {t('common.cancel')}
                                                            </button>
                                                        </div>
                                                    </div>
                                                )}
                                            </div>
                                        </div>
                                    </div>
                                </div>
                            )}

                            {activeTab === 'privacy' && (
                                <div className="space-y-6">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.privacy')}</h3>

                                    <div className="space-y-6">
                                        <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-4">
                                            <div className="flex items-start gap-3">
                                                <div className="p-2 bg-blue-100 dark:bg-blue-900/30 text-blue-600 dark:text-blue-400 rounded-lg">
                                                    <Shield size={24} />
                                                </div>
                                                <div>
                                                    <h4 className="font-medium text-base">{t('settings.privacyDesc')}</h4>
                                                    <p className="text-sm text-gray-500 mt-1">{t('settings.privacyRespect')}</p>
                                                </div>
                                            </div>

                                            <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
                                                <label className="flex items-center gap-3 cursor-pointer">
                                                    <input
                                                        type="checkbox"
                                                        checked={settings.analyticsEnabled}
                                                        onChange={e => updateSetting('analyticsEnabled', e.target.checked)}
                                                        className="w-5 h-5 rounded"
                                                    />
                                                    <div>
                                                        <p className="font-medium">{t('settings.sendAnalytics')}</p>
                                                        <p className="text-sm text-gray-500">{t('settings.analyticsDesc')}</p>
                                                    </div>
                                                </label>
                                            </div>
                                        </div>

                                        <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                                            <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                                                <h4 className="font-medium flex items-center gap-2 text-sm">
                                                    <Key size={14} className="text-gray-500" />
                                                    {t('settings.securityTitle')}
                                                </h4>
                                            </div>
                                            <div className="p-4 space-y-3">
                                                <div className="flex items-start gap-3">
                                                    <div className="w-5 h-5 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                        <svg className="w-3 h-3 text-green-600 dark:text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                        </svg>
                                                    </div>
                                                    <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityLocal')}</span>
                                                </div>
                                                <div className="flex items-start gap-3">
                                                    <div className="w-5 h-5 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                        <svg className="w-3 h-3 text-green-600 dark:text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                        </svg>
                                                    </div>
                                                    <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityOAuth')}</span>
                                                </div>
                                                <div className="flex items-start gap-3">
                                                    <div className="w-5 h-5 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                        <svg className="w-3 h-3 text-green-600 dark:text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                        </svg>
                                                    </div>
                                                    <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityNoSend')}</span>
                                                </div>
                                                <div className="flex items-start gap-3">
                                                    <div className="w-5 h-5 rounded-full bg-green-100 dark:bg-green-900/30 flex items-center justify-center shrink-0 mt-0.5">
                                                        <svg className="w-3 h-3 text-green-600 dark:text-green-400" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                                            <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={3} d="M5 13l4 4L19 7" />
                                                        </svg>
                                                    </div>
                                                    <span className="text-sm text-gray-600 dark:text-gray-300">{t('settings.securityTLS')}</span>
                                                </div>
                                            </div>
                                        </div>
                                    </div>
                                </div>
                            )}
                        </div>
                    </div>



                    {/* Footer */}
                    <div className="flex items-center justify-end gap-3 p-4 border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
                        <button
                            onClick={onClose}
                            className="px-4 py-2 text-sm hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        >
                            {t('common.cancel')}
                        </button>
                        <button
                            onClick={handleSave}
                            disabled={!hasChanges && saveState === 'idle'}
                            className={`px-4 py-2 text-sm rounded-lg transition-all duration-300 flex items-center gap-2 min-w-[140px] justify-center ${saveState === 'saving'
                                    ? 'bg-blue-500 text-white'
                                    : saveState === 'saved'
                                        ? 'bg-emerald-500 text-white'
                                        : hasChanges
                                            ? 'bg-blue-500 hover:bg-blue-600 text-white'
                                            : 'bg-gray-200 dark:bg-gray-700 text-gray-500 cursor-not-allowed'
                                }`}
                        >
                            {saveState === 'saving' ? (
                                <>
                                    <svg className="w-4 h-4 animate-spin" viewBox="0 0 24 24" fill="none">
                                        <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" />
                                        <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" />
                                    </svg>
                                    {t('settings.saving')}
                                </>
                            ) : saveState === 'saved' ? (
                                <>
                                    <svg className="w-4 h-4" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                        <path strokeLinecap="round" strokeLinejoin="round" strokeWidth={2.5} d="M5 13l4 4L19 7" />
                                    </svg>
                                    {t('settings.saved')}
                                </>
                            ) : (
                                t('settings.saveChanges')
                            )}
                        </button>
                    </div>
                </div>
            </div>

            {/* Export/Import Dialog */}
            {showExportImport && (
                <ExportImportDialog
                    servers={servers}
                    onImport={(newServers) => {
                        // Read ground truth from localStorage to avoid stale state
                        let currentServers: ServerProfile[] = [];
                        try {
                            const stored = localStorage.getItem(SERVERS_KEY);
                            if (stored) currentServers = JSON.parse(stored);
                        } catch { /* fallback */ }
                        if (currentServers.length === 0) currentServers = servers;
                        const updated = [...currentServers, ...newServers];
                        setServers(updated);
                        localStorage.setItem(SERVERS_KEY, JSON.stringify(updated));
                        secureStoreAndClean('server_profiles', SERVERS_KEY, updated).catch(() => { });
                        setShowExportImport(false);
                        onServersChanged?.();
                    }}
                    onClose={() => setShowExportImport(false)}
                />
            )}
        </>
    );
};

export default SettingsPanel;
