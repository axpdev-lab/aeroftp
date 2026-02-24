// AeroCloud Panel - Personal FTP-Based Cloud Sync
// Setup wizard, cloud dashboard, and sync controls

import React, { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, emit } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { TransferProgressBar } from './TransferProgressBar';
import {
    Cloud, CloudOff, CloudUpload, CloudDownload, RefreshCw,
    Folder, FolderOpen, Settings, Play, Pause, Check, X,
    AlertCircle, Clock, HardDrive, Server, ChevronRight,
    Loader2, Zap, Shield, History, Radio
} from 'lucide-react';
import { useTraySync } from '../hooks/useTraySync';
import { SyncScheduler } from './SyncScheduler';
import { WatcherStatus } from './WatcherStatus';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';
import { secureGetWithFallback } from '../utils/secureStorage';
import './CloudPanel.css';

// TypeScript interfaces matching Rust structs
interface CloudConfig {
    enabled: boolean;
    cloud_name: string;
    local_folder: string;
    remote_folder: string;
    server_profile: string;
    sync_interval_secs: number;
    sync_on_change: boolean;
    sync_on_startup: boolean;
    exclude_patterns: string[];
    last_sync: string | null;
    conflict_strategy: 'ask_user' | 'keep_both' | 'prefer_local' | 'prefer_remote' | 'prefer_newer';
    public_url_base?: string | null;
    protocol_type: string;
    connection_params: Record<string, unknown>;
}

// Protocol categories for the selector grid
const PROTOCOL_CATEGORIES = [
    {
        label: 'cloud.serverCategory',
        protocols: [
            { id: 'ftp', label: 'FTP' },
            { id: 'ftps', label: 'FTPS' },
            { id: 'sftp', label: 'SFTP' },
            { id: 'webdav', label: 'WebDAV' },
        ],
    },
    {
        label: 'cloud.cloudCategory',
        protocols: [
            { id: 's3', label: 'S3' },
            { id: 'azure', label: 'Azure Blob' },
            { id: 'mega', label: 'MEGA' },
            { id: 'filen', label: 'Filen' },
            { id: 'internxt', label: 'Internxt' },
            { id: 'kdrive', label: 'kDrive' },
            { id: 'jottacloud', label: 'Jottacloud' },
        ],
    },
    {
        label: 'cloud.oauthCategory',
        protocols: [
            { id: 'googledrive', label: 'Google Drive' },
            { id: 'dropbox', label: 'Dropbox' },
            { id: 'onedrive', label: 'OneDrive' },
            { id: 'box', label: 'Box' },
            { id: 'pcloud', label: 'pCloud' },
            { id: 'zohoworkdrive', label: 'Zoho WorkDrive' },
            { id: 'fourshared', label: '4shared' },
        ],
    },
] as const;

const OAUTH2_PROTOCOLS = ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive'];
const OAUTH1_PROTOCOLS = ['fourshared'];
const SERVER_PROTOCOLS = ['ftp', 'ftps', 'sftp', 'webdav'];

/** Protocols requiring email + password (no saved profiles) */
const EMAIL_AUTH_PROTOCOLS = ['mega', 'filen', 'internxt'];

interface CloudSyncStatus {
    type: 'not_configured' | 'idle' | 'syncing' | 'paused' | 'has_conflicts' | 'error';
    last_sync?: string;
    next_sync?: string;
    current_file?: string;
    progress?: number;
    files_done?: number;
    files_total?: number;
    count?: number;
    message?: string;
}

interface SyncResult {
    uploaded: number;
    downloaded: number;
    deleted: number;
    skipped: number;
    conflicts: number;
    errors: string[];
    duration_secs: number;
    file_details?: { path: string; direction: string; size: number }[];
}

interface CloudPanelProps {
    isOpen: boolean;
    onClose: () => void;
}

