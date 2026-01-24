import * as React from 'react';
import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Server, Plus, Trash2, Edit2, X, Check, FolderOpen, Cloud, AlertCircle, Clock } from 'lucide-react';
import { open } from '@tauri-apps/plugin-dialog';
import { ServerProfile, ConnectionParams, ProviderType, isOAuthProvider } from '../types';
import { useTranslation } from '../i18n';
import { getProtocolInfo, ProtocolBadge, ProtocolIcon } from './ProtocolSelector';

// OAuth settings storage key (same as SettingsPanel)
const OAUTH_SETTINGS_KEY = 'aeroftp_oauth_settings';

interface OAuthSettings {
    googledrive: { clientId: string; clientSecret: string };
    dropbox: { clientId: string; clientSecret: string };
    onedrive: { clientId: string; clientSecret: string };
}

// Get OAuth settings from localStorage
const getOAuthSettings = (): OAuthSettings | null => {
    try {
        const stored = localStorage.getItem(OAUTH_SETTINGS_KEY);
        return stored ? JSON.parse(stored) : null;
    } catch {
        return null;
    }
};

// Map protocol to OAuth provider key
const getOAuthProviderKey = (protocol: ProviderType): keyof OAuthSettings | null => {
    switch (protocol) {
        case 'googledrive': return 'googledrive';
        case 'dropbox': return 'dropbox';
        case 'onedrive': return 'onedrive';
        default: return null;
    }
};

interface SavedServersProps {
    onConnect: (params: ConnectionParams, initialPath?: string, localInitialPath?: string) => void;
    currentProfile?: ServerProfile; // For highlighting active connection
    className?: string;
    onEdit: (profile: ServerProfile) => void;
    lastUpdate?: number;
}

const STORAGE_KEY = 'aeroftp-saved-servers';

// Generate a unique ID
const generateId = () => `srv_${Date.now()}_${Math.random().toString(36).substr(2, 9)}`;

// Get saved servers from localStorage
const getSavedServers = (): ServerProfile[] => {
    try {
        const stored = localStorage.getItem(STORAGE_KEY);
        return stored ? JSON.parse(stored) : [];
    } catch {
        return [];
    }
};

// Save servers to localStorage
const saveServers = (servers: ServerProfile[]) => {
    localStorage.setItem(STORAGE_KEY, JSON.stringify(servers));
};

