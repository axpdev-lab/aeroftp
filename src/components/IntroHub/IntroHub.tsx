import * as React from 'react';
import { useState, useCallback, useEffect } from 'react';
import { ConnectionParams, ServerProfile } from '../../types';
import { IntroHubHeader, FormTab } from './IntroHubHeader';
import { MyServersPanel } from './MyServersPanel';
import { DiscoverPanel } from './DiscoverPanel';
// CommandPalette removed — search is redundant with filter chips
import { ConnectionScreen } from '../ConnectionScreen';
import { ExportImportDialog } from '../ExportImportDialog';
import { getTotalServiceCount } from './discoverData';
import { getProviderById } from '../../providers';
import { useTranslation } from '../../i18n';
import { secureStoreAndClean } from '../../utils/secureStorage';
import type { ProviderType } from '../../types';

const TAB_STATE_KEY = 'aeroftp-intro-active-tab';

interface QuickConnectDirs {
    remoteDir: string;
    localDir: string;
}

interface FormTabState extends FormTab {
    connectionParams: ConnectionParams;
    quickConnectDirs: QuickConnectDirs;
    originTab?: string;
    /** Original label set at tab creation — used as fallback when server field is empty */
    defaultLabel: string;
    /** User-typed connection name — takes priority over server-derived label */
    userLabel?: string;
}

export interface IntroHubProps {
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
    isAeroCloudPaused?: boolean;
    onOpenCloudPanel?: () => void;
    onOpenCrossProfile?: () => void;
    hasExistingSessions?: boolean;
    serversRefreshKey?: number;
    onServersChanged?: () => void;
}

function generateTabId(): string {
    return `form_${Date.now()}_${Math.random().toString(36).substr(2, 6)}`;
}

