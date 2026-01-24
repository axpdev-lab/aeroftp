/**
 * ConnectionScreen Component
 * Initial connection form with Quick Connect and Saved Servers
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { FolderOpen, HardDrive, ChevronRight, Save, Cloud, Check, Settings, Clock, Folder, Pencil, X, Lock } from 'lucide-react';
import { ConnectionParams, ProviderType, isOAuthProvider, isAeroCloudProvider, ServerProfile } from '../types';
import { SavedServers } from './SavedServers';
import { useTranslation } from '../i18n';
import { ProtocolSelector, ProtocolFields, getDefaultPort } from './ProtocolSelector';
import { OAuthConnect } from './OAuthConnect';
import { ProviderSelector } from './ProviderSelector';
import { getProviderById, ProviderConfig } from '../providers';

// Storage key for saved servers (same as SavedServers component)
const SERVERS_STORAGE_KEY = 'aeroftp-saved-servers';

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
    onOpenCloudPanel?: () => void;
}

export const ConnectionScreen: React.FC<ConnectionScreenProps> = ({
    connectionParams,
    quickConnectDirs,
    loading,
    onConnectionParamsChange,
    onQuickConnectDirsChange,
    onConnect,
    onSavedServerConnect,
    onSkipToFileManager,
    onOpenCloudPanel,
}) => {
    const t = useTranslation();
    const protocol = connectionParams.protocol; // Can be undefined

    // Save connection state
    const [saveConnection, setSaveConnection] = useState(false);
    const [connectionName, setConnectionName] = useState('');

    // AeroCloud state
    const [aeroCloudConfig, setAeroCloudConfig] = useState<AeroCloudConfig | null>(null);
    const [aeroCloudLoading, setAeroCloudLoading] = useState(false);

    // Edit state
    const [editingProfileId, setEditingProfileId] = useState<string | null>(null);
    const [savedServersUpdate, setSavedServersUpdate] = useState(0);

    // Provider selection state (for S3/WebDAV)
    const [selectedProviderId, setSelectedProviderId] = useState<string | null>(null);
    const selectedProvider = selectedProviderId ? getProviderById(selectedProviderId) : null;

    // Protocol selector open state (to hide form when selector is open)
    const [isProtocolSelectorOpen, setIsProtocolSelectorOpen] = useState(false);

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

    // Save the current connection to saved servers (or update existing)
    const saveToServers = () => {
        // If editing an existing profile (and not creating a copy), name/saveConnection might be implicit
        if (!protocol) return;

        // MEGA: Add/Update session expiry (24h)
        const optionsToSave = { ...connectionParams.options };
        if (protocol === 'mega') {
            optionsToSave.session_expires_at = Date.now() + 24 * 60 * 60 * 1000;
        }

        const existingServers = JSON.parse(localStorage.getItem(SERVERS_STORAGE_KEY) || '[]');

        if (editingProfileId) {
            // Update existing profile
            const updatedServers = existingServers.map((s: ServerProfile) => {
                if (s.id === editingProfileId) {
                    return {
                        ...s,
                        name: connectionName || s.name,
                        host: connectionParams.server,
                        port: connectionParams.port || getDefaultPort(protocol),
                        username: connectionParams.username,
                        password: connectionParams.password,
                        protocol: protocol as ProviderType,
                        options: optionsToSave,
                        initialPath: quickConnectDirs.remoteDir,
                        localInitialPath: quickConnectDirs.localDir,
                    };
                }
                return s;
            });
            localStorage.setItem(SERVERS_STORAGE_KEY, JSON.stringify(updatedServers));
            setSavedServersUpdate(Date.now());
        } else if (saveConnection) {
            // Create new profile
            const newServer: ServerProfile = {
                id: `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
                name: connectionName || connectionParams.server || protocol,
                host: connectionParams.server,
                port: connectionParams.port || getDefaultPort(protocol),
                username: connectionParams.username,
                password: connectionParams.password,
                protocol: protocol as ProviderType,
                initialPath: quickConnectDirs.remoteDir,
                localInitialPath: quickConnectDirs.localDir,
                options: optionsToSave,
            };
            localStorage.setItem(SERVERS_STORAGE_KEY, JSON.stringify([...existingServers, newServer]));
            setSavedServersUpdate(Date.now()); // Trigger refresh
        }
    };

    // Handle connect with optional save
    // Handle connect with optional save
    const handleConnectAndSave = () => {
        if (editingProfileId) {
            // In edit mode, "Save Changes" just saves and exits edit mode
            saveToServers();
            setEditingProfileId(null);
            setConnectionName('');
            setSaveConnection(false);
            // Optionally clear form? Let's leave it as is so user can connect if they want
        } else {
            saveToServers();
            onConnect();
        }
    };

    const handleEdit = (profile: ServerProfile) => {
        setEditingProfileId(profile.id);
        setConnectionName(profile.name);
        setSaveConnection(true); // Implied for editing

        // Update form params
        onConnectionParamsChange({
            server: profile.host,
            port: profile.port,
            username: profile.username,
            password: profile.password || '',
            protocol: profile.protocol || 'ftp',
            options: profile.options || {}
        });

        onQuickConnectDirsChange({
            remoteDir: profile.initialPath || '',
            localDir: profile.localInitialPath || ''
        });
    };

    const handleCancelEdit = () => {
        setEditingProfileId(null);
        setConnectionName('');
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

    const handleProtocolChange = (newProtocol: ProviderType) => {
        // Reset provider selection when protocol changes
        setSelectedProviderId(null);
        onConnectionParamsChange({
            ...connectionParams,
            protocol: newProtocol,
            port: getDefaultPort(newProtocol),
            options: {},
        });
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
            options: {
                ...connectionParams.options,
                pathStyle: provider.defaults?.pathStyle,
                region: provider.defaults?.region,
            },
        };
        onConnectionParamsChange(newParams);
    };

    // Dynamic server placeholder based on protocol
    const getServerPlaceholder = () => {
        switch (protocol) {
            case 'webdav':
                return 'cloud.example.com/remote.php/dav/files/user/';
            case 's3':
                return 's3.amazonaws.com (or MinIO endpoint)';
            default:
                return t('connection.serverPlaceholder');
        }
    };

    // Dynamic username label based on protocol
    const getUsernameLabel = () => {
        if (protocol === 's3') return 'Access Key ID';
        return t('connection.username');
    };

    // Dynamic password label based on protocol
    const getPasswordLabel = () => {
        if (protocol === 's3') return 'Secret Access Key';
        return t('connection.password');
    };

    return (
        <div className="max-w-4xl mx-auto grid md:grid-cols-2 gap-6">
            {/* Quick Connect */}
            <div className="bg-white dark:bg-gray-800 rounded-xl shadow-xl p-6">
                <h2 className="text-xl font-semibold mb-4">{t('connection.quickConnect')}</h2>
                <div className="space-y-3">
                    {/* Protocol Selector - always shown */}
                    <ProtocolSelector
                        value={protocol}
                        onChange={handleProtocolChange}
                        disabled={loading}
                        onOpenChange={setIsProtocolSelectorOpen}
                    />

                    {/* Show form only when protocol is selected AND selector is closed */}
                    {!protocol || isProtocolSelectorOpen ? (
                        /* No protocol selected or selector is open - show selection prompt */
                        <div className="py-8 text-center text-gray-500 dark:text-gray-400">
                            <p className="text-sm">Select a protocol above to configure your connection</p>
                        </div>
                    ) : isAeroCloudProvider(protocol) ? (
                        /* AeroCloud - show status or setup */
                        <div className="py-4 space-y-4">
                            {aeroCloudLoading ? (
                                <div className="text-center py-8">
                                    <div className="animate-spin w-8 h-8 border-2 border-sky-500 border-t-transparent rounded-full mx-auto"></div>
                                    <p className="text-sm text-gray-500 mt-2">Loading AeroCloud...</p>
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
                                                    <Check size={10} /> Active
                                                </span>
                                            </div>
                                            <p className="text-xs text-gray-500 truncate">{aeroCloudConfig.server_profile}</p>
                                        </div>
                                    </div>

                                    {/* Quick info */}
                                    <div className="grid grid-cols-2 gap-3 text-sm">
                                        <div className="p-2 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                                            <div className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400 text-xs mb-1">
                                                <Folder size={12} /> Local Folder
                                            </div>
                                            <p className="truncate text-xs font-medium" title={aeroCloudConfig.local_folder}>
                                                {aeroCloudConfig.local_folder.split('/').pop() || aeroCloudConfig.local_folder}
                                            </p>
                                        </div>
                                        <div className="p-2 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                                            <div className="flex items-center gap-1.5 text-gray-500 dark:text-gray-400 text-xs mb-1">
                                                <Clock size={12} /> Sync Interval
                                            </div>
                                            <p className="text-xs font-medium">{Math.round(aeroCloudConfig.sync_interval_secs / 60)} minutes</p>
                                        </div>
                                    </div>

                                    {/* Actions */}
                                    <div className="flex gap-2">
                                        <button
                                            onClick={onOpenCloudPanel}
                                            className="flex-1 flex items-center justify-center gap-2 px-4 py-2.5 bg-gradient-to-r from-sky-500 to-blue-600 text-white font-medium rounded-xl hover:from-sky-600 hover:to-blue-700 transition-all"
                                        >
                                            <Settings size={16} /> Manage AeroCloud
                                        </button>
                                    </div>

                                    <p className="text-xs text-center text-gray-400">
                                        AeroCloud is already configured and syncing
                                    </p>
                                </div>
                            ) : (
                                /* Not configured - show setup prompt */
                                <div className="text-center space-y-4">
                                    <div className="w-16 h-16 mx-auto bg-gradient-to-br from-sky-400 to-blue-500 rounded-2xl flex items-center justify-center shadow-lg">
                                        <Cloud className="w-8 h-8 text-white" />
                                    </div>
                                    <div>
                                        <h3 className="font-semibold text-lg">AeroCloud</h3>
                                        <p className="text-sm text-gray-500 dark:text-gray-400 mt-1">
                                            Turn any FTP server into your personal cloud with automatic sync
                                        </p>
                                    </div>
                                    <button
                                        onClick={onOpenCloudPanel}
                                        className="px-6 py-3 bg-gradient-to-r from-sky-500 to-blue-600 text-white font-medium rounded-xl hover:from-sky-600 hover:to-blue-700 transition-all shadow-lg hover:shadow-xl"
                                    >
                                        Configure AeroCloud
                                    </button>
                                    <p className="text-xs text-gray-400">
                                        You can also configure AeroCloud from the status bar
                                    </p>
                                </div>
                            )}
                        </div>
                    ) : isOAuthProvider(protocol) ? (
                        <OAuthConnect
                            provider={protocol as 'googledrive' | 'dropbox' | 'onedrive'}
                            initialLocalPath={quickConnectDirs.localDir}
                            onLocalPathChange={(path) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: path })}
                            saveConnection={saveConnection}
                            onSaveConnectionChange={setSaveConnection}
                            connectionName={connectionName}
                            onConnectionNameChange={setConnectionName}
                            onConnected={(displayName) => {
                                // Save OAuth connection if requested
                                if (saveConnection) {
                                    const existingServers = JSON.parse(localStorage.getItem(SERVERS_STORAGE_KEY) || '[]');
                                    const newServer: ServerProfile = {
                                        id: `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`,
                                        name: connectionName || displayName,
                                        host: displayName,  // Use display name as identifier
                                        port: 443,
                                        username: '',  // OAuth doesn't use username/password
                                        password: '',
                                        protocol: protocol as ProviderType,
                                        initialPath: '/',
                                        localInitialPath: quickConnectDirs.localDir,
                                    };
                                    localStorage.setItem(SERVERS_STORAGE_KEY, JSON.stringify([...existingServers, newServer]));
                                }
                                // After OAuth completes, trigger connection
                                onConnect();
                            }}
                        />
                    ) : (protocol === 's3' || protocol === 'webdav' || protocol === 'mega') && !selectedProviderId && !editingProfileId ? (
                        /* Show provider selector for S3/WebDAV/MEGA (skip when editing) */
                        <div className="py-2">
                            <ProviderSelector
                                selectedProvider={selectedProviderId || undefined}
                                onSelect={handleProviderSelect}
                                category={protocol as any}
                                stableOnly={false}
                                compact={false}
                            />
                            <p className="text-xs text-gray-500 text-center mt-3">
                                Select a provider above or choose "Custom" for manual configuration
                            </p>
                        </div>
                    ) : (
                        <>
                            {/* Selected Provider Header (for S3/WebDAV) */}
                            {selectedProvider && (
                                <div className="flex items-center justify-between p-3 bg-gray-100 dark:bg-gray-700/50 rounded-xl mb-3">
                                    <div className="flex items-center gap-2">
                                        <div className="w-8 h-8 bg-gray-200 dark:bg-gray-600 rounded-lg flex items-center justify-center">
                                            <Cloud size={16} style={{ color: selectedProvider.color }} />
                                        </div>
                                        <div>
                                            <span className="font-medium text-sm">{selectedProvider.name}</span>
                                            {selectedProvider.isGeneric && (
                                                <span className="text-xs text-gray-500 ml-2">(Custom)</span>
                                            )}
                                        </div>
                                    </div>
                                    <button
                                        onClick={() => setSelectedProviderId(null)}
                                        className="text-xs text-blue-500 hover:text-blue-600 hover:underline"
                                    >
                                        Change
                                    </button>
                                </div>
                            )}

                            {/* Connection Fields Area */}
                            {protocol === 'mega' ? (
                                /* MEGA Specific Form (Beta v0.5.0) */
                                <div className="space-y-4 pt-2">
                                    <div>
                                        <label className="block text-sm font-medium mb-1.5">Email Account</label>
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
                                            placeholder="your-email@mega.nz"
                                            autoFocus
                                        />
                                    </div>
                                    <div>
                                        <label className="block text-sm font-medium mb-1.5">Password</label>
                                        <input
                                            type="password"
                                            value={connectionParams.password}
                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl focus:ring-2 focus:ring-red-500 focus:border-red-500"
                                            placeholder="Your MEGA password"
                                        />
                                    </div>

                                    <div className="bg-blue-50 dark:bg-blue-900/10 p-3 rounded-lg border border-blue-100 dark:border-blue-900/30 text-xs text-blue-800 dark:text-blue-200">
                                        <p className="font-medium mb-1">⚠️ Requirement: MEGAcmd</p>
                                        <p className="opacity-80">
                                            This feature requires the free <strong>MEGAcmd</strong> tool installed and running on your system.
                                            <a
                                                href="https://mega.io/cmd"
                                                target="_blank"
                                                rel="noopener noreferrer"
                                                className="block mt-1 underline hover:text-blue-600 dark:hover:text-blue-300"
                                            >
                                                Download MEGAcmd
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
                                                <span className="text-sm font-medium text-gray-900 dark:text-gray-200">Remember session (24h)</span>
                                                <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                    Session keys stored securely in system keyring
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
                                                <span className="text-sm font-medium text-gray-900 dark:text-gray-200">Logout on disconnect</span>
                                                <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                                    Terminate MEGAcmd session when closing connection
                                                </p>
                                            </div>
                                        </label>
                                    </div>

                                    {/* Optional Remote Path */}
                                    <div className="pt-2">
                                        <label className="block text-xs font-medium text-gray-500 uppercase tracking-wide mb-1.5">
                                            Optional Settings
                                        </label>
                                        <div className="space-y-2">
                                            <input
                                                type="text"
                                                value={quickConnectDirs.remoteDir}
                                                onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                                className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                placeholder="Initial remote folder (e.g. /Backup)"
                                            />
                                            <div className="flex gap-2">
                                                <input
                                                    type="text"
                                                    value={quickConnectDirs.localDir}
                                                    onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, localDir: e.target.value })}
                                                    className="w-full px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                                    placeholder="Initial local folder"
                                                />
                                                <button
                                                    type="button"
                                                    onClick={handleBrowseLocalDir}
                                                    className="px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg transition-colors"
                                                    title="Browse"
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
                                                Save to Saved Servers
                                            </span>
                                        </label>

                                        {saveConnection && (
                                            <div className="mt-2 animate-fade-in-down">
                                                <input
                                                    type="text"
                                                    value={connectionName}
                                                    onChange={(e) => setConnectionName(e.target.value)}
                                                    placeholder="Connection Name (e.g. My Personal MEGA)"
                                                    className="w-full px-3 py-2 bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg text-sm focus:ring-2 focus:ring-red-500 focus:border-transparent"
                                                />
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
                                                <><div className="w-5 h-5 border-2 border-white/30 border-t-white rounded-full animate-spin" /> Connecting...</>
                                            ) : (
                                                <><Cloud size={20} /> Secure Login</>
                                            )}
                                        </button>
                                        <p className="text-center text-xs text-gray-400 mt-3 flex items-center justify-center gap-1.5">
                                            <Lock size={12} /> End-to-end encrypted connection
                                        </p>
                                    </div>
                                </div>
                            ) : (
                                /* Traditional connection fields (FTP/S3/WebDAV) */
                                <>
                                    <div>
                                        <label className="block text-sm font-medium mb-1.5">
                                            {protocol === 's3' ? 'Endpoint' : t('connection.server')}
                                        </label>
                                        <input
                                            type="text"
                                            value={connectionParams.server}
                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, server: e.target.value })}
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                            placeholder={getServerPlaceholder()}
                                        />
                                    </div>
                                    <div>
                                        <label className="block text-sm font-medium mb-1.5">{getUsernameLabel()}</label>
                                        <input
                                            type="text"
                                            value={connectionParams.username}
                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, username: e.target.value })}
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                            placeholder={protocol === 's3' ? 'AKIAIOSFODNN7EXAMPLE' : t('connection.usernamePlaceholder')}
                                        />
                                    </div>
                                    <div>
                                        <label className="block text-sm font-medium mb-1.5">{getPasswordLabel()}</label>
                                        <input
                                            type="password"
                                            value={connectionParams.password}
                                            onChange={(e) => onConnectionParamsChange({ ...connectionParams, password: e.target.value })}
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                            placeholder={t('connection.passwordPlaceholder')}
                                        />
                                    </div>

                                    {/* Protocol-specific fields */}
                                    <ProtocolFields
                                        protocol={protocol}
                                        options={connectionParams.options || {}}
                                        onChange={(options) => onConnectionParamsChange({ ...connectionParams, options })}
                                        disabled={loading}
                                    />

                                    <div>
                                        <label className="block text-sm font-medium mb-1.5">{t('browser.remote')} {t('browser.path')}</label>
                                        <input
                                            type="text"
                                            value={quickConnectDirs.remoteDir}
                                            onChange={(e) => onQuickConnectDirsChange({ ...quickConnectDirs, remoteDir: e.target.value })}
                                            className="w-full px-4 py-3 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-xl"
                                            placeholder={protocol === 's3' ? '/prefix/' : '/www'}
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
                                                placeholder="/home/user/projects"
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
                                                Save this connection
                                            </span>
                                        </label>

                                        {saveConnection && (
                                            <input
                                                type="text"
                                                value={connectionName}
                                                onChange={(e) => setConnectionName(e.target.value)}
                                                placeholder="Connection name (optional)"
                                                className="w-full mt-2 px-3 py-2 bg-gray-50 dark:bg-gray-700 border border-gray-300 dark:border-gray-600 rounded-lg text-sm"
                                            />
                                        )}
                                    </div>

                                    <div className="flex gap-2">
                                        {editingProfileId && (
                                            <button
                                                onClick={handleCancelEdit}
                                                className="px-4 py-3 bg-gray-200 dark:bg-gray-700 text-gray-700 dark:text-gray-300 font-medium rounded-xl hover:bg-gray-300 dark:hover:bg-gray-600 transition-colors"
                                                title="Cancel Editing"
                                            >
                                                <X size={20} />
                                            </button>
                                        )}
                                        <button
                                            onClick={handleConnectAndSave}
                                            disabled={loading || (protocol === 's3' && !connectionParams.options?.bucket)}
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
                                                    Save Changes
                                                </>
                                            ) : (
                                                saveConnection ? 'Connect & Save' : t('common.connect')
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
            <div className="bg-white dark:bg-gray-800 rounded-xl shadow-xl p-6">
                <SavedServers
                    onConnect={onSavedServerConnect}
                    onEdit={handleEdit}
                    lastUpdate={savedServersUpdate}
                />
            </div>

            {/* Skip to File Manager Button */}
            <div className="md:col-span-2 text-center mt-4">
                <button
                    onClick={onSkipToFileManager}
                    className="group px-6 py-3 bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-xl text-gray-600 dark:text-gray-300 transition-all hover:scale-105 flex items-center gap-2 mx-auto"
                >
                    <HardDrive size={18} className="group-hover:text-blue-500 transition-colors" />
                    <span>{t('browser.local')} {t('browser.files')}</span>
                    <ChevronRight size={16} className="opacity-50 group-hover:translate-x-1 transition-transform" />
                </button>
                <p className="text-xs text-gray-500 mt-2">{t('statusBar.notConnected')}</p>
            </div>
        </div>
    );
};

export default ConnectionScreen;