// Setup Wizard Component (4 steps: Name → Protocol → Connection → Settings)
const SetupWizard: React.FC<{
    savedServers: { id: string; name: string; host: string; port?: number; username?: string; password?: string; initialPath?: string }[];
    onComplete: (config: CloudConfig) => void;
    onCancel: () => void;
}> = ({ savedServers, onComplete, onCancel }) => {
    const t = useTranslation();
    const [step, setStep] = useState(1);
    const [cloudName, setCloudName] = useState(t('cloud.cloudNamePlaceholder'));
    const [localFolder, setLocalFolder] = useState('');
    const [remoteFolder, setRemoteFolder] = useState('/cloud/');
    const [serverProfile, setServerProfile] = useState('');
    const [syncOnChange, setSyncOnChange] = useState(true);
    const [syncInterval, setSyncInterval] = useState(24);
    const [syncUnit, setSyncUnit] = useState<'hours' | 'minutes'>('hours');
    const [isLoading, setIsLoading] = useState(false);

    // Multi-protocol state
    const [selectedProtocol, setSelectedProtocol] = useState('ftp');
    const [connHost, setConnHost] = useState('');
    const [connPort, setConnPort] = useState('');
    const [connUsername, setConnUsername] = useState('');
    const [connPassword, setConnPassword] = useState('');
    const [connExtra, setConnExtra] = useState<Record<string, string>>({});
    const [oauthAuthorized, setOauthAuthorized] = useState(false);
    const [oauthEmail, setOauthEmail] = useState('');
    const [oauthAuthorizing, setOauthAuthorizing] = useState(false);

    useEffect(() => {
        invoke<string>('get_default_cloud_folder').then(setLocalFolder);
    }, []);

    // Reset connection fields when protocol changes
    useEffect(() => {
        setConnHost('');
        setConnPort('');
        setConnUsername('');
        setConnPassword('');
        setConnExtra({});
        setServerProfile('');
        setOauthAuthorized(false);
        setOauthEmail('');
        // Set sensible remote folder defaults
        if (SERVER_PROTOCOLS.includes(selectedProtocol)) {
            setRemoteFolder('/cloud/');
        } else {
            setRemoteFolder('/');
        }
    }, [selectedProtocol]);

    const selectLocalFolder = async () => {
        const selected = await open({ directory: true, multiple: false, title: 'Select AeroCloud Folder' });
        if (selected) setLocalFolder(selected as string);
    };

    const isOAuth = OAUTH2_PROTOCOLS.includes(selectedProtocol) || OAUTH1_PROTOCOLS.includes(selectedProtocol);
    const isServerProtocol = SERVER_PROTOCOLS.includes(selectedProtocol);
    const isEmailAuth = EMAIL_AUTH_PROTOCOLS.includes(selectedProtocol);

    // Build connection_params for the selected protocol
    const buildConnectionParams = (): Record<string, string> => {
        const params: Record<string, string> = { ...connExtra };
        if (connPort) params.port = connPort;
        // OAuth protocols store client_id/client_secret
        if (OAUTH2_PROTOCOLS.includes(selectedProtocol)) {
            if (connExtra.client_id) params.client_id = connExtra.client_id;
            if (connExtra.client_secret) params.client_secret = connExtra.client_secret;
            if (connExtra.region) params.region = connExtra.region;
        }
        if (OAUTH1_PROTOCOLS.includes(selectedProtocol)) {
            if (connExtra.consumer_key) params.consumer_key = connExtra.consumer_key;
            if (connExtra.consumer_secret) params.consumer_secret = connExtra.consumer_secret;
        }
        return params;
    };

    // Determine the effective server profile name for credential storage
    const effectiveProfile = (): string => {
        if (isServerProtocol && serverProfile) return serverProfile;
        return cloudName || selectedProtocol;
    };

    // Check if step can advance
    const canAdvance = (): boolean => {
        switch (step) {
            case 1: return !!localFolder;
            case 2: return !!selectedProtocol;
            case 3:
                if (isOAuth) return oauthAuthorized;
                if (isServerProtocol) return !!serverProfile || (!!connHost && !!connUsername);
                if (isEmailAuth) return !!connUsername && !!connPassword;
                // S3, Azure, kDrive, Jottacloud — need at least some params
                return true;
            default: return true;
        }
    };

    // Handle OAuth authorization
    const handleOAuthAuthorize = async () => {
        setOauthAuthorizing(true);
        try {
            const provider = selectedProtocol === 'fourshared' ? 'fourshared' : selectedProtocol;
            if (OAUTH2_PROTOCOLS.includes(selectedProtocol)) {
                const clientId = connExtra.client_id || '';
                const clientSecret = connExtra.client_secret || '';
                const region = connExtra.region || 'us';
                if (!clientId || !clientSecret) {
                    logger.error('client_id and client_secret are required');
                    return;
                }
                // Full OAuth2 flow (opens browser, waits for callback)
                await invoke('oauth2_full_auth', {
                    params: { provider, client_id: clientId, client_secret: clientSecret, region }
                });
                // Connect to verify and get account info
                const result = await invoke<{ display_name: string; account_email: string | null }>('oauth2_connect', {
                    params: { provider, client_id: clientId, client_secret: clientSecret, region }
                });
                setOauthAuthorized(true);
                setOauthEmail(result.account_email || result.display_name);
            } else if (OAUTH1_PROTOCOLS.includes(selectedProtocol)) {
                const consumerKey = connExtra.consumer_key || '';
                const consumerSecret = connExtra.consumer_secret || '';
                if (!consumerKey || !consumerSecret) {
                    logger.error('consumer_key and consumer_secret are required');
                    return;
                }
                await invoke('fourshared_full_auth', {
                    consumerKey, consumerSecret
                });
                setOauthAuthorized(true);
                setOauthEmail('4shared');
            }
        } catch (error) {
            logger.error('OAuth failed:', error);
        } finally {
            setOauthAuthorizing(false);
        }
    };

    const handleComplete = async () => {
        setIsLoading(true);
        try {
            const profile = effectiveProfile();
            const connectionParams = buildConnectionParams();

            // Save credentials for non-OAuth providers
            if (!isOAuth) {
                if (isServerProtocol && serverProfile) {
                    // Server protocol with saved profile: load & re-save credentials
                    const selectedServer = savedServers.find(s => s.name === serverProfile);
                    if (selectedServer?.username) {
                        const serverString = selectedServer.port && selectedServer.port !== 21
                            ? `${selectedServer.host}:${selectedServer.port}`
                            : selectedServer.host;
                        let password = '';
                        try {
                            password = await invoke<string>('get_credential', { account: `server_${selectedServer.id}` });
                        } catch {
                            password = selectedServer.password || '';
                        }
                        if (password) {
                            await invoke('save_server_credentials', {
                                profileName: profile, server: serverString,
                                username: selectedServer.username, password,
                            });
                        }
                    }
                } else if (connHost || connUsername) {
                    // Manual entry or email/password provider
                    const serverString = connPort ? `${connHost}:${connPort}` : connHost;
                    await invoke('save_server_credentials', {
                        profileName: profile,
                        server: serverString || selectedProtocol,
                        username: connUsername,
                        password: connPassword,
                    });
                }
            }

            const intervalSecs = syncUnit === 'hours' ? syncInterval * 3600 : syncInterval * 60;
            const config = await invoke<CloudConfig>('setup_aerocloud', {
                cloudName,
                localFolder,
                remoteFolder,
                serverProfile: profile,
                syncOnChange,
                syncIntervalSecs: intervalSecs,
                protocolType: selectedProtocol,
                connectionParams,
            });

            // Auto-install badge shell extension on Linux
            try {
                await invoke<string>('install_shell_extension_cmd');
            } catch { /* non-critical */ }

            onComplete(config);
        } catch (error) {
            console.error('Setup failed:', error);
        } finally {
            setIsLoading(false);
        }
    };

    // Render connection fields based on selected protocol
    const renderConnectionFields = () => {
        // Server protocols: saved profile dropdown OR manual entry
        if (isServerProtocol) {
            return (
                <div className="wizard-step">
                    <h3><Server size={20} /> {t('cloud.connectionSettings')}</h3>
                    <div className="server-select">
                        <label>{t('cloud.serverProfile')}:</label>
                        <select
                            value={serverProfile}
                            onChange={(e) => {
                                const name = e.target.value;
                                setServerProfile(name);
                                const server = savedServers.find(s => s.name === name);
                                if (server?.initialPath) {
                                    const basePath = server.initialPath.endsWith('/') ? server.initialPath.slice(0, -1) : server.initialPath;
                                    setRemoteFolder(`${basePath}/cloud/`);
                                }
                            }}
                        >
                            <option value="">{t('cloud.selectServer')}</option>
                            {savedServers.map((server) => (
                                <option key={server.name} value={server.name}>
                                    {server.name} ({server.host})
                                </option>
                            ))}
                        </select>
                    </div>
                    <div className="folder-input mt-3">
                        <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                        <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                            placeholder="/cloud/" className="wizard-input-editable" />
                    </div>
                </div>
            );
        }

        // Email + password providers (MEGA, Filen, Internxt)
        if (isEmailAuth) {
            return (
                <div className="wizard-step">
                    <h3><Shield size={20} /> {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Email</label>
                            <input type="email" value={connUsername} onChange={(e) => setConnUsername(e.target.value)}
                                placeholder="user@example.com" className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Password</label>
                            <input type="password" value={connPassword} onChange={(e) => setConnPassword(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                            <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                                placeholder="/" className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        // S3
        if (selectedProtocol === 's3') {
            return (
                <div className="wizard-step">
                    <h3><HardDrive size={20} /> S3 {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Endpoint</label>
                            <input type="text" value={connHost} onChange={(e) => setConnHost(e.target.value)}
                                placeholder="s3.amazonaws.com" className="wizard-input-editable" />
                        </div>
                        <div className="grid grid-cols-2 gap-2">
                            <div className="folder-input">
                                <label className="block text-sm font-medium mb-1">Bucket</label>
                                <input type="text" value={connExtra.bucket || ''} onChange={(e) => setConnExtra(p => ({ ...p, bucket: e.target.value }))}
                                    className="wizard-input-editable" />
                            </div>
                            <div className="folder-input">
                                <label className="block text-sm font-medium mb-1">Region</label>
                                <input type="text" value={connExtra.region || ''} onChange={(e) => setConnExtra(p => ({ ...p, region: e.target.value }))}
                                    placeholder="us-east-1" className="wizard-input-editable" />
                            </div>
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Access Key</label>
                            <input type="text" value={connUsername} onChange={(e) => setConnUsername(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Secret Key</label>
                            <input type="password" value={connPassword} onChange={(e) => setConnPassword(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Prefix</label>
                            <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                                placeholder="/" className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        // Azure
        if (selectedProtocol === 'azure') {
            return (
                <div className="wizard-step">
                    <h3><HardDrive size={20} /> Azure Blob {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Account Name</label>
                            <input type="text" value={connHost} onChange={(e) => setConnHost(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Access Key</label>
                            <input type="password" value={connPassword} onChange={(e) => setConnPassword(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Container</label>
                            <input type="text" value={connExtra.container || ''} onChange={(e) => setConnExtra(p => ({ ...p, container: e.target.value }))}
                                className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        // kDrive
        if (selectedProtocol === 'kdrive') {
            return (
                <div className="wizard-step">
                    <h3><Cloud size={20} /> kDrive {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">API Token</label>
                            <input type="password" value={connPassword} onChange={(e) => setConnPassword(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Drive ID</label>
                            <input type="text" value={connExtra.drive_id || ''} onChange={(e) => setConnExtra(p => ({ ...p, drive_id: e.target.value }))}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                            <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                                placeholder="/" className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        // Jottacloud
        if (selectedProtocol === 'jottacloud') {
            return (
                <div className="wizard-step">
                    <h3><Cloud size={20} /> Jottacloud {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Personal Login Token</label>
                            <input type="password" value={connPassword} onChange={(e) => setConnPassword(e.target.value)}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                            <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                                placeholder="/" className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        // OAuth2 providers
        if (OAUTH2_PROTOCOLS.includes(selectedProtocol)) {
            const needsRegion = selectedProtocol === 'pcloud' || selectedProtocol === 'zohoworkdrive';
            return (
                <div className="wizard-step">
                    <h3><Shield size={20} /> {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Client ID</label>
                            <input type="text" value={connExtra.client_id || ''} onChange={(e) => setConnExtra(p => ({ ...p, client_id: e.target.value }))}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Client Secret</label>
                            <input type="password" value={connExtra.client_secret || ''} onChange={(e) => setConnExtra(p => ({ ...p, client_secret: e.target.value }))}
                                className="wizard-input-editable" />
                        </div>
                        {needsRegion && (
                            <div className="folder-input">
                                <label className="block text-sm font-medium mb-1">Region</label>
                                <select value={connExtra.region || 'us'} onChange={(e) => setConnExtra(p => ({ ...p, region: e.target.value }))}
                                    className="wizard-input-editable">
                                    <option value="us">US</option>
                                    <option value="eu">EU</option>
                                    {selectedProtocol === 'zohoworkdrive' && <>
                                        <option value="in">IN</option>
                                        <option value="au">AU</option>
                                        <option value="jp">JP</option>
                                        <option value="uk">UK</option>
                                        <option value="ca">CA</option>
                                        <option value="sa">SA</option>
                                    </>}
                                </select>
                            </div>
                        )}
                        <div className="flex items-center gap-3 pt-2">
                            <button onClick={handleOAuthAuthorize}
                                disabled={oauthAuthorizing || !connExtra.client_id || !connExtra.client_secret}
                                className="btn-primary">
                                {oauthAuthorizing ? <Loader2 className="spin" size={16} /> : <Shield size={16} />}
                                {oauthAuthorized ? t('cloud.reauthorize') : t('cloud.authorize')}
                            </button>
                            {oauthAuthorized && (
                                <span className="text-sm text-green-500 flex items-center gap-1">
                                    <Check size={16} /> {t('cloud.authorized')} {oauthEmail && `(${oauthEmail})`}
                                </span>
                            )}
                        </div>
                        <div className="folder-input mt-2">
                            <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                            <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                                placeholder="/" className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        // OAuth1 (4shared)
        if (OAUTH1_PROTOCOLS.includes(selectedProtocol)) {
            return (
                <div className="wizard-step">
                    <h3><Shield size={20} /> 4shared {t('cloud.connectionSettings')}</h3>
                    <div className="space-y-3">
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Consumer Key</label>
                            <input type="text" value={connExtra.consumer_key || ''} onChange={(e) => setConnExtra(p => ({ ...p, consumer_key: e.target.value }))}
                                className="wizard-input-editable" />
                        </div>
                        <div className="folder-input">
                            <label className="block text-sm font-medium mb-1">Consumer Secret</label>
                            <input type="password" value={connExtra.consumer_secret || ''} onChange={(e) => setConnExtra(p => ({ ...p, consumer_secret: e.target.value }))}
                                className="wizard-input-editable" />
                        </div>
                        <div className="flex items-center gap-3 pt-2">
                            <button onClick={handleOAuthAuthorize}
                                disabled={oauthAuthorizing || !connExtra.consumer_key || !connExtra.consumer_secret}
                                className="btn-primary">
                                {oauthAuthorizing ? <Loader2 className="spin" size={16} /> : <Shield size={16} />}
                                {oauthAuthorized ? t('cloud.reauthorize') : t('cloud.authorize')}
                            </button>
                            {oauthAuthorized && (
                                <span className="text-sm text-green-500 flex items-center gap-1">
                                    <Check size={16} /> {t('cloud.authorized')}
                                </span>
                            )}
                        </div>
                        <div className="folder-input mt-2">
                            <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                            <input type="text" value={remoteFolder} onChange={(e) => setRemoteFolder(e.target.value)}
                                placeholder="/" className="wizard-input-editable" />
                        </div>
                    </div>
                </div>
            );
        }

        return null;
    };

    return (
        <div className="cloud-wizard">
            <div className="wizard-header">
                <Cloud size={48} className="wizard-icon" />
                <h2>{t('cloud.title')}</h2>
                <p>{t('cloud.setup')}</p>
            </div>

            <div className="wizard-progress">
                {[1, 2, 3, 4].map((s) => (
                    <div key={s} className={`progress-step ${step >= s ? 'active' : ''} ${step > s ? 'complete' : ''}`}>
                        {step > s ? <Check size={16} /> : s}
                    </div>
                ))}
            </div>

            <div className="wizard-content">
                {/* Step 1: Name & Local Folder */}
                {step === 1 && (
                    <div className="wizard-step">
                        <h3><Cloud size={20} /> {t('cloud.cloudName')}</h3>
                        <p>{t('cloud.cloudNameDesc')}</p>
                        <div className="folder-input">
                            <input type="text" value={cloudName} onChange={(e) => setCloudName(e.target.value)}
                                placeholder={t('cloud.cloudNamePlaceholder')} className="wizard-input-editable" />
                        </div>

                        <h3 className="mt-4"><FolderOpen size={20} /> {t('cloud.localFolder')}</h3>
                        <p>{t('cloud.stepFolder')}</p>
                        <div className="flex items-center gap-2">
                            {localFolder ? (
                                <span className="flex-1 text-sm font-mono text-gray-300 dark:text-gray-300 truncate" title={localFolder}>
                                    {localFolder}
                                </span>
                            ) : (
                                <span className="flex-1 text-sm text-gray-500 italic">{t('cloud.noFolderSelected')}</span>
                            )}
                            <button onClick={selectLocalFolder} className="browse-btn">
                                <Folder size={16} /> {t('common.browse')}
                            </button>
                        </div>
                    </div>
                )}

                {/* Step 2: Protocol Selection */}
                {step === 2 && (
                    <div className="wizard-step">
                        <h3><HardDrive size={20} /> {t('cloud.selectProtocol')}</h3>
                        <p className="text-sm opacity-70 mb-3">{t('cloud.selectProtocolDesc')}</p>
                        <div className="space-y-3 max-h-[340px] overflow-y-auto pr-1">
                            {PROTOCOL_CATEGORIES.map((cat) => (
                                <div key={cat.label}>
                                    <label className="block text-xs font-semibold uppercase opacity-50 mb-1.5">{t(cat.label)}</label>
                                    <div className="grid grid-cols-4 gap-1.5">
                                        {cat.protocols.map((proto) => (
                                            <button key={proto.id}
                                                onClick={() => setSelectedProtocol(proto.id)}
                                                className={`px-2 py-2 rounded-lg text-xs font-medium text-center transition-colors border ${
                                                    selectedProtocol === proto.id
                                                        ? 'border-cyan-500 bg-cyan-500/20 text-cyan-400'
                                                        : 'border-gray-600 hover:border-gray-400 hover:bg-white/5'
                                                }`}>
                                                {proto.label}
                                            </button>
                                        ))}
                                    </div>
                                </div>
                            ))}
                        </div>
                    </div>
                )}

                {/* Step 3: Connection Settings (dynamic per protocol) */}
                {step === 3 && renderConnectionFields()}

                {/* Step 4: Sync Settings + Summary */}
                {step === 4 && (
                    <div className="wizard-step">
                        <h3><Settings size={20} /> {t('cloud.stepSettings')}</h3>
                        <div className="settings-options">
                            <label className="checkbox-option">
                                <input type="checkbox" checked={syncOnChange} onChange={(e) => setSyncOnChange(e.target.checked)} />
                                <Zap size={16} />
                                <span>{t('cloud.syncOnChange')}</span>
                            </label>

                            <div className="interval-option">
                                <Clock size={16} />
                                <span>{t('cloud.syncInterval')}</span>
                                <input type="number" min="1" max={syncUnit === 'hours' ? 168 : 1440}
                                    value={syncInterval} onChange={(e) => setSyncInterval(Math.max(1, parseInt(e.target.value) || 1))} />
                                <select value={syncUnit} onChange={(e) => {
                                    const newUnit = e.target.value as 'hours' | 'minutes';
                                    if (newUnit === 'hours' && syncUnit === 'minutes') setSyncInterval(Math.max(1, Math.round(syncInterval / 60)));
                                    else if (newUnit === 'minutes' && syncUnit === 'hours') setSyncInterval(syncInterval * 60);
                                    setSyncUnit(newUnit);
                                }} className="interval-select">
                                    <option value="minutes">{t('settings.minutes')}</option>
                                    <option value="hours">{t('cloud.hours') || 'ore'}</option>
                                </select>
                            </div>
                        </div>

                        <div className="summary-box">
                            <h4>{t('cloud.summary')}</h4>
                            <p><Folder size={14} /> {t('cloud.localFolder')}: <code>{localFolder}</code></p>
                            <p><HardDrive size={14} /> {t('cloud.protocolType')}: <code>{selectedProtocol.toUpperCase()}</code></p>
                            <p><Server size={14} /> {t('cloud.remoteFolder')}: <code>{remoteFolder}</code></p>
                        </div>
                    </div>
                )}
            </div>

            <div className="wizard-footer">
                <button onClick={onCancel} className="btn-secondary">{t('common.cancel')}</button>
                <div className="wizard-nav">
                    {step > 1 && (
                        <button onClick={() => setStep(step - 1)} className="btn-secondary">{t('common.back')}</button>
                    )}
                    {step < 4 ? (
                        <button onClick={() => setStep(step + 1)} className="btn-primary" disabled={!canAdvance()}>
                            {t('common.next')} <ChevronRight size={16} />
                        </button>
                    ) : (
                        <button onClick={handleComplete} className="btn-primary btn-cloud"
                            disabled={isLoading || !localFolder}>
                            {isLoading ? <Loader2 className="spin" size={16} /> : <Cloud size={16} />}
                            {t('cloud.enableCloud')}
                        </button>
                    )}
                </div>
            </div>
        </div>
    );
};

// Cloud Dashboard Component
const CloudDashboard: React.FC<{
    config: CloudConfig;
    status: CloudSyncStatus;
    onSyncNow: () => void;
    onPause: () => void;
    onResume: () => void;
    onDisable: () => void;
    onOpenFolder: () => void;
    onSettings: () => void;
}> = ({ config, status, onSyncNow, onPause, onResume, onDisable, onOpenFolder, onSettings }) => {
    const t = useTranslation();
    const [countdown, setCountdown] = useState<string>('');

    // Countdown timer effect
    useEffect(() => {
        if (!config.last_sync || status.type === 'syncing' || status.type === 'paused') {
            setCountdown('');
            return;
        }

        const updateCountdown = () => {
            const lastSync = new Date(config.last_sync!).getTime();
            const intervalMs = config.sync_interval_secs * 1000;
            const nextSync = lastSync + intervalMs;
            const now = Date.now();
            const remainingMs = nextSync - now;

            if (remainingMs <= 0) {
                setCountdown('Soon...');
            } else {
                const mins = Math.floor(remainingMs / 60000);
                const secs = Math.floor((remainingMs % 60000) / 1000);
                setCountdown(`${mins}:${secs.toString().padStart(2, '0')}`);
            }
        };

        updateCountdown();
        const interval = setInterval(updateCountdown, 1000);
        return () => clearInterval(interval);
    }, [config.last_sync, config.sync_interval_secs, status.type]);

    const formatLastSync = (timestamp: string | null) => {
        if (!timestamp) return t('cloud.never');
        const date = new Date(timestamp);
        const now = new Date();
        const diffMs = now.getTime() - date.getTime();
        const diffMins = Math.floor(diffMs / 60000);

        if (diffMins < 1) return t('cloud.justNow');
        if (diffMins < 60) return t('cloud.minutesAgo', { count: diffMins });
        if (diffMins < 1440) return t('cloud.hoursAgo', { count: Math.floor(diffMins / 60) });
        return date.toLocaleDateString();
    };

    const getStatusIcon = () => {
        switch (status.type) {
            case 'syncing': return <RefreshCw className="spin" size={24} />;
            case 'paused': return <Pause size={24} />;
            case 'has_conflicts': return <AlertCircle size={24} />;
            case 'error': return <X size={24} />;
            default: return <Check size={24} />;
        }
    };

    const getStatusText = () => {
        switch (status.type) {
            case 'syncing':
                return `${t('cloud.syncing')}: ${status.current_file || t('cloud.starting')} (${status.progress?.toFixed(0) || 0}%)`;
            case 'paused': return t('cloud.paused');
            case 'has_conflicts': return `${status.count} ${t('cloud.conflicts')}`;
            case 'error': return status.message || t('cloud.error');
            default: return `${t('cloud.synced')} ${formatLastSync(config.last_sync)}`;
        }
    };

    return (
        <div className="cloud-dashboard">
            <div className="dashboard-header">
                <div className={`status-indicator status-${status.type}`}>
                    {getStatusIcon()}
                </div>
                <div className="status-info">
                    <h2>{config.cloud_name || t('cloud.title')}</h2>
                    <p className="status-text">
                        {getStatusText()}
                        {countdown && <span className="countdown"> • {t('common.next')}: {countdown}</span>}
                    </p>
                </div>
                <button onClick={onSettings} className="btn-icon" title={t('common.settings')}>
                    <Settings size={20} />
                </button>
            </div>

            {status.type === 'syncing' && (
                <div className="px-3 py-1">
                    <TransferProgressBar
                        percentage={status.progress || 0}
                        currentFile={status.files_done || 0}
                        totalFiles={status.files_total || 0}
                        size="sm"
                        variant="gradient"
                    />
                </div>
            )}

            <div className="dashboard-cards">
                <div className="info-card info-card-clickable" onClick={onOpenFolder} title={t('cloud.openFolder')}>
                    <Folder size={20} />
                    <div>
                        <span className="label">{t('cloud.localFolder')}</span>
                        <span className="value">{config.local_folder} <FolderOpen size={12} className="inline ml-1 opacity-50" /></span>
                    </div>
                </div>

                <div className="info-card">
                    <Server size={20} />
                    <div>
                        <span className="label">{t('cloud.remoteFolder')}</span>
                        <span className="value">{config.remote_folder}</span>
                    </div>
                </div>

                <div className="info-card">
                    <Shield size={20} />
                    <div>
                        <span className="label">{t('cloud.protocolType')}</span>
                        <span className="value">{(config.protocol_type || 'ftp').toUpperCase()} {config.server_profile && `\u2014 ${config.server_profile}`}</span>
                    </div>
                </div>

                <div className="info-card">
                    <History size={20} />
                    <div>
                        <span className="label">{t('cloud.lastSync')}</span>
                        <span className="value">{formatLastSync(config.last_sync)}</span>
                    </div>
                </div>

                <div className="info-card">
                    <Clock size={20} />
                    <div>
                        <span className="label">{t('cloud.syncInterval')}</span>
                        <span className="value">
                            {config.sync_interval_secs >= 3600
                                ? `${Math.round(config.sync_interval_secs / 3600)} ${t('cloud.hours') || 'ore'}`
                                : `${Math.round(config.sync_interval_secs / 60)} ${t('settings.minutes')}`
                            }
                        </span>
                    </div>
                </div>
            </div>

            <div className="dashboard-actions">
                <button
                    onClick={onSyncNow}
                    className="btn-primary"
                    disabled={status.type === 'syncing'}
                >
                    <RefreshCw size={16} /> {t('cloud.syncNow')}
                </button>

                {status.type === 'paused' ? (
                    <button onClick={onResume} className="btn-secondary">
                        <Play size={16} /> {t('cloud.resume')}
                    </button>
                ) : (
                    <button
                        onClick={onPause}
                        className="btn-secondary"
                        disabled={status.type === 'not_configured' || status.type === 'error'}
                    >
                        <Pause size={16} /> {t('cloud.pause')}
                    </button>
                )}

                <button onClick={onDisable} className="btn-danger">
                    <CloudOff size={16} /> {t('cloud.disable')}
                </button>
            </div>
        </div>
    );
};

// Main CloudPanel Component
export const CloudPanel: React.FC<CloudPanelProps> = ({ isOpen, onClose }) => {
    const t = useTranslation();
    const [config, setConfig] = useState<CloudConfig | null>(null);
    const [status, setStatus] = useState<CloudSyncStatus>({ type: 'not_configured' });
    const [isLoading, setIsLoading] = useState(true);
    const [showSettings, setShowSettings] = useState(false);

    // Use modular tray sync hook
    const {
        trayState,
        isRunning: isBackgroundSyncRunning,
        startBackgroundSync,
        stopBackgroundSync,
        toggleBackgroundSync
    } = useTraySync();

    // Load saved servers from vault (with localStorage fallback for pre-migration installs)
    const [savedServers, setSavedServers] = useState<{ id: string; name: string; host: string; port: number; username: string; password: string; initialPath: string }[]>([]);

    useEffect(() => {
        if (!isOpen) return;
        (async () => {
            try {
                const servers = await secureGetWithFallback<{
                    id?: string;
                    name?: string;
                    host: string;
                    port?: number;
                    username?: string;
                    password?: string;
                    initialPath?: string;
                }[]>('server_profiles', 'aeroftp-saved-servers');
                if (servers && servers.length > 0) {
                    setSavedServers(servers.map(s => ({
                        id: s.id || '',
                        name: s.name || s.host,
                        host: s.host,
                        port: s.port || 21,
                        username: s.username || '',
                        password: s.password || '',
                        initialPath: s.initialPath || ''
                    })));
                }
            } catch (e) {
                logger.error('Failed to load saved servers:', e);
            }
        })();
    }, [isOpen]);

    // Load config on mount
    useEffect(() => {
        loadConfig();
    }, []);

    // Listen for status changes
    useEffect(() => {
        const unlisten = listen<CloudSyncStatus>('cloud_status_change', (event) => {
            setStatus(event.payload);
        });

        const unlistenComplete = listen<SyncResult>('cloud_sync_complete', (event) => {
            const result = event.payload;
            if (result.errors.length > 0) {
                logger.debug(`Sync completed with ${result.errors.length} errors`);
            } else {
                logger.debug(
                    `Synced: ↑${result.uploaded} ↓${result.downloaded} files`
                );
            }
        });

        return () => {
            unlisten.then((f) => f());
            unlistenComplete.then((f) => f());
        };
    }, []);

    const loadConfig = async () => {
        setIsLoading(true);
        try {
            const cfg = await invoke<CloudConfig>('get_cloud_config');
            setConfig(cfg);

            if (cfg.enabled) {
                const sts = await invoke<CloudSyncStatus>('get_cloud_status');
                // The status from Rust already has the correct type
                setStatus(sts);
            }
        } catch (error) {
            console.error('Failed to load cloud config:', error);
        } finally {
            setIsLoading(false);
        }
    };

    const handleSetupComplete = (newConfig: CloudConfig) => {
        setConfig(newConfig);
        setStatus({ type: 'idle' });
        // Notify parent that cloud is now active
        emit('cloud-sync-status', { status: 'active', message: 'AeroCloud enabled' });
        logger.debug('AeroCloud enabled successfully!');
    };

    const handleSyncNow = async () => {
        logger.debug('Starting sync...');
        setStatus({ type: 'syncing', current_file: 'Scanning...', progress: 0 });
        try {
            // Start background sync if not already running
            if (!isBackgroundSyncRunning) {
                await startBackgroundSync();
            }
            const result = await invoke<string>('trigger_cloud_sync');
            logger.debug('Sync result:', result);
            setStatus({ type: 'idle' });
            // Reload config to update last_sync
            loadConfig();
        } catch (error) {
            console.error('Sync failed:', error);
            setStatus({ type: 'error', message: String(error) });
        }
    };

    const handlePause = async () => {
        try {
            await stopBackgroundSync();
            setStatus({ type: 'paused' });
            logger.debug('Sync paused');
        } catch (error) {
            console.error('Failed to pause sync:', error);
        }
    };

    const handleResume = async () => {
        try {
            await startBackgroundSync();
            setStatus({ type: 'idle', last_sync: config?.last_sync || undefined });
            logger.debug('Sync resumed');
        } catch (error) {
            console.error('Failed to resume sync:', error);
        }
    };

    const handleDisable = async () => {
        try {
            await stopBackgroundSync();
            await invoke('enable_aerocloud', { enabled: false });
            setConfig(prev => prev ? { ...prev, enabled: false } : null);
            setStatus({ type: 'not_configured' });
            // Notify parent that cloud is now disabled
            emit('cloud-sync-status', { status: 'disabled', message: 'AeroCloud disabled' });
            logger.debug('AeroCloud disabled');
        } catch (error) {
            logger.error('Failed to disable:', error);
        }
    };

    const handleOpenFolder = async () => {
        if (config?.local_folder) {
            try {
                await invoke('open_in_file_manager', { path: config.local_folder });
            } catch (error) {
                logger.error('Failed to open folder:', error);
            }
        }
    };

    // Don't render if not open
    if (!isOpen) return null;

    if (isLoading) {
        return (
            <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label="Cloud Sync Loading">
                <div className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl p-8 max-w-2xl w-full mx-4 max-h-[90vh] overflow-auto" onClick={e => e.stopPropagation()}>
                    <div className="flex items-center justify-center gap-3">
                        <Loader2 className="animate-spin" size={32} />
                        <p>{t('common.loading')} {t('cloud.title')}...</p>
                    </div>
                </div>
            </div>
        );
    }

    // Show setup wizard if not configured
    if (!config?.enabled) {
        return (
            <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label="Cloud Sync Setup">
                <div className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl p-6 max-w-2xl w-full mx-4 max-h-[90vh] overflow-hidden" onClick={e => e.stopPropagation()}>
                    <div className="flex items-center justify-between mb-4">
                        <h2 className="text-xl font-semibold flex items-center gap-2"><Cloud className="text-cyan-500" /> {t('cloud.title')} {t('cloud.setup')}</h2>
                        <button onClick={onClose} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg" title={t('common.close')}><X size={20} /></button>
                    </div>
                    <SetupWizard
                        savedServers={savedServers}
                        onComplete={handleSetupComplete}
                        onCancel={onClose}
                    />
                </div>
            </div>
        );
    }

    // Show settings panel
    if (showSettings) {
        return (
            <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label="Cloud Sync Settings">
                <div className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl p-6 max-w-md w-full mx-4" onClick={e => e.stopPropagation()}>
                    <div className="flex items-center justify-between mb-4">
                        <h2 className="text-xl font-semibold flex items-center gap-2"><Settings className="text-cyan-500" /> {t('cloud.title')} {t('common.settings')}</h2>
                        <button onClick={() => setShowSettings(false)} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg"><X size={20} /></button>
                    </div>

                    <div className="space-y-4">
                        {/* Cloud Name - Custom tab display name */}
                        <div>
                            <label className="block text-sm font-medium mb-1">{t('cloud.cloudName')}</label>
                            <input
                                type="text"
                                value={config?.cloud_name || ''}
                                onChange={e => setConfig(prev => prev ? { ...prev, cloud_name: e.target.value } : null)}
                                className="w-full px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                placeholder={t('cloud.cloudNamePlaceholder')}
                            />
                            <p className="text-xs text-gray-400 mt-1">{t('cloud.cloudNameDesc')}</p>
                        </div>

                        <div>
                            <label className="block text-sm font-medium mb-1">{t('cloud.localFolder')}</label>
                            <input
                                type="text"
                                value={config?.local_folder || ''}
                                readOnly
                                className="w-full px-3 py-2 bg-gray-100 dark:bg-gray-700 rounded-lg text-sm"
                            />
                        </div>

                        <div>
                            <label className="block text-sm font-medium mb-1">{t('cloud.remoteFolder')}</label>
                            <input
                                type="text"
                                value={config?.remote_folder || '/cloud/'}
                                onChange={e => setConfig(prev => prev ? { ...prev, remote_folder: e.target.value } : null)}
                                className="w-full px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                placeholder="/cloud/"
                            />
                        </div>

                        <div>
                            <label className="block text-sm font-medium mb-1">{t('cloud.serverProfile')}</label>
                            <input
                                type="text"
                                value={config?.server_profile || ''}
                                readOnly
                                className="w-full px-3 py-2 bg-gray-100 dark:bg-gray-700 rounded-lg text-sm"
                            />
                        </div>

                        <div>
                            <label className="block text-sm font-medium mb-1">{t('cloud.syncInterval')}</label>
                            <div className="flex items-center gap-2">
                                <input
                                    type="number"
                                    min="1"
                                    max={(() => {
                                        const secs = config?.sync_interval_secs || 300;
                                        return secs >= 3600 ? 168 : 1440;
                                    })()}
                                    value={(() => {
                                        const secs = config?.sync_interval_secs || 300;
                                        return secs >= 3600 ? Math.round(secs / 3600) : Math.round(secs / 60);
                                    })()}
                                    onChange={e => {
                                        const val = Math.max(1, parseInt(e.target.value) || 1);
                                        const secs = config?.sync_interval_secs || 300;
                                        const isHours = secs >= 3600;
                                        setConfig(prev => prev ? {
                                            ...prev,
                                            sync_interval_secs: isHours ? val * 3600 : val * 60
                                        } : null);
                                    }}
                                    className="w-20 px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm text-center"
                                />
                                <select
                                    value={config && config.sync_interval_secs >= 3600 ? 'hours' : 'minutes'}
                                    onChange={e => {
                                        const currentSecs = config?.sync_interval_secs || 300;
                                        const currentVal = currentSecs >= 3600 ? Math.round(currentSecs / 3600) : Math.round(currentSecs / 60);
                                        setConfig(prev => prev ? {
                                            ...prev,
                                            sync_interval_secs: e.target.value === 'hours' ? currentVal * 3600 : currentVal * 60
                                        } : null);
                                    }}
                                    className="px-2 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                >
                                    <option value="minutes">{t('settings.minutes')}</option>
                                    <option value="hours">{t('cloud.hours') || 'ore'}</option>
                                </select>
                            </div>
                            <p className="text-xs text-gray-400 mt-1">{t('cloud.syncIntervalDesc')}</p>
                        </div>

                        {/* Public URL for sharing */}
                        <div className="pt-2 border-t border-gray-200 dark:border-gray-700">
                            <label className="block text-sm font-medium mb-1 flex items-center gap-2">
                                🔗 {t('cloud.publicUrlBase')}
                                <span className="text-xs text-gray-400 font-normal">({t('cloud.forSharing')})</span>
                            </label>
                            <input
                                type="text"
                                value={config?.public_url_base || ''}
                                onChange={e => setConfig(prev => prev ? {
                                    ...prev,
                                    public_url_base: e.target.value || null
                                } : null)}
                                className="w-full px-3 py-2 bg-white dark:bg-gray-900 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                placeholder="https://cloud.yourdomain.com/"
                            />
                            <p className="text-xs text-gray-400 mt-1">
                                {t('cloud.publicUrlDesc')}
                            </p>
                        </div>

                        {/* Sync Scheduler (Phase 3A+) */}
                        <div className="pt-2 border-t border-gray-200 dark:border-gray-700">
                            <SyncScheduler />
                        </div>

                        {/* Watcher Status (Phase 3A+) */}
                        <div className="pt-2 border-t border-gray-200 dark:border-gray-700">
                            <WatcherStatus watchPath={config?.local_folder} />
                        </div>

                        <div className="flex gap-3 pt-4">
                            <button
                                onClick={async () => {
                                    if (config) {
                                        try {
                                            await invoke('save_cloud_config_cmd', { config });
                                            // Sync interval to SyncSchedule so the background worker uses the correct value
                                            try {
                                                const schedule = await invoke<Record<string, unknown>>('get_sync_schedule_cmd');
                                                if (schedule) {
                                                    await invoke('save_sync_schedule_cmd', {
                                                        schedule: { ...schedule, interval_secs: config.sync_interval_secs }
                                                    });
                                                }
                                            } catch { /* scheduler may not be initialized yet */ }
                                            setShowSettings(false);
                                            logger.debug('Settings saved!');
                                        } catch (e) {
                                            console.error('Failed to save settings:', e);
                                        }
                                    }
                                }}
                                className="flex-1 py-2 bg-cyan-500 hover:bg-cyan-600 text-white rounded-lg font-medium"
                            >
                                {t('common.save')} {t('common.settings')}
                            </button>
                            <button
                                onClick={() => setShowSettings(false)}
                                className="flex-1 py-2 bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 rounded-lg font-medium"
                            >
                                {t('common.cancel')}
                            </button>
                        </div>
                    </div>
                </div>
            </div>
        );
    }

    // Show dashboard
    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center backdrop-blur-sm" onClick={onClose} role="dialog" aria-modal="true" aria-label="Cloud Sync Dashboard">
            <div className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl p-6 max-w-2xl w-full mx-4 max-h-[90vh] overflow-hidden" onClick={e => e.stopPropagation()}>
                <div className="flex items-center justify-between mb-4">
                    <h2 className="text-xl font-semibold flex items-center gap-2"><Cloud className="text-cyan-500" /> {t('cloud.title')}</h2>
                    <button onClick={onClose} className="p-2 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg" title={t('common.close')}><X size={20} /></button>
                </div>
                <CloudDashboard
                    config={config}
                    status={status}
                    onSyncNow={handleSyncNow}
                    onPause={handlePause}
                    onResume={handleResume}
                    onDisable={handleDisable}
                    onOpenFolder={handleOpenFolder}
                    onSettings={() => setShowSettings(true)}
                />
            </div>
        </div>
    );
};

export default CloudPanel;
