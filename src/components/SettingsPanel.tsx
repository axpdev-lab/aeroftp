import * as React from 'react';
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { sendNotification } from '@tauri-apps/plugin-notification';
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
        const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', fourshared: '4shared' };
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
    maxConcurrentTransfers: 2,
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
                        secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, { ...defaultSettings, ...saved }).catch(() => {});
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
        secureStoreAndClean('server_profiles', SERVERS_KEY, servers).catch(() => {});
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
            <div className="relative bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-full max-w-3xl max-h-[85vh] overflow-hidden animate-scale-in flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3">
                        <div className={`p-2 rounded-lg bg-gradient-to-br ${
                            appThemeProp === 'tokyo' ? 'from-purple-600 to-violet-500' :
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
                                    const needsHostPort = !isOAuth && !isMega && !isFilen;
                                    const needsPassword = !isOAuth;
                                    const isNewServer = !servers.some(s => s.id === editingServer.id);

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

                                                    {/* Host and Port - for FTP/FTPS/SFTP/WebDAV/S3/Azure */}
                                                    {needsHostPort && (
                                                        <div className="flex gap-2">
                                                            <div className="flex-1">
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">
                                                                    {isS3 ? t('settings.endpointUrl') : t('settings.host')}
                                                                </label>
                                                                <input
                                                                    type="text"
                                                                    placeholder={isS3 ? 's3.amazonaws.com' : 'ftp.example.com'}
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
                                                                {isS3 ? t('settings.accessKeyId') : t('settings.username')}
                                                            </label>
                                                            <input
                                                                type="text"
                                                                placeholder={isS3 ? 'AKIAIOSFODNN7EXAMPLE' : 'username'}
                                                                value={editingServer.username}
                                                                onChange={e => setEditingServer({ ...editingServer, username: e.target.value })}
                                                                className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                            />
                                                        </div>
                                                    )}

                                                    {/* Password - for non-OAuth */}
                                                    {needsPassword && !isMega && (
                                                        <div>
                                                            <label className="block text-xs font-medium text-gray-500 mb-1">
                                                                {isS3 ? t('settings.secretAccessKey') : t('settings.password')}
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
                                                            secureStoreAndClean('server_profiles', SERVERS_KEY, updatedServers).catch(() => {});
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
                                            {[1, 2, 3, 4, 5].map(n => (
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
                                            <div className={`p-2.5 rounded-lg text-xs space-y-2 ${
                                                badgeFeedback.type === 'success'
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
                                            className={`flex items-center gap-1.5 px-3 py-2 text-xs font-medium rounded-t-lg transition-colors ${
                                                appearanceSubTab === sub.id
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
                                                { id: 'tokyo' as Theme, label: t('settings.themeTokyoLabel'), icon: <svg viewBox="0 0 32 32" width={20} height={20} fill="currentColor"><path d="M30.43,12.124c-0.441-1.356-1.289-2.518-2.454-3.358c-1.157-0.835-2.514-1.276-3.924-1.276c-0.44,0.001-0.898,0.055-1.368,0.158c-0.048-0.492-0.145-0.96-0.288-1.395c-0.442-1.345-1.287-2.498-2.442-3.335c-1.111-0.805-2.44-1.212-3.776-1.242C16.054,1.64,16,1.64,16,1.64s-0.105,0.014-0.151,0.036c-1.336,0.029-2.664,0.437-3.776,1.241c-1.155,0.837-2,1.991-2.442,3.335C9.488,6.686,9.392,7.154,9.343,7.648C8.859,7.542,8.415,7.462,7.926,7.491C6.511,7.496,5.153,7.942,4,8.783c-1.151,0.839-1.99,1.994-2.428,3.34s-0.437,2.774,0.001,4.129c0.439,1.358,1.275,2.518,2.417,3.353c0.369,0.271,0.785,0.507,1.239,0.706c-0.251,0.428-0.448,0.863-0.588,1.298c-0.432,1.349-0.427,2.778,0.016,4.135c0.443,1.354,1.282,2.51,2.427,3.341c1.145,0.832,2.503,1.272,3.927,1.275c1.422,0,2.78-0.437,3.926-1.263c0.371-0.268,0.724-0.589,1.053-0.96c0.319,0.36,0.659,0.673,1.013,0.932c1.145,0.839,2.509,1.285,3.946,1.291c1.428,0,2.789-0.441,3.938-1.275c1.153-0.838,1.995-2.004,2.435-3.37c0.439-1.368,0.438-2.804-0.007-4.152c-0.137-0.417-0.329-0.837-0.573-1.251c0.44-0.192,0.842-0.418,1.199-0.675c1.151-0.831,1.998-1.991,2.446-3.355C30.865,14.918,30.869,13.48,30.43,12.124z"/></svg>, colors: ['#1a1b26', '#16161e', '#9d7cd8'], selectedBorder: 'border-purple-500 ring-1 ring-purple-500/30 bg-purple-500/5', selectedIcon: 'text-purple-500', checkBg: 'bg-purple-500', desc: t('settings.themeTokyoDesc') },
                                                { id: 'cyber' as Theme, label: t('settings.themeCyberLabel'), icon: <svg viewBox="31.5 82.5 705.5 705.5" width={20} height={20} fill="currentColor"><path d="m 413.95839,785.385 c 10.2812,-3.209 18.6107,-7.75769 28.0384,-15.31151 21.1336,-16.93304 35.7755,-36.70036 90.4391,-122.09734 41.0968,-64.20239 56.5193,-92.89811 67.9874,-126.5 5.2804,-15.47191 11.3362,-42.0865 11.3366,-49.82332 10e-5,-3.10151 -0.3303,-3.76046 -1.7499,-3.49032 -2.5369,0.48275 -7.8503,8.92686 -9.7679,15.52336 -2.0554,7.0702 -9.6654,26.2121 -17.0999,43.01244 -9.5612,21.6063 -26.2848,49.91776 -51.8504,87.77784 -48.7477,72.19049 -47.3014,70.18612 -49.9192,69.18158 -1.3841,-0.53115 -1.3958,-1.04457 -0.1067,-4.69824 1.7841,-5.05673 9.7203,-18.61617 20.1828,-34.48334 4.3518,-6.6 10.7611,-17.36382 14.2428,-23.91959 3.4817,-6.55578 8.5214,-15.27602 11.1994,-19.37831 3.388,-5.18983 4.8691,-8.48436 4.8691,-10.83041 0,-1.85443 -0.4899,-3.37169 -1.0886,-3.37169 -0.5988,0 -3.4036,2.025 -6.2328,4.5 -4.6608,4.07713 -8.7622,6.06231 -7.1562,3.4638 0.3522,-0.56991 7.8676,-9.23241 16.7007,-19.25 25.2356,-28.6193 33.4111,-40.59765 38.3671,-56.2138 2.8231,-8.89524 4.4619,-24.5 2.573,-24.5 -2.3234,0 -10.0492,11.56409 -23.1529,34.6554 -7.4193,13.07435 -15.1076,25.81298 -17.085,28.30807 -20.1618,25.44015 -57.7915,43.13276 -98.9253,46.51242 -4.125,0.33892 -13.5409,1.7664 -20.9241,3.17218 -13.1742,2.50836 -13.7702,2.5341 -32,1.38248 -21.3367,-1.34788 -31.4673,-3.11017 -34.5552,-6.01113 -1.8305,-1.71963 -3.4968,-2.01942 -11.2247,-2.01942 -11.0647,0 -21.4634,-1.89195 -36.4376,-6.62955 -25.9777,-8.21896 -47.266,-19.11094 -63.2641,-32.36856 -12.2852,-10.18082 -16.6927,-16.50052 -31.0041,-44.45524 -9.6417,-18.83349 -14.9082,-27.54665 -16.65,-27.54665 -2.8273,0 3.9594,22.32503 10.5711,34.77434 10.3659,19.51776 20.6328,32.26524 46.7284,58.01811 37.2419,36.75277 44.8774,42.20863 71.4824,51.07674 22.4366,7.47873 34.659,9.12865 67.6336,9.12997 20.8422,8.4e-4 20.8575,0.002 23.8567,2.52612 2.4201,2.03635 3.6581,2.40204 6.3944,1.88871 2.1955,-0.41188 3.3932,-0.2463 3.3932,0.46911 0,0.60813 -2.7661,4.82071 -6.1469,9.3613 -8.5155,11.4367 -10.307,15.67034 -10.2637,24.2556 0.029,5.73642 0.7461,8.80511 3.973,17 3.9263,9.97113 3.9373,10.03898 3.8337,23.5 -0.097,12.64282 -0.3945,14.61116 -4.6788,31 -6.5274,24.96939 -7.3154,35 -2.7498,35 1.0611,0 5.665,-1.16602 10.2309,-2.59115 z m -54.9142,-6.40885 c -0.1187,-3.575 -2.0622,-13.925 -4.3189,-23 -6.6985,-26.93611 -8.1038,-38.34882 -5.8996,-47.91121 0.5593,-2.42617 3.2483,-9.2131 5.9756,-15.08207 4.4705,-9.6202 4.9546,-11.35198 4.9171,-17.58878 -0.048,-7.95437 -2.0902,-14.39763 -6.5567,-20.68482 -3.6905,-5.19476 -7.0379,-6.91603 -19.999,-10.28361 -5.3732,-1.39607 -14.9744,-4.73803 -21.3361,-7.42657 -6.3616,-2.68855 -14.8518,-5.91445 -18.867,-7.16868 -8.7829,-2.74348 -10.9717,-2.04072 -10.2347,3.28599 0.4952,3.57849 0.4735,12.33109 -0.063,25.35975 -0.2812,6.83172 0.14,9.45455 2.8843,17.96024 3.2407,10.04424 3.8946,14.91707 2.3324,17.38193 -2.3188,3.65854 -8.0662,-4.35362 -14.592,-20.34217 -4.9398,-12.1025 -20.1029,-41.42379 -31.5541,-61.01688 -9.0516,-15.4874 -25.3321,-41.43133 -47.3677,-75.48312 -12.41166,-19.1799 -19.79011,-33.16346 -26.21663,-49.68554 -7.86073,-20.2093 -15.90421,-33.31446 -20.44722,-33.31446 -3.6032,0 -3.24903,4.18127 1.61762,19.09752 16.28268,49.90635 40.63993,98.11942 90.38163,178.90248 19.7033,31.99905 26.2263,41.64641 34.2401,50.64047 3.3616,3.77274 13.1102,17.43453 21.6635,30.35953 12.6987,19.18908 17.3038,25.26212 25.1038,33.10579 5.2537,5.28318 11.6927,10.8402 14.3089,12.34894 6.5961,3.80395 17.6471,7.60029 21.2433,7.2977 l 3,-0.25243 -0.2158,-6.5 z m 27.1335,-58.72632 c -0.089,-4.50338 -2.0953,-7.99601 -3.682,-6.40939 -0.5438,0.54383 -0.6468,3.14679 -0.2411,6.09621 0.556,4.04281 1.0617,5.07534 2.3567,4.81245 1.2312,-0.24994 1.6279,-1.38937 1.5664,-4.49927 z m 23.5757,-120.52368 c 0,-0.6875 -0.894,-2.6 -1.9786,-4.25 -2.6837,-4.08243 -20.7025,-23.45356 -24.9324,-26.80361 -3.2733,-2.59236 -3.5268,-2.63784 -6,-1.07654 -2.9912,1.88826 -2.5678,1.41474 -15.4765,17.30928 -6.7629,8.32726 -9.5694,12.50737 -8.8681,13.20864 1.0237,1.02374 24.7043,2.41777 46.0122,2.70865 8.4183,0.11492 11.2484,-0.16106 11.2434,-1.09642 z m 57.5455,-38.18474 c 28.5871,-6.92592 43.8075,-16.34146 60.0581,-37.15269 7.6105,-9.74633 15.403,-22.16368 15.403,-24.54463 0,-1.88067 -1.0118,-2.06358 -21.4947,-3.88558 -20.6227,-1.83443 -30.0053,-4.96585 -30.0053,-10.01418 0,-2.41879 0.1401,-2.46196 7,-2.15724 3.85,0.17102 12.4734,0.95852 19.1632,1.75 16.1262,1.90794 22.2504,1.8198 40.2291,-0.57896 16.2054,-2.16214 22.4327,-4.09438 30.5019,-9.46426 7.6345,-5.08063 24.6061,-20.52887 29.2698,-26.64256 5.1892,-6.80274 6.5711,-9.59582 9.3383,-18.87516 2.0048,-6.72269 2.2877,-10.3153 2.7304,-34.66389 l 0.4939,-27.16389 -5.8633,-0.6514 c -3.2248,-0.35827 -11.2072,-1.52202 -17.7386,-2.58611 -14.4327,-2.35135 -17.6908,-2.38668 -24.1247,-0.26157 -4.2189,1.39349 -12.4986,1.77054 -53,2.41356 -26.4,0.41914 -51.825,1.07327 -56.5,1.45362 -11.794,0.95954 -15.8696,-0.32055 -23.1597,-7.2741 -3.2122,-3.06386 -5.8403,-6.10001 -5.8403,-6.747 0,-2.61342 6.1985,-10.20719 10.7036,-13.11283 12.0148,-7.74934 29.4167,-17.64593 34.363,-19.54254 13.43,-5.14965 37.0067,-6.3908 53.9334,-2.83922 17.0438,3.57613 32.8738,12.66492 40.4997,23.25289 4.9137,6.8223 5.8079,7.39397 14.388,9.19886 3.7742,0.79392 8.381,2.32255 10.2373,3.39695 4.0209,2.32725 16.1015,6.12667 19.4804,6.12667 2.3747,0 2.3946,-0.0877 2.3946,-10.53868 0,-11.98633 -1.5125,-51.0181 -3.0404,-78.46132 -3.1077,-55.81685 -11.5671,-98.55783 -24.5954,-124.26798 -13.5518,-26.74328 -43.3262,-42.58301 -101.8642,-54.190907 -43.9406,-8.7133 -87.5987,-12.339 -127.5,-10.58856 -95.5719,4.19266 -157.52,18.611137 -186.8548,43.490637 -15.61969,13.2474 -24.90805,32.96649 -32.52811,69.05681 -9.9497,47.12409 -13.46707,84.86293 -14.30664,153.5 l -0.58101,47.5 3.23662,9.5 c 7.95205,23.34048 22.05186,47.35088 34.65557,59.01456 6.94217,6.4244 17.98477,12.77291 24.30057,13.97068 7.7698,1.47351 31.4127,1.20674 47.5778,-0.53684 7.7,-0.83052 18.6125,-1.7858 24.25,-2.12284 9.9703,-0.59606 10.25,-0.55491 10.25,1.50777 0,3.06243 -3.2597,4.69687 -14,7.01969 -21.8041,4.71561 -26.4955,5.35179 -38.7132,5.24965 -14.6582,-0.12254 -15.01,0.17384 -10.2496,8.63517 8.2005,14.57599 18.7824,26.66966 30.0031,34.28948 12.8421,8.72094 43.3835,21.76894 56.1639,23.99454 8.4399,1.46975 15.1923,-0.29877 21.9778,-5.75621 6.833,-5.49562 9.223,-8.95262 9.1051,-13.16978 -0.083,-2.97953 -0.9658,-4.39183 -5.1482,-8.23972 -2.7761,-2.55411 -5.3134,-5.70411 -5.6383,-7 -0.325,-1.29588 0.1026,-6.63115 0.9503,-11.85615 2.6289,-16.20573 1.4678,-27.43417 -2.7711,-26.79696 -1.0865,0.16333 -2.4715,1.19696 -3.0776,2.29696 -0.6062,1.1 -2.5493,4.06437 -4.3181,6.5875 -5.0339,7.18101 -7.2841,13.35197 -7.2841,19.97635 0,5.47544 -0.1812,5.94353 -2.5292,6.53284 -1.8421,0.46233 -3.6156,-0.11802 -6.5275,-2.13603 -8.045,-5.57516 -12.9493,-15.39005 -12.9377,-25.89216 0.012,-11.20982 4.2953,-17.65549 15.9919,-24.06711 8.4019,-4.60561 14.1637,-10.64944 16.4992,-17.30686 2.0216,-5.76243 7.095,-34.27467 9.1094,-51.19453 0.9232,-7.75383 1.5413,-22.76461 1.628,-39.53394 l 0.1398,-27.03394 -7.9369,-15.35226 c -7.1061,-13.7451 -8.6072,-15.93995 -14.3394,-20.96606 -3.5213,-3.08759 -11.6213,-10.26394 -18,-15.94745 -19.8689,-17.70357 -28.8521,-23.13289 -43.4429,-26.25647 -14.1213,-3.02307 -20.6547,-6.19538 -20.6547,-10.029 0,-3.68669 6.0759,-5.2182 18,-4.53717 23.7661,1.35736 37.9651,8.9254 71.2477,37.97499 23.9499,20.90383 29.4533,27.64798 36.4615,44.6813 1.6972,4.125 4.01,11.325 5.1397,16 4.0166,16.62255 4.3336,24.58089 3.2011,80.36346 -0.5864,28.88329 -1.2694,53.10491 -1.5179,53.82581 -1.8176,5.27305 -7.567,48.13486 -7.6176,56.78945 -0.057,9.79567 0.061,10.38603 2.6114,13.0483 6.5803,6.86838 22.8957,10.99253 35.6553,9.01282 7.8852,-1.22344 11.4681,-2.79034 17.4437,-7.62881 2.2687,-1.83695 5.3566,-3.90112 6.8621,-4.58704 3.0567,-1.39272 1.366,-2.82857 15.6873,13.32324 10.6803,12.04551 18.4432,14.00219 37.3646,9.41803 z m -11.4424,-59.31526 c -1.5024,-7.07039 -6.1821,-17.04236 -9.4294,-20.09301 -1.3304,-1.24992 -4.7502,-2.88754 -7.5994,-3.63915 -2.8493,-0.75162 -5.8301,-1.90571 -6.6241,-2.56465 -3.6043,-2.99131 0.1744,-10.76785 4.6824,-9.63641 4.1206,1.0342 14.2711,8.13413 17.5523,12.27727 7.8785,9.94783 10.5796,21.29247 6.3468,26.65595 -1.1936,1.5125 -2.516,2.75 -2.9385,2.75 -0.4226,0 -1.3181,-2.5875 -1.9901,-5.75 z M 163.76001,383.39671 c 0,-1.4187 1.19024,-5.55457 2.64497,-9.19084 2.6349,-6.5862 2.63821,-6.62876 0.86825,-11.17055 -0.97721,-2.50754 -1.98913,-5.76675 -2.24872,-7.24269 -0.50924,-2.89535 -1.3956,-2.45712 11.97038,-5.91827 2.49333,-0.64565 4.65406,-2.2017 6.5,-4.68096 4.83255,-6.49053 12.6122,-13.69689 18.299,-16.95055 14.202,-8.12556 34.5107,-11.82919 55.9661,-10.20637 12.6744,0.95865 20.2923,3.23224 33.5,9.99818 22.4751,11.51333 31.7464,18.16421 33.1732,23.79701 1.3833,5.46121 -5.4349,13.06468 -14.0367,15.65345 -6.6929,2.01426 -23.1882,3.30599 -64.4446,5.0466 -20.5194,0.86571 -40.7694,2.16595 -45,2.88941 -15.20966,2.60101 -30.67387,6.67122 -32.75633,8.62155 -2.83511,2.65524 -4.43555,2.42216 -4.43555,-0.64597 z m 257.66668,-55.08723 c -1.3226,-1.32258 -0.6419,-11.48073 0.9805,-14.63299 6.9436,-13.49101 20.1831,-28.02453 41.8528,-45.94367 25.0509,-20.71509 43.0707,-28.75667 64.4388,-28.75667 8.886,0 13.0612,1.44801 13.0612,4.52973 0,4.11732 -6.2547,7.30363 -23.5681,12.00616 -12.3073,3.34281 -15.2048,4.80192 -25.0703,12.62474 -21.2078,16.81662 -54.6246,47.64765 -63.6116,58.68928 -1.6802,2.06432 -6.5996,2.96712 -8.0833,1.48342 z m -89.5356,-43.5972 c -2.4029,-1.22987 -9.8206,-7.35327 -16.4837,-13.60756 -14.4287,-13.54343 -20.3355,-17.70014 -35.0557,-24.66954 -21.0188,-9.95152 -34.2684,-12.97696 -53.4847,-12.21287 -19.9989,0.7952 -29.5296,5.1125 -45.08685,20.42381 -5.21393,5.13152 -9.82644,9.33003 -10.25001,9.33003 -2.78253,0 2.79639,-11.02836 10.38046,-20.51997 10.2876,-12.87522 27.1804,-24.0405 42.3494,-27.99082 11.2299,-2.9245 26.3904,-3.97349 38.7728,-2.68277 29.2887,3.05299 53.8167,14.04509 74.025,33.17388 12.379,11.71775 16.4366,20.86481 13.7242,30.93804 -2.5529,9.48063 -9.7902,12.47572 -18.8909,7.81777 z m 91.9832,0.63137 c -5.8349,-4.08693 -7.8784,-16.63631 -4.039,-24.80425 2.6047,-5.54125 17.9154,-20.45382 27.5487,-26.83227 32.1866,-21.31167 65.9695,-28.28349 95.7641,-19.76297 16.8713,4.82473 35.0982,17.59234 46.2018,32.36343 5.0377,6.70152 9.7153,15.25299 9.1959,16.81135 -0.1805,0.54148 -5.0118,-3.30575 -10.7361,-8.5494 -17.9897,-16.47897 -30.5119,-21.18049 -54.0497,-20.29311 -28.025,1.05655 -47.4019,10.07505 -82,38.16491 -18.563,15.07113 -22.3125,16.80595 -27.8857,12.90231 z"/></svg>, colors: ['#0a0e17', '#0d1117', '#10b981'], selectedBorder: 'border-emerald-500 ring-1 ring-emerald-500/30 bg-emerald-500/5', selectedIcon: 'text-emerald-500', checkBg: 'bg-emerald-500', desc: t('settings.themeCyberDesc') },
                                            ]).map(themeOption => {
                                                const isSelected = appThemeProp === themeOption.id;
                                                return (
                                                    <button
                                                        key={themeOption.id}
                                                        onClick={() => {
                                                            setAppTheme?.(themeOption.id);
                                                            // Icon theme auto-syncs via useEffect in App.tsx
                                                        }}
                                                        className={`relative flex flex-col items-center gap-2 p-4 rounded-xl border-2 transition-all ${
                                                            isSelected
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
                                                        className={`relative flex flex-col items-center gap-2 p-4 rounded-xl border-2 transition-all ${
                                                            isSelected
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
                                                                className={`relative h-16 rounded-lg border-2 overflow-hidden transition-all ${
                                                                    isSelected
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
                                                                className={`relative h-16 rounded-lg border-2 overflow-hidden transition-all ${
                                                                    isSelected
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
                                        <span className={`inline-flex items-center gap-1.5 px-2.5 py-1 rounded-full text-xs font-medium ${
                                            masterPasswordStatus?.is_set
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
                                                className={`px-4 py-2 min-w-[160px] rounded-lg text-sm font-medium transition-colors flex items-center justify-center gap-2 ${
                                                    passwordBtnState === 'done'
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
                            </div>
                        )}

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
                                            <div className={`p-3 rounded-lg text-sm flex items-center gap-2 ${
                                                keystoreMessage.type === 'success'
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
                        className={`px-4 py-2 text-sm rounded-lg transition-all duration-300 flex items-center gap-2 min-w-[140px] justify-center ${
                            saveState === 'saving'
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
                        const updated = [...servers, ...newServers];
                        setServers(updated);
                        localStorage.setItem(SERVERS_KEY, JSON.stringify(updated));
                        secureStoreAndClean('server_profiles', SERVERS_KEY, updated).catch(() => {});
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
