import * as React from 'react';
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { sendNotification } from '@tauri-apps/plugin-notification';
import { X, Settings, Server, Upload, Palette, Trash2, Edit, Plus, FolderOpen, Wifi, FileCheck, Cloud, ExternalLink, Key, Clock, Shield, Lock, Eye, EyeOff, ShieldCheck } from 'lucide-react';
import { ServerProfile, isOAuthProvider, ProviderType } from '../types';
import { LanguageSelector } from './LanguageSelector';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { ExportImportDialog } from './ExportImportDialog';
import { ImportExportIcon } from './icons/ImportExportIcon';
import { LOCK_SCREEN_PATTERNS } from './LockScreen';
import { APP_BACKGROUND_PATTERNS, APP_BACKGROUND_KEY, DEFAULT_APP_BACKGROUND } from '../utils/appBackgroundPatterns';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';

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
};

// Get display info for a server (matches SavedServers sidebar schema)
const getServerDisplayInfo = (server: ServerProfile) => {
    const protocol = server.protocol || 'ftp';
    const isOAuth = isOAuthProvider(protocol as ProviderType);

    if (isOAuth) {
        const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud' };
        return `OAuth2 — ${server.username || providerNames[protocol] || protocol}`;
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
}

// Settings storage key
const SETTINGS_KEY = 'aeroftp_settings';
const SERVERS_KEY = 'aeroftp-saved-servers';
const OAUTH_SETTINGS_KEY = 'aeroftp_oauth_settings';

interface OAuthSettings {
    googledrive: { clientId: string; clientSecret: string };
    dropbox: { clientId: string; clientSecret: string };
    onedrive: { clientId: string; clientSecret: string };
    box: { clientId: string; clientSecret: string };
    pcloud: { clientId: string; clientSecret: string };
}

const defaultOAuthSettings: OAuthSettings = {
    googledrive: { clientId: '', clientSecret: '' },
    dropbox: { clientId: '', clientSecret: '' },
    onedrive: { clientId: '', clientSecret: '' },
    box: { clientId: '', clientSecret: '' },
    pcloud: { clientId: '', clientSecret: '' },
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
    // Notifications
    showToastNotifications: boolean;
    // Privacy
    analyticsEnabled: boolean;
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
    showToastNotifications: false,  // Default off - use Activity Log instead
    analyticsEnabled: false,
};

type TabId = 'general' | 'connection' | 'servers' | 'transfers' | 'filehandling' | 'cloudproviders' | 'ui' | 'privacy' | 'security';

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
                    alert(`Update Available!\n\nAeroFTP v${info.latest_version} (.${info.install_format})\n\nDownload: ${info.download_url || 'https://github.com/axpnet/aeroftp/releases/latest'}`);
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
                    alert(`Up to date!\n\nRunning AeroFTP v${info.current_version}`);
                }
            }
        } catch (err) {
            console.error('[CheckUpdateButton] Update check failed:', err);
            // Log error to Activity Log
            onActivityLog?.logRaw('activity.update_error', 'UPDATE', { error: String(err) }, 'error');
            alert(`Update check failed\n\n${String(err)}`);
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

export const SettingsPanel: React.FC<SettingsPanelProps> = ({ isOpen, onClose, onOpenCloudPanel, onActivityLog, initialTab, onServersChanged }) => {
    const [activeTab, setActiveTab] = useState<TabId>(initialTab || 'general');

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
    const [autoLockTimeout, setAutoLockTimeout] = useState(5); // minutes
    const [showMasterPassword, setShowMasterPassword] = useState(false);
    const [masterPasswordError, setMasterPasswordError] = useState('');
    const [masterPasswordSuccess, setMasterPasswordSuccess] = useState('');
    const [isSettingPassword, setIsSettingPassword] = useState(false);
    const [passwordBtnState, setPasswordBtnState] = useState<'idle' | 'encrypting' | 'done'>('idle');
    const [isSavingTimeout, setIsSavingTimeout] = useState(false);

    // i18n hook
    const { language, setLanguage, t, availableLanguages } = useI18n();

    // Load settings on open
    useEffect(() => {
        if (isOpen) {
            try {
                const saved = localStorage.getItem(SETTINGS_KEY);
                if (saved) setSettings({ ...defaultSettings, ...JSON.parse(saved) });
                const savedServers = localStorage.getItem(SERVERS_KEY);
                if (savedServers) setServers(JSON.parse(savedServers));
                // Load OAuth settings from secure credential store (fallback: localStorage)
                const loadOAuthFromStore = async () => {
                    const providers = ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud'] as const;
                    const loaded = { ...defaultOAuthSettings };
                    for (const p of providers) {
                        try {
                            const id = await invoke<string>('get_credential', { account: `oauth_${p}_client_id` });
                            const secret = await invoke<string>('get_credential', { account: `oauth_${p}_client_secret` });
                            loaded[p] = { clientId: id || '', clientSecret: secret || '' };
                        } catch {
                            // Fallback: legacy localStorage
                            const legacyOAuth = localStorage.getItem(OAUTH_SETTINGS_KEY);
                            if (legacyOAuth) {
                                const parsed = JSON.parse(legacyOAuth);
                                if (parsed[p]) loaded[p] = parsed[p];
                            }
                        }
                    }
                    setOauthSettings(loaded);
                };
                loadOAuthFromStore();
                const savedAnalytics = localStorage.getItem('analytics_enabled');
                if (savedAnalytics !== null) {
                    setSettings(prev => ({ ...prev, analyticsEnabled: savedAnalytics === 'true' }));
                }
                // Load credential store status
                invoke<{ master_mode: boolean; is_locked: boolean; timeout_seconds: number }>('get_credential_store_status')
                    .then(status => {
                        setMasterPasswordStatus({
                            is_set: status.master_mode,
                            is_locked: status.is_locked,
                            timeout_seconds: Number(status.timeout_seconds),
                        });
                        if (status.timeout_seconds > 0) {
                            setAutoLockTimeout(Math.floor(Number(status.timeout_seconds) / 60));
                        }
                    })
                    .catch(console.error);
            } catch { }
        }
    }, [isOpen]);

    const flashSaved = () => {
        setSaveState('saving');
        setTimeout(() => setSaveState('saved'), 800);
        setTimeout(() => setSaveState('idle'), 1800);
    };

    const handleSave = async () => {
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
        localStorage.setItem(SERVERS_KEY, JSON.stringify(servers));
        // Save OAuth secrets to secure credential store sequentially (avoid vault write races)
        const providers = ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud'] as const;
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
        <div className="fixed inset-0 z-50 flex items-center justify-center">
            {/* Backdrop */}
            <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />

            {/* Panel */}
            <div className="relative bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-full max-w-3xl max-h-[85vh] overflow-hidden animate-scale-in flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3">
                        <div className="p-2 bg-gradient-to-br from-blue-500 to-cyan-500 rounded-lg">
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
                                className={`w-full flex items-center gap-2 px-3 py-2 rounded-lg text-sm transition-colors ${activeTab === tab.id
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
                                                placeholder="e.g., /home/user/Downloads"
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
                                        <span className="text-sm">Show hidden files (dotfiles)</span>
                                    </label>

                                    <label className="flex items-center gap-3 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={settings.confirmBeforeDelete}
                                            onChange={e => updateSetting('confirmBeforeDelete', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <span className="text-sm">Confirm before deleting files</span>
                                    </label>

                                    <label className="flex items-center gap-3 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={settings.rememberLastFolder}
                                            onChange={e => updateSetting('rememberLastFolder', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <div>
                                            <p className="text-sm">Remember last folder</p>
                                            <p className="text-xs text-gray-500">Open the last visited local folder on startup</p>
                                        </div>
                                    </label>

                                    <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
                                        <label className="block text-sm font-medium mb-2">Double-click action</label>
                                        <div className="flex gap-4">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="radio"
                                                    name="doubleClickAction"
                                                    checked={settings.doubleClickAction === 'preview'}
                                                    onChange={() => updateSetting('doubleClickAction', 'preview')}
                                                    className="w-4 h-4"
                                                />
                                                <span className="text-sm">Preview file</span>
                                            </label>
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="radio"
                                                    name="doubleClickAction"
                                                    checked={settings.doubleClickAction === 'download'}
                                                    onChange={() => updateSetting('doubleClickAction', 'download')}
                                                    className="w-4 h-4"
                                                />
                                                <span className="text-sm">Download/Upload</span>
                                            </label>
                                        </div>
                                        <p className="text-xs text-gray-500 mt-1">What happens when you double-click a file</p>
                                    </div>

                                    <div className="pt-4 border-t border-gray-200 dark:border-gray-700">
                                        <h4 className="text-sm font-medium mb-2">Software Updates</h4>
                                        <CheckUpdateButton onActivityLog={onActivityLog} />
                                    </div>
                                </div>
                            </div>
                        )}

                        {activeTab === 'connection' && (
                            <div className="space-y-6">
                                <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">Connection Settings</h3>

                                <div className="space-y-4">
                                    <div>
                                        <label className="block text-sm font-medium mb-1">Connection Timeout</label>
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
                                        <p className="text-xs text-gray-500 mt-1">Time before connection attempt times out</p>
                                    </div>

                                    <div>
                                        <label className="block text-sm font-medium mb-1">TLS Version</label>
                                        <select
                                            value={settings.tlsVersion}
                                            onChange={e => updateSetting('tlsVersion', e.target.value as 'auto' | '1.2' | '1.3')}
                                            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                        >
                                            <option value="auto">Auto (Recommended)</option>
                                            <option value="1.2">TLS 1.2 Minimum</option>
                                            <option value="1.3">TLS 1.3 Only</option>
                                        </select>
                                    </div>

                                    <div className="grid grid-cols-2 gap-4">
                                        <div>
                                            <label className="block text-sm font-medium mb-1">Reconnect Attempts</label>
                                            <select
                                                value={settings.reconnectAttempts}
                                                onChange={e => updateSetting('reconnectAttempts', parseInt(e.target.value))}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                {[0, 1, 2, 3, 5, 10].map(n => (
                                                    <option key={n} value={n}>{n === 0 ? 'Disabled' : `${n} attempts`}</option>
                                                ))}
                                            </select>
                                        </div>
                                        <div>
                                            <label className="block text-sm font-medium mb-1">Reconnect Delay</label>
                                            <select
                                                value={settings.reconnectDelay}
                                                onChange={e => updateSetting('reconnectDelay', parseInt(e.target.value))}
                                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                            >
                                                {[1, 3, 5, 10, 30].map(n => (
                                                    <option key={n} value={n}>{n} seconds</option>
                                                ))}
                                            </select>
                                        </div>
                                    </div>

                                    <div>
                                        <label className="block text-sm font-medium mb-2">FTP Mode</label>
                                        <div className="flex gap-4">
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="radio"
                                                    name="ftpMode"
                                                    checked={settings.ftpMode === 'passive'}
                                                    onChange={() => updateSetting('ftpMode', 'passive')}
                                                    className="w-4 h-4"
                                                />
                                                <span className="text-sm">Passive (Recommended)</span>
                                            </label>
                                            <label className="flex items-center gap-2 cursor-pointer">
                                                <input
                                                    type="radio"
                                                    name="ftpMode"
                                                    checked={settings.ftpMode === 'active'}
                                                    onChange={() => updateSetting('ftpMode', 'active')}
                                                    className="w-4 h-4"
                                                />
                                                <span className="text-sm">Active</span>
                                            </label>
                                        </div>
                                        <p className="text-xs text-gray-500 mt-1">Passive mode works better behind NAT/firewalls</p>
                                    </div>
                                </div>
                            </div>
                        )}

                        {activeTab === 'servers' && (
                            <div className="space-y-6">
                                <div className="flex items-center justify-between">
                                    <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">Saved Servers</h3>
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
                                            <Plus size={14} /> Add Server
                                        </button>
                                    </div>
                                </div>

                                {servers.length === 0 ? (
                                    <div className="text-center py-8 text-gray-500">
                                        <Server size={48} className="mx-auto mb-3 opacity-30" />
                                        <p>No saved servers</p>
                                        <p className="text-sm">Add servers from the connection screen</p>
                                    </div>
                                ) : (
                                    <div className="space-y-2">
                                        {servers.map(server => {
                                            const protocol = server.protocol || 'ftp';
                                            const isOAuth = isOAuthProvider(protocol as ProviderType);
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
                                    const isOAuth = isOAuthProvider(protocol as ProviderType);
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
                                            host: isOAuthProvider(newProtocol as ProviderType) ? '' : editingServer.host,
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
                                                        <label className="block text-xs font-medium text-gray-500 mb-1">Display Name</label>
                                                        <input
                                                            type="text"
                                                            placeholder="My Server"
                                                            value={editingServer.name}
                                                            onChange={e => setEditingServer({ ...editingServer, name: e.target.value })}
                                                            className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg"
                                                        />
                                                    </div>

                                                    {/* OAuth providers - read-only info */}
                                                    {isOAuth && (
                                                        <div className="p-3 bg-blue-50 dark:bg-blue-900/30 rounded-lg border border-blue-200 dark:border-blue-700">
                                                            <p className="text-sm text-blue-700 dark:text-blue-300">
                                                                <strong>OAuth2 Connection</strong>
                                                            </p>
                                                            <p className="text-xs text-blue-600 dark:text-blue-400 mt-1">
                                                                Authentication is managed via browser. Configure credentials in Settings → Cloud Providers.
                                                            </p>
                                                        </div>
                                                    )}

                                                    {/* MEGA - email only */}
                                                    {isMega && (
                                                        <>
                                                            <div>
                                                                <label className="block text-xs font-medium text-gray-500 mb-1">MEGA Email</label>
                                                                <input
                                                                    type="email"
                                                                    placeholder="email@example.com"
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
                                                                    Session {Date.now() > editingServer.options.session_expires_at ? 'expired' : 'expires'}: {new Date(editingServer.options.session_expires_at).toLocaleString()}
                                                                </div>
                                                            )}
                                                        </>
                                                    )}

                                                    {/* Filen - email + password, no host */}
                                                    {isFilen && (
                                                        <div>
                                                            <label className="block text-xs font-medium text-gray-500 mb-1">Filen Email</label>
                                                            <input
                                                                type="email"
                                                                placeholder="email@example.com"
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
                                                                    placeholder="21"
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
                                                                    placeholder="••••••••"
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
                                                                    placeholder="my-bucket"
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
                                                                    placeholder="us-east-1"
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
                                                            placeholder="/home/user or /my-folder"
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
                                                                placeholder="/home/user/downloads"
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
                                <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">File Handling</h3>

                                <div className="space-y-4">
                                    <div>
                                        <label className="block text-sm font-medium mb-1">When file exists on destination</label>
                                        <select
                                            value={settings.fileExistsAction}
                                            onChange={e => updateSetting('fileExistsAction', e.target.value as AppSettings['fileExistsAction'])}
                                            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                        >
                                            <option value="ask">Ask each time</option>
                                            <option value="overwrite">Overwrite</option>
                                            <option value="skip">Skip</option>
                                            <option value="rename">Rename (add number)</option>
                                            <option value="resume">Resume if possible</option>
                                            <option disabled>── Smart Sync ──</option>
                                            <option value="overwrite_if_newer">Overwrite if source is newer</option>
                                            <option value="overwrite_if_different">Overwrite if date or size differs</option>
                                            <option value="skip_if_identical">Skip if identical (date &amp; size)</option>
                                        </select>
                                    </div>

                                    <div>
                                        <label className="block text-sm font-medium mb-1">Transfer Mode</label>
                                        <select
                                            value={settings.transferMode}
                                            onChange={e => updateSetting('transferMode', e.target.value as 'auto' | 'ascii' | 'binary')}
                                            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                        >
                                            <option value="auto">Auto (Recommended)</option>
                                            <option value="binary">Binary (images, archives)</option>
                                            <option value="ascii">ASCII (text files)</option>
                                        </select>
                                        <p className="text-xs text-gray-500 mt-1">Auto mode detects file type and chooses appropriately</p>
                                    </div>

                                    <label className="flex items-center gap-3 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={settings.preserveTimestamps}
                                            onChange={e => updateSetting('preserveTimestamps', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <div>
                                            <p className="font-medium">Preserve file timestamps</p>
                                            <p className="text-sm text-gray-500">Keep original modification dates when transferring</p>
                                        </div>
                                    </label>
                                </div>
                            </div>
                        )}

                        {activeTab === 'transfers' && (
                            <div className="space-y-6">
                                <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">Transfer Settings</h3>

                                <div className="space-y-4">
                                    <div>
                                        <label className="block text-sm font-medium mb-1">Concurrent Transfers</label>
                                        <select
                                            value={settings.maxConcurrentTransfers}
                                            onChange={e => updateSetting('maxConcurrentTransfers', parseInt(e.target.value))}
                                            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                        >
                                            {[1, 2, 3, 4, 5].map(n => (
                                                <option key={n} value={n}>{n} {n === 1 ? 'file' : 'files'} at a time</option>
                                            ))}
                                        </select>
                                    </div>

                                    <div>
                                        <label className="block text-sm font-medium mb-1">Retry Count on Error</label>
                                        <select
                                            value={settings.retryCount}
                                            onChange={e => updateSetting('retryCount', parseInt(e.target.value))}
                                            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                        >
                                            {[0, 1, 2, 3, 5].map(n => (
                                                <option key={n} value={n}>{n === 0 ? 'No retries' : `${n} retries`}</option>
                                            ))}
                                        </select>
                                    </div>

                                    <div>
                                        <label className="block text-sm font-medium mb-1">Connection Timeout</label>
                                        <select
                                            value={settings.timeoutSeconds}
                                            onChange={e => updateSetting('timeoutSeconds', parseInt(e.target.value))}
                                            className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-gray-900 dark:text-gray-100 text-sm"
                                        >
                                            {[10, 30, 60, 120].map(n => (
                                                <option key={n} value={n}>{n} seconds</option>
                                            ))}
                                        </select>
                                    </div>
                                </div>
                            </div>
                        )}

                        {activeTab === 'cloudproviders' && (
                            <div className="space-y-6">
                                <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">Cloud Provider Settings</h3>
                                <p className="text-sm text-gray-500">Configure cloud storage providers. AeroCloud uses your FTP server, OAuth2 providers need API credentials.</p>

                                {/* AeroCloud - First! */}
                                <div className="p-4 bg-gradient-to-r from-sky-50 to-blue-50 dark:from-sky-900/30 dark:to-blue-900/30 border border-sky-200 dark:border-sky-700 rounded-lg space-y-3">
                                    <div className="flex items-center justify-between">
                                        <div className="flex items-center gap-2">
                                            <div className="w-8 h-8 bg-gradient-to-br from-sky-400 to-blue-500 rounded-lg flex items-center justify-center shadow">
                                                <Cloud size={16} className="text-white" />
                                            </div>
                                            <div>
                                                <h4 className="font-medium">AeroCloud</h4>
                                                <p className="text-xs text-gray-500">Personal FTP-based cloud sync</p>
                                            </div>
                                        </div>
                                        <span className="text-xs bg-sky-100 dark:bg-sky-800 text-sky-700 dark:text-sky-300 px-2 py-0.5 rounded-full">
                                            No API keys needed
                                        </span>
                                    </div>
                                    <p className="text-sm text-gray-600 dark:text-gray-400">
                                        Turn any FTP/FTPS server into your personal cloud. Configure sync folders, intervals, and conflict resolution.
                                    </p>
                                    <button
                                        onClick={() => {
                                            onClose();
                                            onOpenCloudPanel?.();
                                        }}
                                        className="w-full py-2 bg-gradient-to-r from-sky-500 to-blue-600 text-white rounded-lg text-sm font-medium hover:from-sky-600 hover:to-blue-700 transition-all"
                                    >
                                        Configure in AeroCloud Panel →
                                    </button>
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
                                                <p className="text-xs text-gray-500">Connect with Google Account</p>
                                            </div>
                                        </div>
                                        <button
                                            onClick={() => openUrl('https://console.cloud.google.com/apis/credentials')}
                                            className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                        >
                                            Get credentials <ExternalLink size={12} />
                                        </button>
                                    </div>
                                    <div className="grid grid-cols-2 gap-3">
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client ID</label>
                                            <input
                                                type="text"
                                                value={oauthSettings.googledrive.clientId}
                                                onChange={e => updateOAuthSetting('googledrive', 'clientId', e.target.value)}
                                                placeholder="xxxxxxxx.apps.googleusercontent.com"
                                                className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client Secret</label>
                                            <input
                                                type="password"
                                                value={oauthSettings.googledrive.clientSecret}
                                                onChange={e => updateOAuthSetting('googledrive', 'clientSecret', e.target.value)}
                                                placeholder="GOCSPX-..."
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
                                                <p className="text-xs text-gray-500">Connect with Dropbox Account</p>
                                            </div>
                                        </div>
                                        <button
                                            onClick={() => openUrl('https://www.dropbox.com/developers/apps')}
                                            className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                        >
                                            Get credentials <ExternalLink size={12} />
                                        </button>
                                    </div>
                                    <div className="grid grid-cols-2 gap-3">
                                        <div>
                                            <label className="block text-xs font-medium mb-1">App Key</label>
                                            <input
                                                type="text"
                                                value={oauthSettings.dropbox.clientId}
                                                onChange={e => updateOAuthSetting('dropbox', 'clientId', e.target.value)}
                                                placeholder="xxxxxxxxxxxxxxx"
                                                className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-xs font-medium mb-1">App Secret</label>
                                            <input
                                                type="password"
                                                value={oauthSettings.dropbox.clientSecret}
                                                onChange={e => updateOAuthSetting('dropbox', 'clientSecret', e.target.value)}
                                                placeholder="xxxxxxxxxxxxxxx"
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
                                                <p className="text-xs text-gray-500">Connect with Microsoft Account</p>
                                            </div>
                                        </div>
                                        <button
                                            onClick={() => openUrl('https://portal.azure.com/#blade/Microsoft_AAD_RegisteredApps')}
                                            className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                        >
                                            Get credentials <ExternalLink size={12} />
                                        </button>
                                    </div>
                                    <div className="grid grid-cols-2 gap-3">
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Application (client) ID</label>
                                            <input
                                                type="text"
                                                value={oauthSettings.onedrive.clientId}
                                                onChange={e => updateOAuthSetting('onedrive', 'clientId', e.target.value)}
                                                placeholder="xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
                                                className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client Secret</label>
                                            <input
                                                type="password"
                                                value={oauthSettings.onedrive.clientSecret}
                                                onChange={e => updateOAuthSetting('onedrive', 'clientSecret', e.target.value)}
                                                placeholder="xxxxxxxx~..."
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
                                                <p className="text-xs text-gray-500">Connect with Box Account</p>
                                            </div>
                                        </div>
                                        <button
                                            onClick={() => openUrl('https://app.box.com/developers/console')}
                                            className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                        >
                                            Get credentials <ExternalLink size={12} />
                                        </button>
                                    </div>
                                    <div className="grid grid-cols-2 gap-3">
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client ID</label>
                                            <input
                                                type="text"
                                                value={oauthSettings.box.clientId}
                                                onChange={e => updateOAuthSetting('box', 'clientId', e.target.value)}
                                                placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
                                                className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client Secret</label>
                                            <input
                                                type="password"
                                                value={oauthSettings.box.clientSecret}
                                                onChange={e => updateOAuthSetting('box', 'clientSecret', e.target.value)}
                                                placeholder="xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx"
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
                                                <p className="text-xs text-gray-500">Connect with pCloud Account</p>
                                            </div>
                                        </div>
                                        <button
                                            onClick={() => openUrl('https://docs.pcloud.com/methods/oauth_2.0/authorize.html')}
                                            className="text-xs text-blue-500 hover:text-blue-600 flex items-center gap-1"
                                        >
                                            Get credentials <ExternalLink size={12} />
                                        </button>
                                    </div>
                                    <div className="grid grid-cols-2 gap-3">
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client ID</label>
                                            <input
                                                type="text"
                                                value={oauthSettings.pcloud.clientId}
                                                onChange={e => updateOAuthSetting('pcloud', 'clientId', e.target.value)}
                                                placeholder="xxxxxxxxxxxxxxx"
                                                className="w-full px-3 py-2 text-sm rounded-lg border dark:bg-gray-800 dark:border-gray-600"
                                            />
                                        </div>
                                        <div>
                                            <label className="block text-xs font-medium mb-1">Client Secret</label>
                                            <input
                                                type="password"
                                                value={oauthSettings.pcloud.clientSecret}
                                                onChange={e => updateOAuthSetting('pcloud', 'clientSecret', e.target.value)}
                                                placeholder="xxxxxxxxxxxxxxx"
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
                            <div className="space-y-6">
                                <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('settings.appearance')}</h3>

                                <div className="space-y-4">
                                    {/* Language Selector */}
                                    <LanguageSelector
                                        currentLanguage={language}
                                        availableLanguages={availableLanguages}
                                        onSelect={(lang) => {
                                            setLanguage(lang);
                                            setHasChanges(true);
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
                                        <input
                                            type="checkbox"
                                            checked={settings.showStatusBar}
                                            onChange={e => updateSetting('showStatusBar', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <div>
                                            <p className="font-medium">{t('settings.showStatusBar')}</p>
                                            <p className="text-sm text-gray-500">{t('settings.showStatusBarDesc')}</p>
                                        </div>
                                    </label>

                                    <label className="flex items-center gap-3 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={settings.showSystemMenu}
                                            onChange={e => updateSetting('showSystemMenu', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <div>
                                            <p className="font-medium">{t('settings.showSystemMenuBar')}</p>
                                            <p className="text-sm text-gray-500">{t('settings.showSystemMenuBarDesc')}</p>
                                        </div>
                                    </label>

                                    <label className="flex items-center gap-3 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={settings.compactMode}
                                            onChange={e => updateSetting('compactMode', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <div>
                                            <p className="font-medium">{t('settings.compactMode')}</p>
                                            <p className="text-sm text-gray-500">{t('settings.compactModeDesc')}</p>
                                        </div>
                                    </label>

                                    <label className="flex items-center gap-3 cursor-pointer">
                                        <input
                                            type="checkbox"
                                            checked={settings.showToastNotifications}
                                            onChange={e => updateSetting('showToastNotifications', e.target.checked)}
                                            className="w-4 h-4 rounded"
                                        />
                                        <div>
                                            <p className="font-medium">{t('settings.toastNotifications')}</p>
                                            <p className="text-sm text-gray-500">{t('settings.toastNotificationsDesc')}</p>
                                        </div>
                                    </label>
                                </div>

                                {/* App Background Pattern */}
                                <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden mt-6">
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
                                                            // Dispatch event to notify App.tsx
                                                            window.dispatchEvent(new CustomEvent('app-background-changed', { detail: pattern.id }));
                                                        }}
                                                        className={`relative h-16 rounded-lg border-2 overflow-hidden transition-all ${
                                                            isSelected
                                                                ? 'border-blue-500 ring-1 ring-blue-500/30'
                                                                : 'border-gray-300 dark:border-gray-600 hover:border-gray-400 dark:hover:border-gray-500'
                                                        }`}
                                                        title={t(pattern.nameKey)}
                                                    >
                                                        {/* Pattern preview - use Lock Screen patterns (full opacity) with opacity-10 for consistent visibility */}
                                                        <div className="absolute inset-0 bg-gradient-to-br from-gray-800 to-gray-900">
                                                            {pattern.svg && (() => {
                                                                // Find matching Lock Screen pattern for preview (full opacity SVGs)
                                                                const lockPattern = LOCK_SCREEN_PATTERNS.find(p => p.id === pattern.id);
                                                                return lockPattern?.svg ? (
                                                                    <div className="absolute inset-0 opacity-10" style={{ backgroundImage: lockPattern.svg }} />
                                                                ) : null;
                                                            })()}
                                                        </div>
                                                        {/* Label */}
                                                        <div className="absolute inset-0 flex items-end justify-center pb-1">
                                                            <span className="text-[9px] text-gray-300 font-medium">{t(pattern.nameKey)}</span>
                                                        </div>
                                                        {/* Selected check */}
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
                                                        placeholder="••••••••"
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
                                                    onChange={e => setAutoLockTimeout(parseInt(e.target.value))}
                                                    className="flex-1 accent-emerald-500"
                                                />
                                                <span className="w-20 text-sm font-medium text-gray-700 dark:text-gray-300 text-right">
                                                    {autoLockTimeout === 0 ? t('settings.autoLockDisabled') : `${autoLockTimeout} min`}
                                                </span>
                                            </div>
                                            <p className="mt-1 text-xs text-gray-500">{t('settings.autoLockDesc')}</p>
                                            {/* Save Timeout Only - when password is already set */}
                                            {masterPasswordStatus?.is_set && (
                                                <button
                                                    onClick={async () => {
                                                        if (!currentMasterPassword) {
                                                            setMasterPasswordError(t('settings.enterCurrentPassword'));
                                                            return;
                                                        }
                                                        setIsSavingTimeout(true);
                                                        setMasterPasswordError('');
                                                        try {
                                                            // Verify password is correct, then update timeout
                                                            await invoke('change_master_password', {
                                                                oldPassword: currentMasterPassword,
                                                                newPassword: currentMasterPassword,
                                                            });
                                                            const status = await invoke<{ master_mode: boolean; is_locked: boolean; timeout_seconds: number }>('get_credential_store_status');
                                                            setMasterPasswordStatus({
                                                                is_set: status.master_mode,
                                                                is_locked: status.is_locked,
                                                                timeout_seconds: Number(status.timeout_seconds),
                                                            });
                                                            setMasterPasswordSuccess(t('settings.timeoutSaved'));
                                                        } catch (err) {
                                                            setMasterPasswordError(String(err));
                                                        } finally {
                                                            setIsSavingTimeout(false);
                                                        }
                                                    }}
                                                    disabled={isSavingTimeout}
                                                    className="mt-2 px-3 py-1.5 bg-blue-100 hover:bg-blue-200 dark:bg-blue-900/30 dark:hover:bg-blue-900/50 text-blue-700 dark:text-blue-400 rounded-lg text-xs font-medium transition-colors disabled:opacity-50 flex items-center gap-1.5"
                                                >
                                                    {isSavingTimeout ? (
                                                        <>
                                                            <svg className="animate-spin h-3 w-3" viewBox="0 0 24 24"><circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="4" fill="none" /><path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z" /></svg>
                                                            {t('common.loading')}
                                                        </>
                                                    ) : (
                                                        <>
                                                            <Clock size={12} />
                                                            {t('settings.saveTimeout')}
                                                        </>
                                                    )}
                                                </button>
                                            )}
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
                                                            // Change password
                                                            await invoke('change_master_password', {
                                                                oldPassword: currentMasterPassword,
                                                                newPassword: newMasterPassword,
                                                            });
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
                                                    onClick={async () => {
                                                        if (!currentMasterPassword) {
                                                            setMasterPasswordError(t('settings.enterCurrentPassword'));
                                                            return;
                                                        }
                                                        if (confirm(t('settings.confirmRemovePassword'))) {
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
                                                        }
                                                    }}
                                                    className="px-4 py-2 bg-red-100 hover:bg-red-200 text-red-700 dark:bg-red-900/30 dark:hover:bg-red-900/50 dark:text-red-400 rounded-lg text-sm font-medium transition-colors"
                                                >
                                                    {t('settings.removePassword')}
                                                </button>
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
                                                        {/* Pattern preview */}
                                                        <div className="absolute inset-0 bg-gradient-to-br from-gray-800 to-gray-900">
                                                            {pattern.svg && (
                                                                <div className="absolute inset-0 opacity-10" style={{ backgroundImage: pattern.svg }} />
                                                            )}
                                                        </div>
                                                        {/* Label */}
                                                        <div className="absolute inset-0 flex items-end justify-center pb-1">
                                                            <span className="text-[9px] text-gray-300 font-medium">{t(pattern.nameKey)}</span>
                                                        </div>
                                                        {/* Selected check */}
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
                                                <p className="text-sm text-gray-500 mt-1">AeroFTP respects your privacy. We collect minimal data to improve the application.</p>
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