export const SavedServers: React.FC<SavedServersProps> = ({
    onConnect,
    currentProfile,
    className = '',
    onEdit,
    lastUpdate
}) => {
    const t = useTranslation();
    const [servers, setServers] = useState<ServerProfile[]>([]);

    const [oauthConnecting, setOauthConnecting] = useState<string | null>(null);
    const [oauthError, setOauthError] = useState<string | null>(null);

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
    };

    useEffect(() => {
        setServers(getSavedServers());
    }, [lastUpdate]);

    const handleDelete = (id: string) => {
        // Prevent deletion if connecting
        if (oauthConnecting === id) return;

        const updated = servers.filter(s => s.id !== id);
        setServers(updated);
        saveServers(updated);
    };

    const handleEdit = (server: ServerProfile) => {
        onEdit(server);
    };

    const handleConnect = async (server: ServerProfile) => {
        // Check expiry for MEGA (Beta v0.5.0)
        if (server.protocol === 'mega' && server.options?.session_expires_at && Date.now() > server.options.session_expires_at) {
            onEdit(server); // Redirect to edit to renew session
            return;
        }

        // Clear any previous OAuth error
        setOauthError(null);

        // Check if this is an OAuth provider
        if (server.protocol && isOAuthProvider(server.protocol)) {
            const providerKey = getOAuthProviderKey(server.protocol);
            const oauthSettings = getOAuthSettings();

            if (!providerKey || !oauthSettings) {
                setOauthError('OAuth credentials not configured. Please go to Settings > Cloud Storage to configure your credentials.');
                return;
            }

            const credentials = oauthSettings[providerKey];
            if (!credentials?.clientId || !credentials?.clientSecret) {
                setOauthError(`Please configure ${server.protocol === 'googledrive' ? 'Google Drive' : server.protocol === 'dropbox' ? 'Dropbox' : 'OneDrive'} credentials in Settings > Cloud Storage.`);
                return;
            }

            // Start OAuth flow
            setOauthConnecting(server.id);
            try {
                const oauthProvider = server.protocol === 'googledrive' ? 'google_drive' : server.protocol;
                const params = {
                    provider: oauthProvider,
                    client_id: credentials.clientId,
                    client_secret: credentials.clientSecret,
                };

                // Check if tokens already exist - if so, skip auth flow and just connect
                const hasTokens = await invoke<boolean>('oauth2_has_tokens', { provider: oauthProvider });

                if (!hasTokens) {
                    // No tokens - need full auth flow (opens browser)
                    console.log('[SavedServers] No OAuth tokens, starting full auth...');
                    await invoke('oauth2_full_auth', { params });
                } else {
                    console.log('[SavedServers] OAuth tokens found, skipping auth flow');
                }

                // Connect using stored tokens
                const displayName = await invoke<string>('oauth2_connect', { params });

                // Update last connected
                const updated = servers.map(s =>
                    s.id === server.id ? { ...s, lastConnected: new Date().toISOString() } : s
                );
                setServers(updated);
                saveServers(updated);

                // Call onConnect with OAuth params
                onConnect({
                    server: displayName,
                    username: '',
                    password: '',
                    protocol: server.protocol,
                }, server.initialPath, server.localInitialPath);

            } catch (e) {
                setOauthError(e instanceof Error ? e.message : String(e));
            } finally {
                setOauthConnecting(null);
            }
            return;
        }

        // Non-OAuth: Update last connected
        const updated = servers.map(s =>
            s.id === server.id ? { ...s, lastConnected: new Date().toISOString() } : s
        );
        setServers(updated);
        saveServers(updated);

        // Build connection params - for S3/WebDAV, don't modify the host with port
        const isProviderProtocol = server.protocol && ['s3', 'webdav'].includes(server.protocol);
        const serverString = isProviderProtocol
            ? server.host  // S3/WebDAV: use host as-is (includes scheme)
            : (server.port !== 21 ? `${server.host}:${server.port}` : server.host);

        onConnect({
            server: serverString,
            username: server.username,
            password: server.password || '',
            protocol: server.protocol || 'ftp',
            port: server.port,
            displayName: server.name,  // Pass custom name for tab display
            options: server.options,   // Pass S3/WebDAV options
        }, server.initialPath, server.localInitialPath);
    };



    return (
        <div className={`${className}`}>
            <div className="mb-4">
                <h3 className="text-lg font-semibold flex items-center gap-2">
                    <Server size={20} />
                    {t('connection.savedServers')}
                </h3>
                <div className="text-xs text-gray-500 font-normal mt-1">
                    Select a server to connect, or click Edit to load into the form.
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
                                Dismiss
                            </button>
                        </div>
                    </div>
                </div>
            )}

            <div className="space-y-2">
                {servers.map(server => (
                    <div
                        key={server.id}
                        className={`flex items-center justify-between p-3 bg-gray-100 dark:bg-gray-700 rounded-xl hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors group ${oauthConnecting === server.id ? 'opacity-75' : ''}`}
                    >
                        <button
                            onClick={() => handleConnect(server)}
                            disabled={oauthConnecting !== null}
                            className="flex-1 text-left flex items-center gap-3 disabled:cursor-wait"
                        >
                            <div className={`w-10 h-10 shrink-0 rounded-lg bg-gradient-to-br ${protocolColors[server.protocol || 'ftp']} flex items-center justify-center text-white ${oauthConnecting === server.id ? 'animate-pulse' : ''}`}>
                                {isOAuthProvider(server.protocol || 'ftp') ? (
                                    <Cloud size={18} />
                                ) : (
                                    <span className="font-bold">{(server.name || server.host).charAt(0).toUpperCase()}</span>
                                )}
                            </div>
                            <div>
                                <div className="font-medium flex items-center gap-2">
                                    {server.name || server.host}
                                    {server.protocol === 'mega' && server.options?.session_expires_at && Date.now() > server.options.session_expires_at && (
                                        <span className="text-[10px] px-1.5 py-0.5 rounded-full bg-red-100 dark:bg-red-900/60 text-red-600 dark:text-red-300 font-bold border border-red-200 dark:border-red-800 flex items-center gap-1" title="Session expired (24h)">
                                            <Clock size={10} /> EXP
                                        </span>
                                    )}
                                    {oauthConnecting === server.id && (
                                        <span className="text-xs text-blue-500 animate-pulse">Authenticating...</span>
                                    )}
                                    <span className="text-xs px-1.5 py-0.5 rounded bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300 uppercase">
                                        {server.protocol || 'ftp'}
                                    </span>
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400">
                                    {isOAuthProvider(server.protocol || 'ftp')
                                        ? 'OAuth2 Connection'
                                        : `${server.username}@${server.host}:${server.port}`
                                    }
                                </div>
                            </div>
                        </button>
                        <div className="flex gap-1 opacity-0 group-hover:opacity-100 transition-opacity">
                            <button
                                onClick={() => handleEdit(server)}
                                className="p-2 text-gray-500 hover:text-blue-500 hover:bg-blue-50 dark:hover:bg-blue-900/30 rounded-lg transition-colors"
                                title={t('connection.editServer')}
                            >
                                <Edit2 size={14} />
                            </button>
                            <button
                                onClick={() => handleDelete(server.id)}
                                className="p-2 text-gray-500 hover:text-red-500 hover:bg-red-50 dark:hover:bg-red-900/30 rounded-lg transition-colors"
                                title={t('connection.deleteServer')}
                            >
                                <Trash2 size={14} />
                            </button>
                        </div>
                    </div>
                ))}
            </div>

            {/* Form removed - use main panel for adding/editing */}
        </div>
    );
};

export default SavedServers;