export function IntroHub(props: IntroHubProps) {
    const {
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
        isAeroCloudPaused,
        onOpenCloudPanel,
        onOpenCrossProfile,
        hasExistingSessions,
        serversRefreshKey,
        onServersChanged,
    } = props;

    const t = useTranslation();

    // Active tab: 'my-servers' | 'discover' | formTab.id
    const [activeTab, setActiveTab] = useState<string>(() => {
        const stored = localStorage.getItem(TAB_STATE_KEY);
        return stored === 'discover' ? 'discover' : 'my-servers';
    });

    // Dynamic form tabs
    const [formTabs, setFormTabs] = useState<FormTabState[]>([]);

    // Command Palette
    // showPalette removed — CommandPalette was redundant with filter chips

    // Saved servers for Command Palette
    const [paletteServers, setPaletteServers] = useState<ServerProfile[]>([]);
    useEffect(() => {
        try {
            const stored = localStorage.getItem('aeroftp-saved-servers');
            if (stored) setPaletteServers(JSON.parse(stored));
        } catch { /* ignore */ }
    }, [serversRefreshKey]);

    // Export/Import dialog
    const [showExportImport, setShowExportImport] = useState(false);

    // Persist static tab (not form tabs)
    useEffect(() => {
        if (activeTab === 'my-servers' || activeTab === 'discover') {
            localStorage.setItem(TAB_STATE_KEY, activeTab);
        }
    }, [activeTab]);

    // --- Tab handlers ---

    const handleTabChange = useCallback((tab: string) => {
        setActiveTab(tab);
    }, []);

    // "+ New" button -> switch to Discover
    const handleNewConnection = useCallback(() => {
        setActiveTab('discover');
    }, []);

    // Create a form tab from Discover (provider selection)
    const handleSelectProvider = useCallback((protocol: ProviderType, providerId?: string, demo?: { server: string; port: number; username: string; password: string }) => {
        const id = generateTabId();
        const PROVIDER_NAMES: Record<string, string> = { pixelunion: 'PixelUnion' };
        const label = demo ? `Demo: ${protocol.toUpperCase()}` : (PROVIDER_NAMES[providerId || ''] || providerId || protocol.toUpperCase());
        // Apply provider defaults (server, port, basePath) when creating the tab
        const provider = providerId ? getProviderById(providerId) : undefined;
        const defaultLabel = `New: ${label}`;
        const newTab: FormTabState = {
            id,
            label: defaultLabel,
            defaultLabel,
            protocol,
            providerId,
            connectionParams: {
                server: demo?.server || provider?.defaults?.server || '',
                username: demo?.username || '',
                password: demo?.password || '',
                protocol,
                port: demo?.port || provider?.defaults?.port || undefined,
                providerId,
                options: {
                    pathStyle: provider?.defaults?.pathStyle,
                    region: provider?.defaults?.region,
                    endpoint: provider?.defaults?.endpoint,
                },
            },
            quickConnectDirs: { remoteDir: provider?.defaults?.basePath || '', localDir: '' },
            originTab: activeTab,
        };
        setFormTabs(prev => [...prev, newTab]);
        setActiveTab(id);
    }, [activeTab]);

    // Create a form tab for editing a saved server
    const handleEdit = useCallback((profile: ServerProfile) => {
        // If already editing this server, switch to existing tab
        const existing = formTabs.find(ft => ft.editingProfile?.id === profile.id);
        if (existing) {
            setActiveTab(existing.id);
            return;
        }

        const id = generateTabId();
        const editLabel = profile.name || profile.host || 'Edit';
        const newTab: FormTabState = {
            id,
            label: editLabel,
            defaultLabel: editLabel,
            editingProfile: profile,
            protocol: profile.protocol,
            providerId: profile.providerId,
            connectionParams: {
                server: profile.host || '',
                port: profile.port || 21,
                username: profile.username || '',
                password: '',
                protocol: profile.protocol || 'ftp',
                options: profile.options || {},
                providerId: profile.providerId,
            },
            quickConnectDirs: {
                remoteDir: profile.initialPath || '',
                localDir: profile.localInitialPath || '',
            },
            originTab: activeTab,
        };
        setFormTabs(prev => [...prev, newTab]);
        setActiveTab(id);
    }, [formTabs, activeTab]);

    // Close a form tab — return to the tab that opened it
    const handleCloseFormTab = useCallback((tabId: string) => {
        setFormTabs(prev => {
            const closing = prev.find(ft => ft.id === tabId);
            const origin = closing?.originTab || 'my-servers';
            setActiveTab(current => current === tabId ? origin : current);
            return prev.filter(ft => ft.id !== tabId);
        });
    }, []);

    // Close all form tabs
    const handleCloseAllFormTabs = useCallback(() => {
        setFormTabs([]);
        setActiveTab('my-servers');
    }, []);

    // Update form tab's connectionParams + derive dynamic tab label from server field.
    // When protocol changes (FTP↔SFTP switch), also update editingProfile so the
    // remounted ConnectionScreen initializes with the correct protocol.
    const updateFormTabParams = useCallback((tabId: string, params: ConnectionParams) => {
        setFormTabs(prev => prev.map(ft => {
            if (ft.id !== tabId) return ft;
            // Sync editingProfile.protocol when protocol switches
            let updatedProfile = ft.editingProfile;
            if (updatedProfile && params.protocol && params.protocol !== updatedProfile.protocol) {
                updatedProfile = { ...updatedProfile, protocol: params.protocol, port: params.port || updatedProfile.port };
            }
            // Only derive label from server if user hasn't set a connection name
            if (ft.userLabel) return { ...ft, connectionParams: params, editingProfile: updatedProfile };
            // Derive label from server field: strip protocol prefix, take hostname only
            const raw = params.server?.trim() || '';
            const cleaned = raw.replace(/^https?:\/\//, '').split('/')[0].split(':')[0];
            const provider = ft.providerId ? getProviderById(ft.providerId) : undefined;
            const isDefault = !cleaned || cleaned === provider?.defaults?.server;
            const label = isDefault ? ft.defaultLabel : cleaned;
            return { ...ft, connectionParams: params, editingProfile: updatedProfile, label };
        }));
    }, []);

    // Update form tab label when user types a connection name (highest priority).
    // Empty name clears the override — label falls back to server hostname or default.
    const updateFormTabLabel = useCallback((tabId: string, name: string) => {
        setFormTabs(prev => prev.map(ft => {
            if (ft.id !== tabId) return ft;
            if (!name) {
                // Clear user override — derive from current server or default
                const raw = ft.connectionParams.server?.trim() || '';
                const cleaned = raw.replace(/^https?:\/\//, '').split('/')[0].split(':')[0];
                const provider = ft.providerId ? getProviderById(ft.providerId) : undefined;
                const isDefault = !cleaned || cleaned === provider?.defaults?.server;
                return { ...ft, label: isDefault ? ft.defaultLabel : cleaned, userLabel: undefined };
            }
            return { ...ft, label: name, userLabel: name };
        }));
    }, []);

    // Update form tab's quickConnectDirs
    const updateFormTabDirs = useCallback((tabId: string, dirs: QuickConnectDirs) => {
        setFormTabs(prev => prev.map(ft => ft.id === tabId ? { ...ft, quickConnectDirs: dirs } : ft));
    }, []);

    // Command Palette
    const handleCommandPalette = useCallback(() => {
        // no-op — CommandPalette removed
    }, []);

    // Connect from Command Palette (saved server)
    const handlePaletteConnect = useCallback(async (server: ServerProfile) => {
        const params: ConnectionParams = {
            server: server.host || '',
            port: server.port || 21,
            username: server.username || '',
            password: '',
            protocol: server.protocol || 'ftp',
            options: server.options || {},
            providerId: server.providerId,
        };
        await onSavedServerConnect(params, server.initialPath, server.localInitialPath);
    }, [onSavedServerConnect]);

    // Keyboard shortcuts
    useEffect(() => {
        const handler = (e: KeyboardEvent) => {
            if ((e.ctrlKey || e.metaKey) && e.key === '1') {
                e.preventDefault();
                handleTabChange('my-servers');
            } else if ((e.ctrlKey || e.metaKey) && e.key === '2') {
                e.preventDefault();
                handleTabChange('discover');
            } else if ((e.ctrlKey || e.metaKey) && e.key === 'n') {
                e.preventDefault();
                handleNewConnection();
            }
        };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [handleTabChange, handleCommandPalette, handleNewConnection]);

    // Find active form tab (if any)
    const activeFormTab = formTabs.find(ft => ft.id === activeTab);

    return (
        <div className="max-w-7xl w-full mx-auto relative z-10 flex flex-col h-full bg-slate-50/50 dark:bg-gray-800/40 backdrop-blur-md rounded-xl border border-gray-200/50 dark:border-gray-700/50 shadow-2xl overflow-hidden">
            {/* Tab Header */}
            <IntroHubHeader
                activeTab={activeTab}
                onTabChange={handleTabChange}
                onNewConnection={handleNewConnection}
                onCommandPalette={handleCommandPalette}
                formTabs={formTabs}
                onCloseFormTab={handleCloseFormTab}
                onCloseAllFormTabs={handleCloseAllFormTabs}
                hasExistingSessions={hasExistingSessions}
                onSkipToFileManager={onSkipToFileManager}
                onAeroCloud={onAeroCloud}
                onAeroFile={onAeroFile}
                onCrossProfileTransfer={onOpenCrossProfile}
                isAeroCloudConnected={isAeroCloudConnected}
                isAeroCloudPaused={isAeroCloudPaused}
                isAeroCloudConfigured={isAeroCloudConfigured}
                serverCount={paletteServers.length}
                serviceCount={getTotalServiceCount()}
            />

            {/* Tab Content */}
            <div className="flex-1 min-h-0 overflow-auto p-6">
                {/* Tab: My Servers */}
                {activeTab === 'my-servers' && (
                    <MyServersPanel
                        onConnect={onSavedServerConnect}
                        onEdit={handleEdit}
                        onQuickConnect={handleNewConnection}
                        lastUpdate={serversRefreshKey}
                        onOpenExportImport={() => setShowExportImport(true)}
                        onServersChange={() => {
                            try {
                                const stored = localStorage.getItem('aeroftp-saved-servers');
                                if (stored) setPaletteServers(JSON.parse(stored));
                                else setPaletteServers([]);
                            } catch { setPaletteServers([]); }
                        }}
                    />
                )}

                {/* Tab: Discover Services */}
                {activeTab === 'discover' && (
                    <DiscoverPanel
                        onSelectProvider={handleSelectProvider}
                    />
                )}

                {/* Dynamic Form Tabs: render the active one */}
                {activeFormTab && (
                    <div className="flex-1 flex flex-col">
                        <ConnectionScreen
                            key={`${activeFormTab.id}-${activeFormTab.connectionParams.protocol || 'none'}`}
                            formOnly
                            connectionParams={activeFormTab.connectionParams}
                            quickConnectDirs={activeFormTab.quickConnectDirs}
                            loading={loading}
                            onConnectionParamsChange={(params) => updateFormTabParams(activeFormTab.id, params)}
                            onQuickConnectDirsChange={(dirs) => updateFormTabDirs(activeFormTab.id, dirs)}
                            onTabLabelChange={(name) => updateFormTabLabel(activeFormTab.id, name)}
                            editingProfile={activeFormTab.editingProfile}
                            onConnect={() => {
                                // Pass params directly to avoid stale React state (#81)
                                onConnectionParamsChange(activeFormTab.connectionParams);
                                onQuickConnectDirsChange(activeFormTab.quickConnectDirs);
                                onConnect(activeFormTab.connectionParams);
                                handleCloseFormTab(activeFormTab.id);
                            }}
                            onSavedServerConnect={async (params, initialPath, localInitialPath) => {
                                handleCloseFormTab(activeFormTab.id);
                                await onSavedServerConnect(params, initialPath, localInitialPath);
                            }}
                            onFormSaved={() => {
                                handleCloseFormTab(activeFormTab.id);
                                setActiveTab('my-servers');
                                onServersChanged?.();
                            }}
                            onSkipToFileManager={() => { handleCloseFormTab(activeFormTab.id); onSkipToFileManager(); }}
                            onAeroFile={onAeroFile}
                            onAeroCloud={onAeroCloud}
                            isAeroCloudConfigured={isAeroCloudConfigured}
                            isAeroCloudConnected={isAeroCloudConnected}
                            onOpenCloudPanel={onOpenCloudPanel}
                            hasExistingSessions={hasExistingSessions}
                            serversRefreshKey={serversRefreshKey}
                        />
                    </div>
                )}
            </div>

            {/* Export/Import Dialog */}
            {showExportImport && (
                <ExportImportDialog
                    servers={paletteServers}
                    onImport={(newServers) => {
                        let currentServers: ServerProfile[] = [];
                        try {
                            const stored = localStorage.getItem('aeroftp-saved-servers');
                            if (stored) currentServers = JSON.parse(stored);
                        } catch { /* fallback */ }
                        if (currentServers.length === 0) currentServers = paletteServers;
                        const updated = [...currentServers, ...newServers];
                        secureStoreAndClean('server_profiles', 'aeroftp-saved-servers', updated).catch(() => {});
                        setPaletteServers(updated);
                        setShowExportImport(false);
                        onServersChanged?.();
                    }}
                    onClose={() => setShowExportImport(false)}
                />
            )}
        </div>
    );
}
