// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { homeDir, downloadDir } from '@tauri-apps/api/path';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { getVersion } from '@tauri-apps/api/app';
import {
  FileListResponse, ConnectionParams, DownloadParams, UploadParams,
  LocalFile, TransferEvent, TransferProgress, RemoteFile, FtpSession, ServerProfile,
  ProviderType, isOAuthProvider, isFourSharedProvider, isNonFtpProvider, isFtpProtocol, supportsStorageQuota, supportsNativeShareLink
} from './types';

interface DownloadFolderParams {
  remote_path: string;
  local_path: string;
  file_exists_action?: string;
  max_concurrent?: number;
  retry_count?: number;
  timeout_seconds?: number;
}

interface UploadFolderParams {
  local_path: string;
  remote_path: string;
  file_exists_action?: string;
  max_concurrent?: number;
  retry_count?: number;
  timeout_seconds?: number;
}

type TransferSpeedPreset = 'base' | 'fast' | 'super';

const TRANSFER_SPEED_PRESETS: Record<TransferSpeedPreset, { label: string; channels: number }> = {
  base: { label: 'Safe', channels: 1 },
  fast: { label: 'Balanced', channels: 3 },
  super: { label: 'Max', channels: 5 },
};

const deriveTransferSpeedPreset = (channels: number): TransferSpeedPreset => {
  if (channels <= 1) return 'base';
  if (channels <= 3) return 'fast';
  return 'super';
};
import { SessionTabs } from './components/SessionTabs';
import { PermissionsDialog } from './components/PermissionsDialog';
import { ToastContainer, useToast } from './components/Toast';
import { ContextMenu, useContextMenu, ContextMenuItem } from './components/ContextMenu';
import { SavedServers } from './components/SavedServers';
import { ConnectionScreen } from './components/ConnectionScreen';
import { IntroHub } from './components/IntroHub';
import { AboutDialog } from './components/AboutDialog';
import { SupportDialog } from './components/SupportDialog';
import { ShortcutsDialog } from './components/ShortcutsDialog';
import { ProvidersDialog } from './components/ProvidersDialog';
import { SettingsPanel } from './components/SettingsPanel';
import { StatusBar } from './components/StatusBar';
import { TransferQueue, useTransferQueue } from './components/TransferQueue';
import { useCircuitBreaker } from './hooks/useCircuitBreaker';
import { RECONNECT_ERROR_KINDS, getErrorKindI18nKey } from './utils/transferErrorClassifier';
import { CustomTitlebar } from './components/CustomTitlebar';
import { WindowResizeEdges } from './components/WindowResizeEdges';
import { DevToolsV2, PreviewFile, isPreviewable } from './components/DevTools';
import { UniversalPreview, PreviewFileData, getPreviewCategory, isPreviewable as isMediaPreviewable } from './components/Preview';
import { SyncPanel } from './components/SyncPanel';
import { VaultPanel } from './components/VaultPanel';
import { CryptomatorBrowser } from './components/CryptomatorBrowser';
import { ArchiveBrowser } from './components/ArchiveBrowser';
import { ZohoTrashManager } from './components/ZohoTrashManager';
import { GoogleDriveCommentDialog } from './components/GoogleDriveCommentDialog';
import { GitHubCommitDialog } from './components/GitHubCommitDialog';
import { GitHubLocalSyncWarning } from './components/GitHubLocalSyncWarning';
import { GitHubBranchSelector } from './components/GitHubBranchSelector';
import { GitHubWriteModeIndicator } from './components/GitHubWriteModeIndicator';
import { GitHubActionsIcon } from './components/icons/GitHubActionsIcon';
import { GitHubReleaseIcon } from './components/icons/GitHubReleaseIcon';
import { GitHubPagesIcon } from './components/icons/GitHubPagesIcon';
import { GitHubReleaseBrowser } from './components/GitHubReleaseBrowser';
import { GitLabReleaseBrowser } from './components/GitLabReleaseBrowser';
import { GitHubPagesBrowser } from './components/GitHubPagesBrowser';
import { GitHubActionsBrowser } from './components/GitHubActionsBrowser';
import { JottacloudTrashManager } from './components/JottacloudTrashManager';
import { MegaTrashManager } from './components/MegaTrashManager';
import { FileLuTrashManager } from './components/FileLuTrashManager';
import { GoogleDriveTrashManager } from './components/GoogleDriveTrashManager';
import { BoxTrashManager } from './components/BoxTrashManager';
import { BoxTagsDialog } from './components/BoxTagsDialog';
import { DropboxTrashManager } from './components/DropboxTrashManager';
import { OneDriveTrashManager } from './components/OneDriveTrashManager';
import { KoofrTrashManager } from './components/KoofrTrashManager';
import { NextcloudTrashManager } from './components/NextcloudTrashManager';
import { OpenDriveTrashManager } from './components/OpenDriveTrashManager';
import { YandexTrashManager } from './components/YandexTrashManager';
import { PCloudTrashManager } from './components/PCloudTrashManager';
import { KDriveTrashManager } from './components/KDriveTrashManager';
import { FilenNotesPanel } from './components/FilenNotesPanel';
import { CompressDialog, CompressOptions } from './components/CompressDialog';
import { ShareLinkModal } from './components/ShareLinkModal';
import CryptomatorCreateDialog from './components/CryptomatorCreateDialog';
import { CloudPanel } from './components/CloudPanel';
import { OverwriteDialog } from './components/OverwriteDialog';
import { FolderOverwriteDialog, FolderMergeAction } from './components/FolderOverwriteDialog';
import { BatchRenameDialog, BatchRenameFile } from './components/BatchRenameDialog';
import { CyberToolsModal } from './components/CyberToolsModal';
import { LockScreen } from './components/LockScreen';
import KeystoreMigrationWizard from './components/KeystoreMigrationWizard';
import { Checkbox } from './components/ui/Checkbox';
import { FileVersionsDialog } from './components/FileVersionsDialog';
import { HostKeyDialog, HostKeyInfo } from './components/HostKeyDialog';
import { APP_BACKGROUND_PATTERNS, APP_BACKGROUND_KEY, DEFAULT_APP_BACKGROUND } from './utils/appBackgroundPatterns';
import { resolveS3Endpoint, getProviderById } from './providers/registry';
import { SharePermissionsDialog } from './components/SharePermissionsDialog';
import { CommandPalette, CommandItem, CommandCategory } from './components/CommandPalette';
import { ScanningToast, INITIAL_SCANNING_STATE } from './components/ScanningToast';
import type { ScanningState } from './components/ScanningToast';
import { ProviderThumbnail } from './components/ProviderThumbnail';
import {
  FolderUp, RefreshCw, FolderPlus, FolderOpen,
  Download, Upload, Pencil, Trash2, X, ShieldCheck, ShieldQuestion, ShieldAlert, Loader2,
  Folder, FileText, Globe, HardDrive, Settings, Search, Eye, Link2, Unlink, Shield, ShieldOff, Cloud,
  Archive, Image, Video, Music, FileType, Code, Database, Clock,
  Copy, Clipboard, ClipboardPaste, ClipboardList, Scissors, ExternalLink, List, LayoutGrid, CheckCircle2, AlertTriangle, Share2, Info,
  Lock, Unlock, Server, XCircle, History, Users, FolderSync, Replace, LogOut, PanelLeft, Rows3, Zap,
  MoreHorizontal, Tag, Bot, Terminal, Star, MessageSquare, Package, FileSpreadsheet, Presentation, LinkIcon, GitCommit
} from 'lucide-react';

const Github = ({ size = 24, className = '' }: { size?: number; className?: string }) => (
  <svg width={size} height={size} viewBox="0 0 24 24" fill="currentColor" className={className}>
    <path fillRule="evenodd" clipRule="evenodd" d="M12.026 2c-5.509 0-9.974 4.465-9.974 9.974 0 4.406 2.857 8.145 6.821 9.465.499.09.679-.217.679-.481 0-.237-.008-.865-.011-1.696-2.775.602-3.361-1.338-3.361-1.338-.452-1.152-1.107-1.459-1.107-1.459-.905-.619.069-.605.069-.605 1.002.07 1.527 1.028 1.527 1.028.89 1.524 2.336 1.084 2.902.829.091-.645.351-1.085.635-1.334-2.214-.251-4.542-1.107-4.542-4.93 0-1.087.389-1.979 1.024-2.675-.101-.253-.446-1.268.099-2.64 0 0 .837-.269 2.742 1.021a9.582 9.582 0 0 1 2.496-.336 9.554 9.554 0 0 1 2.496.336c1.906-1.291 2.742-1.021 2.742-1.021.545 1.372.203 2.387.099 2.64.64.696 1.024 1.587 1.024 2.675 0 3.833-2.33 4.675-4.552 4.922.355.308.675.916.675 1.846 0 1.334-.012 2.41-.012 2.737 0 .267.178.577.687.479C19.146 20.115 22 16.379 22 11.974 22 6.465 17.535 2 12.026 2z"/>
  </svg>
);
import PanelSwitcher from './components/PanelSwitcher';
import { PlacesSidebar } from './components/PlacesSidebar';
import { BreadcrumbBar } from './components/BreadcrumbBar';
import { LargeIconsGrid } from './components/LargeIconsGrid';
import { LocalFilePanel } from './components/LocalFilePanel';
import { QuickLookOverlay } from './components/QuickLookOverlay';
import DuplicateFinderDialog from './components/DuplicateFinderDialog';
import DiskUsageTreemap from './components/DiskUsageTreemap';
import { FileTagBadge } from './components/FileTagBadge';
import { VaultIcon } from './components/icons/VaultIcon';
import type { TrashItem, FolderSizeResult, LocalTab } from './types/aerofile';

// Utilities
import { formatBytes, formatSpeed, formatETA, formatDate } from './utils';
import { useIconTheme, getDefaultIconTheme } from './hooks/useIconTheme';
import { getIconThemeProvider } from './utils/iconThemes';
import { logger } from './utils/logger';
import { initCspReporter } from './utils/cspReporter';
import { secureGetWithFallback, secureStoreAndClean } from './utils/secureStorage';
import { maskCredential } from './utils/maskCredential';
import { useTranslation } from './i18n';

// Components
import { ConfirmDialog, InputDialog, SyncNavDialog, PropertiesDialog, FileProperties, MasterPasswordSetupDialog } from './components/Dialogs';
import { TransferToastContainer, dispatchTransferToast, reopenTransferToast } from './components/Transfer/TransferToastContainer';
import { GlobalTooltip } from './components/GlobalTooltip';
import { TransferProgressBar } from './components/TransferProgressBar';
import { ImageThumbnail } from './components/ImageThumbnail';
import { SortableHeader, SortField, SortOrder } from './components/SortableHeader';
import { FeatureBadge } from './components/FeatureBadge';
import ActivityLogPanel from './components/ActivityLogPanel';
import DebugPanel, { activateGlobalCapture, activateNetworkCapture } from './components/DebugPanel';
import DependenciesPanel from './components/DependenciesPanel';
import { GoogleDriveLogo, DropboxLogo, OneDriveLogo, MegaLogo, BoxLogo, PCloudLogo, FilenLogo, OpenDriveLogo, GitHubLogo, GitLabLogo, FeliCloudLogo, FileLuLogo, KDriveLogo, DrimeCloudLogo, YandexDiskLogo, KoofrLogo, JottacloudLogo, ZohoWorkDriveLogo, InternxtLogo, AzureLogo, PROVIDER_LOGOS } from './components/ProviderLogos';

// Hooks (modularized from App.tsx - see architecture comment below)
import { useTheme, Theme, getLogTheme, getMonacoTheme, getEffectiveTheme } from './hooks/useTheme';
import { useActivityLog } from './hooks/useActivityLog';
import { useHumanizedLog } from './hooks/useHumanizedLog';
import { useSettings } from './hooks/useSettings';
import { useAutoUpdate } from './hooks/useAutoUpdate';
import { usePreview } from './hooks/usePreview';
import { useOverwriteCheck } from './hooks/useOverwriteCheck';
import { useDragAndDrop } from './hooks/useDragAndDrop';
import { useKeyboardShortcuts } from './hooks/useKeyboardShortcuts';
import { useTransferEvents } from './hooks/useTransferEvents';
import { useCloudSync } from './hooks/useCloudSync';
import { useFileTags } from './hooks/useFileTags';
import { useFaviconDetection } from './hooks/useFaviconDetection';

// ============================================================================
// Main App Component
// ============================================================================
// Architecture: App.tsx is the root component orchestrating all FTP client
// functionality. Logic is progressively extracted into custom hooks:
//
// Extracted hooks (src/hooks/):
//   useSettings        - App settings (localStorage persistence, live reload)
//   useAutoUpdate      - Startup update check + manual update trigger
//   usePreview         - Sidebar preview, DevTools editor, Universal media preview
//   useOverwriteCheck  - File overwrite detection, dialog state, "apply to all"
//   useDragAndDrop     - Drag & drop file moves within same panel
//   useTheme           - Dark/light/system theme management
//   useActivityLog     - Structured activity log with filtering
//   useHumanizedLog    - Human-readable log messages with i18n
//   useKeyboardShortcuts - Global keyboard shortcuts
//   useTransferEvents  - Backend transfer/delete event listener + log correlation
//
// Remaining inline logic (candidates for future extraction):
//   - Context menus (showRemoteContextMenu, showLocalContextMenu ~450 lines)
//   - File transfer operations (upload/download/delete ~320 lines)
//   - Connection logic (connectToFtp ~200 lines)
//   - Cloud sync events (~98 lines)
// ============================================================================
const App: React.FC = () => {
  // === Settings (persisted in localStorage, live-reloaded) ===
  const settings = useSettings();
  const {
    compactMode, showHiddenFiles, showToastNotifications, confirmBeforeDelete,
    showStatusBar, defaultLocalPath, fontSize, fontFamily, doubleClickAction, rememberLastFolder, visibleColumns,
    sortFoldersFirst, showFileExtensions, timeoutSeconds, maxConcurrentTransfers, retryCount,
    fileExistsAction, swapPanels,
    showActivityLog, showConnectionScreen,
    showSettingsPanel, setShowSettingsPanel, setShowConnectionScreen,
    setShowActivityLog,
    setShowHiddenFiles, debugMode, setDebugMode,
    setSwapPanels,
    SETTINGS_KEY,
  } = settings;
  const [sessionTransferSpeedPreset, setSessionTransferSpeedPreset] = useState<TransferSpeedPreset>(
    () => deriveTransferSpeedPreset(maxConcurrentTransfers)
  );

  // Sync font CSS variables to <html> so Tailwind rem-based classes scale globally
  useEffect(() => {
    const root = document.documentElement;
    const effectiveSize = compactMode ? Math.round(fontSize * 0.85) : fontSize;
    root.style.setProperty('--app-font-size', `${effectiveSize}px`);
    root.style.setProperty('--app-font-family', fontFamily);
  }, [fontSize, fontFamily, compactMode]);

  const toggleSwapPanels = useCallback(async () => {
    const next = !swapPanels;
    setSwapPanels(next);
    try {
      const existing = await secureGetWithFallback<Record<string, unknown>>('app_settings', SETTINGS_KEY);
      const updated = { ...(existing || {}), swapPanels: next };
      await secureStoreAndClean('app_settings', SETTINGS_KEY, updated);
      window.dispatchEvent(new CustomEvent('aeroftp-settings-changed', { detail: updated }));
    } catch { /* ignore */ }
  }, [swapPanels, setSwapPanels, SETTINGS_KEY]);

  const usesProviderApi = (protocol?: ProviderType) => {
    return !!protocol && (protocol === 'ftp' || protocol === 'ftps' || isNonFtpProvider(protocol));
  };

  // === Master Password / App Lock State ===
  const [isAppLocked, setIsAppLocked] = useState(false);
  const [masterPasswordSet, setMasterPasswordSet] = useState(false);
  const [showMasterPasswordSetup, setShowMasterPasswordSetup] = useState(false);
  const [masterPasswordBootstrapMode, setMasterPasswordBootstrapMode] = useState(false);
  const [showMigrationWizard, setShowMigrationWizard] = useState(false);
  const [settingsInitialTab, setSettingsInitialTab] = useState<'general' | 'connection' | 'servers' | 'filehandling' | 'transfers' | 'cloudproviders' | 'ui' | 'security' | 'privacy' | undefined>(undefined);

  const localFilesInitStarted = useRef(false);  // Guard against re-running init effect

  // === Splash Screen Readiness ===
  const vaultInitDone = useRef(false);
  const localFilesInitDone = useRef(false);
  const appReadySignaled = useRef(false);
  const signalAppReady = useCallback(() => {
    if (appReadySignaled.current) return;
    if (vaultInitDone.current && localFilesInitDone.current) {
      appReadySignaled.current = true;
      invoke('app_ready').catch(() => { });
    }
  }, []);

  // === App Background Pattern ===
  const [appBackgroundId, setAppBackgroundId] = useState(() =>
    localStorage.getItem(APP_BACKGROUND_KEY) || DEFAULT_APP_BACKGROUND
  );
  const appBackgroundPattern = useMemo(() =>
    APP_BACKGROUND_PATTERNS.find(p => p.id === appBackgroundId) || APP_BACKGROUND_PATTERNS.find(p => p.id === 'none'),
    [appBackgroundId]
  );

  const [isConnected, setIsConnected] = useState(false);
  // A6-01: Refs for menu-event listener to avoid stale closures
  const isConnectedRef = useRef(false);
  const currentLocalPathRef = useRef('');
  const themeRef = useRef<Theme>('dark');
  const debugModeRef = useRef(false);
  const [showRemotePanel, setShowRemotePanel] = useState(true);
  const [previewPanelWidth, setPreviewPanelWidth] = useState(280);
  const previewResizing = useRef(false);
  const [storageQuota, setStorageQuota] = useState<{ used: number; total: number; free: number } | null>(null);
  const quotaVersionRef = useRef(0); // Guard against stale async quota responses
  const [remoteSearchQuery, setRemoteSearchQuery] = useState('');
  const [remoteSearchResults, setRemoteSearchResults] = useState<RemoteFile[] | null>(null);
  const [remoteSearching, setRemoteSearching] = useState(false);
  const [showRemoteSearchBar, setShowRemoteSearchBar] = useState(false);
  // File versions dialog
  const [versionsDialog, setVersionsDialog] = useState<{ path: string; name: string } | null>(null);
  // Share permissions dialog
  const [sharePermissionsDialog, setSharePermissionsDialog] = useState<{ path: string; name: string } | null>(null);
  // WebDAV file locks
  const [lockedFiles, setLockedFiles] = useState<Map<string, string>>(new Map());
  // Provider capabilities (cached per session)
  const [providerCaps, setProviderCaps] = useState<{ versions: boolean; thumbnails: boolean; permissions: boolean; locking: boolean }>({ versions: false, thumbnails: false, permissions: false, locking: false });
  const [remoteFiles, setRemoteFiles] = useState<RemoteFile[]>([]);
  const [localFiles, setLocalFiles] = useState<LocalFile[]>([]);
  const [currentRemotePath, setCurrentRemotePath] = useState('/');
  const [currentLocalPath, setCurrentLocalPath] = useState('');
  const [connectionParams, setConnectionParams] = useState<ConnectionParams>({ server: '', username: '', password: '' });
  const [quickConnectDirs, setQuickConnectDirs] = useState({ remoteDir: '', localDir: '' });
  const [loading, setLoading] = useState(false);
  // Transfer progress: ref holds data (no re-renders), boolean state only changes on start/complete
  const activeTransferRef = useRef<TransferProgress | null>(null);
  const [hasActiveTransfer, setHasActiveTransfer] = useState(false);
  const setActiveTransfer = useCallback((transfer: TransferProgress | null) => {
    const wasActive = activeTransferRef.current !== null;
    const isActive = transfer !== null;
    activeTransferRef.current = transfer;
    // Only trigger App re-render on null↔non-null transitions (start/complete)
    if (wasActive !== isActive) setHasActiveTransfer(isActive);
  }, []);
  const [isReconnecting, setIsReconnecting] = useState(false);  // FTP reconnection in progress
  const [scanningState, setScanningState] = useState<ScanningState>(INITIAL_SCANNING_STATE);
  const hasActivity = hasActiveTransfer;  // Track if upload/download in progress
  const [activePanel, setActivePanel] = useState<'remote' | 'local'>('remote');
  const [remoteSortField, setRemoteSortField] = useState<SortField>('name');
  const [remoteSortOrder, setRemoteSortOrder] = useState<SortOrder>('asc');
  const [localSortField, setLocalSortField] = useState<SortField>('name');
  const [localSortOrder, setLocalSortOrder] = useState<SortOrder>('asc');
  const [selectedLocalFiles, setSelectedLocalFiles] = useState<Set<string>>(new Set());
  const [selectedRemoteFiles, setSelectedRemoteFiles] = useState<Set<string>>(new Set());
  const [lastSelectedRemoteIndex, setLastSelectedRemoteIndex] = useState<number | null>(null);
  const [lastSelectedLocalIndex, setLastSelectedLocalIndex] = useState<number | null>(null);
  // Refs for keyboard navigation (sorted arrays defined later via useMemo)
  const sortedLocalFilesRef = useRef<LocalFile[]>([]);
  const sortedRemoteFilesRef = useRef<RemoteFile[]>([]);
  const [permissionsDialog, setPermissionsDialog] = useState<{ file: RemoteFile, visible: boolean } | null>(null);
  // Navigation counter to discard stale async responses from previous navigations
  const remoteNavCounter = useRef(0);

  // Dialogs
  const [confirmDialog, setConfirmDialog] = useState<{ message: string; onConfirm: () => void; onCancel?: () => void } | null>(null);
  const [inputDialog, setInputDialog] = useState<{ title: string; defaultValue: string; onConfirm: (v: string) => void; isPassword?: boolean; placeholder?: string } | null>(null);
  const [zohoShareLinksDialog, setZohoShareLinksDialog] = useState<{ fileName: string; links: Array<{ id: string; attributes: Record<string, unknown> }> } | null>(null);
  const [zohoDeletedLinkIds] = useState(() => new Set<string>());
  const [gitHubCommitDialog, setGitHubCommitDialog] = useState<{
    files: { local: string; remote: string }[];
    operation: 'upload' | 'delete';
    branch: string;
    writeMode: 'direct' | 'branch' | 'readonly';
    workingBranch?: string;
    onCommit: (message: string) => void;
    onCancel?: () => void;
  } | null>(null);
  const [batchRenameDialog, setBatchRenameDialog] = useState<{ files: BatchRenameFile[]; isRemote: boolean } | null>(null);
  // Inline rename state: tracks which file is being renamed directly in the list
  const [inlineRename, setInlineRename] = useState<{ path: string; name: string; isRemote: boolean } | null>(null);
  const [inlineRenameValue, setInlineRenameValue] = useState('');
  const inlineRenameRef = useRef<HTMLInputElement>(null);
  const inlineRenameClickTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [propertiesDialog, setPropertiesDialog] = useState<FileProperties | null>(null);
  const [showAboutDialog, setShowAboutDialog] = useState(false);
  const [showSupportDialog, setShowSupportDialog] = useState(false);
  const [showCommandPalette, setShowCommandPalette] = useState(false);
  const [showShortcutsDialog, setShowShortcutsDialog] = useState(false);
  const [showCyberTools, setShowCyberTools] = useState(false);
  const [showProvidersDialog, setShowProvidersDialog] = useState(false);
  // Overwrite dialog: handled by useOverwriteCheck hook
  const { overwriteDialog, setOverwriteDialog, checkOverwrite, resetOverwriteSettings } = useOverwriteCheck({ localFiles, remoteFiles, fileExistsAction });
  // Folder overwrite dialog state
  const [folderOverwriteDialog, setFolderOverwriteDialog] = useState<{
    isOpen: boolean;
    folderName: string;
    direction: 'upload' | 'download';
    queueCount: number;
    resolve: ((result: { action: FolderMergeAction; applyToAll: boolean }) => void) | null;
  }>({ isOpen: false, folderName: '', direction: 'upload', queueCount: 0, resolve: null });
  const folderOverwriteApplyToAll = useRef<{ action: FolderMergeAction; enabled: boolean }>({ action: 'merge_overwrite', enabled: false });
  // showSettingsPanel provided by useSettings
  const [serversRefreshKey, setServersRefreshKey] = useState(0);
  const [showDebugPanel, setShowDebugPanel] = useState(false);

  // Activate global console capture when debug mode is enabled
  React.useEffect(() => {
    if (debugMode) { activateGlobalCapture(); activateNetworkCapture(); }
  }, [debugMode]);
  const [showDependenciesPanel, setShowDependenciesPanel] = useState(false);
  const [showSyncPanel, setShowSyncPanel] = useState(false);
  const [showVaultPanel, setShowVaultPanel] = useState<false | { mode?: 'home' | 'create' | 'open'; path?: string; files?: string[]; folderPath?: string }>(false);
  const [showCryptomatorBrowser, setShowCryptomatorBrowser] = useState(false);
  const [archiveBrowserState, setArchiveBrowserState] = useState<{ path: string; type: import('./types').ArchiveType; encrypted: boolean } | null>(null);
  const [showZohoTrash, setShowZohoTrash] = useState(false);
  const [showGDriveComment, setShowGDriveComment] = useState<{ path: string; name: string } | null>(null);
  const [showJottaTrash, setShowJottaTrash] = useState(false);
  const [showMegaTrash, setShowMegaTrash] = useState(false);
  const [showGDriveTrash, setShowGDriveTrash] = useState(false);
  const [showBoxTrash, setShowBoxTrash] = useState(false);
  const [boxTagsTarget, setBoxTagsTarget] = useState<{ path: string; tags: string[]; command?: string; providerName?: string } | null>(null);
  const [showDropboxTrash, setShowDropboxTrash] = useState(false);
  const [showOneDriveTrash, setShowOneDriveTrash] = useState(false);
  const [showFileLuTrash, setShowFileLuTrash] = useState(false);
  const [showKoofrTrash, setShowKoofrTrash] = useState(false);
  const [showOpenDriveTrash, setShowOpenDriveTrash] = useState(false);
  const [showYandexTrash, setShowYandexTrash] = useState(false);
  const [showPCloudTrash, setShowPCloudTrash] = useState(false);
  const [showKDriveTrash, setShowKDriveTrash] = useState(false);
  const [showNextcloudTrash, setShowNextcloudTrash] = useState(false);
  const [shareLinkDialog, setShareLinkDialog] = useState<{ path: string; fileName: string; providerName: string; providerType?: string; providerIcon?: React.ReactNode } | null>(null);
  const [fileLuFolderSettingsDialog, setFileLuFolderSettingsDialog] = useState<{
    path: string; name: string; filedrop: boolean; isPublic: boolean;
  } | null>(null);
  const [fileLuRemoteUploadDialog, setFileLuRemoteUploadDialog] = useState<{
    destPath: string;
  } | null>(null);
  const [gitHubRepoInfo, setGitHubRepoInfo] = useState<{
    owner: string;
    repo: string;
    branch: string;
    writeMode: string;
    writeModeKind: 'direct' | 'branch' | 'readonly' | 'unknown';
    workingBranch: string | null;
    repoPrivate: boolean;
  } | null>(null);
  const [gitHubBranches, setGitHubBranches] = useState<Array<{ name: string; protected: boolean }>>([]);
  const [showGitHubReleaseBrowser, setShowGitHubReleaseBrowser] = useState(false);
  const [showGitLabReleaseBrowser, setShowGitLabReleaseBrowser] = useState(false);
  const [showGitHubPages, setShowGitHubPages] = useState(false);
  const [showGitHubActions, setShowGitHubActions] = useState(false);
  const [hasGitHubPages, setHasGitHubPages] = useState(false);
  const [hasActiveGitHubActions, setHasActiveGitHubActions] = useState(false);
  const [showFilenNotes, setShowFilenNotes] = useState(false);
  const [gitHubSyncWarning, setGitHubSyncWarning] = useState<{
    unpushedCount: number;
    branch: string;
    resolve: (action: 'push' | 'continue' | 'cancel') => void;
  } | null>(null);
  // SEC-P1-06: TOFU host key dialog state
  const [hostKeyDialog, setHostKeyDialog] = useState<{
    visible: boolean;
    info: HostKeyInfo | null;
    host: string;
    port: number;
    resolve: ((accepted: boolean) => void) | null;
  }>({ visible: false, info: null, host: '', port: 22, resolve: null });
  const [compressDialogState, setCompressDialogState] = useState<{ files: { name: string; path: string; size: number; isDir: boolean }[]; defaultName: string; outputDir: string } | null>(null);
  const [cryptomatorCreateDialog, setCryptomatorCreateDialog] = useState<{ outputDir: string } | null>(null);
  // AeroCloud state + event listeners managed by useCloudSync hook (initialized below after core hooks)
  const [isSyncNavigation, setIsSyncNavigation] = useState(false); // Navigation Sync feature
  const [syncBasePaths, setSyncBasePaths] = useState<{ remote: string; local: string } | null>(null);
  const [syncNavDialog, setSyncNavDialog] = useState<{ missingPath: string; isRemote: boolean; targetPath: string } | null>(null);
  // AeroFile sidebar state (persisted in localStorage)
  const [showSidebar, setShowSidebar] = useState(() => localStorage.getItem('aerofile_show_sidebar') !== 'false');
  const toggleSidebar = useCallback(() => {
    setShowSidebar(prev => {
      const next = !prev;
      localStorage.setItem('aerofile_show_sidebar', String(next));
      return next;
    });
  }, []);

  // AeroFile toggle handler ref — populated after loadLocalFiles is defined
  const handleToggleAeroFileRef = useRef<() => void>(() => {});
  // Recent locations
  const [recentPaths, setRecentPaths] = useState<string[]>(() => {
    try {
      const stored = localStorage.getItem('aerofile_recent_paths');
      return stored ? JSON.parse(stored) : [];
    } catch { return []; }
  });

  // Quick Look
  const [quickLookOpen, setQuickLookOpen] = useState(false);
  const [quickLookIndex, setQuickLookIndex] = useState(0);

  // Trash view
  const [isTrashView, setIsTrashView] = useState(false);
  const [trashItems, setTrashItems] = useState<TrashItem[]>([]);

  // Folder size cache
  const [folderSizeCache, setFolderSizeCache] = useState<Map<string, FolderSizeResult>>(new Map());
  const [folderSizeCalculating, setFolderSizeCalculating] = useState<Set<string>>(new Set());
  const folderSizeCalculatingRef = useRef<Set<string>>(new Set());

  // Duplicate Finder & Disk Usage dialogs
  const [duplicateFinderPath, setDuplicateFinderPath] = useState<string | null>(null);
  const [diskUsagePath, setDiskUsagePath] = useState<string | null>(null);

  // Local Path Tabs (AeroFile multi-tab browsing)
  const [localTabs, setLocalTabs] = useState<LocalTab[]>(() => {
    try {
      const stored = localStorage.getItem('aerofile_local_tabs');
      return stored ? JSON.parse(stored) : [];
    } catch { return []; }
  });
  const [activeLocalTabId, setActiveLocalTabId] = useState<string | null>(() => {
    try {
      return localStorage.getItem('aerofile_active_tab') || null;
    } catch { return null; }
  });

  // File Tags (Finder-style color labels)
  const fileTags = useFileTags();

  // Multi-Session Tabs (Hybrid Cache Architecture)
  const [sessions, setSessions] = useState<FtpSession[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);

  // Transfer Queue (unified upload + download)
  const transferQueue = useTransferQueue();
  const retryCallbacksRef = React.useRef<Map<string, () => void>>(new Map());
  const batchCancelledRef = React.useRef(false);
  const cancelLevelRef = React.useRef(0); // 0=none, 1=soft, 2=hard

  // Circuit breaker for batch transfers
  const circuitBreaker = useCircuitBreaker();
  const [isBatchPaused, setIsBatchPaused] = React.useState(false);
  const [batchPauseReason, setBatchPauseReason] = React.useState<string | null>(null);
  const batchResumeResolverRef = React.useRef<((action: 'resume' | 'cancel') => void) | null>(null);

  const localSearchRef = React.useRef<HTMLInputElement>(null);

  // Race condition guard for loadLocalFiles — increments on each call,
  // stale responses are discarded when callId !== current counter value.
  const loadLocalCallIdRef = React.useRef(0);

  // File clipboard for Cut/Copy/Paste
  const fileClipboardRef = React.useRef<{
    files: { name: string; path: string; is_dir: boolean }[];
    sourceDir: string;
    isRemote: boolean;
    operation: 'cut' | 'copy';
  } | null>(null);
  const [hasClipboard, setHasClipboard] = React.useState(false);

  // Track if any transfer is active (for Logo animation)
  const hasQueueActivity = transferQueue.hasActiveTransfers;
  // Force stop mode: soft cancel was pressed, current file still transferring
  const isForceStopMode = cancelLevelRef.current === 1 && transferQueue.items.some(i => i.status === 'transferring');

  const [localSearchFilter, setLocalSearchFilter] = useState('');
  const [showLocalSearchBar, setShowLocalSearchBar] = useState(false);
  const insecureCertPreviouslyEnabledRef = useRef(false);

  const t = useTranslation();
  const isImageFile = (name: string) => /\.(jpg|jpeg|png|gif|svg|webp|bmp|ico)$/i.test(name);

  // Sync Badge Helper - returns badge element if file is in cloud folder
  const getSyncBadge = (filePath: string, fileModified: string | undefined, isLocal: boolean) => {
    // Only show badges if cloud is active and we have cloud folder paths
    if (!isCloudActive || !cloudLastSync || !cloudLocalFolder || !cloudRemoteFolder) return null;

    // Check if current path is within cloud folder (local or remote)
    const currentPath = isLocal ? currentLocalPath : currentRemotePath;
    const cloudFolder = isLocal ? cloudLocalFolder : cloudRemoteFolder;
    const isInCloudFolder = currentPath.startsWith(cloudFolder) || currentPath === cloudFolder;

    if (!isInCloudFolder) return null;

    const lastSyncTime = new Date(cloudLastSync).getTime();
    const fileTime = fileModified ? new Date(fileModified).getTime() : 0;

    // If syncing right now
    if (cloudSyncing) {
      return (
        <span title={t('ui.syncing')}>
          <RefreshCw size={12} className="text-cyan-500 animate-spin ml-1" />
        </span>
      );
    }

    // If file modified after last sync -> pending
    if (fileTime > lastSyncTime) {
      return (
        <span title={t('ui.pendingSync')}>
          <RefreshCw size={12} className="text-yellow-500 ml-1" />
        </span>
      );
    }

    // Otherwise synced
    return (
      <span title={t('common.synced')}>
        <CheckCircle2 size={12} className="text-green-500 ml-1" />
      </span>
    );
  };

  const parseMetadataBool = (value: unknown): boolean => {
    if (typeof value === 'boolean') return value;
    if (typeof value !== 'string') return false;
    const normalized = value.trim().toLowerCase();
    return normalized === 'true' || normalized === '1' || normalized === 'yes' || normalized === 'on';
  };

  const isPasswordProtectedFile = (file: RemoteFile): boolean => {
    const metadata = file.metadata;
    if (!metadata) return false;
    return parseMetadataBool(metadata.filelu_password_protected);
  };

  const proBadge = <span className="inline-block px-1 py-0 text-[9px] font-bold rounded bg-gradient-to-r from-amber-400 to-orange-500 text-white leading-tight align-middle">PRO</span>;

  // Check if local path is coherent with the connected remote server
  // Returns true if they match (or we can't determine), false if mismatch
  const isLocalPathCoherent = React.useMemo(() => {
    if (!isConnected || !connectionParams.server || !currentLocalPath) return true;

    // Extract server name without 'ftp.' prefix and port
    // e.g., "ftp.ericsolar.it:21" -> "ericsolar"
    const serverHost = connectionParams.server.split(':')[0]; // Remove port
    const serverName = serverHost.replace(/^ftp\./, '').replace(/^www\./, ''); // Remove ftp./www.
    const serverBase = serverName.split('.')[0]; // Get first part (e.g., "ericsolar" from "ericsolar.it")

    // Check if local path contains a reference to a different server
    // Common patterns: /var/www/html/www.ericsolar.it, /home/user/ericsolar, etc.
    const localPathLower = currentLocalPath.toLowerCase();
    const serverBaseLower = serverBase.toLowerCase();

    // If local path contains the server name, it's coherent
    if (localPathLower.includes(serverBaseLower)) return true;

    // Check if local path contains ANY known server pattern that doesn't match
    // Look for patterns like "www.something.it" or "something.it" in path
    const pathParts = currentLocalPath.split(/[\\/]/);
    for (const part of pathParts) {
      // Check for domain-like patterns (e.g., www.example.it, example.com)
      if (/^(www\.)?[a-z0-9-]+\.(it|com|org|net|io|dev|local)$/i.test(part)) {
        const pathDomain = part.replace(/^www\./, '').split('.')[0].toLowerCase();
        // If we found a domain in the path and it doesn't match our server, it's incoherent
        if (pathDomain !== serverBaseLower) return false;
      }
    }

    // Default: assume coherent if we can't determine otherwise
    return true;
  }, [isConnected, connectionParams.server, currentLocalPath]);

  // Sync navigation path mismatch: detect when local/remote paths diverge
  // Debounced: during sync navigation one panel moves first, the other follows
  // shortly after — we delay the warning to avoid a flash during the transition.
  const [isSyncPathMismatch, setIsSyncPathMismatch] = useState(false);
  useEffect(() => {
    if (!isSyncNavigation || !syncBasePaths) { setIsSyncPathMismatch(false); return; }
    const norm = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;

    // Compare only relative paths under sync base — base folder names may differ
    // legitimately (e.g., FTP home "/home/user" vs local "/var/www/html/site.com")
    // Strip leading "/" from relative portions to handle root-base providers (Zoho "/")
    // where slicing by "/" (len 1) drops the leading slash but slicing by a longer
    // base like "/home/user/Cloud" (no trailing /) preserves it.
    const stripLead = (s: string) => s.startsWith('/') ? s.slice(1) : s;
    const localRel = norm(currentLocalPath).startsWith(norm(syncBasePaths.local))
      ? stripLead(norm(currentLocalPath).slice(norm(syncBasePaths.local).length))
      : null;
    const remoteRel = norm(currentRemotePath).startsWith(norm(syncBasePaths.remote))
      ? stripLead(norm(currentRemotePath).slice(norm(syncBasePaths.remote).length))
      : null;
    const mismatch = localRel === null || remoteRel === null
      ? (localRel !== null || remoteRel !== null) // one navigated outside base, other didn't
      : localRel !== remoteRel;

    // Clear immediately, but delay showing the warning to avoid flash during sync nav
    if (!mismatch) { setIsSyncPathMismatch(false); return; }
    const timer = setTimeout(() => setIsSyncPathMismatch(true), 500);
    return () => clearTimeout(timer);
  }, [isSyncNavigation, syncBasePaths, currentLocalPath, currentRemotePath]);


  // === Universal Vault / Auto-Lock ===
  // Initialize credential vault on app load
  useEffect(() => {
    const initVault = async () => {
      try {
        const result = await invoke<string>('init_credential_store');
        if (result === 'MASTER_PASSWORD_REQUIRED') {
          setIsAppLocked(true);
          setMasterPasswordSet(true);
        } else if (result === 'MASTER_PASSWORD_SETUP_REQUIRED') {
          setMasterPasswordBootstrapMode(true);
          setShowMasterPasswordSetup(true);
        } else {
          // Check if master mode is active (for lock button state)
          const status = await invoke<{ master_mode: boolean; is_locked: boolean; timeout_seconds: number }>('get_credential_store_status');
          setMasterPasswordSet(status.master_mode);
        }
      } catch (err) {
        console.error('Failed to initialize credential vault:', err);
      } finally {
        // Pre-warm vault: fetch server profiles so data is ready for SavedServers
        // If vault has data but localStorage is empty (Windows vault.db recovery), restore it
        try {
          const vaultServers = await secureGetWithFallback<unknown[]>('server_profiles', 'aeroftp-saved-servers');
          if (vaultServers && vaultServers.length > 0) {
            const localStored = localStorage.getItem('aeroftp-saved-servers');
            if (!localStored || localStored === '[]') {
              localStorage.setItem('aeroftp-saved-servers', JSON.stringify(vaultServers));
            }
          }
        } catch { /* non-critical */ }
        // Force SavedServers to re-fetch from vault (now initialized)
        setServersRefreshKey(k => k + 1);
        vaultInitDone.current = true;
        signalAppReady();
      }
    };
    initVault();
  }, [signalAppReady]);

  // Keystore Migration Wizard: auto-trigger if legacy localStorage data exists
  useEffect(() => {
    // CSP Phase 2: register violation reporter (debug-gated, no-op in production)
    initCspReporter();

    const migrationDone = localStorage.getItem('keystore_migration_v2_done');
    if (!migrationDone) {
      const hasLegacy = localStorage.getItem('aeroftp-saved-servers')
        || localStorage.getItem('aeroftp_ai_settings')
        || localStorage.getItem('aeroftp_oauth_settings');
      if (hasLegacy) {
        setShowMigrationWizard(true);
      } else {
        localStorage.setItem('keystore_migration_v2_done', 'true');
      }
    }
  }, []);

  // Listen for app background pattern changes from Settings
  useEffect(() => {
    const handleBackgroundChange = (e: CustomEvent<string>) => {
      setAppBackgroundId(e.detail);
    };
    window.addEventListener('app-background-changed', handleBackgroundChange as EventListener);
    return () => window.removeEventListener('app-background-changed', handleBackgroundChange as EventListener);
  }, []);

  // Auto-lock timer: check every 30 seconds if timeout has expired
  useEffect(() => {
    if (!masterPasswordSet || isAppLocked) return;

    const checkAutoLock = async () => {
      try {
        const shouldLock = await invoke<boolean>('app_master_password_check_timeout');
        if (shouldLock) {
          await invoke('lock_credential_store');
          setIsAppLocked(true);
        }
      } catch (err) {
        console.error('Auto-lock check failed:', err);
      }
    };

    const interval = setInterval(checkAutoLock, 30000); // Check every 30 seconds
    return () => clearInterval(interval);
  }, [masterPasswordSet, isAppLocked]);

  // Update activity timestamp on user interaction
  useEffect(() => {
    if (!masterPasswordSet || isAppLocked) return;

    const updateActivity = () => {
      invoke('app_master_password_update_activity').catch(() => { });
    };

    // Track various user interactions
    const events = ['mousedown', 'keydown', 'scroll', 'touchstart'];
    events.forEach(event => window.addEventListener(event, updateActivity, { passive: true }));

    return () => {
      events.forEach(event => window.removeEventListener(event, updateActivity));
    };
  }, [masterPasswordSet, isAppLocked]);

  // === Core hooks (must be before keyboard shortcuts) ===
  const { theme, setTheme, isDark } = useTheme();
  const { iconTheme, setIconTheme } = useIconTheme();
  const iconProvider = useMemo(() => getIconThemeProvider(iconTheme, getEffectiveTheme(theme, isDark)), [iconTheme, theme, isDark]);

  // Auto-sync icon theme when app theme changes
  const prevEffectiveThemeRef = useRef(getEffectiveTheme(theme, isDark));
  useEffect(() => {
    const effective = getEffectiveTheme(theme, isDark);
    if (effective !== prevEffectiveThemeRef.current) {
      prevEffectiveThemeRef.current = effective;
      setIconTheme(getDefaultIconTheme(effective));
    }
  }, [theme, isDark, setIconTheme]);
  const toast = useToast();
  const contextMenu = useContextMenu();
  const humanLog = useHumanizedLog();
  const activityLog = useActivityLog();

  useEffect(() => {
    const protocol = connectionParams.protocol;
    const isFtpFamily = protocol === 'ftp' || protocol === 'ftps';
    const insecureEnabled = isFtpFamily && connectionParams.options?.verifyCert === false;

    if (insecureEnabled && !insecureCertPreviouslyEnabledRef.current) {
      activityLog.log(
        'INFO',
        t('activity.insecure_cert_enabled', {
          protocol: (protocol || 'ftp').toUpperCase(),
          server: connectionParams.server || '-'
        }),
        'success'
      );
    }

    insecureCertPreviouslyEnabledRef.current = insecureEnabled;
  }, [connectionParams.protocol, connectionParams.options?.verifyCert, connectionParams.server, activityLog, t]);

  // Auto-Update: handled by useAutoUpdate hook
  const { updateAvailable, setUpdateAvailable, checkForUpdate } = useAutoUpdate({ activityLog });
  const [updateToastDismissed, setUpdateToastDismissed] = useState(false);
  const updateToastTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
interface UpdateVerificationInfo {
  mode: 'SigstoreVerified' | 'VerificationUnavailable' | 'VerificationFailed';
  workflow_identity: string | null;
  oidc_issuer: string | null;
  artifact_sha256: string;
  bundle_present: boolean;
  bundle_parsed: boolean;
  message: string;
}

  const [updateDownload, setUpdateDownload] = useState<{
    downloading: boolean;
    percentage: number;
    speed_bps: number;
    eta_seconds: number;
    filename: string;
    downloaded: number;
    total: number;
    completedPath?: string;
    error?: string;
    installing?: boolean;
    installPhase?: 'auth' | 'running' | 'restart';
    verification?: UpdateVerificationInfo;
  } | null>(null);

  // Auto-dismiss update toast after 2 animation cycles (8s) — only when not downloading
  useEffect(() => {
    const shouldShow = updateAvailable?.has_update && !updateToastDismissed && !updateDownload;
    if (shouldShow) {
      updateToastTimerRef.current = setTimeout(() => {
        setUpdateToastDismissed(true);
      }, 8000); // 2 cycles × 4s each
    }
    return () => {
      if (updateToastTimerRef.current) {
        clearTimeout(updateToastTimerRef.current);
        updateToastTimerRef.current = null;
      }
    };
  }, [updateAvailable?.has_update, updateToastDismissed, updateDownload]);

  // AeroCloud state + event listeners (extracted hook)
  const {
    showCloudPanel, setShowCloudPanel,
    cloudSyncing, isCloudActive, setIsCloudActive,
    cloudServerName, setCloudServerName,
    cloudLastSync, setCloudLastSync,
    cloudLocalFolder, setCloudLocalFolder,
    cloudRemoteFolder, setCloudRemoteFolder,
    cloudPublicUrlBase, setCloudPublicUrlBase,
  } = useCloudSync({
    activityLog, humanLog, t, checkForUpdate, isAppLocked,
    onSyncComplete: () => {
      // Refresh both panels after AeroCloud sync completes
      loadRemoteFiles(undefined, true);
      loadLocalFiles(currentLocalPath);
    },
  });

  // showToastNotifications provided by useSettings

  // Wrapper: ActivityLog gets clean messages, Debug Panel gets full details via console
  const notify = React.useMemo(() => ({
    success: (title: string, message?: string): string | null => {
      activityLog.log('INFO', message ? `${title}: ${message}` : title, 'success');
      return showToastNotifications ? toast.success(title, message) : null;
    },
    error: (title: string, message?: string): string | null => {
      // Activity log: short title only. Debug panel: full details via console.error
      activityLog.log('INFO', message ? `${title}: ${message}` : title, 'error');
      if (message) console.error(`[ERROR] ${title}: ${message}`);
      return showToastNotifications ? toast.error(title, message) : null;
    },
    info: (title: string, message?: string): string | null => {
      activityLog.log('INFO', message ? `${title}: ${message}` : title, 'success');
      return showToastNotifications ? toast.info(title, message) : null;
    },
    warning: (title: string, message?: string): string | null => {
      activityLog.log('INFO', message ? `${title}: ${message}` : title, 'success');
      return showToastNotifications ? toast.warning(title, message) : null;
    }
  }), [showToastNotifications, toast, activityLog]);

  // Preview: handled by usePreview hook
  const preview = usePreview({ notify, toast });
  const {
    showLocalPreview, setShowLocalPreview, previewFile, setPreviewFile, previewImageBase64, previewImageDimensions,
    devToolsOpen, setDevToolsOpen, devToolsPreviewFile, setDevToolsPreviewFile, openDevToolsPreview,
    universalPreviewOpen, universalPreviewFile, openUniversalPreview, closeUniversalPreview,
    viewMode, setViewMode,
  } = preview;
  const [devToolsMaximized, setDevToolsMaximized] = useState(false);

  // Filtered files (search filter applied) — memoized to avoid recomputation on unrelated renders (M25)
  const filteredLocalFiles = useMemo(() => localFiles.filter(f => {
    if (!f.name.toLowerCase().includes(localSearchFilter.toLowerCase())) return false;
    if (fileTags.activeTagFilter) {
      const tags = fileTags.getTagsForFile(f.path);
      if (!tags.some(t => t.label_id === fileTags.activeTagFilter)) return false;
    }
    return true;
  }), [localFiles, localSearchFilter, fileTags.activeTagFilter, fileTags.getTagsForFile]);

  // Keyboard Shortcuts
  useKeyboardShortcuts({
    'F1': () => setShowShortcutsDialog(v => !v),
    'Ctrl+,': () => setShowSettingsPanel(true),
    'Ctrl+Shift+P': () => setShowCommandPalette(v => !v),

    // Delete: delete selected files
    'Delete': () => {
      if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
        const names = Array.from(selectedRemoteFiles);
        const files = remoteFiles.filter(f => names.includes(f.name));
        if (files.length > 0) deleteMultipleRemoteFiles(names);
      } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
        const names = Array.from(selectedLocalFiles);
        const files = localFiles.filter(f => names.includes(f.name));
        if (files.length > 0) deleteMultipleLocalFiles(names);
      }
    },

    // Enter: open selected folder (or preview file)
    'Enter': () => {
      if (activePanel === 'remote') {
        const name = Array.from(selectedRemoteFiles)[0];
        if (!name) return;
        const file = remoteFiles.find(f => f.name === name);
        if (file?.is_dir) changeRemoteDirectory(file.name);
      } else {
        const name = Array.from(selectedLocalFiles)[0];
        if (!name) return;
        const file = localFiles.find(f => f.name === name);
        if (file?.is_dir) changeLocalDirectory(file.path);
      }
    },

    // Backspace: go up directory
    'Backspace': () => {
      if (activePanel === 'remote') {
        if (currentRemotePath !== '/') changeRemoteDirectory('..');
      } else {
        if (currentLocalPath !== '/') changeLocalDirectory(currentLocalPath.split(/[\\/]/).slice(0, -1).join('/') || '/');
      }
    },

    // Tab: switch active panel
    'Tab': () => {
      setActivePanel(p => p === 'remote' ? 'local' : 'remote');
    },

    // F2: rename selected file (inline)
    'F2': () => {
      if (activePanel === 'remote' && selectedRemoteFiles.size === 1) {
        const name = Array.from(selectedRemoteFiles)[0];
        const file = remoteFiles.find(f => f.name === name);
        if (file && file.name !== '..') startInlineRename(file.path, file.name, true);
      } else if (activePanel === 'local' && selectedLocalFiles.size === 1) {
        const name = Array.from(selectedLocalFiles)[0];
        const file = localFiles.find(f => f.name === name);
        if (file && file.name !== '..') startInlineRename(file.path, file.name, false);
      }
    },

    // Ctrl+N: new folder
    'Ctrl+N': () => {
      createFolder(activePanel === 'remote');
    },

    // Ctrl+A: select all files
    'Ctrl+A': () => {
      if (activePanel === 'remote') {
        setSelectedRemoteFiles(new Set(remoteFiles.map(f => f.name)));
      } else {
        setSelectedLocalFiles(new Set(localFiles.map(f => f.name)));
      }
    },

    // Ctrl+U: upload selected local files
    'Ctrl+U': () => {
      if (isConnected && selectedLocalFiles.size > 0) {
        uploadMultipleFiles();
      }
    },

    // Ctrl+D: download selected remote files
    'Ctrl+D': () => {
      if (isConnected && selectedRemoteFiles.size > 0) {
        downloadMultipleFiles();
      }
    },

    // Ctrl+R: refresh active panel
    'Ctrl+R': () => {
      if (activePanel === 'remote') loadRemoteFiles();
      else loadLocalFiles(currentLocalPath);
    },

    // Ctrl+F: toggle local search bar
    'Ctrl+F': () => {
      setShowLocalSearchBar(prev => !prev);
    },

    // Ctrl+C: copy selected files
    'Ctrl+C': () => {
      if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
        const files = remoteFiles.filter(f => selectedRemoteFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
        clipboardCopy(files, true, currentRemotePath);
      } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
        const files = localFiles.filter(f => selectedLocalFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
        clipboardCopy(files, false, currentLocalPath);
      }
    },

    // Ctrl+X: cut selected files
    'Ctrl+X': () => {
      if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
        const files = remoteFiles.filter(f => selectedRemoteFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
        clipboardCut(files, true, currentRemotePath);
      } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
        const files = localFiles.filter(f => selectedLocalFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
        clipboardCut(files, false, currentLocalPath);
      }
    },

    // Ctrl+V: paste files
    'Ctrl+V': () => {
      if (activePanel === 'remote') {
        clipboardPaste(true, currentRemotePath);
      } else {
        clipboardPaste(false, currentLocalPath);
      }
    },

    // Space key: Quick Look toggle (local panel) or preview (remote panel)
    'Space': () => {
      if (activePanel === 'local') {
        if (quickLookOpen) {
          setQuickLookOpen(false);
          return;
        }
        const selectedLocal = [...selectedLocalFiles];
        if (selectedLocal.length === 1) {
          const localName = selectedLocal[0];
          const localFile = sortedLocalFiles.find(f => f.name === localName);
          if (localFile && !localFile.is_dir) {
            const idx = sortedLocalFiles.findIndex(f => f.name === localName);
            if (idx !== -1) {
              setQuickLookIndex(idx);
              setQuickLookOpen(true);
            }
          }
        }
      } else {
        const selectedRemoteName = Array.from(selectedRemoteFiles)[0];
        if (selectedRemoteName) {
          const file = remoteFiles.find(f => f.name === selectedRemoteName);
          if (file && !file.is_dir) {
            const category = getPreviewCategory(file.name);
            if (['image', 'audio', 'video', 'pdf', 'markdown', 'text'].includes(category)) {
              openUniversalPreview(file, true);
            } else if (isPreviewable(file.name)) {
              openDevToolsPreview(file, true);
            }
          }
        }
      }
    },

    // Alt+Enter: open properties for selected file
    'Alt+Enter': () => {
      if (activePanel === 'local' && selectedLocalFiles.size === 1) {
        const fileName = [...selectedLocalFiles][0];
        const file = localFiles.find(f => f.name === fileName);
        if (file) {
          setPropertiesDialog({
            name: file.name,
            path: file.path || `${currentLocalPath}/${file.name}`,
            size: file.size,
            is_dir: file.is_dir,
            modified: file.modified,
            isRemote: false,
          });
        }
      }
    },

    // Arrow Down: select next file
    'ArrowDown': () => {
      if (activePanel === 'local') {
        const files = sortedLocalFilesRef.current;
        if (files.length === 0) return;
        const currentName = [...selectedLocalFiles][0];
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : -1;
        const nextIdx = Math.min(currentIdx + 1, files.length - 1);
        const next = files[nextIdx];
        if (next && next.name !== '..') {
          setSelectedLocalFiles(new Set([next.name]));
          setLastSelectedLocalIndex(nextIdx);
          setPreviewFile(next);
        }
      } else {
        const files = sortedRemoteFilesRef.current;
        if (files.length === 0) return;
        const currentName = [...selectedRemoteFiles][0];
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : -1;
        const nextIdx = Math.min(currentIdx + 1, files.length - 1);
        const next = files[nextIdx];
        if (next && next.name !== '..') {
          setSelectedRemoteFiles(new Set([next.name]));
          setLastSelectedRemoteIndex(nextIdx);
        }
      }
    },

    // Arrow Up: select previous file
    'ArrowUp': () => {
      if (activePanel === 'local') {
        const files = sortedLocalFilesRef.current;
        if (files.length === 0) return;
        const currentName = [...selectedLocalFiles][0];
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : files.length;
        const prevIdx = Math.max(currentIdx - 1, 0);
        const prev = files[prevIdx];
        if (prev && prev.name !== '..') {
          setSelectedLocalFiles(new Set([prev.name]));
          setLastSelectedLocalIndex(prevIdx);
          setPreviewFile(prev);
        }
      } else {
        const files = sortedRemoteFilesRef.current;
        if (files.length === 0) return;
        const currentName = [...selectedRemoteFiles][0];
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : files.length;
        const prevIdx = Math.max(currentIdx - 1, 0);
        const prev = files[prevIdx];
        if (prev && prev.name !== '..') {
          setSelectedRemoteFiles(new Set([prev.name]));
          setLastSelectedRemoteIndex(prevIdx);
        }
      }
    },

    // Shift+Arrow Down: extend selection downward
    'Shift+ArrowDown': () => {
      if (activePanel === 'local') {
        const files = sortedLocalFilesRef.current;
        const currentName = [...selectedLocalFiles].pop();
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : -1;
        const nextIdx = Math.min(currentIdx + 1, files.length - 1);
        const next = files[nextIdx];
        if (next && next.name !== '..') {
          setSelectedLocalFiles(prev => new Set([...prev, next.name]));
          setLastSelectedLocalIndex(nextIdx);
        }
      } else {
        const files = sortedRemoteFilesRef.current;
        const currentName = [...selectedRemoteFiles].pop();
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : -1;
        const nextIdx = Math.min(currentIdx + 1, files.length - 1);
        const next = files[nextIdx];
        if (next && next.name !== '..') {
          setSelectedRemoteFiles(prev => new Set([...prev, next.name]));
          setLastSelectedRemoteIndex(nextIdx);
        }
      }
    },

    // Shift+Arrow Up: extend selection upward
    'Shift+ArrowUp': () => {
      if (activePanel === 'local') {
        const files = sortedLocalFilesRef.current;
        const currentName = [...selectedLocalFiles][0];
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : files.length;
        const prevIdx = Math.max(currentIdx - 1, 0);
        const prev = files[prevIdx];
        if (prev && prev.name !== '..') {
          setSelectedLocalFiles(prev2 => new Set([prev.name, ...prev2]));
          setLastSelectedLocalIndex(prevIdx);
        }
      } else {
        const files = sortedRemoteFilesRef.current;
        const currentName = [...selectedRemoteFiles][0];
        const currentIdx = currentName ? files.findIndex(f => f.name === currentName) : files.length;
        const prevIdx = Math.max(currentIdx - 1, 0);
        const prev = files[prevIdx];
        if (prev && prev.name !== '..') {
          setSelectedRemoteFiles(prev2 => new Set([prev.name, ...prev2]));
          setLastSelectedRemoteIndex(prevIdx);
        }
      }
    },

    'Escape': () => {
      if (quickLookOpen) setQuickLookOpen(false);
      else if (universalPreviewOpen) closeUniversalPreview();
      else if (showCyberTools) setShowCyberTools(false);
      else if (showShortcutsDialog) setShowShortcutsDialog(false);
      else if (showAboutDialog) setShowAboutDialog(false);
      else if (showSettingsPanel) setShowSettingsPanel(false);
      else if (inputDialog) setInputDialog(null);
      else if (confirmDialog) setConfirmDialog(null);
    }
  }, [showCyberTools, showShortcutsDialog, showAboutDialog, showSettingsPanel, inputDialog, confirmDialog,
    universalPreviewOpen, quickLookOpen, selectedRemoteFiles, selectedLocalFiles, remoteFiles, localFiles,
    activePanel, currentRemotePath, currentLocalPath, isConnected]);


  // Fetch storage quota for a given protocol (call after successful connection/reconnection)
  const fetchStorageQuota = async (protocol?: string) => {
    const version = ++quotaVersionRef.current;

    // InfiniCloud: use REST API for quota (more accurate than WebDAV PROPFIND)
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const opts = connectionParams.options || activeSession?.connectionParams?.options;
    const pid = connectionParams.providerId || activeSession?.providerId;
    if (pid === 'infinicloud' && opts?.apiKey && opts?.infinicloudNode) {
      try {
        const quota = await invoke<{ total: number; used: number; available: number }>('infinicloud_quota', {
          node: opts.infinicloudNode,
          username: connectionParams.username || activeSession?.connectionParams?.username || '',
          password: connectionParams.password || activeSession?.connectionParams?.password || '',
          apiKey: opts.apiKey,
        });
        if (version === quotaVersionRef.current) {
          setStorageQuota({ used: quota.used, total: quota.total, free: quota.available });
        }
      } catch (e) {
        console.warn('[StorageQuota] InfiniCloud quota failed:', e);
        if (version === quotaVersionRef.current) setStorageQuota(null);
      }
      return;
    }

    if (protocol && supportsStorageQuota(protocol as ProviderType)) {
      try {
        const info = await invoke<{ used: number; total: number; free: number }>('provider_storage_info');
        // Discard stale response if a newer fetch was triggered (e.g., session switch)
        if (version === quotaVersionRef.current) {
          setStorageQuota(info);
        }
      } catch (e) {
        console.warn('[StorageQuota] Failed to fetch:', e);
        if (version === quotaVersionRef.current) {
          setStorageQuota(null);
        }
      }
    } else {
      setStorageQuota(null);
    }
  };

  // Check provider capabilities when connected
  useEffect(() => {
    if (isConnected) {
      refreshProviderCaps();
      setLockedFiles(new Map());
    } else {
      setProviderCaps({ versions: false, thumbnails: false, permissions: false, locking: false });
    }
  }, [isConnected, connectionParams.protocol]);

  // Keep-Alive: Send periodic pings to prevent connection timeout
  // FTP uses NOOP, providers use provider_keep_alive
  useEffect(() => {
    if (!isConnected) return;

    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const isProvider = usesProviderApi(protocol);

    const KEEP_ALIVE_INTERVAL = 60000; // 60 seconds

    const keepAliveInterval = setInterval(async () => {
      try {
        if (isProvider) {
          await invoke('provider_keep_alive');
        } else {
          await invoke('ftp_noop');
        }
      } catch (error) {
        if (isProvider) {
          // Provider keep-alive failed - HTTP-based providers are stateless,
          // connection will be re-established on next operation
          console.warn('Provider keep-alive failed (will retry on next operation):', error);
          return;
        }

        console.warn('Keep-alive NOOP failed, attempting reconnect...', error);

        // FTP connection lost - attempt auto-reconnect
        setIsReconnecting(true);
        humanLog.logRaw('activity.disconnect_start', 'DISCONNECT', {}, 'error');
        notify.info(t('toast.reconnecting'), t('toast.connectionLost'));

        try {
          await invoke('reconnect_ftp');
          humanLog.logRaw('activity.reconnect_success', 'CONNECT', { server: connectionParams.server }, 'success');
          notify.success(t('toast.reconnected'), t('toast.ftpRestored'));
          const response = await invoke<{ files: RemoteFile[]; current_path: string }>('list_files');
          setRemoteFiles(response.files);
          setCurrentRemotePath(response.current_path);
          setSelectedRemoteFiles(new Set());
        } catch (reconnectError) {
          console.error('Auto-reconnect failed:', reconnectError);
          humanLog.logRaw('activity.reconnect_error', 'DISCONNECT', {}, 'error');
          notify.error(t('toast.connectionLostTitle'), t('toast.connectionLostManual'));
          setIsConnected(false);
        } finally {
          setIsReconnecting(false);
        }
      }
    }, KEEP_ALIVE_INTERVAL);

    return () => clearInterval(keepAliveInterval);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [isConnected, connectionParams.server, connectionParams.protocol, activeSessionId]);

  // Display file name (strip extension when setting is off)
  const displayName = (name: string, isDir: boolean) => {
    if (isDir || showFileExtensions) return name;
    const lastDot = name.lastIndexOf('.');
    return lastDot > 0 ? name.substring(0, lastDot) : name;
  };

  // Sorting — memoized to avoid recreation on every render (M26)
  const sortFiles = useCallback(<T extends { name: string; size: number | null; modified: string | null; is_dir: boolean }>(files: T[], field: SortField, order: SortOrder): T[] => {
    return [...files].sort((a, b) => {
      if (sortFoldersFirst) {
        if (a.is_dir && !b.is_dir) return -1;
        if (!a.is_dir && b.is_dir) return 1;
      }
      if (a.name === '..') return -1;
      if (b.name === '..') return 1;
      let cmp = 0;
      if (field === 'name') cmp = a.name.toLowerCase().localeCompare(b.name.toLowerCase());
      else if (field === 'size') cmp = (a.size || 0) - (b.size || 0);
      else if (field === 'type') {
        const extA = a.name.includes('.') ? a.name.split('.').pop()!.toLowerCase() : '';
        const extB = b.name.includes('.') ? b.name.split('.').pop()!.toLowerCase() : '';
        cmp = extA.localeCompare(extB);
      }
      else cmp = (a.modified || '').localeCompare(b.modified || '');
      return order === 'asc' ? cmp : -cmp;
    });
  }, [sortFoldersFirst]);

  const sortedRemoteFiles = useMemo(() => {
    let source = remoteSearchResults !== null ? remoteSearchResults : remoteFiles;
    // Client-side live filter when search bar is open (same as local panel)
    if (remoteSearchResults === null && remoteSearchQuery.trim()) {
      const q = remoteSearchQuery.trim().toLowerCase();
      source = source.filter(f => f.name.toLowerCase().includes(q));
    }
    return sortFiles(source, remoteSortField, remoteSortOrder);
  }, [remoteFiles, remoteSearchResults, remoteSearchQuery, remoteSortField, remoteSortOrder, sortFiles]);
  const sortedLocalFiles = useMemo(() => sortFiles(filteredLocalFiles, localSortField, localSortOrder), [filteredLocalFiles, localSortField, localSortOrder, sortFiles]);
  // Keep refs in sync for keyboard navigation (refs are used in useKeyboardShortcuts above)
  sortedLocalFilesRef.current = sortedLocalFiles;
  sortedRemoteFilesRef.current = sortedRemoteFiles;

  const handleRemoteSort = (field: SortField) => {
    if (remoteSortField === field) setRemoteSortOrder(remoteSortOrder === 'asc' ? 'desc' : 'asc');
    else { setRemoteSortField(field); setRemoteSortOrder('asc'); }
  };

  const handleLocalSort = (field: SortField) => {
    if (localSortField === field) setLocalSortOrder(localSortOrder === 'asc' ? 'desc' : 'asc');
    else { setLocalSortField(field); setLocalSortOrder('asc'); }
  };

  // === Local Path Tabs ===
  // Persist tabs to localStorage
  useEffect(() => {
    localStorage.setItem('aerofile_local_tabs', JSON.stringify(localTabs));
  }, [localTabs]);
  useEffect(() => {
    if (activeLocalTabId) localStorage.setItem('aerofile_active_tab', activeLocalTabId);
  }, [activeLocalTabId]);

  const createLocalTab = useCallback(async () => {
    if (localTabs.length >= 12) return;
    const home = await homeDir().catch(() => '/');
    const id = `tab-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
    const label = home.split(/[\\/]/).filter(Boolean).pop() || '/';
    const newTab: LocalTab = { id, path: home, label, scrollTop: 0 };
    setLocalTabs(prev => [...prev, newTab]);
    setActiveLocalTabId(id);
    changeLocalDirectory(home);
  }, [localTabs.length]);

  const switchLocalTab = useCallback((tabId: string) => {
    // Save current tab state
    if (activeLocalTabId) {
      setLocalTabs(prev => prev.map(tab =>
        tab.id === activeLocalTabId ? { ...tab, path: currentLocalPath, label: currentLocalPath.split(/[\\/]/).filter(Boolean).pop() || '/' } : tab
      ));
    }
    // Switch to target tab
    const target = localTabs.find(t => t.id === tabId);
    if (target) {
      setActiveLocalTabId(tabId);
      changeLocalDirectory(target.path);
    }
  }, [activeLocalTabId, currentLocalPath, localTabs]);

  const closeLocalTab = useCallback((tabId: string) => {
    setLocalTabs(prev => {
      const idx = prev.findIndex(t => t.id === tabId);
      const next = prev.filter(t => t.id !== tabId);
      if (tabId === activeLocalTabId && next.length > 0) {
        // Switch to adjacent tab
        const newIdx = Math.min(idx, next.length - 1);
        setActiveLocalTabId(next[newIdx].id);
        changeLocalDirectory(next[newIdx].path);
      } else if (next.length === 0) {
        setActiveLocalTabId(null);
      }
      return next;
    });
  }, [activeLocalTabId]);

  // AeroFile mode: file manager visible and not viewing a remote server
  const isAeroFileMode = !isConnected || !showRemotePanel;
  const isAeroFileVisible = !showConnectionScreen && isAeroFileMode;

  // Command palette items
  const commandPaletteItems: CommandItem[] = useMemo(() => [
    // Navigation
    { id: 'nav-aerofile', label: t('statusBar.aerofile'), category: 'navigation' as CommandCategory, icon: <FolderOpen size={14} />, action: () => { setShowConnectionScreen(false); setActivePanel('local'); }, keywords: ['local', 'file', 'manager'] },
    { id: 'nav-connect', label: t('connection.quickConnect'), category: 'navigation' as CommandCategory, icon: <Globe size={14} />, action: () => setShowConnectionScreen(true), keywords: ['connection', 'server', 'ftp', 'sftp'] },
    { id: 'nav-settings', label: t('settings.title'), category: 'navigation' as CommandCategory, icon: <Settings size={14} />, action: () => setShowSettingsPanel(true), shortcut: 'Ctrl+,', keywords: ['preferences', 'config'] },
    // File
    { id: 'file-newfolder', label: t('contextMenu.newFolder'), category: 'file' as CommandCategory, icon: <FolderPlus size={14} />, action: () => createFolder(activePanel !== 'remote'), keywords: ['create', 'directory', 'mkdir'] },
    { id: 'file-refresh', label: t('contextMenu.refresh'), category: 'file' as CommandCategory, icon: <RefreshCw size={14} />, action: () => { if (activePanel === 'remote') loadRemoteFiles(); else loadLocalFiles(currentLocalPath); }, keywords: ['reload'] },
    // AI
    { id: 'ai-agent', label: 'AeroAgent', category: 'ai' as CommandCategory, icon: <Bot size={14} />, action: () => window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'agent' })), keywords: ['chat', 'assistant', 'ai'] },
    // Tools
    { id: 'tools-editor', label: t('devtools.codeEditor'), category: 'tools' as CommandCategory, icon: <Code size={14} />, action: () => window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'editor' })), keywords: ['code', 'monaco', 'edit'] },
    { id: 'tools-terminal', label: t('devtools.sshTerminal'), category: 'tools' as CommandCategory, icon: <Terminal size={14} />, action: () => window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'terminal' })), keywords: ['ssh', 'shell', 'console'] },
    // Sync
    { id: 'sync-panel', label: t('syncPanel.title'), category: 'sync' as CommandCategory, icon: <FolderSync size={14} />, action: () => setShowSyncPanel(true), keywords: ['synchronize', 'aerosync'] },
  ], [t, activePanel, currentLocalPath]);

  // Sync active tab path when navigating — only in AeroFile mode
  useEffect(() => {
    if (isAeroFileVisible && activeLocalTabId && currentLocalPath) {
      setLocalTabs(prev => prev.map(tab =>
        tab.id === activeLocalTabId
          ? { ...tab, path: currentLocalPath, label: currentLocalPath.split(/[\\/]/).filter(Boolean).pop() || '/' }
          : tab
      ));
    }
  }, [isAeroFileVisible, activeLocalTabId, currentLocalPath]);

  // Auto-manage local tabs when entering AeroFile mode
  const prevAeroFileVisible = useRef(false);
  const tabAutoCreatedForPath = useRef<string | null>(null);
  useEffect(() => {
    const wasVisible = prevAeroFileVisible.current;
    prevAeroFileVisible.current = isAeroFileVisible;
    if (!isAeroFileVisible || !currentLocalPath) return;

    // Case: no tabs exist → create first tab
    if (localTabs.length === 0) {
      // Guard: don't re-create if we just created for this path (prevents loops)
      if (tabAutoCreatedForPath.current === currentLocalPath) return;
      tabAutoCreatedForPath.current = currentLocalPath;
      const id = `tab-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
      const label = currentLocalPath.split(/[\\/]/).filter(Boolean).pop() || '/';
      setLocalTabs([{ id, path: currentLocalPath, label, scrollTop: 0 }]);
      setActiveLocalTabId(id);
      return;
    }
    tabAutoCreatedForPath.current = null;

    // Case: returning from server/connection screen
    if (!wasVisible) {
      // If active tab still exists, reuse it — the sync effect (line 1203)
      // will update its path to currentLocalPath. Creating a new tab here
      // would race with sync and produce duplicates.
      if (activeLocalTabId && localTabs.some(t => t.id === activeLocalTabId)) {
        return;
      }
      // No valid active tab — find matching path or activate first available
      const normalize = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;
      const match = localTabs.find(t => normalize(t.path) === normalize(currentLocalPath));
      setActiveLocalTabId(match ? match.id : localTabs[0].id);
    }
  }, [isAeroFileVisible, currentLocalPath, localTabs.length]); // eslint-disable-line react-hooks/exhaustive-deps

  // Load tags for visible files
  useEffect(() => {
    if (sortedLocalFiles.length > 0) {
      const paths = sortedLocalFiles.map(f => f.path);
      fileTags.loadTagsForFiles(paths);
    }
  }, [sortedLocalFiles, fileTags.loadTagsForFiles]);

  // Quick Look toggle
  const toggleQuickLook = useCallback(() => {
    if (activePanel !== 'local') return;
    if (quickLookOpen) {
      setQuickLookOpen(false);
      return;
    }
    const selected = [...selectedLocalFiles];
    if (selected.length !== 1) return;
    const fileName = selected[0];
    const fileIndex = sortedLocalFiles.findIndex(f => f.name === fileName);
    if (fileIndex === -1) return;
    const file = sortedLocalFiles[fileIndex];
    if (file.is_dir) return;
    setQuickLookIndex(fileIndex);
    setQuickLookOpen(true);
  }, [activePanel, quickLookOpen, selectedLocalFiles, sortedLocalFiles]);

  // Recent paths persistence
  useEffect(() => {
    localStorage.setItem('aerofile_recent_paths', JSON.stringify(recentPaths));
  }, [recentPaths]);

  const addRecentPath = useCallback((path: string) => {
    const normalize = (p: string) => p.replace(/\/+/g, '/').replace(/\/$/, '') || '/';
    const normalized = normalize(path);
    setRecentPaths(prev => {
      const filtered = prev.filter(p => normalize(p) !== normalized);
      return [normalized, ...filtered].slice(0, 20);
    });
  }, []);

  // Trash functions
  const loadTrashItems = useCallback(async () => {
    try {
      const items = await invoke<TrashItem[]>('list_trash_items');
      setTrashItems(items);
    } catch (err) {
      notify.error(t('toast.loadTrashFailed'), String(err));
    }
  }, [notify, t]);

  const handleNavigateTrash = useCallback(() => {
    setIsTrashView(true);
    loadTrashItems();
  }, [loadTrashItems]);

  const handleRestoreTrashItem = useCallback(async (item: TrashItem) => {
    try {
      await invoke('restore_trash_item', { id: item.id, originalPath: item.original_path });
      notify.success(t('trash.restore'), item.name);
      loadTrashItems();
    } catch (err) {
      notify.error(t('toast.restoreFailed'), String(err));
    }
  }, [loadTrashItems, notify, t]);

  const handleEmptyTrash = useCallback(() => {
    // Confirmation to prevent accidental permanent deletion (M36)
    setConfirmDialog({
      message: t('trash.emptyConfirm'),
      onConfirm: async () => {
        setConfirmDialog(null);
        try {
          const count = await invoke<number>('empty_trash');
          notify.success(t('trash.empty'), t('toast.itemsDeleted', { count }));
          setTrashItems([]);
        } catch (err) {
          notify.error(t('toast.emptyTrashFailed'), String(err));
        }
      }
    });
  }, [notify, t]);

  // Folder size calculation
  const calculateFolderSize = useCallback(async (path: string) => {
    if (folderSizeCalculatingRef.current.has(path)) return;
    folderSizeCalculatingRef.current.add(path);
    setFolderSizeCalculating(new Set(folderSizeCalculatingRef.current));
    try {
      const result = await invoke<FolderSizeResult>('calculate_folder_size', { path });
      setFolderSizeCache(prev => {
        const next = new Map(prev);
        next.set(path, result);
        return next;
      });
    } catch (err) {
      notify.error(t('toast.sizeFailed'), String(err));
    } finally {
      folderSizeCalculatingRef.current.delete(path);
      setFolderSizeCalculating(new Set(folderSizeCalculatingRef.current));
    }
  }, [notify, t]);

  // Remote folder size calculation via provider
  const calculateRemoteFolderSize = useCallback(async (path: string) => {
    if (folderSizeCalculatingRef.current.has(path)) return;
    folderSizeCalculatingRef.current.add(path);
    setFolderSizeCalculating(new Set(folderSizeCalculatingRef.current));

    // Listen for progress events
    const unlisten = await listen<{ total_bytes: number; file_count: number; dir_count: number; scanning: boolean }>(
      'folder-size-progress',
      (event) => {
        const p = event.payload;
        // Update cache with partial results during scan
        setFolderSizeCache(prev => {
          const next = new Map(prev);
          next.set(path, { total_bytes: p.total_bytes, file_count: p.file_count, dir_count: p.dir_count });
          return next;
        });
      }
    );

    try {
      await invoke<{ total_bytes: number; file_count: number; dir_count: number }>('provider_calculate_folder_size', { path });
    } catch (err) {
      notify.error(t('toast.sizeFailed'), String(err));
    } finally {
      unlisten();
      folderSizeCalculatingRef.current.delete(path);
      setFolderSizeCalculating(new Set(folderSizeCalculatingRef.current));
    }
  }, [notify, t]);

  // Auto-start folder size calculation when properties dialog opens on a folder
  useEffect(() => {
    if (!propertiesDialog || !propertiesDialog.is_dir) return;
    const p = propertiesDialog.path;
    // Skip if already cached or calculating
    if (folderSizeCache.has(p) || folderSizeCalculatingRef.current.has(p)) return;
    if (propertiesDialog.isRemote) {
      calculateRemoteFolderSize(p);
    } else {
      calculateFolderSize(p);
    }
  }, [propertiesDialog?.path, propertiesDialog?.is_dir]); // eslint-disable-line react-hooks/exhaustive-deps

  // Stuck detection moved to TransferToastContainer (isolated from App re-renders)

  // Update download progress listener
  useEffect(() => {
    const unlisten = listen<{
      downloaded: number; total: number; percentage: number;
      speed_bps: number; eta_seconds: number; filename: string;
    }>('update-download-progress', (event) => {
      const p = event.payload;
      setUpdateDownload(prev => ({
        ...(prev || { downloading: true, error: undefined, completedPath: undefined }),
        ...p,
        downloading: p.percentage < 100,
        completedPath: p.percentage >= 100 ? prev?.completedPath : undefined,
      }));
    });
    
    // Listen for phase updates during install_deb/etc
    const unlistenPhase = listen<string>('update_install_phase', (event) => {
      setUpdateDownload(prev => prev ? { 
        ...prev, 
        installPhase: event.payload as 'auth' | 'running' | 'restart',
        installing: true 
      } : null);
    });
    
    return () => { 
      unlisten.then(fn => fn()); 
      unlistenPhase.then(fn => fn());
    };
  }, []);

  // 2.4 Post-Restart Confirmation
  useEffect(() => {
    const checkPostUpdateMarker = async () => {
      try {
        const markerJson = await invoke<string | null>('read_update_marker');
        if (markerJson) {
           const data = JSON.parse(markerJson);
           // Show green success toast (5s) — current version IS the updated version after restart
           const currentVersion = await getVersion().catch(() => '');
           toast.addToast('success',
             t('ui.updateSuccess'),
             `AeroFTP v${currentVersion}`,
             5000
           );

           activityLog.log('INFO',
               `Post-restart check: Update completed via .${data.install_format}${data.verified ? ' (Verified)' : ''}`,
               'success'
           );

           await invoke('clear_update_marker');
        }
      } catch (err) {
        console.error('Failed to read update marker', err);
      }
    };
    checkPostUpdateMarker();
  }, [t, activityLog, toast]);

  // Start update download
  const startUpdateDownload = useCallback(async () => {
    if (!updateAvailable?.download_url) return;
    setUpdateDownload({
      downloading: true, percentage: 0, speed_bps: 0, eta_seconds: 0,
      filename: '', downloaded: 0, total: 0,
    });
    try {
      const resp = await invoke<{ path: string; verification: UpdateVerificationInfo }>('download_update', { url: updateAvailable.download_url });
      setUpdateDownload(prev => prev ? { ...prev, downloading: false, completedPath: resp.path, verification: resp.verification } : null);
      activityLog.log('INFO', `Update downloaded: ${resp.path} (Mode: ${resp.verification.mode})`, 'success');
    } catch (error) {
      setUpdateDownload(prev => prev ? { ...prev, downloading: false, error: String(error) } : null);
      activityLog.log('ERROR', `Update download failed: ${error}`, 'error');
    }
  }, [updateAvailable, activityLog]);

  // A6-01: Keep refs in sync for menu-event listener (avoids stale closures)
  useEffect(() => { isConnectedRef.current = isConnected; }, [isConnected]);
  useEffect(() => { currentLocalPathRef.current = currentLocalPath; }, [currentLocalPath]);
  useEffect(() => { themeRef.current = theme; }, [theme]);
  useEffect(() => { debugModeRef.current = debugMode; }, [debugMode]);

  // Menu events from native menu — registered once, reads from refs to avoid stale closures
  useEffect(() => {
    const unlisten = listen<string>('menu-event', (event) => {
      const id = event.payload;
      switch (id) {
        case 'about': setShowAboutDialog(true); break;
        case 'support': setShowSupportDialog(true); break;
        case 'shortcuts': setShowShortcutsDialog(true); break;
        case 'settings': setShowSettingsPanel(true); break;
        case 'refresh':
          if (isConnectedRef.current) loadRemoteFiles();
          loadLocalFiles(currentLocalPathRef.current);
          break;
        case 'toggle_theme': {
          const order: Theme[] = ['light', 'dark', 'tokyo', 'cyber', 'auto'];
          const nextTheme = order[(order.indexOf(themeRef.current) + 1) % order.length];
          setTheme(nextTheme);
          // Icon theme auto-syncs via useEffect above
          break;
        }
        case 'new_folder':
          if (isConnectedRef.current) createFolder(true);
          break;
        case 'toggle_devtools':
          setDevToolsOpen(prev => !prev);
          break;
        case 'toggle_debug_mode':
          // L53: In production, debug mode is only auto-enabled by Cyber theme
          if (import.meta.env.DEV) setDebugMode(prev => !prev);
          break;
        case 'show_dependencies':
          setShowDependenciesPanel(true);
          break;
        case 'toggle_editor':
        case 'toggle_terminal':
        case 'toggle_agent':
          // Emit event for DevToolsV2 to handle
          window.dispatchEvent(new CustomEvent('devtools-panel-toggle', { detail: id.replace('toggle_', '') }));
          break;
        case 'toggle_aerofile':
          // Dispatch to StatusBar's AeroFile button handler
          window.dispatchEvent(new CustomEvent('toggle-aerofile'));
          break;
        case 'check_update':
          checkForUpdate(true);
          break;
        case 'quit':
          // Will be handled by Tauri
          break;
      }
    });
    return () => { unlisten.then(fn => fn()); };
  // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  // AeroAgent app tool events — theme switching + sync control
  useEffect(() => {
    const unlistenTheme = listen<{ theme: string }>('ai-set-theme', (event) => {
      const t = event.payload.theme;
      if (['light', 'dark', 'tokyo', 'cyber'].includes(t)) {
        setTheme(t as Theme);
      }
    });
    const unlistenSync = listen<{ action: string }>('ai-sync-control', (event) => {
      const action = event.payload.action;
      if (action === 'start') {
        invoke('start_background_sync').catch(() => { });
      } else if (action === 'stop') {
        invoke('stop_background_sync').catch(() => { });
      }
    });
    return () => {
      unlistenTheme.then(fn => fn());
      unlistenSync.then(fn => fn());
    };
  }, []);

  // OS file association: listen for .aerovault files opened via double-click or single-instance forwarding
  useEffect(() => {
    const unlisten = listen<string>('vault-open-file', (event) => {
      const vaultPath = event.payload;
      if (vaultPath && vaultPath.endsWith('.aerovault')) {
        setShowVaultPanel({ mode: 'open', path: vaultPath });
      }
    });
    return () => { unlisten.then(fn => fn()); };
  }, []);

  // Auto-enable debug mode when Cyber theme is active, disable when switching away
  useEffect(() => {
    const isCyber = getEffectiveTheme(theme, isDark) === 'cyber';
    setDebugMode(isCyber);
  }, [theme, isDark]);


  // File loading (race-condition safe: stale responses are discarded)
  const loadLocalFiles = useCallback(async (path: string): Promise<boolean> => {
    const callId = ++loadLocalCallIdRef.current;
    try {
      const files: LocalFile[] = await invoke('get_local_files', { path, showHidden: showHiddenFiles });
      // Discard stale response if a newer call was issued while awaiting
      if (callId !== loadLocalCallIdRef.current) return false;
      setLocalFiles(files);
      setCurrentLocalPath(path);
      setSelectedLocalFiles(new Set());
      return true;
    } catch (error) {
      if (callId !== loadLocalCallIdRef.current) return false;
      notify.error(t('common.error'), `Failed to list local files: ${error}`);
      return false;
    }
  }, [showHiddenFiles, notify, t]);

  const loadRemoteFiles = async (overrideProtocol?: string, silent?: boolean): Promise<FileListResponse | null> => {
    try {
      // Check if we're connected to a Provider (OAuth, S3, WebDAV)
      // Use override protocol if provided, then connectionParams, then active session (most robust)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = (overrideProtocol || connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
      const isProvider = usesProviderApi(protocol);
      logger.debug('[loadRemoteFiles] protocol:', protocol, 'isProvider:', isProvider, 'override:', overrideProtocol);

      let response: FileListResponse;
      if (isProvider) {
        // Use provider API
        logger.debug('[loadRemoteFiles] Calling provider_list_files...');
        response = await invoke('provider_list_files', { path: null });
        logger.debug('[loadRemoteFiles] Provider response:', {
          fileCount: response.files?.length ?? 0,
          currentPath: response.current_path,
          files: response.files?.slice(0, 5) // Log first 5 files
        });

        // Log to activity only on explicit loads (not after mutations like rename/delete)
        if (!silent) {
          if (response.files?.length > 0) {
            humanLog.logRaw('activity.loaded_items', 'INFO', { count: response.files.length, provider: protocol || 'unknown' }, 'success');
          } else {
            activityLog.log('INFO', `No files returned from ${protocol} provider`, 'running');
          }
        }
      } else {
        // Use FTP API
        response = await invoke('list_files');
      }
      setRemoteFiles(response.files);
      setCurrentRemotePath(response.current_path);
      setSelectedRemoteFiles(new Set());
      return response;
    } catch (error) {
      console.error('[loadRemoteFiles] Error:', error);
      activityLog.log('ERROR', `Failed to list files: ${error}`, 'error');
      notify.error(t('common.error'), `Failed to list files: ${error}`);
      return null;
    }
  };

  // Transfer events (backend progress updates) - handles downloads, uploads, and deletes
  const { pendingFileLogIds, pendingDeleteLogIds } = useTransferEvents({
    t, activityLog, humanLog, transferQueue, notify,
    setActiveTransfer, loadRemoteFiles, loadLocalFiles, currentLocalPath,
    currentRemotePath,
    onTransferStart: () => {
      if (!showActivityLog) setShowActivityLog(true);
    },
    onScanningUpdate: setScanningState,
    maxChannels: TRANSFER_SPEED_PRESETS[sessionTransferSpeedPreset].channels,
  });

  // AeroFile toggle — shared between StatusBar, IntroHub header, and View > AeroFile menu
  const handleToggleAeroFile = useCallback(async () => {
    if (showConnectionScreen) {
      setShowConnectionScreen(false);
      setActivePanel('local');
      setShowSidebar(true);
      await loadLocalFiles(currentLocalPath || '/');
    } else if (isConnected) {
      setShowRemotePanel(prev => {
        if (prev) { setActivePanel('local'); setShowSidebar(true); }
        else { setShowLocalPreview(false); }
        return !prev;
      });
    } else {
      toggleSidebar();
    }
  }, [showConnectionScreen, isConnected, currentLocalPath, loadLocalFiles, toggleSidebar]);
  handleToggleAeroFileRef.current = handleToggleAeroFile;

  // Listen for View > AeroFile menu event
  useEffect(() => {
    const handler = () => { handleToggleAeroFileRef.current(); };
    window.addEventListener('toggle-aerofile', handler);
    return () => window.removeEventListener('toggle-aerofile', handler);
  }, []);

  const handleRemoteSearch = async (query: string) => {
    if (!query.trim()) {
      setRemoteSearchResults(null);
      return;
    }
    setRemoteSearching(true);
    try {
      const results = await invoke<RemoteFile[]>('provider_find', {
        path: currentRemotePath || '/',
        pattern: `*${query.trim()}*`,
      });
      setRemoteSearchResults(results);
    } catch {
      setRemoteSearchResults([]);
    } finally {
      setRemoteSearching(false);
    }
  };

  // Check provider capabilities after connection (for context menu features)
  const refreshProviderCaps = async () => {
    const protocol = connectionParams.protocol;
    if (!protocol || ['ftp', 'ftps', 'sftp'].includes(protocol)) {
      // FTP/SFTP don't use provider_* capability commands
      setProviderCaps({ versions: false, thumbnails: false, permissions: false, locking: protocol === 'webdav' });
      return;
    }
    try {
      const [versions, thumbnails, permissions, locking] = await Promise.all([
        invoke<boolean>('provider_supports_versions').catch(() => false),
        invoke<boolean>('provider_supports_thumbnails').catch(() => false),
        invoke<boolean>('provider_supports_permissions').catch(() => false),
        invoke<boolean>('provider_supports_locking').catch(() => false),
      ]);
      setProviderCaps({ versions, thumbnails, permissions, locking });
    } catch {
      setProviderCaps({ versions: false, thumbnails: false, permissions: false, locking: false });
    }
  };

  useEffect(() => {
    // Guard: only run once — loadLocalFiles deps (notify, t) may change after
    // initial render (e.g. translations loading), which would re-trigger this
    // effect and overwrite currentLocalPath after the user has already navigated.
    if (localFilesInitStarted.current) return;
    localFilesInitStarted.current = true;
    (async () => {
      // Check for saved settings with defaultLocalPath or last visited folder
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>('app_settings', SETTINGS_KEY);
        if (parsed) {

          // If rememberLastFolder is enabled, try to load last visited folder
          if (parsed.rememberLastFolder === true && typeof parsed.lastLocalPath === 'string') {
            try {
              await loadLocalFiles(parsed.lastLocalPath);
              return;
            } catch { /* Fall through to next option */ }
          }

          // Try defaultLocalPath from settings
          if (typeof parsed.defaultLocalPath === 'string' && parsed.defaultLocalPath.length > 0) {
            try {
              await loadLocalFiles(parsed.defaultLocalPath);
              return;
            } catch { /* Fall through to next option */ }
          }
        }
      } catch { /* Fall through to default */ }

      // Default: try home directory, then downloads
      try { await loadLocalFiles(await homeDir()); }
      catch { try { await loadLocalFiles(await downloadDir()); } catch { } }
    })().finally(() => {
      localFilesInitDone.current = true;
      signalAppReady();
    });
  }, [loadLocalFiles, signalAppReady]);

  // Reload local files when showHiddenFiles setting changes
  const isFirstRender = React.useRef(true);
  useEffect(() => {
    if (isFirstRender.current) {
      isFirstRender.current = false;
      return;
    }
    // Reload current local directory with new hidden files setting
    if (currentLocalPath) {
      loadLocalFiles(currentLocalPath);
    }
  }, [showHiddenFiles]); // eslint-disable-line react-hooks/exhaustive-deps

  // Drag & Drop (cross-panel callback set below after upload/download are defined)
  const crossPanelDropRef = React.useRef<((files: { name: string; path: string }[], fromRemote: boolean, targetDir: string) => Promise<void>) | null>(null);

  const {
    dragData, dropTargetPath, crossPanelTarget,
    handleDragStart, handleDragOver, handleDrop, handleDragEnd, handleDragLeave,
    handlePanelDragOver, handlePanelDrop, handlePanelDragLeave,
  } = useDragAndDrop({
    notify,
    humanLog,
    currentRemotePath,
    currentLocalPath,
    loadRemoteFiles,
    loadLocalFiles,
    activeSessionId,
    sessions: sessions as Array<{ id: string; connectionParams?: { protocol?: string } }>,
    connectionParams,
    onCrossPanelDrop: async (files, fromRemote, targetDir) => {
      await crossPanelDropRef.current?.(files, fromRemote, targetDir);
    },
  });

  // --- Connection step logging helpers ---
  const CLOUD_API_PROTOCOLS = ['mega', 'googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'fourshared', 'filen', 'internxt', 'kdrive', 'jottacloud', 'drime', 'zohoworkdrive', 'azure', 'filelu', 'koofr', 'opendrive', 'yandexdisk', 'github', 'gitlab'];
  // Providers that support server-side copy (for context menu)
  const SERVER_COPY_PROVIDERS = ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 's3', 'webdav', 'zohoworkdrive', 'mega', 'kdrive', 'jottacloud', 'drime', 'koofr', 'yandexdisk'];

  const getProviderHostFallback = (protocol?: string, username?: string): string => {
    switch (protocol) {
      case 'azure':
        return username ? `${username}.blob.core.windows.net` : 'blob.core.windows.net';
      case 'internxt':
        return 'gateway.internxt.com';
      case 'kdrive':
        return 'api.infomaniak.com';
      case 'jottacloud':
        return 'jfs.jottacloud.com';
      case 'drime':
        return 'app.drime.cloud';
      case 'mega':
        return 'mega.nz';
      case 'filen':
        return 'filen.io';
      case 'filelu':
        return 'filelu.com';
      case 'koofr':
        return 'app.koofr.net';
      case 'opendrive':
        return 'dev.opendrive.com';
      case 'yandexdisk':
        return 'cloud-api.yandex.net';
      case 'github':
        return 'api.github.com';
      default:
        return 'localhost';
    }
  };

  const normalizeProviderConnectionParams = (params: ConnectionParams): ConnectionParams => {
    const protocol = params.protocol;
    if (protocol === 'filelu') {
      return {
        ...params,
        server: params.server || 'filelu.com',
        port: params.port || 443,
        username: params.username || 'api-key',
      };
    }
    if (protocol === 'koofr') {
      return {
        ...params,
        server: params.server || 'app.koofr.net',
        port: params.port || 443,
      };
    }
    if (protocol === 'opendrive') {
      return {
        ...params,
        server: params.server || 'dev.opendrive.com',
        port: params.port || 443,
      };
    }
    if (protocol === 'yandexdisk') {
      return {
        ...params,
        server: params.server || 'cloud-api.yandex.net',
        port: params.port || 443,
      };
    }
    if (protocol === 'github') {
      return {
        ...params,
        server: params.server || '',
        port: params.port || 443,
        username: params.username || 'token',
      };
    }
    if (protocol === 'swift') {
      return {
        ...params,
        server: params.server || 'https://authenticate.blomp.com',
        port: params.port || 443,
      };
    }
    if (protocol === 'immich') {
      return {
        ...params,
        port: params.port || 443,
        username: params.username || 'api-key',
      };
    }
    return params;
  };

  const getActiveProviderProtocol = (): ProviderType | undefined => {
    const activeSession = sessions.find(s => s.id === activeSessionId);
    return (connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
  };

  const activeTransferProtocol = getActiveProviderProtocol();
  const supportsParallelTransferPresets = !!activeTransferProtocol && isFtpProtocol(activeTransferProtocol);
  const transferPresetLabelMap: Record<TransferSpeedPreset, string> = {
    base: t('transfer.modeSafe'),
    fast: t('transfer.modeBalanced'),
    super: t('transfer.modeMax'),
  };
  const effectiveTransferSpeedPreset: TransferSpeedPreset = supportsParallelTransferPresets
    ? sessionTransferSpeedPreset
    : 'base';
  const effectiveMaxConcurrentTransfers = supportsParallelTransferPresets
    ? TRANSFER_SPEED_PRESETS[effectiveTransferSpeedPreset].channels
    : 1;
  const effectiveTransferSpeedLabel = `${transferPresetLabelMap[effectiveTransferSpeedPreset]} ${TRANSFER_SPEED_PRESETS[effectiveTransferSpeedPreset].channels}x`;

  const cycleTransferSpeedPreset = useCallback(() => {
    if (!supportsParallelTransferPresets) {
      return;
    }

    setSessionTransferSpeedPreset(current => {
      if (current === 'base') return 'fast';
      if (current === 'fast') return 'super';
      return 'base';
    });
  }, [supportsParallelTransferPresets]);

  const buildProviderParams = async (params: ConnectionParams, initialPath: string | null) => {
    let effectiveParams = normalizeProviderConnectionParams(params);

    // GitHub OAuth mode: clear stale saved password, use held token from Device Flow.
    // On app restart (held empty), reload OAuth token from vault (saved under 'github_oauth_token').
    if (effectiveParams.protocol === 'github' &&
        effectiveParams.options?.githubAuthMode === 'authorize') {
      effectiveParams = { ...effectiveParams, password: '' };
      await invoke('github_load_oauth_token').catch(() => {});
    }

    // GitHub PAT mode: load PAT from vault if password is empty (reconnect scenario)
    if (effectiveParams.protocol === 'github' &&
        effectiveParams.options?.githubAuthMode === 'pat' &&
        !effectiveParams.password) {
      await invoke('github_get_pat').catch(() => {});
    }

    if (effectiveParams.protocol === 'github' && effectiveParams.options?.githubAuthMode === 'app') {
      // App mode: token always comes from PEM/vault, never from saved password
      effectiveParams = { ...effectiveParams, password: '', options: effectiveParams.options || {} };
      const opts = effectiveParams.options!;
      const appId = opts.githubAppId?.trim();
      const installationId = opts.githubInstallationId?.trim();
      const pemPath = opts.githubPemPath?.trim();
      const pemStored = opts.githubPemStored === true;
      // App mode: always refresh the installation token from PEM/vault.
      // Installation tokens are short-lived (1h) and the held token does not
      // survive app restarts, so we always generate a fresh one on connect.
      {
        if (!appId || !installationId) {
          throw new Error('GitHub App mode requires App ID and Installation ID to refresh the installation token');
        }

        // SEC-GH-001: Installation token is held backend-side and never returned via IPC.
        // The backend stores the token in ProviderState.held_github_app_token,
        // and provider_connect consumes it automatically for GitHub protocol.
        let tokenExpiresAt: string;

        // Try vault first (most common path after initial import), then disk, then vault fallback
        const hasVaultPem = await invoke<boolean>('github_has_vault_pem', { appId, installationId }).catch(() => false);

        if (pemStored || hasVaultPem) {
          // PEM in vault — preferred path, no file on disk needed
          const resp = await invoke<{ success: boolean; expires_at: string }>('github_app_token_from_vault', {
            appId,
            installationId,
          });
          tokenExpiresAt = resp.expires_at;
        } else if (pemPath) {
          // PEM not in vault — try reading from disk (first import or vault lost)
          const resp = await invoke<{ success: boolean; expires_at: string }>('github_app_token_from_pem', {
            pemPath,
            appId,
            installationId,
          });
          tokenExpiresAt = resp.expires_at;
        } else {
          throw new Error('No PEM key found. Import a .pem file first or check your App ID and Installation ID.');
        }

        // Password left empty — backend will inject the held token during connect
        effectiveParams = {
          ...effectiveParams,
          password: '',
          options: {
            ...effectiveParams.options,
            githubPemPath: pemPath,
            githubPemStored: true,
            githubTokenExpiresAt: tokenExpiresAt,
          },
        };
      }
    }

    // InfiniCloud: auto-discover node server via REST API when API key is provided
    if (effectiveParams.providerId === 'infinicloud' && effectiveParams.options?.apiKey && !effectiveParams.server) {
      try {
        const discovery = await invoke<{
          node: string;
          webdav_url: string;
          capacity: number;
          user: string;
          introduce_code: string;
        }>('infinicloud_discover', {
          username: effectiveParams.username || '',
          password: effectiveParams.password || '',
          apiKey: effectiveParams.options.apiKey,
        });
        effectiveParams = {
          ...effectiveParams,
          server: discovery.webdav_url,
          options: {
            ...effectiveParams.options,
            infinicloudNode: discovery.node,
            infinicloudCapacityGb: discovery.capacity,
            infinicloudIntroduceCode: discovery.introduce_code,
          },
        };
      } catch (e) {
        throw new Error(`InfiniCloud discovery failed: ${e}`);
      }
    }

    const protocol = effectiveParams.protocol;
    const providerParams = {
      protocol,
      server: effectiveParams.server,
      port: effectiveParams.port,
      username: effectiveParams.username,
      password: effectiveParams.password,
      initial_path: initialPath,
      bucket: effectiveParams.options?.bucket,
      region: effectiveParams.options?.region || (effectiveParams.providerId === 'filelu-s3' ? 'global' : 'us-east-1'),
      endpoint: effectiveParams.options?.endpoint || resolveS3Endpoint(effectiveParams.providerId, effectiveParams.options?.region as string) || (protocol === 's3' && effectiveParams.server && !effectiveParams.server.includes('amazonaws.com') ? effectiveParams.server : null),
      path_style: effectiveParams.options?.pathStyle || false,
      storage_class: effectiveParams.options?.storage_class || null,
      sse_mode: effectiveParams.options?.sse_mode || null,
      sse_kms_key_id: effectiveParams.options?.sse_kms_key_id || null,
      save_session: effectiveParams.options?.save_session,
      mega_mode: effectiveParams.options?.mega_mode || null,
      session_expires_at: effectiveParams.options?.session_expires_at,
      logout_on_disconnect: effectiveParams.options?.logout_on_disconnect || false,
      private_key_path: effectiveParams.options?.private_key_path || null,
      key_passphrase: effectiveParams.options?.key_passphrase || null,
      timeout: effectiveParams.options?.timeout || 30,
      tls_mode: effectiveParams.options?.tlsMode || (protocol === 'ftps' ? 'implicit' : protocol === 'ftp' ? 'explicit' : undefined),
      verify_cert: effectiveParams.options?.verifyCert !== undefined ? effectiveParams.options.verifyCert : true,
      two_factor_code: effectiveParams.options?.two_factor_code || null,
      github_auth_mode: effectiveParams.options?.githubAuthMode || null,
      github_app_id: effectiveParams.options?.githubAppId || null,
      github_installation_id: effectiveParams.options?.githubInstallationId || null,
      github_pem_path: effectiveParams.options?.githubPemPath || null,
      github_token_expires_at: effectiveParams.options?.githubTokenExpiresAt || null,
      github_branch: effectiveParams.options?.githubBranch || null,
    };

    return { effectiveParams, providerParams };
  };

  const refreshGitHubContext = useCallback(async (refreshBranches = false) => {
    const protocol = getActiveProviderProtocol();
    const isGitForge = protocol === 'github' || protocol === 'gitlab';
    if (!isConnected || !isGitForge) {
      setGitHubRepoInfo(null);
      setGitHubBranches([]);
      return;
    }

    try {
      // Use protocol-specific commands but same state shape
      const infoCommand = protocol === 'gitlab' ? 'gitlab_get_info' : 'github_get_info';
      const info = await invoke<{
        owner: string;
        repo: string;
        branch: string;
        writeMode: string;
        writeModeKind: 'direct' | 'branch' | 'readonly' | 'unknown';
        workingBranch: string | null;
        repoPrivate: boolean;
      }>(infoCommand);
      setGitHubRepoInfo(info);

      // GitHub-only: Pages and Actions checks
      if (protocol === 'github') {
        invoke<unknown>('github_get_pages')
          .then(result => setHasGitHubPages(result !== null))
          .catch(() => setHasGitHubPages(false));

        invoke<Array<{ status: string }>>('github_list_actions_runs', { perPage: 5 })
          .then(runs => setHasActiveGitHubActions(runs.some(r => r.status === 'in_progress' || r.status === 'queued')))
          .catch(() => setHasActiveGitHubActions(false));
      } else {
        setHasGitHubPages(false);
        setHasActiveGitHubActions(false);
      }

      if (refreshBranches) {
        const branchCommand = protocol === 'gitlab' ? 'gitlab_list_branches' : 'github_list_branches';
        const branches = await invoke<Array<{ name: string; protected: boolean }>>(branchCommand);
        setGitHubBranches(branches);
      }
    } catch (error) {
      console.warn(`Failed to refresh ${protocol} context:`, error);
      setGitHubRepoInfo(null);
      setGitHubBranches([]);
      setHasGitHubPages(false);
      setHasActiveGitHubActions(false);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId, connectionParams.protocol, isConnected]);

  // Poll GitHub Actions status every 60s when connected to GitHub
  useEffect(() => {
    if (!isConnected || getActiveProviderProtocol() !== 'github') return;
    const interval = setInterval(() => {
      invoke<Array<{ status: string }>>('github_list_actions_runs', { perPage: 5 })
        .then(runs => setHasActiveGitHubActions(runs.some(r => r.status === 'in_progress' || r.status === 'queued')))
        .catch(() => {});
    }, 60_000);
    return () => clearInterval(interval);
  }, [isConnected, getActiveProviderProtocol]);

  const switchGitHubBranch = useCallback(async (branch: string) => {
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const baseParams = normalizeProviderConnectionParams((connectionParams.protocol ? connectionParams : activeSession?.connectionParams || connectionParams));
    const nextParams: ConnectionParams = {
      ...baseParams,
      options: {
        ...baseParams.options,
        githubBranch: branch,
      },
    };

    setLoading(true);
    try {
      const { effectiveParams, providerParams } = await buildProviderParams(nextParams, currentRemotePath || null);
      await invoke('provider_disconnect').catch(() => {});
      await invoke('provider_connect', { params: providerParams });

      let response: FileListResponse;
      try {
        response = await invoke<FileListResponse>('provider_list_files', { path: currentRemotePath || null });
      } catch {
        response = await invoke<FileListResponse>('provider_list_files', { path: null });
      }

      setConnectionParams(effectiveParams);
      setRemoteFiles(response.files);
      setCurrentRemotePath(response.current_path);
      setSelectedRemoteFiles(new Set());
      if (activeSessionId) {
        setSessions(prev => prev.map(session =>
          session.id === activeSessionId
            ? {
                ...session,
                connectionParams: effectiveParams,
                remoteFiles: response.files,
                remotePath: response.current_path,
              }
            : session
        ));
      }
      await refreshGitHubContext(true);
      notify.success('GitHub', t('github.branchSwitched', { branch }));
    } catch (error) {
      notify.error(t('common.error'), String(error));
    } finally {
      setLoading(false);
    }
  }, [activeSessionId, connectionParams, currentRemotePath, notify, refreshGitHubContext, sessions, t]);

  const requestGitHubBatchCommitMessage = useCallback((
    files: { local: string; remote: string }[],
    operation: 'upload' | 'delete',
  ): Promise<string | null> => {
    if (!gitHubRepoInfo || gitHubRepoInfo.writeModeKind === 'unknown') {
      return Promise.resolve(null);
    }

    const writeMode = gitHubRepoInfo.writeModeKind as 'direct' | 'branch' | 'readonly';

    return new Promise((resolve) => {
      setGitHubCommitDialog({
        files,
        operation,
        branch: gitHubRepoInfo.branch,
        writeMode,
        workingBranch: gitHubRepoInfo.workingBranch || undefined,
        onCommit: (message: string) => {
          setGitHubCommitDialog(null);
          resolve(message);
        },
        onCancel: () => {
          setGitHubCommitDialog(null);
          resolve(null);
        },
      });
    });
  }, [gitHubRepoInfo]);

  useEffect(() => {
    const protocol = getActiveProviderProtocol();
    if (!isConnected || (protocol !== 'github' && protocol !== 'gitlab')) {
      setGitHubRepoInfo(null);
      setGitHubBranches([]);
      return;
    }

    void refreshGitHubContext(true);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [activeSessionId, connectionParams.protocol, isConnected, refreshGitHubContext]);

  const logConnectionSteps = async (
    server: string,
    port: number,
    protocol: string,
  ): Promise<{ resolvedIp: string | null; connectingLogId: string | null }> => {
    if (!server || CLOUD_API_PROTOCOLS.includes(protocol)) return { resolvedIp: null, connectingLogId: null };
    // Extract pure hostname for DNS resolution
    // Full URLs: "https://webdav.cloudme.com/path/" → "webdav.cloudme.com"
    // Path-style: "axpnas.ddns.net/axpdev/dav" → "axpnas.ddns.net"
    let hostname = server;
    try { hostname = new URL(server).hostname; } catch {
      // Not a full URL — strip path and port for DNS lookup
      const slashIdx = hostname.indexOf('/');
      if (slashIdx > 0) hostname = hostname.substring(0, slashIdx);
      const colonIdx = hostname.indexOf(':');
      if (colonIdx > 0) hostname = hostname.substring(0, colonIdx);
    }
    const dnsLogId = humanLog.logRaw('activity.dns_resolving', 'INFO', { hostname }, 'running');
    let resolvedIp: string | null = null;
    try {
      resolvedIp = await invoke<string>('resolve_hostname', { hostname, port });
      humanLog.updateEntry(dnsLogId, { status: 'success', message: t('activity.dns_resolved', { hostname, ip: resolvedIp }) });
    } catch (e) {
      humanLog.updateEntry(dnsLogId, { status: 'error', message: t('activity.dns_failed', { hostname, error: String(e) }) });
    }
    const connectingLogId = humanLog.logRaw('activity.connecting_to', 'CONNECT',
      { ip: resolvedIp || hostname, port: String(port) }, 'running');
    return { resolvedIp, connectingLogId };
  };

  const logConnectionSuccess = (
    protocol: string,
    username: string,
    options?: { tlsMode?: string; private_key_path?: string }
  ) => {
    if (protocol === 'sftp') {
      humanLog.logRaw('activity.ssh_established', 'CONNECT', {}, 'success');
    } else if (protocol === 'ftps') {
      const mode = options?.tlsMode || 'explicit';
      humanLog.logRaw('activity.tls_established', 'CONNECT', { mode }, 'success');
    } else if (['webdav', 's3', 'filen'].includes(protocol)) {
      humanLog.logRaw('activity.https_established', 'CONNECT', {}, 'success');
    }
    if (protocol === 'sftp' && options?.private_key_path) {
      humanLog.logRaw('activity.auth_method_key', 'CONNECT', { username: maskCredential(username) }, 'success');
    } else if (username) {
      humanLog.logRaw('activity.auth_success', 'CONNECT', { username: maskCredential(username) }, 'success');
    }
  };

  const logListingComplete = (path: string, count: number) => {
    humanLog.logRaw('activity.listing_complete', 'INFO', { path, count: String(count) }, 'success');
  };

  // SEC-P1-06: TOFU host key check — returns true if key is accepted or already known
  const checkSftpHostKey = async (host: string, port: number): Promise<boolean> => {
    try {
      const info = await invoke<HostKeyInfo>('sftp_check_host_key', { host, port });
      if (info.status === 'known') return true;
      if (info.status === 'error') {
        notify.error('Host key error', `Could not verify host key for ${host}:${port}`);
        return false;
      }
      // Show TOFU or key-changed dialog
      return new Promise<boolean>((resolve) => {
        setHostKeyDialog({ visible: true, info, host, port, resolve });
      });
    } catch (error) {
      notify.error('Host key check failed', String(error));
      return false;
    }
  };

  const handleHostKeyAccept = async () => {
    const { info, host, port, resolve } = hostKeyDialog;
    try {
      if (info?.status === 'changed' && info.changed_line !== undefined) {
        await invoke('sftp_remove_host_key', { host, port, line: info.changed_line });
      }
      await invoke('sftp_accept_host_key', { host, port });
      resolve?.(true);
    } catch (error) {
      notify.error('Failed to save host key', String(error));
      resolve?.(false);
    }
    setHostKeyDialog({ visible: false, info: null, host: '', port: 22, resolve: null });
  };

  const handleHostKeyReject = () => {
    hostKeyDialog.resolve?.(false);
    setHostKeyDialog({ visible: false, info: null, host: '', port: 22, resolve: null });
  };

  // FTP operations
  const connectToFtp = async (overrideParams?: ConnectionParams) => {
    // Parse host:port from server field if user entered it inline
    // Use a local copy to avoid direct React state mutation (C3 audit fix)
    // overrideParams: used by IntroHub form tabs to pass params directly (avoids stale React state)
    let effectiveParams = overrideParams || connectionParams;
    if (connectionParams.server && connectionParams.server.includes(':')) {
      const lastColon = connectionParams.server.lastIndexOf(':');
      const possiblePort = connectionParams.server.substring(lastColon + 1);
      const parsedPort = parseInt(possiblePort, 10);
      if (parsedPort > 0 && parsedPort <= 65535 && String(parsedPort) === possiblePort) {
        const cleanHost = connectionParams.server.substring(0, lastColon);
        effectiveParams = { ...connectionParams, server: cleanHost, port: parsedPort };
        setConnectionParams(prev => ({ ...prev, server: cleanHost, port: parsedPort }));
      }
    }

    effectiveParams = normalizeProviderConnectionParams(effectiveParams);

    // OAuth providers don't need server/username validation - they're already connected
    const protocol = effectiveParams.protocol;
    logger.debug('[connectToFtp] effectiveParams:', effectiveParams);
    logger.debug('[connectToFtp] protocol:', protocol);
    const isOAuth = !!protocol && (isOAuthProvider(protocol) || isFourSharedProvider(protocol));
    const isProvider = !!protocol && !isOAuth && usesProviderApi(protocol);
    logger.debug('[connectToFtp] isOAuth:', isOAuth, 'isProvider:', isProvider);

    if (isOAuth) {
      // OAuth provider is already connected via OAuthConnect/FourSharedConnect component
      // Just switch to file manager view
      setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
      setLoading(false);
      setShowConnectionScreen(false);
      const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', fourshared: '4shared' };
      const providerName = providerNames[protocol] || protocol;
      notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));
      // Load remote files for OAuth provider - pass protocol explicitly
      const oauthResponse = await loadRemoteFiles(protocol);
      // Navigate to initial local directory if specified
      if (quickConnectDirs.localDir) {
        await changeLocalDirectory(quickConnectDirs.localDir);
      }
      // Create session with provider name — pass fresh files to avoid stale closure
      createSession(
        providerName,
        effectiveParams,
        oauthResponse?.current_path || '/',
        quickConnectDirs.localDir || currentLocalPath,
        oauthResponse?.files
      );
      fetchStorageQuota(protocol);
      return;
    }

    // FTP/FTPS and all provider-backed protocols use provider_connect
    if (isProvider) {
      const infinicloudWithApiKey = effectiveParams.providerId === 'infinicloud' && !!effectiveParams.options?.apiKey;
      if ((!effectiveParams.server && !infinicloudWithApiKey && protocol !== 'ftp' && protocol !== 'ftps' && protocol !== 'mega' && protocol !== 'internxt' && protocol !== 'filen' && protocol !== 'kdrive' && protocol !== 'jottacloud' && protocol !== 'drime' && protocol !== 'azure' && protocol !== 'opendrive' && protocol !== 'yandexdisk' && protocol !== 'github' && protocol !== 'swift') || (!effectiveParams.username && protocol !== 'github')) {
        notify.error(t('toast.missingFields'), t('toast.fillEndpointCreds'));
        return;
      }
      if (protocol === 's3' && !effectiveParams.options?.bucket) {
        notify.error(t('toast.missingFields'), t('toast.fillBucket'));
        return;
      }

      setLoading(true);
      setIsSyncNavigation(false);
      setSyncBasePaths(null);
      // Use displayName if available, otherwise use server/bucket/username
      // No protocol prefix in tab name - icon distinguishes the protocol
      const providerName = effectiveParams.displayName || (protocol === 's3'
        ? effectiveParams.options?.bucket || 'S3'
        : protocol === 'azure'
          ? effectiveParams.options?.bucket || 'Azure'
          : protocol === 'filelu'
            ? 'FileLu'
          : protocol === 'koofr'
            ? `Koofr ${effectiveParams.username}`
          : protocol === 'opendrive'
            ? t('savedServers.opendriveDisplay', { username: effectiveParams.username })
          : protocol === 'yandexdisk'
            ? `Yandex Disk ${effectiveParams.username}`
          : protocol === 'kdrive'
            ? `kDrive ${effectiveParams.options?.bucket || ''}`
            : protocol === 'jottacloud'
              ? `Jottacloud ${effectiveParams.username}`
              : protocol === 'mega' || protocol === 'internxt' || protocol === 'filen'
                ? effectiveParams.username
                : protocol === 'swift'
                  ? `Blomp ${effectiveParams.username}`
                  : effectiveParams.providerId === 'felicloud'
                    ? `Felicloud ${effectiveParams.username}`
                    : effectiveParams.providerId === 'infinicloud'
                      ? `InfiniCLOUD ${effectiveParams.username}`
                      : protocol === 'immich'
                        ? (effectiveParams.providerId === 'pixelunion' ? 'PixelUnion' : effectiveParams.server.replace(/^https?:\/\//, ''))
                        : effectiveParams.server.split(':')[0]);
      const protocolLabel = protocol.toUpperCase();
      // SEC: mask credentials in log-only provider name to prevent data leakage
      const maskedProviderName = effectiveParams.username && providerName.includes(effectiveParams.username)
        ? providerName.replace(effectiveParams.username, maskCredential(effectiveParams.username))
        : providerName;
      const logId = humanLog.logStart('CONNECT', { server: maskedProviderName, protocol: protocolLabel });

      try {
        // Disconnect any existing provider first
        try {
          await invoke('provider_disconnect');
        } catch {
          // Ignore if not connected
        }
        try {
          await invoke('disconnect_ftp');
        } catch {
          // Ignore if not connected
        }

        const providerPayload = await buildProviderParams(effectiveParams, quickConnectDirs.remoteDir || null);
        effectiveParams = providerPayload.effectiveParams;
        setConnectionParams(effectiveParams);
        const providerParams = providerPayload.providerParams;


        logger.debug('[connectToFtp] provider_connect params:', { ...providerParams, password: providerParams.password ? '***' : null, key_passphrase: providerParams.key_passphrase ? '***' : null });
        // SEC-P1-06: TOFU host key check for SFTP
        if (protocol === 'sftp') {
          const accepted = await checkSftpHostKey(effectiveParams.server, effectiveParams.port || 22);
          if (!accepted) { setLoading(false); return; }
        }
        const connHost = effectiveParams.server || getProviderHostFallback(protocol, effectiveParams.username);
        const { resolvedIp: connIp, connectingLogId } = await logConnectionSteps(connHost, effectiveParams.port || 443, protocol);
        await invoke('provider_connect', { params: providerParams });
        if (connectingLogId) humanLog.updateEntry(connectingLogId, { status: 'success', message: t('activity.connected_to', { ip: connIp || connHost, port: String(effectiveParams.port || 443) }) });

        logConnectionSuccess(protocol, effectiveParams.username, {
          tlsMode: effectiveParams.options?.tlsMode,
          private_key_path: effectiveParams.options?.private_key_path || undefined,
        });
        setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
        humanLog.logSuccess('CONNECT', { server: maskedProviderName, protocol: protocolLabel }, logId);
        notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));

        // Load files using provider API
        logger.debug('[connectToFtp] Calling provider_list_files for:', protocol);
        const response = await invoke<{ files: any[]; current_path: string }>('provider_list_files', {
          path: quickConnectDirs.remoteDir || null
        });
        logger.debug('[connectToFtp] provider_list_files response:', {
          fileCount: response.files?.length ?? 0,
          currentPath: response.current_path,
          rawFiles: response.files
        });

        // Convert provider entries to RemoteFile format
        const files = response.files.map(f => ({
          name: f.name,
          path: f.path,
          size: f.size,
          is_dir: f.is_dir,
          modified: f.modified,
          permissions: f.permissions,
          metadata: f.metadata,
        }));
        logger.debug('[connectToFtp] Converted files:', files.length);
        setRemoteFiles(files);
        setCurrentRemotePath(response.current_path);
        logListingComplete(response.current_path, files.length);

        // Navigate to initial local directory if specified
        if (quickConnectDirs.localDir) {
          await changeLocalDirectory(quickConnectDirs.localDir);
        }

        // Create session with provider options preserved
        const sessionParams: ConnectionParams = {
          protocol: protocol,
          server: effectiveParams.server,
          port: effectiveParams.port,
          username: effectiveParams.username,
          password: effectiveParams.password,
          options: effectiveParams.options,
        };
        createSession(
          providerName,
          sessionParams,
          response.current_path,
          quickConnectDirs.localDir || currentLocalPath,
          files
        );
        fetchStorageQuota(protocol);
      } catch (error) {
        humanLog.logError('CONNECT', { server: maskedProviderName }, logId);
        notify.error(t('connection.connectionFailed'), String(error));
      }
      finally { setLoading(false); }
      return;
    }

    // FTP/FTPS/SFTP - use legacy commands
    if (!effectiveParams.server || !effectiveParams.username) { notify.error(t('toast.missingFields'), t('toast.fillServerUser')); return; }
    setLoading(true);
    // Reset navigation sync for new connection
    setIsSyncNavigation(false);
    setSyncBasePaths(null);
    const protocolLabel = (effectiveParams.protocol || 'FTP').toUpperCase();
    const logId = humanLog.logStart('CONNECT', { server: effectiveParams.server, protocol: protocolLabel });
    try {
      // First disconnect any active OAuth provider to avoid conflicts
      try {
        await invoke('provider_disconnect');
      } catch {
        // Ignore if not connected to OAuth
      }
      const ftpProto = effectiveParams.protocol || 'ftp';
      const { resolvedIp: ftpIp, connectingLogId: ftpConnLogId } = await logConnectionSteps(effectiveParams.server, effectiveParams.port || 21, ftpProto);
      await invoke('connect_ftp', { params: effectiveParams });
      if (ftpConnLogId) humanLog.updateEntry(ftpConnLogId, { status: 'success', message: t('activity.connected_to', { ip: ftpIp || effectiveParams.server, port: String(effectiveParams.port || 21) }) });
      logConnectionSuccess(ftpProto, effectiveParams.username, {
        tlsMode: effectiveParams.options?.tlsMode,
        private_key_path: effectiveParams.options?.private_key_path || undefined,
      });
      setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
      const protocol = (effectiveParams.protocol || 'FTP').toUpperCase();
      humanLog.logSuccess('CONNECT', { server: effectiveParams.server, protocol }, logId);
      notify.success(t('toast.connected'), t('toast.connectedTo', { server: effectiveParams.server }));
      // Navigate to initial remote directory if specified
      let ftpResponse: FileListResponse | null = null;
      if (quickConnectDirs.remoteDir) {
        // Pass protocol explicitly to avoid stale state from previous provider session
        await changeRemoteDirectory(quickConnectDirs.remoteDir, effectiveParams.protocol || 'ftp');
      } else {
        ftpResponse = await loadRemoteFiles();
      }
      if (ftpResponse) {
        logListingComplete(ftpResponse.current_path || '/', ftpResponse.files?.length || 0);
      }
      // Navigate to initial local directory if specified
      if (quickConnectDirs.localDir) {
        await changeLocalDirectory(quickConnectDirs.localDir);
      }
      // Create session tab for FTP/FTPS/SFTP connections — pass fresh files to avoid stale closure
      createSession(
        effectiveParams.displayName || effectiveParams.server,
        effectiveParams,
        ftpResponse?.current_path || quickConnectDirs.remoteDir || currentRemotePath,
        quickConnectDirs.localDir || currentLocalPath,
        ftpResponse?.files
      );
    } catch (error) {
      humanLog.logError('CONNECT', { server: effectiveParams.server }, logId);
      notify.error(t('connection.connectionFailed'), String(error));
    }
    finally { setLoading(false); }
  };

  const disconnectFromFtp = async (reason?: 'button' | 'tab-close' | 'close-all') => {
    const logId = humanLog.logStart('DISCONNECT', { server: connectionParams.server });
    try {
      await invoke('disconnect_ftp');
      setIsConnected(false);
      setActivePanel('local');
      setRemoteFiles([]);
      setCurrentRemotePath('/');
      // Close all session tabs on disconnect
      setSessions([]);
      setActiveSessionId(null);
      // Close DevTools panel and clear preview
      setDevToolsOpen(false);
      setDevToolsPreviewFile(null);
      // Go to AeroFile mode instead of ConnectionScreen
      setShowConnectionScreen(false);
      setShowRemotePanel(false);
      humanLog.logSuccess('DISCONNECT', {}, logId);
      if (showToastNotifications) {
        const msgKey = reason === 'tab-close' ? 'toast.disconnectedTabClosed'
          : reason === 'close-all' ? 'toast.disconnectedAllClosed'
            : 'toast.disconnectedFrom';
        toast.info(t('toast.disconnectedTitle'), t(msgKey, { server: connectionParams.server }));
      }
    } catch (error) {
      humanLog.logError('DISCONNECT', {}, logId);
      notify.error(t('common.error'), t('toast.disconnectFailed', { error: String(error) }));
    }
  };

  // Session Management for Multi-Tab
  // Accept optional file lists to avoid stale closure captures — callers that
  // just did setRemoteFiles/setLocalFiles should pass the fresh arrays here.
  const createSession = async (serverName: string, params: ConnectionParams, remotePath: string, localPath: string, freshRemoteFiles?: RemoteFile[], freshLocalFiles?: LocalFile[]) => {
    // Deep copy params to prevent reference mutation when switching tabs
    const paramsCopy: ConnectionParams = JSON.parse(JSON.stringify(params));

    // Look up cached icons from saved server profile (vault-first, localStorage may be empty)
    let cachedFavicon: string | undefined;
    let cachedCustomIcon: string | undefined;
    let cachedPublicUrlBase: string | undefined;
    let cachedInitialPath: string | undefined;
    try {
      const servers = await secureGetWithFallback<ServerProfile[]>('server_profiles', 'aeroftp-saved-servers');
      if (servers) {
        const match = servers.find(s => s.id === serverName || s.name === serverName || s.host === serverName);
        if (match?.faviconUrl) cachedFavicon = match.faviconUrl;
        if (match?.customIconUrl) cachedCustomIcon = match.customIconUrl;
        if (match?.publicUrlBase) cachedPublicUrlBase = match.publicUrlBase;
        if (match?.initialPath) cachedInitialPath = match.initialPath;
      }
    } catch { /* ignore */ }

    const newSession: FtpSession = {
      id: `session_${Date.now()}`,
      serverId: serverName,
      serverName,
      status: 'connected',
      remotePath,
      localPath,
      remoteFiles: freshRemoteFiles ? [...freshRemoteFiles] : [...remoteFiles],
      localFiles: freshLocalFiles ? [...freshLocalFiles] : [...localFiles],
      lastActivity: new Date(),
      connectionParams: paramsCopy,
      providerId: paramsCopy.providerId,
      faviconUrl: cachedFavicon,
      customIconUrl: cachedCustomIcon,
      publicUrlBase: cachedPublicUrlBase,
      serverInitialPath: cachedInitialPath,
      // New sessions start with navigation sync disabled
      isSyncNavigation: false,
      syncBasePaths: null,
    };
    // Reset global sync state for new session
    setIsSyncNavigation(false);
    setSyncBasePaths(null);
    setSessions(prev => [...prev, newSession]);
    setActiveSessionId(newSession.id);
  };

  // Favicon detection: auto-detect project favicon from FTP/SFTP web projects
  const handleFaviconDetected = React.useCallback(async (serverId: string, faviconUrl: string) => {
    // Update active session immediately (tab icon shows right away)
    setSessions(prev => prev.map(s =>
      s.serverId === serverId ? { ...s, faviconUrl } : s
    ));
    // Update saved servers in vault (localStorage may be empty after vault migration)
    try {
      const servers = await secureGetWithFallback<ServerProfile[]>('server_profiles', 'aeroftp-saved-servers');
      if (servers) {
        const idx = servers.findIndex(s => s.id === serverId || s.name === serverId || s.host === serverId);
        if (idx !== -1) {
          servers[idx].faviconUrl = faviconUrl;
          try { await secureStoreAndClean('server_profiles', 'aeroftp-saved-servers', servers); } catch { /* ignore */ }
        }
      }
    } catch { /* ignore */ }
    // Refresh SavedServers UI (vault is now up-to-date)
    setServersRefreshKey(k => k + 1);
  }, []);

  useFaviconDetection(sessions, activeSessionId, handleFaviconDetected);

  const switchSession = async (sessionId: string) => {
    // Find the target session from current sessions state
    const targetSession = sessions.find(s => s.id === sessionId);
    if (!targetSession) return;

    // If already on this session but in AeroFile mode, just restore remote panel
    if (activeSessionId === sessionId) {
      if (!showRemotePanel) setShowRemotePanel(true);
      return;
    }

    // Capture current state values before any async operations
    const capturedRemoteFiles = [...remoteFiles];
    const capturedLocalFiles = [...localFiles];
    const capturedRemotePath = currentRemotePath;
    const capturedLocalPath = currentLocalPath;
    const capturedSyncNav = isSyncNavigation;
    const capturedSyncPaths = syncBasePaths;

    // Save current session state before switching (including sync navigation state)
    setSessions(prev => prev.map(s =>
      s.id === activeSessionId
        ? {
          ...s,
          remoteFiles: capturedRemoteFiles,
          localFiles: capturedLocalFiles,
          remotePath: capturedRemotePath,
          localPath: capturedLocalPath,
          isSyncNavigation: capturedSyncNav,
          syncBasePaths: capturedSyncPaths
        }
        : s
    ));

    // Set active session immediately
    setActiveSessionId(sessionId);
    setShowRemotePanel(true); // Exit AeroFile mode when switching to a connection tab
    quotaVersionRef.current++; // Invalidate any in-flight quota response
    setStorageQuota(null); // Clear stale quota while reconnecting

    // Load cached data immediately (zero latency UX)
    setRemoteFiles(targetSession.remoteFiles);
    setLocalFiles(targetSession.localFiles);
    setCurrentRemotePath(targetSession.remotePath);
    setCurrentLocalPath(targetSession.localPath);
    setConnectionParams(targetSession.connectionParams);
    setSelectedRemoteFiles(new Set());
    setSelectedLocalFiles(new Set());
    setRemoteSearchResults(null);

    // Restore per-session navigation sync state
    setIsSyncNavigation(targetSession.isSyncNavigation ?? false);
    setSyncBasePaths(targetSession.syncBasePaths ?? null);

    // Determine if this is an OAuth provider session
    const protocol = targetSession.connectionParams?.protocol;
    const isOAuth = !!protocol && (isOAuthProvider(protocol) || isFourSharedProvider(protocol));
    // All non-OAuth protocols on the provider API path use provider_connect/provider_list_files
    const usesProviderApiForSession = !!protocol && !isOAuth && usesProviderApi(protocol);

    // Reconnect to the new server and refresh data
    setSessions(prev => prev.map(s => s.id === sessionId ? { ...s, status: 'connecting' } : s));
    const protocolLabel = (protocol || 'FTP').toUpperCase();
    const reconnectLogId = humanLog.logRaw('activity.reconnect_start', 'CONNECT', { server: targetSession.serverName }, 'running');

    try {
      let response: FileListResponse;

      if (isOAuth) {
        // OAuth providers - need to reconnect because ProviderState may have a different provider
        logger.debug('[switchSession] OAuth provider, reconnecting...');

        // First disconnect any existing provider to avoid conflicts
        try {
          await invoke('provider_disconnect');
        } catch {
          // Ignore if not connected
        }
        try {
          await invoke('disconnect_ftp');
        } catch {
          // Ignore if not connected
        }

        // Get OAuth credentials from vault (with localStorage fallback)
        // Try new structured format first (aeroftp_oauth_settings)
        let clientId: string | null = null;
        let clientSecret: string | null = null;

        try {
          const oauthSettings = await secureGetWithFallback<Record<string, { clientId: string; clientSecret: string }>>('oauth_clients', 'aeroftp_oauth_settings');
          if (oauthSettings) {
            const providerKey = protocol === 'googledrive' ? 'googledrive' : protocol;
            if (oauthSettings[providerKey]) {
              clientId = oauthSettings[providerKey].clientId;
              clientSecret = oauthSettings[providerKey].clientSecret;
            }
          }
        } catch (e) {
          console.warn('[switchSession] Failed to parse OAuth settings:', e);
        }

        // Fall back to OS keyring (Box, pCloud, and others store credentials there)
        if (!clientId || !clientSecret) {
          try {
            const keyringProvider = protocol; // Credentials stored with protocol name as-is (e.g., 'googledrive')
            const kid = await invoke<string>('get_credential', { account: `oauth_${keyringProvider}_client_id` });
            const ksecret = await invoke<string>('get_credential', { account: `oauth_${keyringProvider}_client_secret` });
            if (kid && ksecret) {
              clientId = kid;
              clientSecret = ksecret;
            }
          } catch {
            // Keyring not available or credentials not stored
          }
        }

        if (!clientId || !clientSecret) {
          throw new Error(`OAuth credentials not found for ${protocol}`);
        }

        if (isFourSharedProvider(protocol)) {
          // 4shared uses OAuth 1.0 — needs fourshared_connect, not oauth2_connect
          await invoke('fourshared_connect', {
            params: { consumer_key: clientId, consumer_secret: clientSecret }
          });
        } else {
          // OAuth 2.0 providers (Google Drive, Dropbox, OneDrive, Box, pCloud, Zoho)
          const oauthProvider = protocol === 'googledrive' ? 'google_drive' : protocol;

          // Zoho requires region for correct API endpoints (us/eu/in/au/jp/ca/sa)
          let region: string | undefined = targetSession.connectionParams?.options?.region;
          if (!region && protocol === 'zohoworkdrive') {
            try {
              region = await invoke<string>('get_credential', { account: `oauth_${protocol}_region` });
            } catch {
              // Default "us" will be applied by Rust serde default
            }
          }

          const oauthParams = {
            provider: oauthProvider,
            client_id: clientId,
            client_secret: clientSecret,
            ...(region && { region }),
          };
          try {
            await invoke('oauth2_connect', { params: oauthParams });
          } catch (connectErr) {
            const errMsg = connectErr instanceof Error ? connectErr.message : String(connectErr);
            const lower = errMsg.toLowerCase();
            // Token invalid/expired — re-authenticate and retry
            if (lower.includes('authentication failed') ||
                (lower.includes('invalid') && lower.includes('access_token')) ||
                lower.includes('token expired') ||
                (lower.includes('token') && lower.includes('refresh'))) {
              logger.debug('[switchSession] OAuth token invalid, re-authenticating...');
              await invoke('oauth2_full_auth', { params: oauthParams });
              await invoke('oauth2_connect', { params: oauthParams });
            } else {
              throw connectErr;
            }
          }
        }

        // Now navigate to the session's path
        response = await invoke('provider_change_dir', { path: targetSession.remotePath || '/' });
      } else if (usesProviderApiForSession) {
        logger.debug('[switchSession] Provider (S3/WebDAV), reconnecting...');

        let connectParams = targetSession.connectionParams;
        // Safety check: recover missing S3 options
        if (protocol === 's3' && (!connectParams.options || !connectParams.options.bucket)) {
          try {
            const savedServers = await secureGetWithFallback<any[]>('server_profiles', 'aeroftp-saved-servers');
            if (savedServers) {
              const found = savedServers.find((s: any) =>
                (s.name === targetSession.serverName) ||
                (s.host === connectParams.server && s.username === connectParams.username)
              );
              if (found && found.options && found.options.bucket) {
                logger.debug('[switchSession] Auto-recovered missing S3 options');
                connectParams = { ...connectParams, options: found.options };
              }
            }
          } catch (e) { console.error('Option recovery failed', e); }
        }

        // First disconnect any existing connections
        try { await invoke('provider_disconnect'); } catch { }
        try { await invoke('disconnect_ftp'); } catch { }

        const providerPayload = await buildProviderParams(connectParams, targetSession.remotePath || null);
        connectParams = providerPayload.effectiveParams;
        const providerParams = providerPayload.providerParams;

        logger.debug('[switchSession] provider_connect params:', { ...providerParams, password: providerParams.password ? '***' : null });
        // SEC-P1-06: TOFU host key check for SFTP
        if (protocol === 'sftp') {
          const accepted = await checkSftpHostKey(connectParams.server, connectParams.port || 22);
          if (!accepted) throw new Error('Host key rejected by user');
        }
        await invoke('provider_connect', { params: providerParams });
        if (targetSession.remotePath && targetSession.remotePath !== '/') {
          try { await invoke('provider_change_dir', { path: targetSession.remotePath }); } catch (e) { console.warn('Restore path failed', e); }
        }
        response = await invoke('provider_list_files', { path: null });
      } else {
        // FTP/FTPS - reconnect and navigate
        logger.debug('[switchSession] FTP provider, reconnecting...');
        // First disconnect any active OAuth provider to avoid conflicts
        try {
          await invoke('provider_disconnect');
        } catch {
          // Ignore if not connected to OAuth
        }
        await invoke('connect_ftp', { params: targetSession.connectionParams });

        // Navigate to the saved path to restore session state.
        // Avoid using paths from previous WebDAV/S3 sessions (e.g., /wwwhome, /bucket-name)
        const savedPath = targetSession.remotePath;
        const isValidFtpPath = savedPath &&
          !savedPath.includes('wwwhome') &&
          !savedPath.includes('webdav') &&
          savedPath.startsWith('/');

        if (isValidFtpPath) {
          try {
            await invoke('change_directory', { path: savedPath });
          } catch (pathError) {
            console.warn('[switchSession] Could not restore FTP path, using home:', pathError);
            // Path doesn't exist on this server, stay at login home directory
          }
        }
        response = await invoke('list_files');
      }

      setSessions(prev => prev.map(s => s.id === sessionId ? { ...s, status: 'connected' } : s));
      activityLog.updateEntry(reconnectLogId, {
        status: 'success',
        message: t('activity.reconnect_success', { server: targetSession.serverName })
      });

      // Refresh remote files with real data
      setRemoteFiles(response.files);
      setCurrentRemotePath(response.current_path);
      setSelectedRemoteFiles(new Set());

      // Update session cache with fresh data
      setSessions(prev => prev.map(s =>
        s.id === sessionId
          ? { ...s, remoteFiles: response.files, remotePath: response.current_path }
          : s
      ));

      // Fetch storage quota after successful reconnection
      fetchStorageQuota(protocol);

      // Also refresh local files for this session's local path
      const localFilesData: LocalFile[] = await invoke('get_local_files', {
        path: targetSession.localPath,
        showHidden: showHiddenFiles
      });
      setLocalFiles(localFilesData);
      setCurrentLocalPath(targetSession.localPath);

    } catch (e) {
      logger.error('Reconnect error:', e);
      activityLog.updateEntry(reconnectLogId, {
        status: 'error',
        message: t('activity.reconnect_error', { server: targetSession.serverName })
      });
      setSessions(prev => prev.map(s => s.id === sessionId ? { ...s, status: 'cached' } : s));
      // Even on error, ensure local path is set correctly from session cache
      setCurrentLocalPath(targetSession.localPath);
    }
  };

  const closeSession = async (sessionId: string) => {
    const session = sessions.find(s => s.id === sessionId);
    if (!session) return;

    // If closing active session, switch to another or disconnect
    if (sessionId === activeSessionId) {
      const remaining = sessions.filter(s => s.id !== sessionId);
      if (remaining.length > 0) {
        await switchSession(remaining[0].id);
      } else {
        await disconnectFromFtp('tab-close');
      }
    }

    setSessions(prev => prev.filter(s => s.id !== sessionId));
  };

  const closeAllSessions = async () => {
    await disconnectFromFtp('close-all');
    setSessions([]);
  };

  const handleNewTabFromSavedServer = () => {
    // Capture current state before any changes
    const capturedRemoteFiles = [...remoteFiles];
    const capturedLocalFiles = [...localFiles];
    const capturedRemotePath = currentRemotePath;
    const capturedLocalPath = currentLocalPath;
    const capturedSyncNav = isSyncNavigation;
    const capturedSyncPaths = syncBasePaths;

    // Mark current session as cached and go to connection screen (including sync state)
    if (activeSessionId) {
      setSessions(prev => prev.map(s =>
        s.id === activeSessionId
          ? { ...s, status: 'cached', remoteFiles: capturedRemoteFiles, localFiles: capturedLocalFiles, remotePath: capturedRemotePath, localPath: capturedLocalPath, isSyncNavigation: capturedSyncNav, syncBasePaths: capturedSyncPaths }
          : s
      ));
    }
    // Deselect the active session since we're going to connection screen
    setActiveSessionId(null);
    // Reset sync state for new connection
    setIsSyncNavigation(false);
    setSyncBasePaths(null);
    setIsConnected(false);
    // Reset connection form for a fresh "new server" experience
    setConnectionParams({ server: '', username: '', password: '' });
    setQuickConnectDirs({ remoteDir: '', localDir: '' });
    // Show the connection screen for selecting a new server
    setShowConnectionScreen(true);
  };

  // Handle click on Cloud Tab - auto-connect to cloud server profile
  // Supports all protocols: FTP, FTPS, SFTP, WebDAV, S3, MEGA, Azure, Filen, Koofr, etc.
  const handleCloudTabClick = async () => {
    logger.debug('Cloud Tab clicked');

    try {
      // Get cloud config to know which server profile and folders
      const cloudConfig = await invoke<{
        enabled: boolean;
        local_folder: string;
        remote_folder: string;
        server_profile: string;
        protocol_type?: string;
      }>('get_cloud_config');

      logger.debug('Cloud config:', cloudConfig);

      if (!cloudConfig.enabled) {
        setShowCloudPanel(true);
        return;
      }

      // Get saved servers from vault (with localStorage fallback)
      const savedServers = await secureGetWithFallback<any[]>('server_profiles', 'aeroftp-saved-servers');
      if (!savedServers || savedServers.length === 0) {
        notify.error(t('toast.noSavedServers'), t('toast.saveServerFirst'));
        setShowCloudPanel(true);
        return;
      }

      const cloudServer = savedServers.find((s: { name: string }) => s.name === cloudConfig.server_profile);

      if (!cloudServer) {
        notify.error(t('toast.serverNotFound'), t('toast.serverProfileNotFound', { name: cloudConfig.server_profile }));
        setShowCloudPanel(true);
        return;
      }

      // Determine protocol (from cloud config, server profile, or default)
      const protocol = (cloudConfig.protocol_type || cloudServer.protocol || 'ftp') as ProviderType;
      const isProvider = usesProviderApi(protocol);
      const isFtp = isFtpProtocol(protocol);
      const protocolLabel = protocol.toUpperCase();

      // Build connection server string
      const defaultPort = protocol === 'sftp' ? 22 : protocol === 'ftps' ? 990 : 21;
      const cloudServerString = cloudServer.port && cloudServer.port !== defaultPort
        ? `${cloudServer.host}:${cloudServer.port}`
        : cloudServer.host;

      // Load password from OS keyring (localStorage never stores passwords)
      let cloudPassword = '';
      try {
        cloudPassword = await invoke<string>('get_credential', { account: `server_${cloudServer.id}` });
      } catch (e) {
        console.warn('Failed to load cloud server credential from keyring:', e);
      }

      // Helper: connect to cloud server using the correct protocol
      const connectToCloudServer = async (): Promise<void> => {
        if (isProvider) {
          // Non-FTP: use provider_connect (SFTP, WebDAV, S3, MEGA, Azure, Filen, Koofr, etc.)
          const providerParams = {
            protocol,
            server: cloudServer.host,
            port: cloudServer.port || (protocol === 'sftp' ? 22 : 443),
            username: cloudServer.username || '',
            password: cloudPassword,
            initial_path: cloudConfig.remote_folder || null,
            bucket: cloudServer.options?.bucket,
            region: cloudServer.options?.region || (protocol === 's3' ? 'us-east-1' : undefined),
            endpoint: cloudServer.options?.endpoint || null,
            path_style: cloudServer.options?.pathStyle || false,
            private_key_path: cloudServer.options?.private_key_path || null,
            key_passphrase: cloudServer.options?.key_passphrase || null,
            timeout: cloudServer.options?.timeout || 30,
            tls_mode: cloudServer.options?.tlsMode || (protocol === 'ftps' ? 'implicit' : protocol === 'ftp' ? 'explicit' : undefined),
            verify_cert: cloudServer.options?.verifyCert !== undefined ? cloudServer.options.verifyCert : true,
            two_factor_code: cloudServer.options?.two_factor_code || null,
          };
          await invoke('provider_connect', { params: providerParams });
        } else {
          // FTP/FTPS: use connect_ftp
          const ftpParams = {
            server: cloudServerString,
            username: cloudServer.username || '',
            password: cloudPassword,
            protocol,
          };
          await invoke('connect_ftp', { params: ftpParams });
        }
      };

      // Helper: navigate to remote folder using correct API
      const navigateToRemoteFolder = async (folder: string): Promise<void> => {
        if (isProvider) {
          const response = await invoke<{ files: any[]; current_path: string }>('provider_list_files', { path: folder || null });
          const files = response.files.map((f: any) => ({
            name: f.name, path: f.path || f.name, size: f.size, is_dir: f.is_dir,
            modified: f.modified, permissions: f.permissions || null,
          }));
          setRemoteFiles(files);
          setCurrentRemotePath(response.current_path);
        } else {
          const response: FileListResponse = await invoke('change_directory', { path: folder });
          setRemoteFiles(response.files);
          setCurrentRemotePath(response.current_path);
        }
      };

      // Connection params to set (for protocol detection in other parts of the app)
      const connParams = {
        server: cloudServerString,
        username: cloudServer.username || '',
        password: cloudPassword,
        protocol,
        port: cloudServer.port,
        options: cloudServer.options,
      };

      // === SCENARIO 1: ALREADY CONNECTED ===
      if (isConnected) {
        logger.debug('Already connected, checking if same server...');

        const currentServer = connectionParams.server;
        const isSameServer = currentServer === cloudServerString
          && connectionParams.protocol === protocol;

        logger.debug(`Current: ${currentServer} (${connectionParams.protocol}), Cloud: ${cloudServerString} (${protocol}), Same: ${isSameServer}`);

        // Save current session state before switching
        const capturedSessionId = activeSessionId;
        if (capturedSessionId) {
          setSessions(prev => prev.map(s =>
            s.id === capturedSessionId
              ? {
                ...s,
                status: 'cached',
                remoteFiles: [...remoteFiles],
                localFiles: [...localFiles],
                remotePath: currentRemotePath,
                localPath: currentLocalPath,
                isSyncNavigation,
                syncBasePaths,
              }
              : s
          ));
        }

        // Deselect current session tab since we're going to AeroCloud
        setActiveSessionId(null);

        // If different server/protocol, reconnect
        if (!isSameServer) {
          logger.debug('Different server, reconnecting to cloud server...');
          try { await invoke('provider_disconnect'); } catch { }
          try { await invoke('disconnect_ftp'); } catch { }

          try {
            setLoading(true);
            await connectToCloudServer();
            setConnectionParams(connParams);
            humanLog.logRaw('activity.connect_success', 'CONNECT', { server: `AeroCloud (${cloudServerName})`, protocol: protocolLabel }, 'success');
          } catch (connError) {
            logger.error('Failed to connect to cloud server:', connError);
            notify.error(t('toast.connectionFailedTitle'), String(connError));
            // Restore previous session
            if (capturedSessionId) {
              setActiveSessionId(capturedSessionId);
              setSessions(prev => prev.map(s =>
                s.id === capturedSessionId ? { ...s, status: 'connected' } : s
              ));
            }
            setLoading(false);
            return;
          } finally {
            setLoading(false);
          }
        }

        // Ensure panels are visible (user might have been in local-only mode)
        setShowRemotePanel(true);
        setShowLocalPreview(false);
        setShowConnectionScreen(false);
        // Lock navigation to cloud folders (prevent navigating above remote_folder)
        setIsSyncNavigation(true);
        setSyncBasePaths({ remote: cloudConfig.remote_folder, local: cloudConfig.local_folder });

        // Navigate to cloud folders
        try {
          await navigateToRemoteFolder(cloudConfig.remote_folder);
          const localFilesData: LocalFile[] = await invoke('get_local_files', { path: cloudConfig.local_folder, showHidden: showHiddenFiles });
          setLocalFiles(localFilesData);
          setCurrentLocalPath(cloudConfig.local_folder);
          setCloudRemoteFolder(cloudConfig.remote_folder);
          setCloudLocalFolder(cloudConfig.local_folder);
          if (isSameServer) {
            humanLog.logRaw('activity.connect_success', 'CONNECT', { server: `AeroCloud (${cloudServerName})`, protocol: protocolLabel }, 'success');
          }
        } catch (navError) {
          logger.error('Cloud navigation error:', navError);
          notify.error('AeroCloud', t('toast.navigationFailed', { error: String(navError) }));
        }

        // Trigger sync
        try { await invoke('trigger_cloud_sync'); } catch { }
        return;
      }

      // === SCENARIO 2: NOT CONNECTED (AeroFile mode or connection screen) ===
      setLoading(true);
      const logId = humanLog.logStart('CONNECT', { server: `AeroCloud (${cloudConfig.server_profile})`, protocol: protocolLabel });
      if (showToastNotifications) {
        toast.info(t('toast.connecting'), t('toast.connectingTo', { server: cloudConfig.server_profile }));
      }

      // Disconnect any stale connections
      try { await invoke('provider_disconnect'); } catch { }
      try { await invoke('disconnect_ftp'); } catch { }

      await connectToCloudServer();
      setIsConnected(true);
      setShowRemotePanel(true);
      setShowLocalPreview(false);
      setConnectionParams(connParams);
      setShowConnectionScreen(false);
      setIsSyncNavigation(false);
      setSyncBasePaths(null);

      // Navigate to cloud folders
      await navigateToRemoteFolder(cloudConfig.remote_folder);
      const cloudLocalFilesData: LocalFile[] = await invoke('get_local_files', { path: cloudConfig.local_folder, showHidden: showHiddenFiles });
      setLocalFiles(cloudLocalFilesData);
      setCurrentLocalPath(cloudConfig.local_folder);
      setCloudRemoteFolder(cloudConfig.remote_folder);
      setCloudLocalFolder(cloudConfig.local_folder);

      humanLog.logSuccess('CONNECT', { server: `AeroCloud (${cloudConfig.server_profile})`, protocol: protocolLabel }, logId);
      notify.success(t('toast.connected'), t('toast.connectedTo', { server: `AeroCloud (${cloudConfig.server_profile})` }));

      // Trigger sync
      try {
        if (showToastNotifications) {
          toast.info(t('toast.syncStartedTitle'), t('toast.syncingCloudFiles'));
        }
        await invoke('trigger_cloud_sync');
      } catch (e) {
        logger.error('Sync trigger error:', e);
      }

    } catch (error) {
      logger.error('Cloud tab click error:', error);
      notify.error(t('connection.connectionFailed'), String(error));
      setShowCloudPanel(true);
    } finally {
      setLoading(false);
    }
  };
  const changeRemoteDirectory = async (path: string, overrideProtocol?: string) => {
    // Sync navigation guard: prevent navigating above the sync base path
    if (isSyncNavigation && syncBasePaths && path === '..') {
      const norm = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;
      if (norm(currentRemotePath) === norm(syncBasePaths.remote)) return;
    }
    // Increment navigation counter — used to discard stale async responses
    const navId = ++remoteNavCounter.current;
    try {
      // Check if we're connected to a Provider (OAuth, S3, WebDAV)
      // Use override protocol if provided, then connectionParams, then active session (most robust)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = (overrideProtocol || connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
      const isProvider = usesProviderApi(protocol);

      let response: FileListResponse;
      if (isProvider) {
        // Use provider API
        response = await invoke('provider_change_dir', { path });
      } else {
        // Use FTP API
        response = await invoke('change_directory', { path });
      }
      // Discard response if a newer navigation was initiated while we awaited
      if (navId !== remoteNavCounter.current) return;
      setRemoteFiles(response.files);
      setCurrentRemotePath(response.current_path);
      setSelectedRemoteFiles(new Set());
      setRemoteSearchResults(null);
      humanLog.logNavigate(response.current_path, true);

      // Navigation Sync: mirror to local panel if enabled
      if (isSyncNavigation && syncBasePaths) {
        const relativePath = response.current_path.startsWith(syncBasePaths.remote)
          ? response.current_path.slice(syncBasePaths.remote.length)
          : '';
        // Join paths avoiding double slashes
        const basePath = syncBasePaths.local.endsWith('/') ? syncBasePaths.local.slice(0, -1) : syncBasePaths.local;
        const relPath = relativePath.startsWith('/') ? relativePath : '/' + relativePath;
        const newLocalPath = (relativePath ? basePath + relPath : basePath) || '/';
        // Check if local path exists
        try {
          if (navId !== remoteNavCounter.current) return;
          const files: LocalFile[] = await invoke('get_local_files', { path: newLocalPath, showHidden: showHiddenFiles });
          if (navId !== remoteNavCounter.current) return;
          setLocalFiles(files);
          setCurrentLocalPath(newLocalPath);
          setSelectedLocalFiles(new Set());
        } catch {
          // Local directory doesn't exist - show dialog
          if (navId !== remoteNavCounter.current) return;
          setSyncNavDialog({ missingPath: newLocalPath, isRemote: false, targetPath: newLocalPath });
        }
      }
    } catch (error) {
      if (navId !== remoteNavCounter.current) return;
      notify.error(t('common.error'), t('toast.changeDirFailed', { error: String(error) }));
    }
  };

  const changeLocalDirectory = async (path: string) => {
    // Sync navigation guard: prevent navigating above the sync base path
    if (isSyncNavigation && syncBasePaths) {
      const norm = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;
      const normBase = norm(syncBasePaths.local);
      const normTarget = norm(path);
      // Block if target is a proper ancestor of the base path
      if (normTarget !== normBase && (normTarget === '/' || normBase.startsWith(normTarget + '/'))) return;
    }
    const success = await loadLocalFiles(path);
    if (!success) return; // Don't record failed navigations
    humanLog.logNavigate(path, false);
    addRecentPath(path);
    // Exit trash view when navigating to a regular path
    if (isTrashView) setIsTrashView(false);

    // Save last local path if remember folder is enabled
    if (rememberLastFolder) {
      secureGetWithFallback<Record<string, unknown>>('app_settings', SETTINGS_KEY)
        .then(existing => secureStoreAndClean('app_settings', SETTINGS_KEY, { ...(existing || {}), lastLocalPath: path }))
        .catch((e) => {
          console.error('Failed to save last local path:', e);
        });
    }

    // Navigation Sync: mirror to remote panel if enabled
    if (isSyncNavigation && syncBasePaths && isConnected) {
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = (connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
      const isProvider = usesProviderApi(protocol);

      const relativePath = path.startsWith(syncBasePaths.local)
        ? path.slice(syncBasePaths.local.length)
        : '';
      // Join paths avoiding double slashes
      const basePath = syncBasePaths.remote.endsWith('/') ? syncBasePaths.remote.slice(0, -1) : syncBasePaths.remote;
      const relPath = relativePath.startsWith('/') ? relativePath : '/' + relativePath;
      const newRemotePath = (relativePath ? basePath + relPath : basePath) || '/';

      // Check if remote path exists
      const navId = ++remoteNavCounter.current;
      try {
        const response: FileListResponse = isProvider
          ? await invoke('provider_change_dir', { path: newRemotePath })
          : await invoke('change_directory', { path: newRemotePath });
        if (navId !== remoteNavCounter.current) return;
        setRemoteFiles(response.files);
        setCurrentRemotePath(response.current_path);
        setSelectedRemoteFiles(new Set());
        setRemoteSearchResults(null);
      } catch {
        // Remote directory doesn't exist - show dialog
        if (navId !== remoteNavCounter.current) return;
        setSyncNavDialog({ missingPath: newRemotePath, isRemote: true, targetPath: newRemotePath });
      }
    }
  };

  // Safe local path navigation with fallback for invalid paths
  // (e.g. imported backup from another PC with different directory structure)
  const safeChangeLocalDirectory = async (path: string): Promise<string> => {
    const success = await loadLocalFiles(path);
    if (success) {
      humanLog.logNavigate(path, false);
      addRecentPath(path);
      return path;
    }
    // Path doesn't exist — fallback to home directory
    const fallback = await homeDir().catch(() => '/');
    const fallbackSuccess = await loadLocalFiles(fallback);
    if (fallbackSuccess) {
      humanLog.logNavigate(fallback, false);
      notify.warning?.(t('toast.localPathNotFound', { path })) ??
        notify.error(t('common.error'), `Local path not found: ${path}. Navigated to ${fallback}`);
      return fallback;
    }
    // Last resort: root
    await loadLocalFiles('/');
    return '/';
  };

  // Handle sync nav dialog actions
  const handleSyncNavCreateFolder = async () => {
    if (!syncNavDialog) return;
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = (connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
    const isProviderConn = usesProviderApi(protocol);
    try {
      if (syncNavDialog.isRemote) {
        if (isProviderConn) {
          await invoke('provider_mkdir', { path: syncNavDialog.targetPath });
          const response: FileListResponse = await invoke('provider_change_dir', { path: syncNavDialog.targetPath });
          setRemoteFiles(response.files);
          setCurrentRemotePath(response.current_path);
          setSelectedRemoteFiles(new Set());
        } else {
          await invoke('create_remote_folder', { path: syncNavDialog.targetPath });
          const response: FileListResponse = await invoke('change_directory', { path: syncNavDialog.targetPath });
          setRemoteFiles(response.files);
          setCurrentRemotePath(response.current_path);
          setSelectedRemoteFiles(new Set());
        }
        notify.success(t('toast.folderCreated'), syncNavDialog.missingPath);
      } else {
        await invoke('create_local_folder', { path: syncNavDialog.targetPath });
        await loadLocalFiles(syncNavDialog.targetPath);
        notify.success(t('toast.folderCreated'), syncNavDialog.missingPath);
      }
    } catch (error) {
      notify.error(t('toast.failedCreateFolder'), String(error));
    }
    setSyncNavDialog(null);
  };

  const handleSyncNavDisable = () => {
    setIsSyncNavigation(false);
    setSyncBasePaths(null);
    notify.info(t('toast.navSyncDisabled'));
    setSyncNavDialog(null);
  };

  // Toggle navigation sync and set base paths
  const toggleSyncNavigation = () => {
    if (!isSyncNavigation) {
      // Use current paths as sync base — folder name alignment is NOT forced
      // because remote FTP home dir name often differs from local folder name.
      // The mismatch indicator on address bars will warn if paths diverge during navigation.
      setSyncBasePaths({ remote: currentRemotePath, local: currentLocalPath });
      setIsSyncNavigation(true);
      notify.success(t('toast.navSyncEnabled'), t('toast.syncingPaths', { remote: currentRemotePath, local: currentLocalPath }));
      humanLog.logRaw('activity.nav_sync_enabled', 'NAVIGATE', { remote: currentRemotePath, local: currentLocalPath }, 'success');
    } else {
      setSyncBasePaths(null);
      setIsSyncNavigation(false);
      notify.info(t('toast.navSyncDisabled'));
      humanLog.logRaw('activity.nav_sync_disabled', 'NAVIGATE', {}, undefined);
    }
  };

  // checkOverwrite and resetOverwriteSettings provided by useOverwriteCheck hook

  // Helper: check folder overwrite in 'ask' mode — shows FolderOverwriteDialog
  const checkFolderOverwrite = useCallback(async (
    folderName: string,
    direction: 'upload' | 'download',
    queueCount: number = 0
  ): Promise<{ action: FolderMergeAction; applyToAll: boolean }> => {
    // If apply-to-all was set previously in this batch, reuse it
    if (folderOverwriteApplyToAll.current.enabled) {
      return { action: folderOverwriteApplyToAll.current.action, applyToAll: true };
    }
    // Check if destination folder exists
    const exists = direction === 'upload'
      ? remoteFiles.some(f => f.name === folderName && f.is_dir)
      : localFiles.some(f => f.name === folderName && f.is_dir);
    if (!exists) {
      return { action: 'merge_overwrite', applyToAll: false };
    }
    // Read settings
    if (fileExistsAction !== 'ask') {
      // Map setting to folder action without showing dialog
      return { action: 'merge_overwrite', applyToAll: false };
    }
    // Show dialog and wait
    return new Promise((resolve) => {
      setFolderOverwriteDialog({
        isOpen: true,
        folderName,
        direction,
        queueCount,
        resolve: (result) => {
          if (result.applyToAll) {
            folderOverwriteApplyToAll.current = { action: result.action, enabled: true };
          }
          resolve(result);
        },
      });
    });
  }, [localFiles, remoteFiles, fileExistsAction]);

  const downloadFile = async (remoteFilePath: string, fileName: string, destinationPath?: string, isDir: boolean = false, fileSize?: number, _skipConflictCheck: boolean = false) => {
    const logId = humanLog.logStart('DOWNLOAD', { filename: fileName });
    pendingFileLogIds.current.set(fileName, logId); // Dedup
    const startTime = Date.now();

    // Check if we're using a Provider (get protocol from active session as fallback)
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const isProvider = usesProviderApi(protocol);

    try {
      if (isDir) {
        const downloadPath = destinationPath || await open({ directory: true, multiple: false, defaultPath: await downloadDir() });
        if (downloadPath) {
          const folderPath = `${downloadPath}/${fileName}`;
          // For folders, 'ask' defaults to 'overwrite' (FolderOverwriteDialog handles the ask mode at batch level)
          const folderAction = fileExistsAction === 'ask' ? '' : fileExistsAction;
          let folderResult: string;
          if (isProvider) {
            folderResult = await invoke<string>('provider_download_folder', {
              remotePath: remoteFilePath,
              localPath: folderPath,
              fileExistsAction: folderAction || undefined,
              maxConcurrent: effectiveMaxConcurrentTransfers,
              retryCount,
              timeoutSeconds,
            });
          } else {
            const params: DownloadFolderParams = {
              remote_path: remoteFilePath,
              local_path: folderPath,
              file_exists_action: folderAction || undefined,
              max_concurrent: effectiveMaxConcurrentTransfers,
              retry_count: retryCount,
              timeout_seconds: timeoutSeconds,
            };
            folderResult = await invoke<string>('download_folder', { params });
          }
          // Don't log success if the transfer was cancelled
          if (!folderResult.toLowerCase().includes('cancelled')) {
            const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
            humanLog.log('DOWNLOAD', `[Local] Downloaded folder ${folderPath} in ${elapsed}s`, 'success');
            humanLog.updateEntry(logId, { status: 'success', message: `Downloaded folder ${folderPath} in ${elapsed}s` });
          }
        } else {
          humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
        }
      } else {
        const downloadPath = destinationPath || await open({ directory: true, multiple: false, defaultPath: await downloadDir() });
        if (downloadPath) {
          let targetName = fileName;
          const remoteFileInfo = remoteFiles.find(f => f.name === fileName && !f.is_dir);

          if (!_skipConflictCheck) {
            // Check file conflict before downloading (single file transfers only)
            const overwriteResult = await checkOverwrite(
              fileName,
              fileSize || remoteFileInfo?.size || 0,
              remoteFileInfo?.modified ? new Date(remoteFileInfo.modified) : undefined,
              true, // sourceIsRemote = true for download
              0
            );

            if (overwriteResult.action === 'cancel' || overwriteResult.action === 'skip') {
              humanLog.updateEntry(logId, { status: 'success', message: `Skipped ${fileName}` });
              return;
            }

            if (overwriteResult.newName) {
              targetName = overwriteResult.newName;
            }
          }

          const localFilePath = `${downloadPath}/${targetName}`;
          const remoteModified = remoteFileInfo?.modified || undefined;
          if (isProvider) {
            // Use provider command for file download
            await invoke('provider_download_file', { remotePath: remoteFilePath, localPath: localFilePath, modified: remoteModified });
          } else {
            const params: DownloadParams = { remote_path: remoteFilePath, local_path: localFilePath, modified: remoteModified };
            await invoke('download_file', { params });
          }
          const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
          const sizeStr = fileSize ? formatBytes(fileSize) : '';
          const details = sizeStr ? `(${sizeStr} in ${elapsed}s)` : `(${elapsed}s)`;
          const msg = t('activity.download_success', { filename: localFilePath, details });
          humanLog.updateEntry(logId, { status: 'success', message: msg });
        } else {
          humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
        }
      }
    } catch (error) {
      humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
      // Don't spam toasts when batch was cancelled — one summary toast is enough
      if (!batchCancelledRef.current) {
        notify.error(t('toast.downloadFailed'), String(error));
      }
    }
  };

  const uploadFile = async (localFilePath: string, fileName: string, isDir: boolean = false, fileSize?: number, _skipConflictCheck: boolean = false, commitMessage?: string) => {
    const startTime = Date.now();
    try {
      // Check if we're using a Provider (get protocol from active session as fallback)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = usesProviderApi(protocol);
      const isGitHubRepoMode = (protocol === 'github' && !currentRemotePath.startsWith('/.github-releases')) || protocol === 'gitlab';

      if (isDir) {
        const logId = humanLog.logStart('UPLOAD', { filename: fileName });
        pendingFileLogIds.current.set(fileName, logId); // Register for adoption by backend event
        if (isProvider) {
          const remoteRootForFolder = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;
          setScanningState({ active: true, folderName: fileName, message: t('activity.upload_start', { filename: fileName }) || `Uploading ${fileName}...`, operation: 'upload' });
          try {
            const folderAction2 = fileExistsAction === 'ask' ? '' : fileExistsAction;
            await invoke<string>('provider_upload_folder', {
              localPath: localFilePath,
              remotePath: remoteRootForFolder,
              fileExistsAction: folderAction2 || null,
              maxConcurrent: effectiveMaxConcurrentTransfers,
              retryCount: retryCount,
              timeoutSeconds: timeoutSeconds,
              commitMessage: commitMessage || null,
            });
          } finally {
            setScanningState(INITIAL_SCANNING_STATE);
          }

          const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
          const details = `(${elapsed}s)`;
          const msg = t('activity.upload_success', { filename: remoteRootForFolder, details });
          humanLog.updateEntry(logId, { message: msg });
          // Refresh list
          loadRemoteFiles();
          return;
        }
        const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;
        const folderAction2 = fileExistsAction === 'ask' ? '' : fileExistsAction;
        const params: UploadFolderParams = {
          local_path: localFilePath,
          remote_path: remotePath,
          file_exists_action: folderAction2 || undefined,
          max_concurrent: effectiveMaxConcurrentTransfers,
          retry_count: retryCount,
          timeout_seconds: timeoutSeconds,
        };
        const uploadResult = await invoke<string>('upload_folder', { params });
        // Don't log success if the transfer was cancelled
        if (!uploadResult.toLowerCase().includes('cancelled')) {
          const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
          const details = `(${elapsed}s)`;
          const msg = t('activity.upload_success', { filename: remotePath, details });
          humanLog.updateEntry(logId, { status: 'success', message: msg });
        }
      } else {
        let targetName = fileName;

        if (!_skipConflictCheck) {
          // Check file conflict before uploading (single file transfers only)
          const localFileInfo = localFiles.find(f => f.name === fileName && !f.is_dir);
          const overwriteResult = await checkOverwrite(
            fileName,
            fileSize || localFileInfo?.size || 0,
            localFileInfo?.modified ? new Date(localFileInfo.modified) : undefined,
            false, // sourceIsRemote = false for upload
            0
          );

          if (overwriteResult.action === 'cancel' || overwriteResult.action === 'skip') {
            humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: fileName }, 'success');
            return;
          }

          if (overwriteResult.newName) {
            targetName = overwriteResult.newName;
          }
        }

        const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${targetName}`;

        if (isGitHubRepoMode && !commitMessage && gitHubRepoInfo && gitHubRepoInfo.writeModeKind !== 'unknown') {
          setGitHubCommitDialog({
            files: [{ local: localFilePath, remote: remotePath }],
            operation: 'upload',
            branch: gitHubRepoInfo.branch,
            writeMode: gitHubRepoInfo.writeModeKind,
            workingBranch: gitHubRepoInfo.workingBranch || undefined,
            onCommit: (message: string) => {
              setGitHubCommitDialog(null);
              void uploadFile(localFilePath, fileName, isDir, fileSize, _skipConflictCheck, message);
            },
          });
          return;
        }

        const logId = humanLog.logStart('UPLOAD', { filename: fileName });
        pendingFileLogIds.current.set(fileName, logId); // Register for adoption by backend event

        if (isProvider) {
          await invoke('provider_upload_file', { localPath: localFilePath, remotePath, commitMessage: commitMessage || null });
        } else {
          await invoke('upload_file', { params: { local_path: localFilePath, remote_path: remotePath } as UploadParams });
        }

        const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
        const sizeStr = fileSize ? formatBytes(fileSize) : '';
        const details = sizeStr ? `(${sizeStr} in ${elapsed}s)` : `(${elapsed}s)`;
        const msg = t('activity.upload_success', { filename: remotePath, details });
        humanLog.updateEntry(logId, { status: 'success', message: msg });
      }
    } catch (error) {
      humanLog.logRaw('activity.upload_error', 'UPLOAD', { filename: fileName }, 'error');
      if (!batchCancelledRef.current) {
        notify.error(t('toast.uploadFailed'), String(error));
      }
    }
  };

  // Two-level cancel: Stop (finish current file) → Force Stop (interrupt immediately)
  const cancelTransfer = async () => {
    // If batch is paused by circuit breaker, resolve the pause promise
    if (batchResumeResolverRef.current) {
      batchResumeResolverRef.current('cancel');
    }
    if (cancelLevelRef.current === 0) {
      // Level 1 — Soft cancel: finish current file, stop queue
      cancelLevelRef.current = 1;
      batchCancelledRef.current = true;
      transferQueue.stopPending(); // Only stop pending items, let current finish
      dispatchTransferToast(null);
    } else {
      // Level 2 — Hard cancel: interrupt current transfer immediately
      cancelLevelRef.current = 2;
      setActiveTransfer(null);
      dispatchTransferToast(null);
      transferQueue.stopAll(); // Stop everything including current
      try { await invoke('cancel_transfer'); } catch { }
    }
    setIsBatchPaused(false);
    setBatchPauseReason(null);
  };

  // Circuit breaker: attempt reconnection during batch transfer
  const attemptBatchReconnect = async (): Promise<boolean> => {
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const isProvider = usesProviderApi(protocol);

    circuitBreaker.markReconnecting();
    notify.info(t('toast.reconnecting'), t('transfer.circuitBreaker.attemptingReconnect'));

    try {
      if (isProvider) {
        await invoke('provider_keep_alive');
      } else {
        await invoke('reconnect_ftp');
      }
      circuitBreaker.markReconnected();
      notify.success(t('toast.reconnected'), t('transfer.circuitBreaker.reconnectSuccess'));
      return true;
    } catch {
      circuitBreaker.markReconnectFailed();
      return false;
    }
  };

  // Circuit breaker: pause batch and wait for user decision
  // Tracks consecutive resume attempts per file index to prevent infinite i-- loops
  const batchResumeAttemptsRef = React.useRef({ fileIndex: -1, count: 0 });

  const waitForBatchResume = (currentFileIndex: number): Promise<'resume' | 'cancel'> => {
    // Track resume attempts for same file index
    if (batchResumeAttemptsRef.current.fileIndex === currentFileIndex) {
      batchResumeAttemptsRef.current.count++;
    } else {
      batchResumeAttemptsRef.current = { fileIndex: currentFileIndex, count: 1 };
    }

    // Auto-cancel after too many resume attempts on same file
    if (batchResumeAttemptsRef.current.count > 3) {
      notify.error(t('transfer.circuitBreaker.batchStopped'), t('transfer.circuitBreaker.reconnectFailed'));
      setIsBatchPaused(false);
      setBatchPauseReason(null);
      return Promise.resolve('cancel');
    }

    setIsBatchPaused(true);
    setBatchPauseReason(t('transfer.circuitBreaker.connectionLost'));
    return new Promise<'resume' | 'cancel'>((resolve) => {
      batchResumeResolverRef.current = (action) => {
        setIsBatchPaused(false);
        setBatchPauseReason(null);
        batchResumeResolverRef.current = null;
        resolve(action);
      };
    });
  };

  // Resume batch after pause — attempt connection verify first
  const resumeBatch = async () => {
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const isProvider = usesProviderApi(protocol);

    // Lightweight connection verify — warn on failure but still resume (CB will catch next error)
    try {
      if (isProvider) {
        await invoke('provider_keep_alive');
      } else {
        await invoke('reconnect_ftp');
      }
    } catch {
      notify.warning?.(t('transfer.circuitBreaker.reconnectFailed')) ??
        notify.error(t('toast.connectionLostTitle'), t('transfer.circuitBreaker.reconnectFailed'));
    }

    circuitBreaker.reset();
    if (batchResumeResolverRef.current) {
      batchResumeResolverRef.current('resume');
    }
  };

  // Retry all failed items in queue
  const retryAllFailedItems = async () => {
    const failedIds = transferQueue.retryAllFailed();
    for (const id of failedIds) {
      if (batchCancelledRef.current) break;
      const cb = retryCallbacksRef.current.get(id);
      if (cb) await cb();
    }
  };

  // ======== File Clipboard (Cut/Copy/Paste) ========

  const clipboardCopy = (files: { name: string; path: string; is_dir: boolean }[], isRemote: boolean, sourceDir: string) => {
    fileClipboardRef.current = { files, sourceDir, isRemote, operation: 'copy' };
    setHasClipboard(true);
    notify.success(t('contextMenu.copied') || 'Copied', `${files.length} file(s)`);
  };

  const clipboardCut = (files: { name: string; path: string; is_dir: boolean }[], isRemote: boolean, sourceDir: string) => {
    fileClipboardRef.current = { files, sourceDir, isRemote, operation: 'cut' };
    setHasClipboard(true);
    notify.success(t('contextMenu.cut') || 'Cut', `${files.length} file(s)`);
  };

  const clipboardPaste = async (targetIsRemote: boolean, targetDir: string) => {
    const cb = fileClipboardRef.current;
    if (!cb) return;

    const { files, isRemote: sourceIsRemote, operation, sourceDir } = cb;

    // Cross-panel paste → upload or download using clipboard paths directly
    if (sourceIsRemote !== targetIsRemote) {
      if (sourceIsRemote) {
        // Remote → Local: download each file using stored path (not current listing)
        for (const file of files) {
          try {
            if (batchCancelledRef.current) break;
            await downloadFile(file.path, file.name, currentLocalPath, file.is_dir);
          } catch (e) {
            if (!batchCancelledRef.current) {
              notify.error(t('toast.downloadFailed'), `${file.name}: ${String(e)}`);
            }
          }
        }
        loadLocalFiles(currentLocalPath);
      } else {
        // Local → Remote: upload each file using stored path (not current listing)
        for (const file of files) {
          try {
            if (batchCancelledRef.current) break;
            await uploadFile(file.path, file.name, file.is_dir);
          } catch (e) {
            if (!batchCancelledRef.current) {
              notify.error(t('toast.uploadFailed'), `${file.name}: ${String(e)}`);
            }
          }
        }
        loadRemoteFiles();
      }
      if (operation === 'cut') {
        // Delete source after successful transfer
        for (const file of files) {
          try {
            if (sourceIsRemote) {
              const activeSession = sessions.find(s => s.id === activeSessionId);
              const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
              if (usesProviderApi(protocol)) {
                await invoke('provider_delete_file', { path: file.path });
              } else {
                await invoke('delete_remote_file', { path: file.path });
              }
            } else {
              await invoke('delete_local_file', { path: file.path });
            }
          } catch (e) {
            console.error(`Failed to delete source after cut: ${file.name}`, e);
          }
        }
        if (sourceIsRemote) loadRemoteFiles(undefined, true);
        else loadLocalFiles(currentLocalPath);
      }
    }
    // Same-panel paste
    else {
      if (operation === 'cut') {
        // Move files to target directory
        const activeSession = sessions.find(s => s.id === activeSessionId);
        const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
        const useProvider = usesProviderApi(protocol) && protocol !== 'sftp';

        for (const file of files) {
          const sep = targetIsRemote ? '/' : (targetDir.includes('\\') ? '\\' : '/');
          const destPath = `${targetDir}${targetDir.endsWith(sep) ? '' : sep}${file.name}`;
          try {
            if (targetIsRemote) {
              if (useProvider) {
                await invoke('provider_rename', { from: file.path, to: destPath });
              } else {
                await invoke('rename_remote_file', { from: file.path, to: destPath });
              }
            } else {
              await invoke('rename_local_file', { from: file.path, to: destPath });
            }
          } catch (e) {
            notify.error(t('toast.renameFailed'), `${file.name}: ${String(e)}`);
          }
        }
        if (targetIsRemote) loadRemoteFiles(undefined, true);
        else loadLocalFiles(currentLocalPath);
      } else {
        // Copy files within same panel
        if (!targetIsRemote) {
          // Local copy
          for (const file of files) {
            const sep = targetDir.includes('\\') ? '\\' : '/';
            const destPath = `${targetDir}${targetDir.endsWith(sep) ? '' : sep}${file.name}`;
            if (destPath === file.path) {
              // Same directory: add " (copy)" suffix
              const ext = file.name.includes('.') ? '.' + file.name.split('.').pop() : '';
              const baseName = ext ? file.name.slice(0, -ext.length) : file.name;
              const copyName = `${baseName} (copy)${ext}`;
              const copyDest = `${targetDir}${targetDir.endsWith(sep) ? '' : sep}${copyName}`;
              try {
                await invoke('copy_local_file', { from: file.path, to: copyDest });
              } catch (e) {
                notify.error(t('toast.copyFailed'), `${file.name}: ${String(e)}`);
              }
            } else {
              try {
                await invoke('copy_local_file', { from: file.path, to: destPath });
              } catch (e) {
                notify.error(t('toast.copyFailed'), `${file.name}: ${String(e)}`);
              }
            }
          }
          loadLocalFiles(currentLocalPath);
        } else {
          // Remote copy via server-side copy API
          let anyFailed = false;
          for (const file of files) {
            const destPath = `${targetDir}${targetDir.endsWith('/') ? '' : '/'}${file.name}`;
            const from = file.path || `${targetDir}${targetDir.endsWith('/') ? '' : '/'}${file.name}`;
            // Same directory: add " (copy)" suffix
            const finalDest = destPath === from
              ? (() => {
                const ext = file.name.includes('.') ? '.' + file.name.split('.').pop() : '';
                const baseName = ext ? file.name.slice(0, -ext.length) : file.name;
                return `${targetDir}${targetDir.endsWith('/') ? '' : '/'}${baseName} (copy)${ext}`;
              })()
              : destPath;
            try {
              await invoke('provider_server_copy', { from, to: finalDest });
            } catch (e) {
              notify.error(t('toast.copyFailed'), `${file.name}: ${String(e)}`);
              anyFailed = true;
            }
          }
          if (!anyFailed && files.length > 0) {
            notify.info(t('toast.copyCompleted'), files.length === 1 ? files[0].name : `${files.length} ${t('browser.files')}`);
          }
          loadRemoteFiles(undefined, true);
        }
      }
    }

    // Clear clipboard after cut (not after copy, so user can paste multiple times)
    if (operation === 'cut') {
      fileClipboardRef.current = null;
      setHasClipboard(false);
    }
  };

  // openDevToolsPreview, openUniversalPreview, closeUniversalPreview provided by usePreview hook

  // Upload files (Selected or Dialog)
  const uploadMultipleFiles = async (filesOverride?: string[]) => {
    if (!isConnected) return;

    // Reset apply-to-all for new batch
    resetOverwriteSettings();

    // Use override or fallback to selected state
    const targetNames = filesOverride || Array.from(selectedLocalFiles);

    // Priority 1: Upload specific target files
    if (targetNames.length > 0) {
      const filesToUpload = targetNames.map(name => {
        const file = localFiles.find(f => f.name === name);
        // Use verified absolute path from backend
        return file ? { path: file.path, file } : null;
      }).filter(Boolean) as { path: string; file: LocalFile }[];

      if (filesToUpload.length > 0) {
        const activeSession = sessions.find(s => s.id === activeSessionId);
        const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
        const isProvider = usesProviderApi(protocol);
        const isGitHubRepoMode = (protocol === 'github' && !currentRemotePath.startsWith('/.github-releases')) || protocol === 'gitlab';
        let batchCommitMessage: string | undefined;

        if (isGitHubRepoMode) {
          try {
            const syncStatus = await invoke<{
              is_local_repo: boolean;
              repo_matches?: boolean;
              unpushed_count?: number;
              branch?: string;
            }>('github_check_local_sync', { localPath: currentLocalPath });
            if (syncStatus.is_local_repo && syncStatus.repo_matches && (syncStatus.unpushed_count || 0) > 0) {
              const action = await new Promise<'push' | 'continue' | 'cancel'>((resolve) => {
                setGitHubSyncWarning({
                  unpushedCount: syncStatus.unpushed_count!,
                  branch: syncStatus.branch || 'main',
                  resolve,
                });
              });
              setGitHubSyncWarning(null);
              if (action === 'cancel') return;
              if (action === 'push') {
                try {
                  await invoke('github_push_local', { localPath: currentLocalPath });
                  notify.success('Local commits pushed to GitHub');
                } catch (e) {
                  notify.error('Push failed', String(e));
                  return;
                }
              }
            }
          } catch { /* git not available or not a repo — continue normally */ }
        }

        if (isGitHubRepoMode) {
          const commitMessage = await requestGitHubBatchCommitMessage(
            filesToUpload.map(({ path: filePath, file }) => ({
              local: filePath,
              remote: `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${file.name}`,
            })),
            'upload',
          );

          if (!commitMessage) {
            return;
          }

          batchCommitMessage = commitMessage;
        }

        const canUseNativeUploadBatch = !isProvider
          && !isGitHubRepoMode
          && filesToUpload.length > 1
          && filesToUpload.every(({ file }) => !file.is_dir);

        if (canUseNativeUploadBatch) {
          batchCancelledRef.current = false;
          cancelLevelRef.current = 0;
          circuitBreaker.reset();
          try { await invoke('reset_cancel_flag'); } catch { }

          let skippedCount = 0;
          const entries: Array<{
            id: string;
            display_name: string;
            remote_path: string;
            local_path: string;
            size: number;
            modified: string | null;
          }> = [];

          for (let i = 0; i < filesToUpload.length; i++) {
            const { path: filePath, file } = filesToUpload[i];
            const remainingInQueue = filesToUpload.length - i - 1;
            const overwriteResult = await checkOverwrite(
              file.name,
              file.size || 0,
              file.modified ? new Date(file.modified) : undefined,
              false,
              remainingInQueue
            );

            if (overwriteResult.action === 'cancel') {
              resetOverwriteSettings();
              folderOverwriteApplyToAll.current = { action: 'merge_overwrite', enabled: false };
              return;
            }

            if (overwriteResult.action === 'skip') {
              humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: file.name }, 'success');
              skippedCount++;
              continue;
            }

            const finalName = overwriteResult.newName || file.name;
            entries.push({
              id: '',
              display_name: finalName,
              remote_path: `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${finalName}`,
              local_path: filePath,
              size: file.size || 0,
              modified: null,
            });
          }

          resetOverwriteSettings();
          folderOverwriteApplyToAll.current = { action: 'merge_overwrite', enabled: false };

          if (entries.length === 0) {
            if (skippedCount > 0) notify.info(t('toast.fileSkipped', { count: skippedCount }));
            setSelectedLocalFiles(new Set());
            return;
          }

          for (const entry of entries) {
            transferQueue.addItem(entry.display_name, entry.remote_path, entry.size, 'upload');
          }

          try {
            await invoke<string>('upload_files_batch', {
              params: {
                entries,
                max_concurrent: effectiveMaxConcurrentTransfers,
                retry_count: retryCount,
                timeout_seconds: timeoutSeconds,
              }
            });
          } catch (error) {
            if (!batchCancelledRef.current) {
              notify.error(t('toast.uploadFailed'), String(error));
            }
          } finally {
            retryCallbacksRef.current.clear();
          }

          if (skippedCount > 0) {
            notify.info(t('toast.fileSkipped', { count: skippedCount }));
          }
          setSelectedLocalFiles(new Set());
          loadRemoteFiles();
          return;
        }

        // Queue shows progress - no toast needed

        // Add all files to queue first
        const queueItems = filesToUpload.map(({ path: filePath, file }) => {
          const fileName = filePath.split(/[/\\]/).pop() || filePath;
          const size = file?.size || 0;
          const id = transferQueue.addItem(fileName, filePath, size, 'upload');
          retryCallbacksRef.current.set(id, async () => {
            transferQueue.startTransfer(id);
            try {
              await uploadFile(filePath, fileName, file?.is_dir || false, file?.size || undefined, false, batchCommitMessage);
              transferQueue.completeTransfer(id);
            } catch (error) {
              transferQueue.failTransfer(id, String(error));
            }
          });
          return { id, filePath, fileName, file };
        });

        // Reset cancel flags and circuit breaker before starting batch
        batchCancelledRef.current = false;
        cancelLevelRef.current = 0;
        circuitBreaker.reset();
        try { await invoke('reset_cancel_flag'); } catch { }

        // GitHub atomic batch upload: commit all non-dir files in a single commit
        if (isGitHubRepoMode && batchCommitMessage && queueItems.filter(i => !i.file.is_dir).length >= 1) {
          const nonDirItems = queueItems.filter(i => !i.file.is_dir);
          const totalSize = nonDirItems.reduce((sum, i) => sum + (i.file.size || 0), 0);
          const MAX_BATCH_SIZE = 50 * 1024 * 1024; // 50 MB GraphQL limit

          if (totalSize <= MAX_BATCH_SIZE) {
            try {
              const files = nonDirItems.map(item => ({
                localPath: item.filePath,
                remotePath: `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${item.fileName}`,
              }));
              const batchCommand = protocol === 'gitlab' ? 'gitlab_batch_upload' : 'github_batch_upload';
              const result = await invoke<{ commit_sha: string; files_count: number }>(
                batchCommand, { files, message: batchCommitMessage }
              );
              // Mark all as complete
              for (const item of nonDirItems) {
                transferQueue.completeTransfer(item.id);
              }
              notify.success(
                `Atomic commit: ${result.files_count} files`,
                result.commit_sha.substring(0, 7)
              );
            } catch (error) {
              for (const item of nonDirItems) {
                transferQueue.failTransfer(item.id, String(error));
              }
              notify.error('Batch upload failed', String(error));
            }
            // Process remaining dir items sequentially if any
            const dirItems = queueItems.filter(i => i.file.is_dir);
            if (dirItems.length > 0) {
              for (const item of dirItems) {
                transferQueue.startTransfer(item.id);
                try {
                  await uploadFile(item.filePath, item.fileName, true, item.file.size || undefined, false, batchCommitMessage);
                  transferQueue.completeTransfer(item.id);
                } catch (error) {
                  transferQueue.failTransfer(item.id, String(error));
                }
              }
            }
            loadRemoteFiles(undefined, true);
            return;
          }
          // If total > 50MB, fall through to sequential upload
        }

        // Upload sequentially with queue tracking and overwrite checking
        let skippedCount = 0;
        for (let i = 0; i < queueItems.length; i++) {
          const item = queueItems[i];
          const remainingInQueue = queueItems.length - i - 1;

          if (item.file.is_dir) {
            // Folder: show FolderOverwriteDialog in 'ask' mode
            const folderResult = await checkFolderOverwrite(item.fileName, 'upload', remainingInQueue);
            if (folderResult.action === 'cancel') {
              transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
              for (let j = i + 1; j < queueItems.length; j++) {
                transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
              }
              break;
            }
            if (folderResult.action === 'skip') {
              transferQueue.completeTransfer(item.id);
              skippedCount++;
              continue;
            }
          } else {
            // File: use standard overwrite check
            const overwriteResult = await checkOverwrite(
              item.fileName,
              item.file.size || 0,
              item.file.modified ? new Date(item.file.modified) : undefined,
              false, // sourceIsRemote = false for upload
              remainingInQueue
            );

            if (overwriteResult.action === 'cancel') {
              transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
              for (let j = i + 1; j < queueItems.length; j++) {
                transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
              }
              break;
            }

            if (overwriteResult.action === 'skip') {
              transferQueue.completeTransfer(item.id);
              humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: item.fileName }, 'success');
              skippedCount++;
              continue;
            }

            // Use renamed target if rename was chosen
            if (overwriteResult.newName) {
              item.fileName = overwriteResult.newName;
            }
          }

          // Skip if batch was cancelled
          if (batchCancelledRef.current) {
            transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
            continue;
          }

          // Circuit breaker: check before attempting file
          if (circuitBreaker.stateRef.current === 'open') {
            const reason = circuitBreaker.pauseReasonRef.current;
            const errorKind = circuitBreaker.tripErrorKindRef.current;

            if (reason === 'fatal_error') {
              const label = errorKind ? t(getErrorKindI18nKey(errorKind)) : 'Fatal error';
              for (let j = i; j < queueItems.length; j++) {
                transferQueue.failTransfer(queueItems[j].id, label);
              }
              notify.error(t('transfer.circuitBreaker.batchStopped'), t('transfer.circuitBreaker.fatalError', { reason: label }));
              break;
            }

            if (errorKind && RECONNECT_ERROR_KINDS.has(errorKind)) {
              const reconnected = await attemptBatchReconnect();
              if (!reconnected) {
                const decision = await waitForBatchResume(i);
                if (decision === 'cancel') {
                  for (let j = i; j < queueItems.length; j++) {
                    transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
                  }
                  break;
                }
                i--;
                continue;
              }
            } else {
              // Non-network consecutive errors (rate_limit, unknown) — pause and wait
              const decision = await waitForBatchResume(i);
              if (decision === 'cancel') {
                for (let j = i; j < queueItems.length; j++) {
                  transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
                }
                break;
              }
              circuitBreaker.reset();
              i--;
              continue;
            }
          }

          // Per-file retry with exponential backoff
          transferQueue.startTransfer(item.id);
          for (let attempt = 0; attempt <= circuitBreaker.config.maxRetriesPerFile; attempt++) {
            if (attempt > 0) {
              await new Promise(r => setTimeout(r, circuitBreaker.getRetryDelay(attempt)));
              transferQueue.startTransfer(item.id);
            }
            try {
              await uploadFile(item.filePath, item.fileName, item.file?.is_dir || false, item.file?.size || undefined, true, batchCommitMessage);
              transferQueue.completeTransfer(item.id);
              circuitBreaker.recordSuccess();
              break;
            } catch (error) {
              const result = circuitBreaker.recordFailure(String(error));
              if (result.isFatal || !result.retryable || result.shouldPause || attempt >= circuitBreaker.config.maxRetriesPerFile) {
                transferQueue.failTransfer(item.id, String(error));
                break;
              }
            }
          }
        }

        // Reset apply-to-all and cleanup after batch completes
        resetOverwriteSettings();
        folderOverwriteApplyToAll.current = { action: 'merge_overwrite', enabled: false };
        retryCallbacksRef.current.clear();

        // Queue shows completion - no toast needed
        if (skippedCount > 0) {
          notify.info(t('toast.fileSkipped', { count: skippedCount }));
        }
        setSelectedLocalFiles(new Set());
        loadRemoteFiles();
        return;
      }
    }

    // Priority 2: Open Dialog if no selection
    const selected = await open({
      multiple: true,
      directory: false,
      title: t('dialog.selectFilesToUpload'),
    });

    if (!selected) return;
    const files = Array.isArray(selected) ? selected : [selected];

    if (files.length > 0) {
      // Reset for dialog-selected files too
      resetOverwriteSettings();
      batchCancelledRef.current = false;
      cancelLevelRef.current = 0;
      circuitBreaker.reset();
      try { await invoke('reset_cancel_flag'); } catch { }
      let skippedCount = 0;

      // Add to transfer queue for tracking
      const queueItems = files.map(filePath => {
        const fileName = filePath.replace(/^.*[\\\/]/, '');
        const id = transferQueue.addItem(fileName, filePath, 0, 'upload');
        retryCallbacksRef.current.set(id, async () => {
          transferQueue.startTransfer(id);
          try {
            await uploadFile(filePath, fileName, false, undefined, true);
            transferQueue.completeTransfer(id);
          } catch (error) {
            transferQueue.failTransfer(id, String(error));
          }
        });
        return { id, filePath, fileName };
      });

      for (let i = 0; i < queueItems.length; i++) {
        const item = queueItems[i];
        const remainingInQueue = queueItems.length - i - 1;

        // Check for overwrite
        const overwriteResult = await checkOverwrite(
          item.fileName,
          0, // Size unknown from dialog
          undefined, // Modified unknown from dialog
          false, // sourceIsRemote = false for upload
          remainingInQueue
        );

        if (overwriteResult.action === 'cancel') {
          transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
          for (let j = i + 1; j < queueItems.length; j++) {
            transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
          }
          break;
        }

        if (overwriteResult.action === 'skip') {
          transferQueue.completeTransfer(item.id);
          humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: item.fileName }, 'success');
          skippedCount++;
          continue;
        }

        if (overwriteResult.newName) {
          item.fileName = overwriteResult.newName;
        }

        if (batchCancelledRef.current) {
          transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
          continue;
        }

        // Circuit breaker: check before attempting file
        if (circuitBreaker.stateRef.current === 'open') {
          const reason = circuitBreaker.pauseReasonRef.current;
          const errorKind = circuitBreaker.tripErrorKindRef.current;

          if (reason === 'fatal_error') {
            const label = errorKind ? t(getErrorKindI18nKey(errorKind)) : 'Fatal error';
            for (let j = i; j < queueItems.length; j++) {
              transferQueue.failTransfer(queueItems[j].id, label);
            }
            notify.error(t('transfer.circuitBreaker.batchStopped'), t('transfer.circuitBreaker.fatalError', { reason: label }));
            break;
          }

          if (errorKind && RECONNECT_ERROR_KINDS.has(errorKind)) {
            const reconnected = await attemptBatchReconnect();
            if (!reconnected) {
              const decision = await waitForBatchResume(i);
              if (decision === 'cancel') {
                for (let j = i; j < queueItems.length; j++) {
                  transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
                }
                break;
              }
              i--;
              continue;
            }
          } else {
            // Non-network consecutive errors (rate_limit, unknown) — pause and wait
            const decision = await waitForBatchResume(i);
            if (decision === 'cancel') {
              for (let j = i; j < queueItems.length; j++) {
                transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
              }
              break;
            }
            circuitBreaker.reset();
            i--;
            continue;
          }
        }

        // Per-file retry with exponential backoff
        transferQueue.startTransfer(item.id);
        for (let attempt = 0; attempt <= circuitBreaker.config.maxRetriesPerFile; attempt++) {
          if (attempt > 0) {
            await new Promise(r => setTimeout(r, circuitBreaker.getRetryDelay(attempt)));
            transferQueue.startTransfer(item.id);
          }
          try {
            await uploadFile(item.filePath, item.fileName, false, undefined, true);
            transferQueue.completeTransfer(item.id);
            circuitBreaker.recordSuccess();
            break;
          } catch (error) {
            const result = circuitBreaker.recordFailure(String(error));
            if (result.isFatal || !result.retryable || result.shouldPause || attempt >= circuitBreaker.config.maxRetriesPerFile) {
              transferQueue.failTransfer(item.id, String(error));
              break;
            }
          }
        }
      }

      resetOverwriteSettings();
      retryCallbacksRef.current.clear();
      if (skippedCount > 0) {
        notify.info(t('toast.fileSkipped', { count: skippedCount }));
      }
    }
  };

  // === Bulk Operations ===
  const downloadMultipleFiles = async (filesOverride?: string[]) => {
    if (!isConnected) return;
    const names = filesOverride || Array.from(selectedRemoteFiles);
    if (names.length === 0) return;

    // Reset apply-to-all for new batch
    resetOverwriteSettings();

    const filesToDownload = names.map(n => remoteFiles.find(f => f.name === n)).filter(Boolean) as RemoteFile[];
    if (filesToDownload.length > 0) {
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = usesProviderApi(protocol);

      const canUseNativeDownloadBatch = !isProvider
        && filesToDownload.length > 1
        && filesToDownload.every(file => !file.is_dir);

      if (canUseNativeDownloadBatch) {
        batchCancelledRef.current = false;
        cancelLevelRef.current = 0;
        circuitBreaker.reset();
        try { await invoke('reset_cancel_flag'); } catch { }

        let skippedCount = 0;
        const entries: Array<{
          id: string;
          display_name: string;
          remote_path: string;
          local_path: string;
          size: number;
          modified: string | null;
        }> = [];

        for (let i = 0; i < filesToDownload.length; i++) {
          const file = filesToDownload[i];
          const remainingInQueue = filesToDownload.length - i - 1;
          const overwriteResult = await checkOverwrite(
            file.name,
            file.size || 0,
            file.modified ? new Date(file.modified) : undefined,
            true,
            remainingInQueue
          );

          if (overwriteResult.action === 'cancel') {
            resetOverwriteSettings();
            return;
          }

          if (overwriteResult.action === 'skip') {
            humanLog.logRaw('activity.download_skipped', 'DOWNLOAD', { filename: file.name }, 'success');
            skippedCount++;
            continue;
          }

          const finalName = overwriteResult.newName || file.name;
          entries.push({
            id: '',
            display_name: finalName,
            remote_path: file.path,
            local_path: `${currentLocalPath}/${finalName}`,
            size: file.size || 0,
            modified: file.modified || null,
          });
        }

        resetOverwriteSettings();

        if (entries.length === 0) {
          if (skippedCount > 0) notify.info(t('toast.fileSkipped', { count: skippedCount }));
          setSelectedRemoteFiles(new Set());
          return;
        }

        for (const entry of entries) {
          transferQueue.addItem(entry.display_name, entry.remote_path, entry.size, 'download');
        }

        try {
          await invoke<string>('download_files_batch', {
            params: {
              entries,
              max_concurrent: effectiveMaxConcurrentTransfers,
              retry_count: retryCount,
              timeout_seconds: timeoutSeconds,
            }
          });
        } catch (error) {
          if (!batchCancelledRef.current) {
            notify.error(t('toast.downloadFailed'), String(error));
          }
        } finally {
          retryCallbacksRef.current.clear();
        }

        if (skippedCount > 0) {
          notify.info(t('toast.fileSkipped', { count: skippedCount }));
        }
        setSelectedRemoteFiles(new Set());
        await loadLocalFiles(currentLocalPath);
        return;
      }

      // Queue shows progress - no toast needed

      // Add all files to queue first
      const queueItems = filesToDownload.map(file => {
        const id = transferQueue.addItem(file.name, file.path, file.size || 0, 'download');
        retryCallbacksRef.current.set(id, async () => {
          transferQueue.startTransfer(id);
          try {
            await downloadFile(file.path, file.name, currentLocalPath, file.is_dir, file.size || undefined);
            transferQueue.completeTransfer(id);
          } catch (error) {
            transferQueue.failTransfer(id, String(error));
          }
        });
        return { id, file };
      });

      // Reset cancel flags and circuit breaker before starting batch
      batchCancelledRef.current = false;
      cancelLevelRef.current = 0;
      circuitBreaker.reset();
      try { await invoke('reset_cancel_flag'); } catch { }

      // Download sequentially with queue tracking and overwrite checking
      let skippedCount = 0;
      for (let i = 0; i < queueItems.length; i++) {
        const item = queueItems[i];
        const remainingInQueue = queueItems.length - i - 1;

        if (item.file.is_dir) {
          // Folder: show FolderOverwriteDialog in 'ask' mode
          const folderResult = await checkFolderOverwrite(item.file.name, 'download', remainingInQueue);
          if (folderResult.action === 'cancel') {
            transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
            for (let j = i + 1; j < queueItems.length; j++) {
              transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
            }
            break;
          }
          if (folderResult.action === 'skip') {
            transferQueue.completeTransfer(item.id);
            skippedCount++;
            continue;
          }
        } else {
          // File: use standard overwrite check
          const overwriteResult = await checkOverwrite(
            item.file.name,
            item.file.size || 0,
            item.file.modified ? new Date(item.file.modified) : undefined,
            true, // sourceIsRemote
            remainingInQueue
          );

          if (overwriteResult.action === 'cancel') {
            transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
            for (let j = i + 1; j < queueItems.length; j++) {
              transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
            }
            break;
          }

          if (overwriteResult.action === 'skip') {
            transferQueue.completeTransfer(item.id);
            humanLog.logRaw('activity.download_skipped', 'DOWNLOAD', { filename: item.file.name }, 'success');
            skippedCount++;
            continue;
          }

          // Use renamed target if rename was chosen
          if (overwriteResult.newName) {
            item.file = { ...item.file, name: overwriteResult.newName };
          }
        }

        // Skip if batch was cancelled
        if (batchCancelledRef.current) {
          transferQueue.failTransfer(item.id, t('transfer.cancelledByUser'));
          continue;
        }

        // Circuit breaker: check before attempting file
        if (circuitBreaker.stateRef.current === 'open') {
          const reason = circuitBreaker.pauseReasonRef.current;
          const errorKind = circuitBreaker.tripErrorKindRef.current;

          if (reason === 'fatal_error') {
            const label = errorKind ? t(getErrorKindI18nKey(errorKind)) : 'Fatal error';
            for (let j = i; j < queueItems.length; j++) {
              transferQueue.failTransfer(queueItems[j].id, label);
            }
            notify.error(t('transfer.circuitBreaker.batchStopped'), t('transfer.circuitBreaker.fatalError', { reason: label }));
            break;
          }

          if (errorKind && RECONNECT_ERROR_KINDS.has(errorKind)) {
            const reconnected = await attemptBatchReconnect();
            if (!reconnected) {
              const decision = await waitForBatchResume(i);
              if (decision === 'cancel') {
                for (let j = i; j < queueItems.length; j++) {
                  transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
                }
                break;
              }
              i--;
              continue;
            }
          } else {
            // Non-network consecutive errors (rate_limit, unknown) — pause and wait
            const decision = await waitForBatchResume(i);
            if (decision === 'cancel') {
              for (let j = i; j < queueItems.length; j++) {
                transferQueue.failTransfer(queueItems[j].id, t('transfer.cancelledByUser'));
              }
              break;
            }
            circuitBreaker.reset();
            i--;
            continue;
          }
        }

        // Per-file retry with exponential backoff
        transferQueue.startTransfer(item.id);
        for (let attempt = 0; attempt <= circuitBreaker.config.maxRetriesPerFile; attempt++) {
          if (attempt > 0) {
            await new Promise(r => setTimeout(r, circuitBreaker.getRetryDelay(attempt)));
            transferQueue.startTransfer(item.id);
          }
          try {
            await downloadFile(item.file.path, item.file.name, currentLocalPath, item.file.is_dir, item.file.size || undefined, true);
            transferQueue.completeTransfer(item.id);
            circuitBreaker.recordSuccess();
            break;
          } catch (error) {
            const result = circuitBreaker.recordFailure(String(error));
            if (result.isFatal || !result.retryable || result.shouldPause || attempt >= circuitBreaker.config.maxRetriesPerFile) {
              transferQueue.failTransfer(item.id, String(error));
              break;
            }
          }
        }
      }

      // Reset apply-to-all and cleanup after batch completes
      resetOverwriteSettings();
      retryCallbacksRef.current.clear();

      // Queue shows completion - no toast needed
      if (skippedCount > 0) {
        notify.info(t('toast.fileSkipped', { count: skippedCount }));
      }
      setSelectedRemoteFiles(new Set());
      await loadLocalFiles(currentLocalPath);  // Refresh local panel
    }
  };

  // Wire cross-panel drag & drop callback now that upload/download are defined
  crossPanelDropRef.current = async (files, fromRemote, _targetDir) => {
    if (!isConnected || files.length === 0) return;
    if (fromRemote) {
      await downloadMultipleFiles(files.map(f => f.name));
    } else {
      await uploadMultipleFiles(files.map(f => f.name));
    }
  };

  const deleteMultipleRemoteFiles = (filesOverride?: string[]) => {
    const names = filesOverride || Array.from(selectedRemoteFiles);
    if (names.length === 0) return;

    const performDelete = async () => {
      const deletedFiles: string[] = [];
      const deletedFolders: string[] = [];
      // Get protocol from active session as fallback (outside loop for efficiency)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = usesProviderApi(protocol);
      const isGitHubRepoMode = (protocol === 'github' && !currentRemotePath.startsWith('/.github-releases')) || protocol === 'gitlab';
      let batchCommitMessage: string | undefined;

      if (isGitHubRepoMode) {
        const selectedFiles = names
          .map(name => remoteFiles.find(f => f.name === name))
          .filter(Boolean) as RemoteFile[];
        const commitMessage = await requestGitHubBatchCommitMessage(
          selectedFiles.map((file) => ({ local: '', remote: file.path })),
          'delete',
        );

        if (!commitMessage) {
          return;
        }

        batchCommitMessage = commitMessage;
      }

      // Only create frontend log for provider deletes — FTP/SFTP backend emits
      // transfer events (delete_start/delete_complete) that are logged by useTransferEvents
      const logId = isProvider ? humanLog.logStart('DELETE_MULTIPLE', { count: names.length, isRemote: true }) : null;

      // Reset cancel flags before batch delete
      batchCancelledRef.current = false;
      cancelLevelRef.current = 0;
      try { await invoke('reset_cancel_flag'); } catch { }

      // GitHub atomic batch delete: single commit for all non-dir files
      if (isGitHubRepoMode && batchCommitMessage) {
        const resolvedFiles = names
          .map(name => remoteFiles.find(f => f.name === name))
          .filter(Boolean) as RemoteFile[];
        const filePaths = resolvedFiles.filter(f => !f.is_dir).map(f => f.path);
        const dirFiles = resolvedFiles.filter(f => f.is_dir);

        // Atomic delete for all non-dir files in one commit
        if (filePaths.length > 0) {
          try {
            const deleteCommand = protocol === 'gitlab' ? 'gitlab_batch_delete' : 'github_batch_delete';
            await invoke(deleteCommand, {
              paths: filePaths,
              message: batchCommitMessage,
            });
            deletedFiles.push(...filePaths);
          } catch (err) {
            notify.error(t('toast.deleteFail'), String(err));
          }
        }

        // Dirs: sequential delete (GitHub needs recursive tree traversal)
        for (const dir of dirFiles) {
          if (batchCancelledRef.current) break;
          try {
            await invoke('provider_delete_dir', { path: dir.path, recursive: true, commitMessage: batchCommitMessage });
            deletedFolders.push(dir.path);
          } catch (err) {
            if (batchCancelledRef.current) break;
            notify.error(t('toast.deleteFail'), `${dir.name}: ${String(err)}`);
          }
        }
      } else {
        // Show scanning toast for batch delete (provider path — FTP/SFTP uses backend events)
        if (isProvider && names.length > 1) {
          setScanningState({ active: true, folderName: `${names.length} items`, message: t('activity.delete_scanning') || `Deleting ${names.length} items...`, operation: 'delete' });
        }
        // Non-GitHub: sequential delete
        for (const name of names) {
          if (batchCancelledRef.current) break;

          const file = remoteFiles.find(f => f.name === name);
          if (file) {
            try {
              if (isProvider) {
                if (file.is_dir) {
                  await invoke('provider_delete_dir', { path: file.path, recursive: true, commitMessage: batchCommitMessage || null });
                } else {
                  await invoke('provider_delete_file', { path: file.path, commitMessage: batchCommitMessage || null });
                }
              } else {
                await invoke('delete_remote_file', { path: file.path, isDir: file.is_dir });
              }

              if (file.is_dir) {
                deletedFolders.push(file.path);
              } else {
                deletedFiles.push(file.path);
              }
            } catch (err) {
              if (batchCancelledRef.current) break;
              notify.error(t('toast.deleteFail'), `${name}: ${String(err)}`);
            }
          }
        }
      }
      // Dismiss scanning toast
      setScanningState(INITIAL_SCANNING_STATE);

      await loadRemoteFiles(undefined, true);
      setSelectedRemoteFiles(new Set());
      // Summary log for provider deletes only (FTP/SFTP is handled by useTransferEvents)
      if (logId) {
        const loc = t('browser.remote');
        const count = deletedFolders.length + deletedFiles.length;
        if (count === 1 && deletedFolders.length === 1) {
          humanLog.updateEntry(logId, { status: 'success', message: t('activity.delete_dir_success', { location: loc, filename: deletedFolders[0] }) });
        } else if (count === 1 && deletedFiles.length === 1) {
          humanLog.updateEntry(logId, { status: 'success', message: t('activity.delete_file_success', { location: loc, filename: deletedFiles[0] }) });
        } else {
          const allDeleted = [...deletedFolders.map(n => `📁 ${n}`), ...deletedFiles.map(n => `📄 ${n}`)];
          humanLog.updateEntry(logId, {
            status: 'success',
            message: `[${loc}] ${t('activity.delete_multiple_success', { count })}`,
            details: allDeleted.join('\n')
          });
        }
      }
      const totalDeleted = deletedFolders.length + deletedFiles.length;
      notify.success(t('toast.deleted'), t('toast.deleteSuccess', { count: totalDeleted }));
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: t('dialog.deleteSelectedItems', { count: names.length }),
        onConfirm: async () => {
          setConfirmDialog(null);
          await performDelete();
        }
      });
    } else {
      performDelete();
    }
  };

  const deleteMultipleLocalFiles = (filesOverride?: string[]) => {
    const names = filesOverride || Array.from(selectedLocalFiles);
    if (names.length === 0) return;

    const performDelete = async () => {
      const logId = humanLog.logStart('DELETE_MULTIPLE', { count: names.length, isRemote: false });
      const deletedFiles: string[] = [];
      const deletedFolders: string[] = [];
      const failedFiles: string[] = [];

      // Reset cancel flags before batch delete
      batchCancelledRef.current = false;
      cancelLevelRef.current = 0;

      // Show scanning toast for batch local delete
      if (names.length > 1) {
        setScanningState({ active: true, folderName: `${names.length} items`, message: t('activity.delete_scanning') || `Deleting ${names.length} items...`, operation: 'delete' });
      }

      for (const name of names) {
        if (batchCancelledRef.current) break;

        const file = localFiles.find(f => f.name === name);
        if (file) {
          try {
            await invoke('delete_to_trash', { path: file.path });
            if (file.is_dir) {
              deletedFolders.push(file.path);
            } else {
              deletedFiles.push(file.path);
            }
          } catch (err) {
            failedFiles.push(name);
            notify.error(t('toast.deleteFail'), `${name}: ${String(err)}`);
          }
        }
      }
      setScanningState(INITIAL_SCANNING_STATE);
      await loadLocalFiles(currentLocalPath);
      setSelectedLocalFiles(new Set());
      // Summary: same logic as remote (see deleteMultipleRemoteFiles)
      const loc = t('browser.local');
      const count = deletedFolders.length + deletedFiles.length;
      if (count === 1 && deletedFolders.length === 1) {
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.delete_dir_success', { location: loc, filename: deletedFolders[0] }) });
      } else if (count === 1 && deletedFiles.length === 1) {
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.delete_file_success', { location: loc, filename: deletedFiles[0] }) });
      } else {
        const allDeleted = [...deletedFolders.map(n => `📁 ${n}`), ...deletedFiles.map(n => `📄 ${n}`)];
        humanLog.updateEntry(logId, {
          status: 'success',
          message: `[${loc}] ${t('activity.delete_multiple_success', { count })}`,
          details: allDeleted.join('\n')
        });
      }
      const totalDeleted = deletedFolders.length + deletedFiles.length;
      notify.success(t('toast.deleted'), t('toast.deleteSuccess', { count: totalDeleted }));
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: t('dialog.deleteSelectedItems', { count: names.length }),
        onConfirm: async () => {
          setConfirmDialog(null);
          await performDelete();
        }
      });
    } else {
      performDelete();
    }
  };

  // File operations with proper confirm BEFORE action (respects confirmBeforeDelete setting)
  const deleteRemoteFile = (path: string, isDir: boolean, commitMessage?: string) => {
    const fileName = path.split(/[\\/]/).pop() || path;
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const isGitHubRepoMode = (protocol === 'github' && !path.startsWith('/.github-releases')) || protocol === 'gitlab';

    if (!isDir && isGitHubRepoMode && !commitMessage && gitHubRepoInfo && gitHubRepoInfo.writeModeKind !== 'unknown') {
      setGitHubCommitDialog({
        files: [{ local: '', remote: path }],
        operation: 'delete',
        branch: gitHubRepoInfo.branch,
        writeMode: gitHubRepoInfo.writeModeKind,
        workingBranch: gitHubRepoInfo.workingBranch || undefined,
        onCommit: (message: string) => {
          setGitHubCommitDialog(null);
          deleteRemoteFile(path, isDir, message);
        },
      });
      return;
    }

    const performDelete = async () => {
      const logId = humanLog.logStart('DELETE', { filename: path });
      try {
        const isProvider = usesProviderApi(protocol);

        if (isProvider) {
          if (isDir) {
            await invoke('provider_delete_dir', { path, recursive: true });
          } else {
            await invoke('provider_delete_file', { path, commitMessage: commitMessage || null });
          }
        } else {
          await invoke('delete_remote_file', { path, isDir });
        }
        humanLog.logSuccess('DELETE', { filename: path }, logId);
        notify.success(t('toast.deleted'), fileName);
        await loadRemoteFiles(undefined, true);
      }
      catch (error) {
        humanLog.logError('DELETE', { filename: path }, logId);
        notify.error(t('toast.deleteFail'), String(error));
      }
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: t('dialog.deleteFile', { name: fileName }),
        onConfirm: async () => {
          setConfirmDialog(null);
          await performDelete();
        }
      });
    } else {
      performDelete();
    }
  };

  const deleteLocalFile = (path: string) => {
    const fileName = path.split(/[\\/]/).pop() || path;

    const performDelete = async () => {
      const logId = humanLog.logStart('DELETE', { filename: path });
      try {
        await invoke('delete_to_trash', { path });
        humanLog.logSuccess('DELETE', { filename: path }, logId);
        notify.success(t('toast.deleted'), fileName);
        await loadLocalFiles(currentLocalPath);
      }
      catch (error) {
        humanLog.logError('DELETE', { filename: path }, logId);
        notify.error(t('toast.deleteFail'), String(error));
      }
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: t('dialog.deleteFile', { name: fileName }),
        onConfirm: async () => {
          setConfirmDialog(null);
          await performDelete();
        }
      });
    } else {
      performDelete();
    }
  };

  const renameFile = (path: string, currentName: string, isRemote: boolean) => {
    setInputDialog({
      title: t('common.rename'),
      defaultValue: currentName,
      onConfirm: async (newName: string) => {
        setInputDialog(null);
        if (!newName || newName === currentName) return;

        // Reject path separators and traversal in filename
        if (newName.includes('/') || newName.includes('\\') || newName.includes('..') || newName.includes('\0')) {
          notify.error(t('common.error'), t('rename.invalidCharacters'));
          return;
        }

        const logId = humanLog.logStart('RENAME', { oldname: currentName, newname: newName, isRemote });
        try {
          // Get parent directory from the file's path
          const parentDir = path.substring(0, path.lastIndexOf('/'));
          const newPath = parentDir + '/' + newName;

          if (isRemote) {
            // Get protocol from active session as fallback
            const activeSession = sessions.find(s => s.id === activeSessionId);
            const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
            const isProvider = usesProviderApi(protocol);

            if (isProvider) {
              await invoke('provider_rename', { from: path, to: newPath });
            } else {
              await invoke('rename_remote_file', { from: path, to: newPath });
            }
            await loadRemoteFiles(undefined, true);
          } else {
            await invoke('rename_local_file', { from: path, to: newPath });
            await loadLocalFiles(currentLocalPath);
          }
          humanLog.logSuccess('RENAME', { oldname: currentName, newname: newName, isRemote }, logId);
          notify.success(t('toast.renamed'), newName);
        } catch (error) {
          humanLog.logError('RENAME', { oldname: currentName, newname: newName, isRemote }, logId);
          notify.error(t('toast.renameFail'), String(error));
        }
      }
    });
  };

  // Inline rename: start editing directly in the file list
  const startInlineRename = (path: string, name: string, isRemote: boolean) => {
    if (name === '..') return;
    if (isRemote && connectionParams.protocol === 'immich') return;
    setInlineRename({ path, name, isRemote });
    setInlineRenameValue(name);
    // Focus input after render
    setTimeout(() => {
      if (inlineRenameRef.current) {
        inlineRenameRef.current.focus();
        // Select filename without extension
        const dotIndex = name.lastIndexOf('.');
        if (dotIndex > 0) {
          inlineRenameRef.current.setSelectionRange(0, dotIndex);
        } else {
          inlineRenameRef.current.select();
        }
      }
    }, 10);
  };

  // Inline rename: commit the rename
  const commitInlineRename = async () => {
    if (!inlineRename) return;
    const { path, name, isRemote } = inlineRename;
    const newName = inlineRenameValue.trim();

    // Cancel if empty or unchanged
    if (!newName || newName === name) {
      setInlineRename(null);
      return;
    }

    // Reject path separators and traversal in filename
    if (newName.includes('/') || newName.includes('\\') || newName.includes('..') || newName.includes('\0')) {
      notify.error(t('common.error'), t('rename.invalidCharacters'));
      setInlineRename(null);
      return;
    }

    const logId = humanLog.logStart('RENAME', { oldname: name, newname: newName, isRemote });
    try {
      const parentDir = path.substring(0, path.lastIndexOf('/'));
      const newPath = parentDir + '/' + newName;

      if (isRemote) {
        const activeSession = sessions.find(s => s.id === activeSessionId);
        const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
        const isProvider = usesProviderApi(protocol);

        if (isProvider) {
          await invoke('provider_rename', { from: path, to: newPath });
        } else {
          await invoke('rename_remote_file', { from: path, to: newPath });
        }
        await loadRemoteFiles(undefined, true);
      } else {
        await invoke('rename_local_file', { from: path, to: newPath });
        await loadLocalFiles(currentLocalPath);
      }
      humanLog.logSuccess('RENAME', { oldname: name, newname: newName, isRemote }, logId);
      notify.success(t('toast.renamed'), newName);
    } catch (error) {
      humanLog.logError('RENAME', { oldname: name, newname: newName, isRemote }, logId);
      notify.error(t('toast.renameFail'), String(error));
    }
    setInlineRename(null);
  };

  // Inline rename: cancel and close
  const cancelInlineRename = () => {
    setInlineRename(null);
  };

  // Inline rename: handle keyboard
  const handleInlineRenameKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitInlineRename();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      cancelInlineRename();
    }
  };

  // Batch rename handler for multiple selected files
  const handleBatchRename = async (renames: Map<string, string>) => {
    const isRemote = batchRenameDialog?.isRemote ?? true;
    const logId = humanLog.logStart('RENAME', {
      count: renames.size,
      isRemote,
      message: `Batch rename (${isRemote ? 'remote' : 'local'})`
    });

    let successCount = 0;
    let errorCount = 0;

    for (const [oldPath, newName] of renames) {
      try {
        const parentDir = oldPath.substring(0, oldPath.lastIndexOf('/'));
        const newPath = parentDir + '/' + newName;

        if (isRemote) {
          const activeSession = sessions.find(s => s.id === activeSessionId);
          const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
          const isProvider = usesProviderApi(protocol);

          if (isProvider) {
            await invoke('provider_rename', { from: oldPath, to: newPath });
          } else {
            await invoke('rename_remote_file', { from: oldPath, to: newPath });
          }
        } else {
          await invoke('rename_local_file', { from: oldPath, to: newPath });
        }
        successCount++;
      } catch (error) {
        console.error(`[BatchRename] Failed to rename ${oldPath}:`, error);
        errorCount++;
      }
    }

    // Refresh file list
    if (isRemote) {
      await loadRemoteFiles(undefined, true);
    } else {
      await loadLocalFiles(currentLocalPath);
    }

    // Log and notify
    if (errorCount === 0) {
      humanLog.logSuccess('RENAME', { count: successCount, isRemote }, logId);
      notify.success(
        t('toast.batchRenameSuccess'),
        `${successCount} ${t('browser.files')}`
      );
    } else {
      humanLog.logError('RENAME', { count: successCount, isRemote, message: `${errorCount} failed` }, logId);
      notify.warning(
        t('toast.batchRenamePartial'),
        t('toast.batchRenameResult', { success: successCount, errors: errorCount })
      );
    }

    setBatchRenameDialog(null);
  };

  const createFolder = (isRemote: boolean) => {
    setInputDialog({
      title: t('dialog.newFolder'),
      defaultValue: '',
      onConfirm: async (name: string) => {
        setInputDialog(null);
        if (!name) return;
        const logId = humanLog.logStart('MKDIR', { foldername: name, isRemote });
        try {
          if (isRemote) {
            // Get protocol from active session as fallback
            const activeSession = sessions.find(s => s.id === activeSessionId);
            const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
            const isProvider = usesProviderApi(protocol);

            const path = currentRemotePath + (currentRemotePath.endsWith('/') ? '' : '/') + name;

            if (isProvider) {
              await invoke('provider_mkdir', { path });
            } else {
              await invoke('create_remote_folder', { path });
            }
            await loadRemoteFiles(undefined, true);
          } else {
            // Create local folder
            const path = currentLocalPath + '/' + name;
            await invoke('create_local_folder', { path });
            await loadLocalFiles(currentLocalPath);
          }
          humanLog.logSuccess('MKDIR', { foldername: name, isRemote }, logId);
          notify.success(t('toast.folderCreated'), name);
        } catch (error) {
          humanLog.logError('MKDIR', { foldername: name, isRemote }, logId);
          notify.error(t('toast.folderCreateFailed'), String(error));
        }
      }
    });
  };

  const showRemoteContextMenu = (e: React.MouseEvent, file: RemoteFile) => {
    e.preventDefault();

    // Auto-select logic
    let selection = new Set(selectedRemoteFiles);
    if (!selection.has(file.name)) {
      selection = new Set([file.name]);
      setSelectedRemoteFiles(selection);
    }

    const count = selection.size;
    const downloadLabel = count > 1 ? t('contextMenu.downloadCount', { count }) : t('common.download');
    const filesToUse = Array.from(selection);

    // Get protocol from active session as fallback (for context menu operations)
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const currentProtocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const currentServer = connectionParams.server || activeSession?.connectionParams?.server;
    const currentUsername = connectionParams.username || activeSession?.connectionParams?.username;
    const filePrivacy = (file.permissions || '').toLowerCase();
    const isFileLuContext = currentProtocol === 'filelu' && count === 1;
    const isFileLuPrivate = isFileLuContext && filePrivacy === 'private';
    const isFileLuPublic = isFileLuContext && filePrivacy === 'public';

    const items: ContextMenuItem[] = [
      { label: downloadLabel, icon: <Download size={14} />, action: () => downloadMultipleFiles(filesToUse) },
      // Media files (images, audio, video, pdf) use Universal Preview modal
      { label: t('common.preview'), icon: <Eye size={14} />, action: () => openUniversalPreview(file, true), disabled: count > 1 || file.is_dir || !isMediaPreviewable(file.name) },
      // Code files use DevTools source viewer
      { label: t('contextMenu.viewSource'), icon: <Code size={14} />, action: () => openDevToolsPreview(file, true), disabled: count > 1 || file.is_dir || !isPreviewable(file.name) },
      { label: (currentProtocol === 'github' || currentProtocol === 'gitlab') ? t('github.renameCommit') : t('common.rename'), icon: currentProtocol === 'github' ? <Github size={14} /> : currentProtocol === 'gitlab' ? <GitLabLogo size={14} /> : <Pencil size={14} />, action: () => renameFile(file.path, file.name, true), disabled: count > 1 || currentProtocol === 'immich' },
      ...(count > 1 && currentProtocol !== 'immich' ? [{
        label: t('batchRename.title') || 'Batch Rename',
        icon: <Replace size={14} />,
        action: () => {
          const selectedFiles = remoteFiles
            .filter(f => selection.has(f.name))
            .map(f => ({ name: f.name, path: f.path, isDir: f.is_dir }));
          setBatchRenameDialog({ files: selectedFiles, isRemote: true });
        }
      }] : []),
      ...(!currentProtocol || !isNonFtpProvider(currentProtocol) || currentProtocol === 'sftp' ? [{ label: t('contextMenu.permissions'), icon: <Shield size={14} />, action: () => setPermissionsDialog({ file, visible: true }), disabled: count > 1 }] : []),
      {
        label: t('contextMenu.properties'), icon: <Info size={14} />, action: () => setPropertiesDialog({
          name: file.name,
          path: file.path,
          size: file.size,
          is_dir: file.is_dir,
          modified: file.modified,
          permissions: file.permissions,
          isRemote: true,
          protocol: currentProtocol,
        }), disabled: count > 1
      },
      { label: ['zohoworkdrive', 'opendrive'].includes(currentProtocol || '') ? t('contextMenu.moveToTrash') : (currentProtocol === 'github' || currentProtocol === 'gitlab') ? t('github.deleteCommit') : t('contextMenu.delete'), icon: currentProtocol === 'github' ? <Github size={14} className="text-red-500" /> : currentProtocol === 'gitlab' ? <GitLabLogo size={14} /> : <Trash2 size={14} />, action: () => deleteMultipleRemoteFiles(filesToUse), danger: true, divider: !['jottacloud', 'mega', 'googledrive', 'box', 'dropbox', 'onedrive', 'zohoworkdrive', 'opendrive'].includes(currentProtocol || '') },
      // Jottacloud: Move to Trash (soft delete — recoverable, separate from hard delete above)
      ...(currentProtocol === 'jottacloud' ? [{
        label: t('contextMenu.moveToTrash'),
        icon: <Trash2 size={14} className="text-orange-500" />,
        action: async () => {
          try {
            const paths = filesToUse.map(name => {
              const f = remoteFiles.find(rf => rf.name === name);
              return f?.path || `${currentRemotePath === '/' ? '' : currentRemotePath}/${name}`;
            });
            const logId = humanLog.logRaw('activity.trash_move_start', 'DELETE', { provider: 'Jottacloud', filename: filesToUse.join(', ') }, 'running');
            await invoke('jottacloud_move_to_trash', { paths });
            humanLog.updateEntry(logId, { status: 'success', message: `[Jottacloud] Moved ${paths.length} item(s) to trash` });
            notify.success(t('toast.movedToTrash', { count: paths.length }));
            loadRemoteFiles(undefined, true);
          } catch (err) {
            notify.error(t('toast.moveToTrashFailed'), String(err));
          }
        },
        divider: true,
      }] : []),
      // MEGA: Move to Trash (soft delete — recoverable via Rubbish Bin)
      // MEGA: Delete now does soft-delete (move to //bin/) via the trait — no separate menu item needed
      // Google Drive: Delete now does soft-delete (trash) via the trait — no separate menu item needed
      // Box: Move to Trash (soft delete — recoverable)
      ...(currentProtocol === 'box' ? [{
        label: t('contextMenu.moveToTrash'),
        icon: <Trash2 size={14} className="text-blue-500" />,
        action: async () => {
          try {
            const paths = filesToUse.map(name => {
              const f = remoteFiles.find(rf => rf.name === name);
              return f?.path || `${currentRemotePath === '/' ? '' : currentRemotePath}/${name}`;
            });
            const logId = humanLog.logRaw('activity.trash_move_start', 'DELETE', { provider: 'Box', filename: filesToUse.join(', ') }, 'running');
            await invoke('box_trash_files', { paths });
            humanLog.updateEntry(logId, { status: 'success', message: `[Box] Moved ${paths.length} item(s) to trash` });
            notify.success(t('toast.movedToTrash', { count: paths.length }));
            loadRemoteFiles(undefined, true);
          } catch (err) {
            notify.error(t('toast.moveToTrashFailed'), String(err));
          }
        },
        divider: true,
      }] : []),
      // FileLu: provider-specific actions
      ...(currentProtocol === 'filelu' ? [
        ...(() => {
          if (isFileLuPrivate) {
            return [{
              label: t('filelu.makePublic'),
              icon: <span style={{ fontSize: 13 }}>🌐</span>,
              action: async () => {
                const logId = humanLog.logRaw('activity.filelu_set_public', 'INFO', { provider: 'FileLu', filename: file.name }, 'running');
                try {
                  await invoke('filelu_set_file_privacy', { path: file.path, onlyMe: false });
                  setRemoteFiles(prev => prev.map(r => r.path === file.path ? { ...r, permissions: 'public' } : r));
                  notify.success(t('filelu.fileSetPublic'));
                  humanLog.updateEntry(logId, { status: 'success', message: '[FileLu] Made file public' });
                } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[FileLu] Make file public failed' }); }
              },
            }];
          }
          if (isFileLuPublic) {
            return [{
              label: t('filelu.togglePrivate'),
              icon: <span style={{ fontSize: 13 }}>👁</span>,
              action: async () => {
                const logId = humanLog.logRaw('activity.filelu_set_private', 'INFO', { provider: 'FileLu', filename: file.name }, 'running');
                try {
                  await invoke('filelu_set_file_privacy', { path: file.path, onlyMe: true });
                  setRemoteFiles(prev => prev.map(r => r.path === file.path ? { ...r, permissions: 'private' } : r));
                  notify.success(t('filelu.fileSetPrivate'));
                  humanLog.updateEntry(logId, { status: 'success', message: '[FileLu] Made file private' });
                } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[FileLu] Make file private failed' }); }
              },
            }];
          }
          return [];
        })(),
        // --- File actions (single file, not dir) ---
        ...(!file.is_dir && filesToUse.length === 1 ? [
          {
            label: t('filelu.setFilePassword'),
            icon: <span style={{ fontSize: 13 }}>🔒</span>,
            disabled: isFileLuPrivate,
            action: async () => {
              setInputDialog({
                title: t('filelu.setFilePassword'),
                defaultValue: '',
                isPassword: true,
                placeholder: t('filelu.enterFilePassword'),
                onConfirm: async (pwd: string) => {
                  setInputDialog(null);
                  const logId = humanLog.logRaw('activity.filelu_set_file_password', 'INFO', { provider: 'FileLu', filename: file.name }, 'running');
                  try {
                    await invoke('filelu_set_file_password', { path: file.path, password: pwd });
                    setRemoteFiles(prev => prev.map(r => r.path === file.path
                      ? {
                          ...r,
                          metadata: {
                            ...(r.metadata || {}),
                            filelu_password_protected: pwd ? 'true' : 'false',
                          },
                        }
                      : r,
                    ));
                    notify.success(pwd ? t('filelu.filePasswordSet') : t('filelu.filePasswordRemoved'));
                    humanLog.updateEntry(logId, { status: 'success', message: '[FileLu] Set file password' });
                  } catch (err) {
                    notify.error(t('filelu.filePasswordError'), String(err));
                    humanLog.updateEntry(logId, { status: 'error', message: '[FileLu] Set file password failed' });
                  }
                }
              });
            },
          },
          {
            label: t('filelu.cloneFile'),
            icon: <span style={{ fontSize: 13 }}>📋</span>,
            action: async () => {
              try {
                const url = await invoke<string>('filelu_clone_file', { path: file.path });
                if (url) {
                  try {
                    await navigator.clipboard.writeText(url);
                  } catch {
                    await invoke('copy_to_clipboard', { text: url });
                  }
                }
                notify.success(t('filelu.cloneSuccess'));
                loadRemoteFiles(undefined, true);
              } catch (err) { notify.error(t('filelu.cloneError'), String(err)); }
            },
          },
        ] : []),
        // --- Folder actions (single folder) ---
        ...(file.is_dir && filesToUse.length === 1 ? [
          {
            label: t('filelu.setFolderPassword'),
            icon: <span style={{ fontSize: 13 }}>🔒</span>,
            disabled: isFileLuPrivate,
            action: async () => {
              setInputDialog({
                title: t('filelu.setFolderPassword'),
                defaultValue: '',
                isPassword: true,
                placeholder: t('filelu.enterFolderPassword'),
                onConfirm: async (pwd: string) => {
                  setInputDialog(null);
                  const logId = humanLog.logRaw('activity.filelu_set_folder_password', 'INFO', { provider: 'FileLu', filename: file.name }, 'running');
                  try {
                    await invoke('filelu_set_folder_password', { path: file.path, password: pwd });
                    setRemoteFiles(prev => prev.map(r => r.path === file.path
                      ? {
                          ...r,
                          metadata: {
                            ...(r.metadata || {}),
                            filelu_password_protected: pwd ? 'true' : 'false',
                          },
                        }
                      : r,
                    ));
                    notify.success(pwd ? t('filelu.folderPasswordSet') : t('filelu.folderPasswordRemoved'));
                    humanLog.updateEntry(logId, { status: 'success', message: '[FileLu] Set folder password' });
                  } catch (err) {
                    notify.error(t('filelu.folderPasswordError'), String(err));
                    humanLog.updateEntry(logId, { status: 'error', message: '[FileLu] Set folder password failed' });
                  }
                }
              });
            },
          },
          {
            label: t('filelu.folderSettings'),
            icon: <span style={{ fontSize: 13 }}>⚙️</span>,
            action: () => setFileLuFolderSettingsDialog({
              path: file.path, name: file.name, filedrop: false, isPublic: false,
            }),
          },
        ] : []),
        // View Trash moved to toolbar button
        ...(file.is_dir && filesToUse.length === 1 ? [{
          label: t('filelu.remoteUrlUpload'),
          icon: <span style={{ fontSize: 13 }}>🌐</span>,
          action: () => setFileLuRemoteUploadDialog({ destPath: file.path }),
          divider: true,
        }] : []),
      ] : []),
      {
        label: t('contextMenu.cut') || 'Cut', icon: <Scissors size={14} />, action: () => {
          const selectedFiles = remoteFiles.filter(f => selection.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
          clipboardCut(selectedFiles, true, currentRemotePath);
        },
        disabled: currentProtocol === 'immich',
      },
      ...(currentProtocol && SERVER_COPY_PROVIDERS.includes(currentProtocol) ? [{
        label: t('contextMenu.copy') || 'Copy', icon: <Copy size={14} />, action: () => {
          const selectedFiles = remoteFiles.filter(f => selection.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
          clipboardCopy(selectedFiles, true, currentRemotePath);
        }
      }] : []),
      {
        label: t('contextMenu.paste') || 'Paste', icon: <ClipboardPaste size={14} />,
        action: () => clipboardPaste(true, currentRemotePath),
        disabled: !hasClipboard || (fileClipboardRef.current?.isRemote && fileClipboardRef.current?.operation === 'copy' && currentProtocol && !SERVER_COPY_PROVIDERS.includes(currentProtocol)),
        divider: true,
      },
      { label: t('contextMenu.copyPath'), icon: <Copy size={14} />, action: () => { navigator.clipboard.writeText(file.path); notify.success(t('contextMenu.pathCopied')); } },
      { label: t('contextMenu.copyName'), icon: <Clipboard size={14} />, action: () => { navigator.clipboard.writeText(file.name); notify.success(t('contextMenu.nameCopied')); } },
    ];

    // Copy FTP/SFTP URL — only for traditional protocols
    if (currentProtocol && (currentProtocol === 'ftp' || currentProtocol === 'ftps' || currentProtocol === 'sftp')) {
      const scheme = currentProtocol === 'sftp' ? 'sftp' : 'ftp';
      items.push({
        label: t('contextMenu.copyUrl', { scheme: scheme.toUpperCase() }), icon: <Link2 size={14} />, action: () => {
          const url = `${scheme}://${currentUsername}@${currentServer}${file.path}`;
          navigator.clipboard.writeText(url);
          notify.success(t('contextMenu.urlCopied', { scheme: scheme.toUpperCase() }));
        }
      });
    }

    // Add Share Link option if AeroCloud is active with public_url_base configured
    // and the file is within the AeroCloud remote folder
    if (isCloudActive && cloudPublicUrlBase && cloudRemoteFolder && file.path.startsWith(cloudRemoteFolder)) {
      items.push({
        label: t('contextMenu.copyShareLink'),
        icon: <Share2 size={14} />,
        action: async () => {
          try {
            const shareUrl = await invoke<string>('generate_share_link_remote', { remotePath: file.path });
            await invoke('copy_to_clipboard', { text: shareUrl });
            notify.success(t('contextMenu.shareLinkCopied'), shareUrl);
          } catch (err) {
            notify.error(t('toast.generateShareLinkFailed'), String(err));
          }
        }
      });
    }

    // Add Share Link for FTP/SFTP/WebDAV servers with publicUrlBase configured
    const sessionPublicUrl = activeSession?.publicUrlBase;
    const hasConfiguredServerShareLink = !!sessionPublicUrl
      && count === 1
      && !!currentProtocol
      && ['ftp', 'ftps', 'sftp', 'webdav'].includes(currentProtocol);
    if (hasConfiguredServerShareLink) {
      items.push({
        label: t('contextMenu.copyShareLink'),
        icon: <Share2 size={14} />,
        action: async () => {
          try {
            const shareUrl = await invoke<string>('generate_server_share_link', {
              publicUrlBase: sessionPublicUrl,
              initialPath: activeSession?.serverInitialPath || '',
              remotePath: file.path,
            });
            await invoke('copy_to_clipboard', { text: shareUrl });
            notify.success(t('contextMenu.shareLinkCopied'), shareUrl);
          } catch (err) {
            notify.error(t('contextMenu.shareLinkFailed'), String(err));
          }
        }
      });
    }

    // Add native Share Link for providers that support it (OAuth + S3 pre-signed URLs + MEGA)
    const hasNativeShareLink = currentProtocol && supportsNativeShareLink(currentProtocol);
    if (hasNativeShareLink) {
      // Resolve share link icon from provider logo map (supports S3/WebDAV sub-providers)
      const shareProviderId = connectionParams.providerId || sessions.find(s => s.id === activeSessionId)?.providerId;
      const shareIcon = (() => {
        // Try sub-provider first (e.g. mega-s4, felicloud, backblaze)
        if (shareProviderId) {
          const SubLogo = PROVIDER_LOGOS[shareProviderId];
          if (SubLogo) return <SubLogo size={14} />;
        }
        // Fall back to protocol-level logo
        const protoKey = currentProtocol || '';
        const ProtoLogo = PROVIDER_LOGOS[protoKey];
        if (ProtoLogo) return <ProtoLogo size={14} />;
        return <Share2 size={14} />;
      })();

      if (currentProtocol === 'filelu' && isFileLuPrivate) {
        items.push({
          label: t('contextMenu.createShareLink'),
          icon: shareIcon,
          disabled: true,
          action: () => {},
        });

        if (!file.is_dir) {
          items.push({
            label: `${t('filelu.makePublic')} + ${t('contextMenu.createShareLink')}`,
            icon: <Share2 size={14} />,
            action: async () => {
              const logId = humanLog.logRaw('activity.filelu_make_public_share', 'INFO', { provider: 'FileLu', filename: file.name }, 'running');
              try {
                await invoke('filelu_set_file_privacy', { path: file.path, onlyMe: false });
                setRemoteFiles(prev => prev.map(r => r.path === file.path ? { ...r, permissions: 'public' } : r));
                notify.info(t('contextMenu.creatingShareLink'), t('contextMenu.shareLinkMoment'));
                const result = await invoke<{ url: string; password: string | null }>('provider_create_share_link', { path: file.path });
                await invoke('copy_to_clipboard', { text: result.url });
                notify.success(t('contextMenu.shareLinkCopied'), result.url);
                humanLog.updateEntry(logId, { status: 'success', message: '[FileLu] Made file public + shared' });
              } catch (err: unknown) {
                notify.error(t('contextMenu.shareLinkFailed'), String(err));
                humanLog.updateEntry(logId, { status: 'error', message: '[FileLu] Make public + share failed' });
              }
            }
          });
        }
      } else {
      items.push({
        label: t('contextMenu.createShareLink'),
        icon: shareIcon,
        action: () => {
          // Resolve provider display name (supports S3/WebDAV sub-providers via registry)
          const providerLabel = (() => {
            if (shareProviderId) {
              const reg = getProviderById(shareProviderId);
              if (reg) return reg.name;
            }
            switch (currentProtocol) {
              case 'googledrive': return 'Google Drive';
              case 'dropbox': return 'Dropbox';
              case 'onedrive': return 'OneDrive';
              case 'mega': return 'MEGA';
              case 'box': return 'Box';
              case 'pcloud': return 'pCloud';
              case 'filen': return 'Filen';
              case 'opendrive': return 'OpenDrive';
              case 'github': return 'GitHub';
              case 'gitlab': return 'GitLab';
              case 'filelu': return 'FileLu';
              case 'webdav': return 'Nextcloud';
              case 'kdrive': return 'kDrive';
              case 'drime': return 'Drime Cloud';
              case 'azure': return 'Azure Blob';
              case 's3': return 'S3';
              default: return currentProtocol?.toUpperCase() || 'Provider';
            }
          })();
          setShareLinkDialog({ path: file.path, fileName: file.name, providerName: providerLabel, providerType: currentProtocol || undefined, providerIcon: shareIcon });
        }
      });
      }

      // Zoho WorkDrive: Manage Share Links (list + delete)
      if (currentProtocol === 'zohoworkdrive') {
        items.push({
          label: t('contextMenu.manageShareLinks'),
          icon: <LinkIcon size={14} />,
          action: async () => {
            try {
              const allLinks = await invoke<Array<{ id: string; attributes: Record<string, unknown> }>>('zoho_get_file_share_links', { path: file.path });
              const links = allLinks.filter(l => !zohoDeletedLinkIds.has(l.id));
              if (links.length === 0) {
                notify.info(t('contextMenu.noShareLinks'), t('contextMenu.noShareLinksDesc'));
                return;
              }
              setZohoShareLinksDialog({ fileName: file.name, links });
            } catch (err) {
              notify.error(t('contextMenu.shareLinkFailed'), String(err));
            }
          }
        });
      }
    }

    // Add Import MEGA Link option (MEGA only)
    if (currentProtocol === 'mega') {
      items.push({
        label: t('contextMenu.importMegaLink'),
        icon: <MegaLogo size={14} />,
        action: () => {
          setInputDialog({
            title: t('contextMenu.importMegaPrompt'),
            defaultValue: '',
            placeholder: 'https://mega.nz/file/...',
            onConfirm: async (link: string) => {
              if (!link.trim()) return;
              try {
                notify.info(t('contextMenu.importingLink'), t('contextMenu.shareLinkMoment'));
                const dest = currentRemotePath || '/';
                await invoke('provider_import_link', { link: link.trim(), dest });
                notify.success(t('contextMenu.linkImported'), t('contextMenu.importedTo', { path: dest }));
                loadRemoteFiles(undefined, true);
              } catch (err) {
                notify.error(t('contextMenu.importLinkFailed'), String(err));
              }
            }
          });
        }
      });
    }

    // File Versions (GDrive, Dropbox, OneDrive)
    if (providerCaps.versions && !file.is_dir && count === 1) {
      items.push({
        label: t('versions.menu') || 'File Versions',
        icon: <History size={14} />,
        action: () => setVersionsDialog({ path: file.path, name: file.name }),
      });
    }

    // Share Permissions (GDrive, OneDrive)
    if (providerCaps.permissions && count === 1) {
      items.push({
        label: t('sharing.menu') || 'Sharing',
        icon: <Users size={14} />,
        action: () => setSharePermissionsDialog({ path: file.path, name: file.name }),
      });
    }

    // Lock/Unlock (WebDAV)
    if (providerCaps.locking && !file.is_dir && count === 1) {
      const lockToken = lockedFiles.get(file.path);
      if (lockToken) {
        items.push({
          label: t('locking.unlock') || 'Unlock File',
          icon: <Unlock size={14} />,
          action: async () => {
            try {
              await invoke('provider_unlock_file', { path: file.path, lockToken });
              setLockedFiles(prev => { const next = new Map(prev); next.delete(file.path); return next; });
              notify.success(t('toast.fileUnlocked'));
            } catch (err) {
              notify.error(t('toast.unlockFailed'), String(err));
            }
          },
        });
      } else {
        items.push({
          label: t('locking.lock') || 'Lock File',
          icon: <Lock size={14} />,
          action: async () => {
            try {
              const info = await invoke<{ token: string; owner: string | null; timeout: number; exclusive: boolean }>('provider_lock_file', { path: file.path, timeout: 3600 });
              setLockedFiles(prev => new Map(prev).set(file.path, info.token));
              notify.success(t('toast.fileLocked'), `Token: ${info.token.slice(0, 20)}...`);
            } catch (err) {
              notify.error(t('toast.lockFailed'), String(err));
            }
          },
        });
      }
    }

    // Cloud provider: provider-specific context menu items
    // Note: "View Trash" moved to toolbar button for all providers
    if (currentProtocol === 'box') {
      // Box-specific: Tags (single file or folder)
      if (filesToUse.length === 1) {
        const currentTags = file.metadata?.box_tags ? file.metadata.box_tags.split(',') : [];
        items.push({
          label: t('box.manageTags'),
          icon: <Tag size={14} className="text-blue-500" />,
          action: () => setBoxTagsTarget({ path: file.path, tags: currentTags }),
        });
      }
      // Box-specific: Watermark (single file only)
      if (!file.is_dir && filesToUse.length === 1) {
        const hasWatermark = file.metadata?.watermarked === 'true';
        items.push({
          label: t('box.addWatermark'),
          icon: <Shield size={14} className={hasWatermark ? 'text-gray-400' : 'text-blue-500'} />,
          badge: proBadge,
          disabled: hasWatermark,
          action: async () => {
            const logId = humanLog.logRaw('activity.box_set_watermark', 'INFO', { provider: 'Box', filename: file.name }, 'running');
            try {
              await invoke('box_set_watermark', { path: file.path });
              notify.success(t('box.watermarkApplied'));
              loadRemoteFiles(undefined, true);
              humanLog.updateEntry(logId, { status: 'success', message: '[Box] Set watermark' });
            } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[Box] Set watermark failed' }); }
          },
        });
        items.push({
          label: t('box.removeWatermark'),
          icon: <ShieldOff size={14} className={!hasWatermark ? 'text-gray-400' : 'text-blue-500'} />,
          badge: proBadge,
          disabled: !hasWatermark,
          action: async () => {
            const logId = humanLog.logRaw('activity.box_remove_watermark', 'INFO', { provider: 'Box', filename: file.name }, 'running');
            try {
              await invoke('box_remove_watermark', { path: file.path });
              notify.success(t('box.watermarkRemoved'));
              loadRemoteFiles(undefined, true);
              humanLog.updateEntry(logId, { status: 'success', message: '[Box] Removed watermark' });
            } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[Box] Remove watermark failed' }); }
          },
        });
      }
      // Box-specific: Lock Folder (single folder only)
      if (file.is_dir && filesToUse.length === 1) {
        items.push({
          label: t('box.lockFolder'),
          icon: <Lock size={14} className="text-amber-500" />,
          badge: proBadge,
          action: async () => {
            const logId = humanLog.logRaw('activity.box_lock_folder', 'INFO', { provider: 'Box', filename: file.name }, 'running');
            try {
              await invoke('box_lock_folder', { path: file.path });
              notify.success(t('box.folderLocked'));
              humanLog.updateEntry(logId, { status: 'success', message: '[Box] Locked folder' });
            } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[Box] Lock folder failed' }); }
          },
          divider: true,
        });
      }
    }
    // Note: Dropbox tags API exists but returns errors — disabled for now
    // OneDrive: Delete already does soft-delete (Graph API DELETE = move to recycle bin).
    // Trash button added to toolbar for viewing/restoring trash items.
    // Koofr, OpenDrive, Yandex: View Trash moved to toolbar button
    // GitHub: View on GitHub, Copy Raw URL, File History
    if (currentProtocol === 'github' && currentServer) {
      const [ghOwner, ghRepo] = (currentServer || '').split('/');
      if (ghOwner && ghRepo) {
        // Resolve branch asynchronously in each action via github_get_info
        const getGhBranch = async (): Promise<string> => {
          try {
            const info = await invoke<{ branch: string; workingBranch: string | null }>('github_get_info');
            // Use workingBranch if in branch workflow mode, otherwise base branch
            return info.workingBranch || info.branch || 'main';
          } catch {
            return 'main';
          }
        };
        items.push({
          label: t('github.viewOnGithub') || 'View on GitHub',
          icon: <ExternalLink size={14} />,
          action: async () => {
            const ghBranch = await getGhBranch();
            const filePath = file.path.replace(/^\//, '');
            window.open(`https://github.com/${ghOwner}/${ghRepo}/blob/${ghBranch}/${filePath}`, '_blank');
          },
          divider: true,
        });
        items.push({
          label: t('github.copyRawUrl') || 'Copy Raw URL',
          icon: <Link2 size={14} />,
          action: async () => {
            const ghBranch = await getGhBranch();
            const filePath = file.path.replace(/^\//, '');
            const url = `https://raw.githubusercontent.com/${ghOwner}/${ghRepo}/${ghBranch}/${filePath}`;
            try {
              await navigator.clipboard.writeText(url);
            } catch {
              await invoke('copy_to_clipboard', { text: url });
            }
            notify.success(t('github.rawUrlCopied') || 'Raw URL copied');
          },
        });
        items.push({
          label: t('github.fileHistory') || 'File History',
          icon: <History size={14} />,
          action: async () => {
            const ghBranch = await getGhBranch();
            const filePath = file.path.replace(/^\//, '');
            window.open(`https://github.com/${ghOwner}/${ghRepo}/commits/${ghBranch}/${filePath}`, '_blank');
          },
        });
      }
    }

    if (currentProtocol === 'googledrive') {
      // Google Drive: Star/Unstar (single file)
      if (filesToUse.length === 1) {
        const isStarred = file.metadata?.starred === 'true';
        items.push({
          label: isStarred ? t('googledrive.unstar') : t('googledrive.star'),
          icon: <Star size={14} className={isStarred ? 'text-yellow-400 fill-yellow-400' : 'text-yellow-400'} />,
          action: async () => {
            const logId = humanLog.logRaw(isStarred ? 'activity.gdrive_unstar' : 'activity.gdrive_star', 'INFO', { provider: 'Google Drive', filename: file.name }, 'running');
            try {
              await invoke('google_drive_set_starred', { paths: [file.path], starred: !isStarred });
              notify.success(isStarred ? t('googledrive.unstarred') : t('googledrive.starred'));
              loadRemoteFiles(undefined, true);
              humanLog.updateEntry(logId, { status: 'success', message: isStarred ? '[Google Drive] Unstarred file' : '[Google Drive] Starred file' });
            } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: isStarred ? '[Google Drive] Unstar failed' : '[Google Drive] Star failed' }); }
          },
        });
        // Google Drive: Add Comment (single file only)
        if (!file.is_dir) {
          items.push({
            label: t('googledrive.addComment'),
            icon: <MessageSquare size={14} className="text-blue-500" />,
            action: () => {
              setShowGDriveComment({ path: file.path, name: file.name });
            },
          });
        }
      }
    }

    // Ask AeroAgent
    items.push({
      label: t('contextMenu.askAeroAgent'),
      icon: <Bot size={14} />,
      action: () => {
        if (!devToolsOpen) {
          setDevToolsOpen(true);
          setTimeout(() => {
            window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'agent' }));
            window.dispatchEvent(new CustomEvent('aeroagent-ask', {
              detail: { code: '', fileName: file.name, filePath: file.path, context: 'file' }
            }));
          }, 50);
        } else {
          window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'agent' }));
          window.dispatchEvent(new CustomEvent('aeroagent-ask', {
            detail: { code: '', fileName: file.name, filePath: file.path, context: 'file' }
          }));
        }
      },
      divider: true,
    });

    contextMenu.show(e, items);
  };

  const showLocalContextMenu = (e: React.MouseEvent, file: LocalFile) => {
    e.preventDefault();

    // Auto-select if not part of current selection
    let selection = new Set(selectedLocalFiles);
    if (!selection.has(file.name)) {
      selection = new Set([file.name]);
      setSelectedLocalFiles(selection);
    }

    const count = selection.size;
    const _activeProto = connectionParams.protocol || sessions.find(s => s.id === activeSessionId)?.connectionParams?.protocol;
    const isGitHub = _activeProto === 'github' || _activeProto === 'gitlab';
    const uploadLabel = isGitHub
      ? (count > 1 ? t('github.commitFiles', { count }) : t('github.commit'))
      : (count > 1 ? t('contextMenu.uploadCount', { count }) : t('common.upload'));
    const filesToUpload = Array.from(selection);

    // Detect .aerovault early for context menu ordering
    const isAeroVaultFile = count === 1 && !file.is_dir && /\.aerovault$/i.test(file.name);

    const items: ContextMenuItem[] = [
      {
        label: uploadLabel,
        icon: _activeProto === 'github' ? <Github size={14} /> : _activeProto === 'gitlab' ? <GitLabLogo size={14} /> : <Cloud size={14} />,
        action: () => uploadMultipleFiles(filesToUpload),
        disabled: !isConnected
      },
      // .aerovault — Open with AeroVault (right after Upload)
      ...(isAeroVaultFile ? [{
        label: t('contextMenu.openWithAeroVault') || 'Open with AeroVault',
        icon: <VaultIcon size={14} />,
        action: () => { setShowVaultPanel({ mode: 'open', path: file.path }); },
      }] : []),
      // .aerovault — Extract Here / Extract to Folder
      ...(isAeroVaultFile ? [{
        label: t('contextMenu.extractSubmenu'),
        icon: <FolderOpen size={14} />,
        action: () => { },
        children: [
          {
            label: t('contextMenu.extractHere'),
            icon: <FolderOpen size={14} />,
            action: () => {
              setInputDialog({
                title: t('contextMenu.passwordRequired'),
                defaultValue: '',
                isPassword: true,
                onConfirm: async (password: string) => {
                  setInputDialog(null);
                  if (!password) { notify.warning(t('contextMenu.passwordRequired'), t('contextMenu.enterArchivePassword')); return; }
                  try {
                    notify.info(t('contextMenu.extracting'), file.name);
                    await invoke<unknown>('vault_v2_extract_all', { vaultPath: file.path, password, destDir: currentLocalPath });
                    notify.success(t('toast.extracted'), t('toast.extractedTo', { dest: currentLocalPath }));
                    await loadLocalFiles(currentLocalPath);
                  } catch (err) {
                    notify.error(t('contextMenu.extractionFailed'), String(err));
                  }
                }
              });
            },
          },
          {
            label: t('contextMenu.extractToFolder'),
            icon: <FolderOpen size={14} />,
            action: () => {
              const subFolder = `${currentLocalPath}/${file.name.replace(/\.aerovault$/i, '')}`;
              setInputDialog({
                title: t('contextMenu.passwordRequired'),
                defaultValue: '',
                isPassword: true,
                onConfirm: async (password: string) => {
                  setInputDialog(null);
                  if (!password) { notify.warning(t('contextMenu.passwordRequired'), t('contextMenu.enterArchivePassword')); return; }
                  try {
                    notify.info(t('contextMenu.extracting'), file.name);
                    await invoke<unknown>('vault_v2_extract_all', { vaultPath: file.path, password, destDir: subFolder });
                    notify.success(t('toast.extracted'), t('toast.extractedTo', { dest: subFolder }));
                    await loadLocalFiles(currentLocalPath);
                  } catch (err) {
                    notify.error(t('contextMenu.extractionFailed'), String(err));
                  }
                }
              });
            },
          },
        ],
      }] : []),
      // Media files (images, audio, video, pdf) use Universal Preview modal
      { label: t('common.preview'), icon: <Eye size={14} />, action: () => openUniversalPreview(file, false), disabled: count > 1 || file.is_dir || !isMediaPreviewable(file.name) },
      // Code files use DevTools source viewer
      { label: t('contextMenu.viewSource'), icon: <Code size={14} />, action: () => openDevToolsPreview(file, false), disabled: count > 1 || file.is_dir || !isPreviewable(file.name) },
      { label: t('common.rename'), icon: <Pencil size={14} />, action: () => renameFile(file.path, file.name, false), disabled: count > 1 },
      ...(count > 1 ? [{
        label: t('batchRename.title') || 'Batch Rename',
        icon: <Replace size={14} />,
        action: () => {
          const selectedFiles = localFiles
            .filter(f => selection.has(f.name))
            .map(f => ({ name: f.name, path: f.path, isDir: f.is_dir }));
          setBatchRenameDialog({ files: selectedFiles, isRemote: false });
        }
      }] : []),
      {
        label: t('contextMenu.properties'), icon: <Info size={14} />, action: async () => {
          const filePath = file.path || `${currentLocalPath}/${file.name}`;
          setPropertiesDialog({
            name: file.name,
            path: filePath,
            size: file.size,
            is_dir: file.is_dir,
            modified: file.modified,
            isRemote: false,
          });
          // Load extended properties asynchronously
          try {
            const detailed = await invoke<any>('get_file_properties', { path: filePath });
            setPropertiesDialog(prev => prev ? {
              ...prev,
              created: detailed.created,
              accessed: detailed.accessed,
              owner: detailed.owner,
              group: detailed.group,
              permissions: detailed.permissions_text,
              permissions_mode: detailed.permissions_mode,
              is_symlink: detailed.is_symlink,
              link_target: detailed.link_target,
              inode: detailed.inode,
              hard_links: detailed.hard_links,
            } : null);
          } catch {
            // Silently fail - basic properties are still shown
          }
        }, disabled: count > 1
      },
      { label: t('contextMenu.delete'), icon: <Trash2 size={14} />, action: () => deleteMultipleLocalFiles(filesToUpload), danger: true, divider: true },
      {
        label: t('contextMenu.cut') || 'Cut', icon: <Scissors size={14} />, action: () => {
          const selectedFiles = localFiles.filter(f => selection.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
          clipboardCut(selectedFiles, false, currentLocalPath);
        }
      },
      {
        label: t('contextMenu.copy') || 'Copy', icon: <Copy size={14} />, action: () => {
          const selectedFiles = localFiles.filter(f => selection.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
          clipboardCopy(selectedFiles, false, currentLocalPath);
        }
      },
      {
        label: t('contextMenu.paste') || 'Paste', icon: <ClipboardPaste size={14} />,
        action: () => clipboardPaste(false, currentLocalPath),
        disabled: !hasClipboard,
        divider: true,
      },
      { label: t('contextMenu.copyPath'), icon: <Copy size={14} />, action: () => { navigator.clipboard.writeText(file.path); notify.success(t('contextMenu.pathCopied')); } },
      { label: t('contextMenu.copyName'), icon: <Clipboard size={14} />, action: () => { navigator.clipboard.writeText(file.name); notify.success(t('contextMenu.nameCopied')); }, divider: true },
      { label: t('contextMenu.openInFileManager'), icon: <ExternalLink size={14} />, action: () => openInFileManager(file.is_dir ? file.path : currentLocalPath) },
      // Directory-specific actions: Calculate Size, Find Duplicates, Disk Usage
      ...(file.is_dir ? [
        {
          label: t('contextMenu.calculateSize'),
          icon: <HardDrive size={14} />,
          action: () => calculateFolderSize(file.path || `${currentLocalPath}/${file.name}`),
        },
        {
          label: t('contextMenu.findDuplicates'),
          icon: <Copy size={14} />,
          action: () => setDuplicateFinderPath(file.path || `${currentLocalPath}/${file.name}`),
        },
        {
          label: t('contextMenu.diskUsage'),
          icon: <HardDrive size={14} />,
          action: () => setDiskUsagePath(file.path || `${currentLocalPath}/${file.name}`),
        },
      ] : []),
    ];

    // Helper: get paths for compression
    const getCompressPaths = () => filesToUpload.map(name => {
      const f = sortedLocalFiles.find(lf => lf.name === name);
      return f ? f.path : `${currentLocalPath}/${name}`;
    });
    const baseName = count === 1 ? file.name.replace(/\.[^/.]+$/, '') : 'archive';

    // Compress — opens CompressDialog with all format options
    items.push({
      label: t('contextMenu.compressSubmenu'),
      icon: <Archive size={14} />,
      action: () => {
        const compressFiles = filesToUpload.map(name => {
          const f = sortedLocalFiles.find(lf => lf.name === name);
          return { name, path: f ? f.path : `${currentLocalPath}/${name}`, size: f?.size ?? 0, isDir: f?.is_dir ?? false };
        });
        setCompressDialogState({ files: compressFiles, defaultName: baseName, outputDir: currentLocalPath });
      }
    });

    // Extract option (for archive files - ZIP, 7z, TAR, RAR variants)
    const isZipArchive = !file.is_dir && /\.(zip)$/i.test(file.name);
    const is7zArchive = !file.is_dir && /\.(7z)$/i.test(file.name);
    const isRarArchive = !file.is_dir && /\.(rar)$/i.test(file.name);
    const isTarArchive = !file.is_dir && /\.(tar|tar\.gz|tgz|tar\.xz|txz|tar\.bz2|tbz2)$/i.test(file.name);
    const isArchive = isZipArchive || is7zArchive || isRarArchive || isTarArchive;

    if (isArchive && count === 1) {
      const doExtract = async (createSubfolder: boolean) => {
        try {
          // Check encryption for 7z, ZIP, and RAR archives
          const isEncrypted = is7zArchive
            ? await invoke<boolean>('is_7z_encrypted', { archivePath: file.path })
            : isZipArchive
              ? await invoke<boolean>('is_zip_encrypted', { archivePath: file.path })
              : isRarArchive
                ? await invoke<boolean>('is_rar_encrypted', { archivePath: file.path })
                : false;
          if (isEncrypted) {
            setInputDialog({
              title: t('contextMenu.passwordRequired'),
              defaultValue: '',
              isPassword: true,
              onConfirm: async (password: string) => {
                setInputDialog(null);
                if (!password) {
                  notify.warning(t('contextMenu.passwordRequired'), t('contextMenu.enterArchivePassword'));
                  return;
                }
                try {
                  const dest = createSubfolder ? `📁 ${file.name.replace(/\.[^.]+$/, '')}/` : currentLocalPath;
                  notify.info(t('contextMenu.extracting'), file.name);
                  const logId = activityLog.log('INFO', `Extracting ${file.name}${createSubfolder ? ` → ${dest}` : ''}...`, 'running');
                  if (is7zArchive) {
                    await invoke<string>('extract_7z', { archivePath: file.path, outputDir: currentLocalPath, password, createSubfolder });
                  } else if (isRarArchive) {
                    await invoke<string>('extract_rar', { archivePath: file.path, outputDir: currentLocalPath, password, createSubfolder });
                  } else {
                    await invoke<string>('extract_archive', { archivePath: file.path, outputDir: currentLocalPath, createSubfolder, password });
                  }
                  activityLog.updateEntry(logId, { status: 'success', message: `Extracted ${file.name}${createSubfolder ? ` → ${dest}` : ''}` });
                  notify.success(t('toast.extracted'), t('toast.extractedTo', { dest }));
                  await loadLocalFiles(currentLocalPath);
                } catch (err) {
                  activityLog.log('ERROR', `Extraction failed: ${String(err)}`, 'error');
                  notify.error(t('contextMenu.extractionFailed'), t('contextMenu.wrongPassword'));
                }
              }
            });
            return;
          }
          const dest = createSubfolder ? `📁 ${file.name.replace(/\.[^.]+$/, '')}/` : currentLocalPath;
          notify.info(t('contextMenu.extracting'), file.name);
          const logId = activityLog.log('INFO', `Extracting ${file.name}${createSubfolder ? ` → ${dest}` : ''}...`, 'running');
          if (isZipArchive) {
            await invoke<string>('extract_archive', { archivePath: file.path, outputDir: currentLocalPath, createSubfolder, password: null });
          } else if (is7zArchive) {
            await invoke<string>('extract_7z', { archivePath: file.path, outputDir: currentLocalPath, password: null, createSubfolder });
          } else if (isRarArchive) {
            await invoke<string>('extract_rar', { archivePath: file.path, outputDir: currentLocalPath, password: null, createSubfolder });
          } else if (isTarArchive) {
            await invoke<string>('extract_tar', { archivePath: file.path, outputDir: currentLocalPath, createSubfolder });
          }
          activityLog.updateEntry(logId, { status: 'success', message: `Extracted ${file.name}${createSubfolder ? ` → ${dest}` : ''}` });
          notify.success(t('toast.extracted'), t('toast.extractedTo', { dest }));
          await loadLocalFiles(currentLocalPath);
        } catch (err) {
          activityLog.log('ERROR', `Extraction failed: ${String(err)}`, 'error');
          notify.error(t('contextMenu.extractionFailed'), String(err));
        }
      };

      items.push({
        label: t('contextMenu.extractSubmenu'),
        icon: <FolderOpen size={14} />,
        divider: true,
        action: () => { },
        children: [
          {
            label: t('contextMenu.extractHere'),
            icon: <FolderOpen size={14} />,
            action: () => doExtract(false),
          },
          {
            label: t('contextMenu.extractToFolder'),
            icon: <FolderOpen size={14} />,
            action: () => doExtract(true),
          },
        ],
      });

      // Browse Archive option
      const archType: import('./types').ArchiveType = isZipArchive ? 'zip' : is7zArchive ? '7z' : isRarArchive ? 'rar' : 'tar';
      items.push({
        label: t('contextMenu.browseArchive') || 'Browse Archive',
        icon: <Search size={14} />,
        action: async () => {
          let encrypted = false;
          try {
            encrypted = is7zArchive
              ? await invoke<boolean>('is_7z_encrypted', { archivePath: file.path })
              : isZipArchive
                ? await invoke<boolean>('is_zip_encrypted', { archivePath: file.path })
                : isRarArchive
                  ? await invoke<boolean>('is_rar_encrypted', { archivePath: file.path })
                  : false;
          } catch { /* ignore */ }
          setArchiveBrowserState({ path: file.path, type: archType, encrypted });
        },
      });
    }

    // Add Share Link option if AeroCloud is active with public_url_base configured
    // and the file is within the AeroCloud local folder
    if (isCloudActive && cloudPublicUrlBase && cloudLocalFolder && file.path.startsWith(cloudLocalFolder)) {
      items.push({
        label: t('contextMenu.copyShareLink'),
        icon: <Share2 size={14} />,
        action: async () => {
          try {
            const shareUrl = await invoke<string>('generate_share_link', { localPath: file.path });
            await invoke('copy_to_clipboard', { text: shareUrl });
            notify.success(t('toast.shareUrlCopied'), shareUrl);
          } catch (err) {
            notify.error(t('toast.shareLinkFailed'), String(err));
          }
        }
      });
    }

    // ─── Encrypted container detection ───────────────────────────────
    const isCryptomatorMarker = count === 1 && !file.is_dir &&
      /^(vault\.cryptomator|masterkey\.cryptomator)$/i.test(file.name);

    // vault.cryptomator / masterkey.cryptomator → Open as Cryptomator Vault
    if (isCryptomatorMarker) {
      items.push({
        label: t('contextMenu.openAsCryptomator') || 'Open as Cryptomator Vault',
        icon: <Lock size={14} className="text-emerald-500" />,
        action: () => setShowCryptomatorBrowser(true),
      });
    }

    // Always show "Create AeroVault..." and "More" (except on vault/cryptomator files)
    if (!isAeroVaultFile && !isCryptomatorMarker) {
      // Folder-specific: "Encrypt Folder as AeroVault..."
      if (count === 1 && file.is_dir) {
        items.push({
          label: t('contextMenu.encryptFolderAsVault') || 'Encrypt Folder as AeroVault...',
          icon: <VaultIcon size={14} />,
          action: () => {
            setShowVaultPanel({ mode: 'create', folderPath: file.path });
          },
        });
      }
      items.push({
        label: t('contextMenu.createAeroVault') || 'Create AeroVault...',
        icon: <VaultIcon size={14} />,
        action: () => {
          const paths = filesToUpload.map(name => {
            const f = sortedLocalFiles.find(lf => lf.name === name);
            return f ? f.path : `${currentLocalPath}/${name}`;
          });
          setShowVaultPanel({ mode: 'create', files: paths });
        },
      });

      // "More" sub-menu with Cryptomator create
      items.push({
        label: t('contextMenu.more') || 'More',
        icon: <MoreHorizontal size={14} />,
        action: () => { },
        children: [
          {
            label: t('contextMenu.createCryptomator') || 'Create Cryptomator Vault...',
            icon: <Lock size={14} />,
            action: () => setCryptomatorCreateDialog({ outputDir: currentLocalPath }),
          },
        ],
      });
    }

    // Tags submenu — Finder-style color labels
    if (fileTags.labels.length > 0) {
      const selectedPaths = Array.from(selection).map(name => {
        const f = localFiles.find(lf => lf.name === name);
        return f ? f.path : `${currentLocalPath}/${name}`;
      });

      const tagChildren: ContextMenuItem[] = fileTags.labels.map(label => {
        // Check if ALL selected files already have this tag
        const allHaveTag = selectedPaths.every(p =>
          fileTags.getTagsForFile(p).some(t => t.label_id === label.id)
        );
        return {
          label: `${allHaveTag ? '✓ ' : ''}${label.name}`,
          icon: <span className="w-3 h-3 rounded-full inline-block shrink-0" style={{ backgroundColor: label.color }} />,
          action: async () => {
            if (allHaveTag) {
              // Remove tag from all selected files
              for (const p of selectedPaths) {
                await fileTags.removeTag(p, label.id);
              }
            } else {
              // Add tag to all selected files
              await fileTags.setTags(selectedPaths, [label.id]);
            }
          },
        };
      });

      // Clear All Tags option
      tagChildren.push({
        label: t('tags.clearAll') || 'Clear All Tags',
        icon: <X size={14} className="text-gray-400" />,
        action: async () => {
          for (const p of selectedPaths) {
            const tags = fileTags.getTagsForFile(p);
            for (const tag of tags) {
              await fileTags.removeTag(p, tag.label_id);
            }
          }
        },
        divider: true,
      });

      items.push({
        label: t('tags.tags') || 'Tags',
        icon: <Tag size={14} />,
        action: () => { },
        children: tagChildren,
        divider: true,
      });
    }

    // Ask AeroAgent
    items.push({
      label: t('contextMenu.askAeroAgent'),
      icon: <Bot size={14} />,
      action: () => {
        const fullPath = currentLocalPath.endsWith('/') ? currentLocalPath + file.name : currentLocalPath + '/' + file.name;
        if (!devToolsOpen) {
          setDevToolsOpen(true);
          setTimeout(() => {
            window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'agent' }));
            window.dispatchEvent(new CustomEvent('aeroagent-ask', {
              detail: { code: '', fileName: file.name, filePath: fullPath, context: 'file' }
            }));
          }, 50);
        } else {
          window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'agent' }));
          window.dispatchEvent(new CustomEvent('aeroagent-ask', {
            detail: { code: '', fileName: file.name, filePath: fullPath, context: 'file' }
          }));
        }
      },
      divider: true,
    });

    contextMenu.show(e, items);
  };

  // Empty-area context menu for remote panel (right-click on background)
  const showRemoteEmptyContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setSelectedRemoteFiles(new Set());

    const activeSession = sessions.find(s => s.id === activeSessionId);
    const currentProtocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;

    const items: ContextMenuItem[] = [
      {
        label: t('contextMenu.paste') || 'Paste', icon: <ClipboardPaste size={14} />,
        action: () => clipboardPaste(true, currentRemotePath),
        disabled: !hasClipboard,
      },
      {
        label: t('contextMenu.newFolder'), icon: <FolderPlus size={14} />,
        action: () => createFolder(true),
        divider: currentProtocol !== 'zohoworkdrive',
        disabled: currentProtocol === 'immich' && currentRemotePath !== '/',
      },
      ...(currentProtocol === 'zohoworkdrive' ? [
        {
          label: 'Zoho Writer',
          icon: <FileText size={14} />,
          action: () => {
            setInputDialog({
              title: 'Zoho Writer',
              defaultValue: '',
              placeholder: t('contextMenu.newDocumentName'),
              onConfirm: async (name: string) => {
                setInputDialog(null);
                if (!name.trim()) return;
                const logId = humanLog.logRaw('activity.zoho_create_document', 'INFO', { provider: 'Zoho WorkDrive', filename: name.trim() }, 'running');
                try {
                  const url = await invoke<string>('zoho_create_native_document', { name: name.trim(), docType: 'writer', folderPath: currentRemotePath || '' });
                  notify.success('Zoho Writer', url);
                  loadRemoteFiles(undefined, true);
                  humanLog.updateEntry(logId, { status: 'success', message: '[Zoho WorkDrive] Created Writer document' });
                } catch (err) { notify.error('Create failed', String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[Zoho WorkDrive] Create Writer document failed' }); }
              },
            });
          },
        },
        {
          label: 'Zoho Sheet',
          icon: <FileSpreadsheet size={14} />,
          action: () => {
            setInputDialog({
              title: 'Zoho Sheet',
              defaultValue: '',
              placeholder: t('contextMenu.newDocumentName'),
              onConfirm: async (name: string) => {
                setInputDialog(null);
                if (!name.trim()) return;
                const logId = humanLog.logRaw('activity.zoho_create_document', 'INFO', { provider: 'Zoho WorkDrive', filename: name.trim() }, 'running');
                try {
                  const url = await invoke<string>('zoho_create_native_document', { name: name.trim(), docType: 'sheet', folderPath: currentRemotePath || '' });
                  notify.success('Zoho Sheet', url);
                  loadRemoteFiles(undefined, true);
                  humanLog.updateEntry(logId, { status: 'success', message: '[Zoho WorkDrive] Created Sheet document' });
                } catch (err) { notify.error('Create failed', String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[Zoho WorkDrive] Create Sheet document failed' }); }
              },
            });
          },
        },
        {
          label: 'Zoho Show',
          icon: <Presentation size={14} />,
          divider: true,
          action: () => {
            setInputDialog({
              title: 'Zoho Show',
              defaultValue: '',
              placeholder: t('contextMenu.newDocumentName'),
              onConfirm: async (name: string) => {
                setInputDialog(null);
                if (!name.trim()) return;
                const logId = humanLog.logRaw('activity.zoho_create_document', 'INFO', { provider: 'Zoho WorkDrive', filename: name.trim() }, 'running');
                try {
                  const url = await invoke<string>('zoho_create_native_document', { name: name.trim(), docType: 'show', folderPath: currentRemotePath || '' });
                  notify.success('Zoho Show', url);
                  loadRemoteFiles(undefined, true);
                  humanLog.updateEntry(logId, { status: 'success', message: '[Zoho WorkDrive] Created Show document' });
                } catch (err) { notify.error('Create failed', String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[Zoho WorkDrive] Create Show document failed' }); }
              },
            });
          },
        },
      ] : []),
      {
        label: t('contextMenu.refresh') || 'Refresh', icon: <RefreshCw size={14} />,
        action: () => loadRemoteFiles(),
      },
      ...(currentProtocol === 'filelu' ? [{
        label: t('filelu.remoteUrlUpload'),
        icon: <span style={{ fontSize: 13 }}>🌐</span>,
        action: () => setFileLuRemoteUploadDialog({ destPath: currentRemotePath }),
      }] : []),
      {
        label: t('contextMenu.selectAll') || 'Select All', icon: <CheckCircle2 size={14} />,
        action: () => setSelectedRemoteFiles(new Set(remoteFiles.map(f => f.name))),
        disabled: remoteFiles.length === 0,
      },
    ];
    contextMenu.show(e, items);
  };

  // Empty-area context menu for local panel (right-click on background)
  const showLocalEmptyContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    e.stopPropagation();
    setSelectedLocalFiles(new Set());

    const items: ContextMenuItem[] = [
      {
        label: t('contextMenu.paste') || 'Paste', icon: <ClipboardPaste size={14} />,
        action: () => clipboardPaste(false, currentLocalPath),
        disabled: !hasClipboard,
      },
      {
        label: t('contextMenu.newFolder'), icon: <FolderPlus size={14} />,
        action: () => createFolder(false),
        divider: true,
      },
      {
        label: t('contextMenu.refresh') || 'Refresh', icon: <RefreshCw size={14} />,
        action: () => loadLocalFiles(currentLocalPath),
      },
      {
        label: t('contextMenu.selectAll') || 'Select All', icon: <CheckCircle2 size={14} />,
        action: () => setSelectedLocalFiles(new Set(localFiles.map(f => f.name))),
        disabled: localFiles.length === 0,
      },
    ];
    contextMenu.show(e, items);
  };

  const handleRemoteFileAction = async (file: RemoteFile) => {
    if (file.is_dir) {
      // Use file.path for providers (WebDAV/S3) that need absolute paths
      // file.name works for FTP which handles relative paths
      // Get protocol from active session as fallback
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = usesProviderApi(protocol);
      const targetPath = isProvider ? file.path : file.name;
      await changeRemoteDirectory(targetPath);
    } else {
      // Respect double-click action setting
      if (doubleClickAction === 'preview') {
        const category = getPreviewCategory(file.name);
        if (['image', 'audio', 'video', 'pdf', 'markdown', 'text'].includes(category)) {
          await openUniversalPreview(file, true);
        } else if (isPreviewable(file.name)) {
          openDevToolsPreview(file, true);
        }
        // If file is not previewable, do nothing on double-click
      } else {
        // Download action
        await downloadFile(file.path, file.name, currentLocalPath, false);
      }
    }
  };

  const openInFileManager = async (path: string) => { try { await invoke('open_in_file_manager', { path }); } catch { } };

  return (
    <>
      {/* Lock Screen - shown when app is locked with master password */}
      {isAppLocked && masterPasswordSet && (
        <LockScreen onUnlock={() => { setIsAppLocked(false); setServersRefreshKey(k => k + 1); }} />
      )}

      {/* Master Password Setup Dialog (standalone from header button) */}
      {showMasterPasswordSetup && (
        <MasterPasswordSetupDialog
          onComplete={() => {
            setShowMasterPasswordSetup(false);
            setMasterPasswordBootstrapMode(false);
            setMasterPasswordSet(true);
          }}
          onClose={() => {
            setShowMasterPasswordSetup(false);
            setMasterPasswordBootstrapMode(false);
          }}
          bootstrapMode={masterPasswordBootstrapMode}
        />
      )}

      {/* Keystore Migration Wizard — one-time migration from localStorage to vault */}
      <KeystoreMigrationWizard
        isOpen={showMigrationWizard}
        onComplete={() => setShowMigrationWizard(false)}
        onSkip={() => setShowMigrationWizard(false)}
        isLightTheme={theme === 'light'}
      />

      <div
        className={`app-root isolate relative h-screen bg-gradient-to-br from-gray-50 to-gray-100 dark:from-gray-900 dark:to-gray-800 text-gray-900 dark:text-gray-100 transition-colors duration-300 flex flex-col overflow-hidden ${compactMode ? 'compact-mode' : ''}`}
        style={{
          '--app-font-size': `${fontSize}px`,
          '--app-font-family': fontFamily,
        } as React.CSSProperties}
      >
        {/* App Background Pattern Overlay — behind content (-z-10) but above gradient bg */}
        {appBackgroundPattern?.svg && (
          <div className="absolute inset-0 pointer-events-none -z-10">
            <div className="absolute inset-0 invert dark:invert-0 dark:opacity-50" style={{ backgroundImage: appBackgroundPattern.svg }} />
          </div>
        )}
        {/* Resize edges for undecorated window (Wayland needs client-side resize zones) */}
        <WindowResizeEdges />

        {/* Custom Titlebar — data-tauri-drag-region for Wayland compatibility */}
        <CustomTitlebar
          appTheme={getEffectiveTheme(theme, isDark)}
          theme={theme}
          setTheme={setTheme}
          isConnected={isConnected}
          onDisconnect={() => disconnectFromFtp('button')}
          onShowConnectionScreen={() => setShowConnectionScreen(true)}
          showConnectionScreen={showConnectionScreen}
          onOpenSettings={() => setShowSettingsPanel(true)}
          onShowSupport={() => setShowSupportDialog(true)}
          onShowCyberTools={() => setShowCyberTools(true)}
          onShowVault={() => setShowVaultPanel({ mode: 'home' })}
          onShowAbout={() => setShowAboutDialog(true)}
          onShowShortcuts={() => setShowShortcutsDialog(true)}
          onShowDependencies={() => setShowDependenciesPanel(true)}
          onShowProviders={() => setShowProvidersDialog(true)}
          masterPasswordSet={masterPasswordSet}
          onLockApp={async () => { await invoke('lock_credential_store'); setIsAppLocked(true); }}
          onSetupMasterPassword={() => setShowMasterPasswordSetup(true)}
          onRefresh={() => { if (isConnected) loadRemoteFiles(); loadLocalFiles(currentLocalPath); }}
          onNewFolder={() => { if (isConnected) createFolder(true); }}
          onToggleDevTools={() => setDevToolsOpen(prev => !prev)}
          onToggleTheme={() => {
            const order: Theme[] = ['light', 'dark', 'tokyo', 'cyber', 'auto'];
            setTheme(order[(order.indexOf(theme) + 1) % order.length]);
          }}
          onToggleDebugMode={() => setDebugMode(!debugMode)}
          onRename={() => {
            if (activePanel === 'remote' && selectedRemoteFiles.size === 1) {
              const name = Array.from(selectedRemoteFiles)[0];
              const file = remoteFiles.find(f => f.name === name);
              if (file) startInlineRename(file.path, file.name, true);
            } else if (activePanel === 'local' && selectedLocalFiles.size === 1) {
              const name = Array.from(selectedLocalFiles)[0];
              const file = localFiles.find(f => f.name === name);
              if (file) startInlineRename(file.path, file.name, false);
            }
          }}
          onDelete={() => {
            if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
              deleteMultipleRemoteFiles(Array.from(selectedRemoteFiles));
            } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
              deleteMultipleLocalFiles(Array.from(selectedLocalFiles));
            }
          }}
          onSelectAll={() => {
            if (activePanel === 'remote') {
              setSelectedRemoteFiles(new Set(remoteFiles.map(f => f.name)));
            } else {
              setSelectedLocalFiles(new Set(localFiles.map(f => f.name)));
            }
          }}
          onCut={() => {
            if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
              const files = remoteFiles.filter(f => selectedRemoteFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
              clipboardCut(files, true, currentRemotePath);
            } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
              const files = localFiles.filter(f => selectedLocalFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
              clipboardCut(files, false, currentLocalPath);
            }
          }}
          onCopy={() => {
            if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
              const files = remoteFiles.filter(f => selectedRemoteFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
              clipboardCopy(files, true, currentRemotePath);
            } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
              const files = localFiles.filter(f => selectedLocalFiles.has(f.name)).map(f => ({ name: f.name, path: f.path, is_dir: f.is_dir }));
              clipboardCopy(files, false, currentLocalPath);
            }
          }}
          onPaste={() => {
            if (activePanel === 'remote') {
              clipboardPaste(true, currentRemotePath);
            } else {
              clipboardPaste(false, currentLocalPath);
            }
          }}
          hasSelection={selectedRemoteFiles.size > 0 || selectedLocalFiles.size > 0}
          hasClipboard={hasClipboard}
          onToggleEditor={() => window.dispatchEvent(new CustomEvent('devtools-panel-toggle', { detail: 'editor' }))}
          onToggleTerminal={() => window.dispatchEvent(new CustomEvent('devtools-panel-toggle', { detail: 'terminal' }))}
          onToggleAgent={() => window.dispatchEvent(new CustomEvent('devtools-panel-toggle', { detail: 'agent' }))}
          onQuit={async () => { try { await getCurrentWindow().close(); } catch { /* noop */ } }}
          onCheckForUpdates={() => checkForUpdate(true)}
          hasActivity={hasActivity || hasQueueActivity}
        />

        <ToastContainer toasts={toast.toasts} onRemove={toast.removeToast} />
        <ScanningToast state={scanningState} t={t} />

        {/* Update Available Toast with inline download */}
        {updateAvailable?.has_update && !updateToastDismissed && (
          <div className={`fixed top-4 right-4 bg-blue-600 dark:bg-blue-700 text-white px-4 py-3 rounded-xl shadow-2xl z-50 flex flex-col gap-2 border border-blue-400/30 min-w-[320px] max-w-[380px] ${!updateDownload ? 'animate-update-flash' : ''}`}>
            <div className="flex items-center justify-between">
              <div className="flex flex-col">
                <span className="font-semibold flex items-center gap-1.5">
                  <Download size={14} />
                  AeroFTP v{updateAvailable.latest_version} {t('statusbar.updateAvailable')}
                </span>
                <span className="text-xs opacity-80">
                  {t('ui.currentVersion', { version: updateAvailable.current_version, format: updateAvailable.install_format?.toUpperCase() })}
                </span>
              </div>
              <button
                onClick={() => { setUpdateToastDismissed(true); setUpdateDownload(null); }}
                className="text-white/70 hover:text-white p-1 hover:bg-white/10 rounded-full transition-colors"
                title={t('update.skipForNow')}
              >
                <X size={16} />
              </button>
            </div>

            {/* State: Ready to download */}
            {!updateDownload && (
              <div className="flex flex-col gap-1.5">
                <button
                  onClick={startUpdateDownload}
                  className="bg-white text-blue-600 px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-blue-50 transition-colors shadow-sm w-full"
                >
                  {t('update.downloadNow')} (.{updateAvailable.install_format || 'deb'})
                </button>
                <button
                  onClick={() => { setUpdateToastDismissed(true); }}
                  className="text-white/60 hover:text-white text-xs py-1 transition-colors flex items-center justify-center gap-1"
                >
                  <Clock size={10} /> {t('update.skipForNow')}
                </button>
              </div>
            )}

            {/* State: Downloading */}
            {updateDownload?.downloading && (
              <TransferProgressBar
                percentage={updateDownload.percentage}
                speedBps={updateDownload.speed_bps}
                etaSeconds={updateDownload.eta_seconds}
                size="lg"
                variant="gradient"
              />
            )}

            {/* State: Download complete — Install & Restart */}
            {updateDownload?.completedPath && !updateDownload?.installing && (
              <div className="flex flex-col gap-2">
                <span className="text-xs text-green-200 flex items-center gap-1">
                  <CheckCircle2 size={12} /> {t('update.downloadComplete')}
                </span>
                <span className="text-xs opacity-60 truncate" title={updateDownload.completedPath}>
                  {updateDownload.completedPath}
                </span>

                {/* Sigstore Badge */}
                {updateDownload.verification && (
                  <div className={`mt-1 flex flex-col gap-1 text-xs border rounded-lg p-2 ${
                    updateDownload.verification.mode === 'VerificationFailed' ? 'bg-red-500/10 border-red-500/20 text-red-300' :
                    'bg-green-500/10 border-green-500/20 text-green-300'
                  }`}>
                    <div className="flex items-center justify-between font-medium">
                      <div className="flex items-center gap-1.5 truncate">
                        {updateDownload.verification.mode === 'VerificationFailed' ? <ShieldAlert size={14} className="flex-shrink-0" /> : <ShieldCheck size={14} className="flex-shrink-0" />}
                        <span className="truncate">
                          {updateDownload.verification.mode === 'SigstoreVerified' && 'Signed by axpdev-lab/aeroftp CI'}
                          {updateDownload.verification.mode === 'VerificationUnavailable' && `SHA-256 verified (${updateDownload.verification.artifact_sha256.slice(0, 12)}...)`}
                          {updateDownload.verification.mode === 'VerificationFailed' && 'Signature verification failed'}
                        </span>
                      </div>
                    </div>
                  </div>
                )}

                {/* Install & Restart — platform-aware, block if VerificationFailed */}
                {updateDownload.verification?.mode !== 'VerificationFailed' && (
                  ['appimage', 'deb', 'rpm'].includes(updateAvailable.install_format) ? (
                    <button
                      onClick={async () => {
                        const cmd = updateAvailable.install_format === 'appimage'
                          ? 'install_appimage_update'
                          : updateAvailable.install_format === 'rpm'
                            ? 'install_rpm_update'
                            : 'install_deb_update';
                        setUpdateDownload(prev => prev ? { ...prev, installing: true } : null);
                        try {
                          await invoke(cmd, { downloadedPath: updateDownload.completedPath, verificationMode: updateDownload.verification?.mode ?? 'VerificationUnavailable' });
                        } catch (e) {
                          setUpdateDownload(prev => prev ? { ...prev, installing: false, error: String(e) } : null);
                        }
                      }}
                      className="bg-green-500 text-white px-3 py-2 rounded-lg font-medium text-sm hover:bg-green-400 transition-colors shadow-sm w-full flex items-center justify-center gap-1.5"
                    >
                      <RefreshCw size={13} /> {t('update.installRestart')}
                    </button>
                  ) : (
                    <button
                      onClick={() => invoke('open_in_file_manager', { path: updateDownload.completedPath })}
                      className="bg-green-500 text-white px-3 py-2 rounded-lg font-medium text-sm hover:bg-green-400 transition-colors shadow-sm w-full flex items-center justify-center gap-1.5"
                    >
                      <ExternalLink size={13} /> {t('update.openInstaller')}
                    </button>
                  )
                )}

                {/* Secondary: skip for now */}
                <button
                  onClick={() => { setUpdateToastDismissed(true); }}
                  className="text-white/60 hover:text-white text-xs py-0.5 transition-colors flex items-center justify-center gap-1"
                >
                  <Clock size={10} /> {t('update.skipForNow')}
                </button>
              </div>
            )}

            {/* State: Installing — shown in overlay below */}
            {updateDownload?.installing && (
              <div className="flex items-center gap-2 py-1">
                <svg className="w-4 h-4 animate-spin text-white" viewBox="0 0 24 24" fill="none">
                  <circle className="opacity-25" cx="12" cy="12" r="10" stroke="currentColor" strokeWidth="3" />
                  <path className="opacity-75" fill="currentColor" d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4z" />
                </svg>
                <span className="text-xs">{t('update.installing')}</span>
              </div>
            )}

            {/* State: Error */}
            {updateDownload?.error && (
              <div className="flex flex-col gap-1.5">
                <span className="text-xs text-red-200 flex items-center gap-1">
                  <AlertTriangle size={12} /> {updateDownload.error}
                </span>
                <div className="flex gap-2">
                  <button
                    onClick={startUpdateDownload}
                    className="bg-white text-blue-600 px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-blue-50 transition-colors shadow-sm flex-1"
                  >
                    {t('update.retry')}
                  </button>
                  <button
                    onClick={() => invoke('open_in_file_manager', { path: updateDownload.completedPath || updateDownload.filename })}
                    className="bg-white/20 text-white px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-white/30 transition-colors shadow-sm"
                    title={t('update.openFolder')}
                  >
                    <ExternalLink size={13} />
                  </button>
                </div>
              </div>
            )}
          </div>
        )}

        {/* Fullscreen overlay during update installation */}
        {updateDownload?.installing && (
          <div className="fixed inset-0 z-[100] bg-black/70 backdrop-blur-sm flex flex-col items-center justify-center gap-5 animate-scale-in">
            <Loader2 className="w-12 h-12 animate-spin text-blue-400" />
            <div className="text-center">
              <p className="text-white text-lg font-semibold">
                {updateDownload.installPhase === 'auth' && 'Authenticating privileges...'}
                {updateDownload.installPhase === 'running' && 'Installing package update...'}
                {updateDownload.installPhase === 'restart' && 'Restarting AeroFTP...'}
                {!updateDownload.installPhase && t('update.installing')}
              </p>
              <p className="text-white/60 text-sm mt-1">{t('update.installingDesc')}</p>
              {updateAvailable?.latest_version && (
                <p className="text-white/40 text-xs mt-2">AeroFTP v{updateAvailable.latest_version}</p>
              )}
            </div>
            {/* Verification hash — shown plainly in overlay, badge is in download toast */}
            {updateDownload.verification && (
              <p className={`text-xs mt-2 ${updateDownload.verification.mode === 'VerificationFailed' ? 'text-red-400' : 'text-white/40'}`}>
                {updateDownload.verification.mode === 'VerificationFailed'
                  ? 'Verification Error'
                  : updateDownload.verification.artifact_sha256
                    ? `SHA-256: ${updateDownload.verification.artifact_sha256.slice(0, 16)}...`
                    : ''}
              </p>
            )}
          </div>
        )}

        <TransferQueue
          items={transferQueue.items}
          isVisible={transferQueue.isVisible}
          onToggle={transferQueue.toggle}
          onClear={transferQueue.clear}
          onClearCompleted={transferQueue.clearCompleted}
          onStopAll={cancelTransfer}
          forceStopMode={isForceStopMode}
          onRemoveItem={transferQueue.removeItem}
          onRetryItem={(id: string) => {
            transferQueue.retryItem(id);
            const cb = retryCallbacksRef.current.get(id);
            if (cb) cb();
          }}
          isPaused={isBatchPaused}
          pauseReason={batchPauseReason}
          onResume={resumeBatch}
          onRetryAllFailed={retryAllFailedItems}
        />
        {contextMenu.state.visible && <ContextMenu x={contextMenu.state.x} y={contextMenu.state.y} items={contextMenu.state.items} onClose={contextMenu.hide} />}
        <TransferToastContainer />
        <GlobalTooltip />
        {confirmDialog && <ConfirmDialog message={confirmDialog.message} onConfirm={confirmDialog.onConfirm} onCancel={confirmDialog.onCancel || (() => setConfirmDialog(null))} />}
        {inputDialog && <InputDialog title={inputDialog.title} defaultValue={inputDialog.defaultValue} onConfirm={inputDialog.onConfirm} onCancel={() => setInputDialog(null)} isPassword={inputDialog.isPassword} placeholder={inputDialog.placeholder} />}
        {zohoShareLinksDialog && (
          <div className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50" onClick={() => setZohoShareLinksDialog(null)}>
            <div className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-[480px] max-h-[70vh] flex flex-col" onClick={e => e.stopPropagation()}>
              <div className="px-5 py-3 border-b border-gray-200 dark:border-gray-700 flex items-center justify-between">
                <h3 className="text-sm font-semibold text-gray-800 dark:text-gray-200">{t('contextMenu.shareLinksFor')} {zohoShareLinksDialog.fileName}</h3>
                <button onClick={() => setZohoShareLinksDialog(null)} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded"><X size={16} /></button>
              </div>
              <div className="overflow-y-auto flex-1 p-3 space-y-2">
                {zohoShareLinksDialog.links.map(link => (
                  <div key={link.id} className="bg-gray-50 dark:bg-gray-700/50 rounded-lg p-3 text-xs space-y-1.5">
                    <div className="flex items-center justify-between">
                      <span className="font-medium text-gray-800 dark:text-gray-200">{String(link.attributes.link_name || 'Link')}</span>
                      <span className="text-gray-500 dark:text-gray-400 text-[10px]">{String(link.attributes.link_type || '')}{link.attributes.is_password_protected ? ' 🔒' : ''}</span>
                    </div>
                    <div className="flex items-center gap-1.5">
                      <div className="text-blue-600 dark:text-blue-400 break-all cursor-pointer hover:underline flex-1" onClick={async (e) => {
                        const el = e.currentTarget;
                        await invoke('copy_to_clipboard', { text: String(link.attributes.link || '') });
                        el.classList.add('text-green-500', 'dark:text-green-400');
                        el.textContent = `✓ ${t('contextMenu.shareLinkCopied')}`;
                        setTimeout(() => { el.classList.remove('text-green-500', 'dark:text-green-400'); el.textContent = String(link.attributes.link || ''); }, 1500);
                      }}>{String(link.attributes.link || '')}</div>
                      <button onClick={async (e) => {
                        const el = e.currentTarget;
                        await invoke('copy_to_clipboard', { text: String(link.attributes.link || '') });
                        el.textContent = '✓';
                        setTimeout(() => { el.textContent = '📋'; }, 1500);
                      }} className="shrink-0 px-1.5 py-0.5 hover:bg-gray-200 dark:hover:bg-gray-600 rounded text-xs" title={t('contextMenu.shareLinkCopied')}>📋</button>
                    </div>
                    <div className="flex items-center justify-between pt-1">
                      <span className="text-gray-400 dark:text-gray-500">{String(link.attributes.created_time || '')}</span>
                      <button onClick={async () => {
                        const logId = humanLog.logRaw('activity.zoho_delete_share_link', 'INFO', { provider: 'Zoho WorkDrive', filename: link.id }, 'running');
                        try {
                          await invoke('zoho_delete_share_link', { linkId: link.id });
                        } catch { /* Ghost link — already deleted on server, remove from UI */ }
                        // Track deleted IDs so Zoho GET cache won't re-show them
                        zohoDeletedLinkIds.add(link.id);
                        notify.success(t('contextMenu.shareLinkDeleted'), link.id);
                        humanLog.updateEntry(logId, { status: 'success', message: '[Zoho WorkDrive] Deleted share link' });
                        setZohoShareLinksDialog(prev => prev ? { ...prev, links: prev.links.filter(l => l.id !== link.id) } : null);
                      }} className="px-2 py-0.5 text-red-500 hover:bg-red-50 dark:hover:bg-red-900/30 rounded text-[11px] font-medium">
                        <Trash2 size={12} className="inline mr-1" />{t('common.delete') || 'Delete'}
                      </button>
                    </div>
                  </div>
                ))}
              </div>
              {zohoShareLinksDialog.links.length === 0 && (
                <div className="p-5 text-center text-sm text-gray-400">{t('contextMenu.noShareLinks')}</div>
              )}
            </div>
          </div>
        )}
        {gitHubCommitDialog && (
          <GitHubCommitDialog
            isOpen={true}
            files={gitHubCommitDialog.files}
            operation={gitHubCommitDialog.operation}
            branch={gitHubCommitDialog.branch}
            writeMode={gitHubCommitDialog.writeMode}
            workingBranch={gitHubCommitDialog.workingBranch}
            onCommit={gitHubCommitDialog.onCommit}
            onCancel={gitHubCommitDialog.onCancel || (() => setGitHubCommitDialog(null))}
          />
        )}
        <GitHubPagesBrowser
          isOpen={showGitHubPages}
          onClose={() => setShowGitHubPages(false)}
        />
        <GitHubActionsBrowser
          isOpen={showGitHubActions}
          onClose={() => setShowGitHubActions(false)}
        />
        <GitHubReleaseBrowser
          isOpen={showGitHubReleaseBrowser}
          onClose={() => setShowGitHubReleaseBrowser(false)}
          onError={(title: string, msg: string) => notify.error(title, msg)}
        />
        <GitLabReleaseBrowser
          isOpen={showGitLabReleaseBrowser}
          onClose={() => setShowGitLabReleaseBrowser(false)}
          onError={(title: string, msg: string) => notify.error(title, msg)}
        />
        <FilenNotesPanel
          isOpen={showFilenNotes}
          onClose={() => setShowFilenNotes(false)}
        />
        {gitHubSyncWarning && (
          <GitHubLocalSyncWarning
            unpushedCount={gitHubSyncWarning.unpushedCount}
            branch={gitHubSyncWarning.branch}
            onPushFirst={() => gitHubSyncWarning.resolve('push')}
            onContinue={() => gitHubSyncWarning.resolve('continue')}
            onCancel={() => gitHubSyncWarning.resolve('cancel')}
          />
        )}
        {batchRenameDialog && (
          <BatchRenameDialog
            isOpen={true}
            files={batchRenameDialog.files}
            isRemote={batchRenameDialog.isRemote}
            onConfirm={handleBatchRename}
            onClose={() => setBatchRenameDialog(null)}
          />
        )}
        {propertiesDialog && (
          <PropertiesDialog
            file={propertiesDialog}
            onClose={() => {
              // Cancel remote folder size scan if active
              if (propertiesDialog.isRemote && propertiesDialog.is_dir) {
                invoke('provider_cancel_folder_size').catch(() => {});
              }
              setPropertiesDialog(null);
            }}
            onCalculateChecksum={async (algorithm: 'md5' | 'sha1' | 'sha256' | 'sha512') => {
              if (!propertiesDialog || propertiesDialog.isRemote) return;
              setPropertiesDialog(prev => prev ? { ...prev, checksum: { ...prev.checksum, calculating: true } } : null);
              try {
                const hash = await invoke<string>('calculate_checksum', { path: propertiesDialog.path, algorithm });
                setPropertiesDialog(prev => {
                  if (!prev) return null;
                  return {
                    ...prev,
                    checksum: {
                      ...prev.checksum,
                      calculating: false,
                      [algorithm]: hash
                    }
                  };
                });
              } catch (err) {
                notify.error(t('toast.checksumFailed'), String(err));
                setPropertiesDialog(prev => prev ? { ...prev, checksum: { ...prev.checksum, calculating: false } } : null);
              }
            }}
            onCalculateFolderSize={propertiesDialog.is_dir ? () => {
              const p = propertiesDialog.path;
              if (propertiesDialog.isRemote) {
                calculateRemoteFolderSize(p);
              } else {
                calculateFolderSize(p);
              }
            } : undefined}
            folderSize={propertiesDialog.is_dir ? folderSizeCache.get(propertiesDialog.path) ?? null : null}
            folderSizeCalculating={propertiesDialog.is_dir ? folderSizeCalculating.has(propertiesDialog.path) : false}
          />
        )}
        {quickLookOpen && sortedLocalFiles[quickLookIndex] && (
          <QuickLookOverlay
            file={sortedLocalFiles[quickLookIndex]}
            allFiles={sortedLocalFiles}
            currentIndex={quickLookIndex}
            currentPath={currentLocalPath}
            onClose={() => setQuickLookOpen(false)}
            onNavigate={(idx) => {
              setQuickLookIndex(idx);
              setSelectedLocalFiles(new Set([sortedLocalFiles[idx].name]));
            }}
            t={t}
          />
        )}
        {duplicateFinderPath && (
          <DuplicateFinderDialog
            isOpen={true}
            scanPath={duplicateFinderPath}
            onClose={() => setDuplicateFinderPath(null)}
            onDeleteFiles={async (paths) => {
              // Respect confirmBeforeDelete setting
              if (confirmBeforeDelete) {
                const confirmed = await new Promise<boolean>(resolve => {
                  setConfirmDialog({
                    message: t('duplicates.deleteConfirm', { count: paths.length }),
                    onConfirm: () => { setConfirmDialog(null); resolve(true); },
                    onCancel: () => { setConfirmDialog(null); resolve(false); },
                  });
                });
                if (!confirmed) return;
              }
              for (const p of paths) {
                try {
                  await invoke('delete_to_trash', { path: p });
                } catch {
                  try {
                    await invoke('delete_local_file', { path: p });
                  } catch (err) {
                    const fileName = p.split(/[\\/]/).pop() || p;
                    notify.error(t('toast.deleteFail'), `${fileName}: ${String(err)}`);
                  }
                }
              }
              loadLocalFiles(currentLocalPath);
            }}
          />
        )}
        {diskUsagePath && (
          <DiskUsageTreemap
            isOpen={true}
            scanPath={diskUsagePath}
            onClose={() => setDiskUsagePath(null)}
          />
        )}
        {syncNavDialog && (
          <SyncNavDialog
            missingPath={syncNavDialog.missingPath}
            isRemote={syncNavDialog.isRemote}
            onCreateFolder={handleSyncNavCreateFolder}
            onDisableSync={handleSyncNavDisable}
            onCancel={() => setSyncNavDialog(null)}
          />
        )}
        <PermissionsDialog
          isOpen={permissionsDialog?.visible || false}
          onClose={() => setPermissionsDialog(null)}
          onSave={async (mode) => {
            if (permissionsDialog?.file) {
              try {
                await invoke('chmod_remote_file', { path: permissionsDialog.file.path, mode });
                notify.success(t('toast.permissionsUpdated'), t('toast.permissionsUpdatedDesc', { name: permissionsDialog.file.name, mode }));
                await loadRemoteFiles(undefined, true);
                setPermissionsDialog(null);
              } catch (e) { notify.error(t('common.failed'), String(e)); }
            }
          }}
          fileName={permissionsDialog?.file.name || ''}
          currentPermissions={permissionsDialog?.file.permissions || undefined}
        />
        {versionsDialog && (
          <FileVersionsDialog
            filePath={versionsDialog.path}
            fileName={versionsDialog.name}
            onClose={() => setVersionsDialog(null)}
            onRestore={() => { setVersionsDialog(null); loadRemoteFiles(undefined, true); }}
          />
        )}
        {sharePermissionsDialog && (
          <SharePermissionsDialog
            filePath={sharePermissionsDialog.path}
            fileName={sharePermissionsDialog.name}
            onClose={() => setSharePermissionsDialog(null)}
          />
        )}
        {showCyberTools && <CyberToolsModal onClose={() => setShowCyberTools(false)} />}
        <AboutDialog isOpen={showAboutDialog} onClose={() => setShowAboutDialog(false)} />
        <SupportDialog isOpen={showSupportDialog} onClose={() => setShowSupportDialog(false)} />
        <ProvidersDialog isOpen={showProvidersDialog} onClose={() => setShowProvidersDialog(false)} />
        {showCommandPalette && (
          <CommandPalette
            commands={commandPaletteItems}
            onClose={() => setShowCommandPalette(false)}
          />
        )}
        <HostKeyDialog
          visible={hostKeyDialog.visible}
          info={hostKeyDialog.info}
          host={hostKeyDialog.host}
          port={hostKeyDialog.port}
          onAccept={handleHostKeyAccept}
          onReject={handleHostKeyReject}
        />
        <OverwriteDialog
          isOpen={overwriteDialog.isOpen}
          source={overwriteDialog.source!}
          destination={overwriteDialog.destination!}
          queueCount={overwriteDialog.queueCount}
          onDecision={(action, applyToAll, newName) => {
            if (overwriteDialog.resolve) {
              overwriteDialog.resolve({ action, applyToAll, newName });
            }
            setOverwriteDialog(prev => ({ ...prev, isOpen: false }));
          }}
          onCancel={() => {
            if (overwriteDialog.resolve) {
              overwriteDialog.resolve({ action: 'cancel', applyToAll: false });
            }
            setOverwriteDialog(prev => ({ ...prev, isOpen: false }));
          }}
        />
        <FolderOverwriteDialog
          isOpen={folderOverwriteDialog.isOpen}
          folderName={folderOverwriteDialog.folderName}
          direction={folderOverwriteDialog.direction}
          queueCount={folderOverwriteDialog.queueCount}
          onDecision={(action, applyToAll) => {
            if (folderOverwriteDialog.resolve) {
              folderOverwriteDialog.resolve({ action, applyToAll });
            }
            setFolderOverwriteDialog(prev => ({ ...prev, isOpen: false }));
          }}
          onCancel={() => {
            if (folderOverwriteDialog.resolve) {
              folderOverwriteDialog.resolve({ action: 'cancel', applyToAll: false });
            }
            setFolderOverwriteDialog(prev => ({ ...prev, isOpen: false }));
          }}
        />
        <ShortcutsDialog isOpen={showShortcutsDialog} onClose={() => setShowShortcutsDialog(false)} />
        <SettingsPanel
          isOpen={showSettingsPanel}
          onClose={() => { setShowSettingsPanel(false); setSettingsInitialTab(undefined); }}
          onOpenCloudPanel={() => setShowCloudPanel(true)}
          onActivityLog={{ logRaw: humanLog.logRaw }}
          initialTab={settingsInitialTab}
          onServersChanged={() => setServersRefreshKey(k => k + 1)}
          theme={theme}
          setTheme={setTheme}
        />

        {/* Universal Preview Modal for Media Files */}
        <UniversalPreview
          isOpen={universalPreviewOpen}
          file={universalPreviewFile}
          onClose={closeUniversalPreview}
        />
        <SyncPanel
          isOpen={showSyncPanel}
          onClose={() => setShowSyncPanel(false)}
          localPath={currentLocalPath}
          remotePath={currentRemotePath}
          isConnected={isConnected}
          protocol={connectionParams.protocol || sessions.find(s => s.id === activeSessionId)?.connectionParams?.protocol}
          onSyncComplete={async () => {
            await loadRemoteFiles();
            await loadLocalFiles(currentLocalPath);
          }}
        />
        <CloudPanel
          isOpen={showCloudPanel}
          onClose={() => setShowCloudPanel(false)}
        />
        {showVaultPanel && <VaultPanel onClose={() => setShowVaultPanel(false)} initialMode={showVaultPanel.mode} initialPath={showVaultPanel.path} initialFiles={showVaultPanel.files} initialFolderPath={showVaultPanel.folderPath} isConnected={isConnected} iconProvider={iconProvider} />}
        {showCryptomatorBrowser && <CryptomatorBrowser onClose={() => setShowCryptomatorBrowser(false)} />}
        {archiveBrowserState && (
          <ArchiveBrowser
            archivePath={archiveBrowserState.path}
            archiveType={archiveBrowserState.type}
            isEncrypted={archiveBrowserState.encrypted}
            onClose={() => setArchiveBrowserState(null)}
          />
        )}

        {showZohoTrash && (
          <ZohoTrashManager
            onClose={() => setShowZohoTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showGDriveComment && (
          <GoogleDriveCommentDialog
            filePath={showGDriveComment.path}
            fileName={showGDriveComment.name}
            onClose={() => setShowGDriveComment(null)}
          />
        )}
        {showJottaTrash && (
          <JottacloudTrashManager
            onClose={() => setShowJottaTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showMegaTrash && (
          <MegaTrashManager
            onClose={() => setShowMegaTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showGDriveTrash && (
          <GoogleDriveTrashManager
            onClose={() => setShowGDriveTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showBoxTrash && (
          <BoxTrashManager
            onClose={() => setShowBoxTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showDropboxTrash && (
          <DropboxTrashManager
            onClose={() => setShowDropboxTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
            currentPath={currentRemotePath}
          />
        )}
        {showOneDriveTrash && (
          <OneDriveTrashManager
            onClose={() => setShowOneDriveTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {boxTagsTarget && (
          <BoxTagsDialog
            filePath={boxTagsTarget.path}
            currentTags={boxTagsTarget.tags}
            onClose={() => setBoxTagsTarget(null)}
            onUpdated={() => loadRemoteFiles(undefined, true)}
            command={boxTagsTarget.command}
            providerName={boxTagsTarget.providerName}
          />
        )}
        {showFileLuTrash && (
          <FileLuTrashManager
            onClose={() => setShowFileLuTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showKoofrTrash && (
          <KoofrTrashManager
            onClose={() => setShowKoofrTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showNextcloudTrash && (
          <NextcloudTrashManager
            providerName={(connectionParams.providerId || sessions.find(s => s.id === activeSessionId)?.providerId) === 'felicloud' ? 'Felicloud' : 'Nextcloud'}
            onClose={() => setShowNextcloudTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {shareLinkDialog && (
          <ShareLinkModal
            path={shareLinkDialog.path}
            fileName={shareLinkDialog.fileName}
            providerName={shareLinkDialog.providerName}
            providerType={shareLinkDialog.providerType}
            providerIcon={shareLinkDialog.providerIcon}
            onClose={() => setShareLinkDialog(null)}
          />
        )}
        {showOpenDriveTrash && (
          <OpenDriveTrashManager
            onClose={() => setShowOpenDriveTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showYandexTrash && (
          <YandexTrashManager
            onClose={() => setShowYandexTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showPCloudTrash && (
          <PCloudTrashManager
            onClose={() => setShowPCloudTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {showKDriveTrash && (
          <KDriveTrashManager
            onClose={() => setShowKDriveTrash(false)}
            onRefreshFiles={() => loadRemoteFiles(undefined, true)}
          />
        )}
        {/* FileLu: Folder Settings Dialog */}
        {fileLuFolderSettingsDialog && (
          <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm">
            <div className="w-full max-w-sm mx-4 rounded-xl shadow-2xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 animate-scale-in p-6">
              <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100 mb-4">
                {t('filelu.folderSettings')}: <span className="text-blue-600 dark:text-blue-400">{fileLuFolderSettingsDialog.name}</span>
              </h2>
              <div className="space-y-3 mb-5">
                <Checkbox
                  checked={fileLuFolderSettingsDialog.filedrop}
                  onChange={(v) => setFileLuFolderSettingsDialog(d => d ? { ...d, filedrop: v } : null)}
                  label={<div>
                    <div className="text-sm font-medium text-gray-900 dark:text-gray-100">{t('filelu.filedrop')}</div>
                    <div className="text-xs text-gray-500 dark:text-gray-400">{t('filelu.filedropDesc')}</div>
                  </div>}
                />
                <Checkbox
                  checked={fileLuFolderSettingsDialog.isPublic}
                  onChange={(v) => setFileLuFolderSettingsDialog(d => d ? { ...d, isPublic: v } : null)}
                  label={<div>
                    <div className="text-sm font-medium text-gray-900 dark:text-gray-100">{t('filelu.folderPublic')}</div>
                    <div className="text-xs text-gray-500 dark:text-gray-400">{t('filelu.folderPublicDesc')}</div>
                  </div>}
                />
              </div>
              <div className="flex gap-3 justify-end">
                <button onClick={() => setFileLuFolderSettingsDialog(null)} className="px-4 py-2 text-sm rounded-lg bg-gray-100 dark:bg-gray-700 text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 transition-colors">
                  {t('common.cancel')}
                </button>
                <button
                  onClick={async () => {
                    const d = fileLuFolderSettingsDialog;
                    if (!d) return;
                    const logId = humanLog.logRaw('activity.filelu_folder_settings', 'INFO', { provider: 'FileLu', filename: d.name }, 'running');
                    try {
                      await invoke('filelu_set_folder_settings', { path: d.path, filedrop: d.filedrop, isPublic: d.isPublic });
                      notify.success(t('filelu.folderSettingsSaved'));
                      humanLog.updateEntry(logId, { status: 'success', message: '[FileLu] Updated folder settings' });
                    } catch (err) { notify.error(String(err)); humanLog.updateEntry(logId, { status: 'error', message: '[FileLu] Update folder settings failed' }); }
                    setFileLuFolderSettingsDialog(null);
                  }}
                  className="px-4 py-2 text-sm rounded-lg bg-blue-600 text-white hover:bg-blue-700 transition-colors"
                >
                  {t('common.save')}
                </button>
              </div>
            </div>
          </div>
        )}
        {/* FileLu: Remote URL Upload Dialog */}
        {fileLuRemoteUploadDialog && (
          <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm">
            <div className="w-full max-w-md mx-4 rounded-xl shadow-2xl bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 animate-scale-in p-6">
              <h2 className="text-base font-semibold text-gray-900 dark:text-gray-100 mb-4">🌐 {t('filelu.remoteUrlUpload')}</h2>
              <p className="text-sm text-gray-500 dark:text-gray-400 mb-4">{t('filelu.remoteUrlUploadDesc')}</p>
              <input
                id="filelu-remote-url-input"
                type="url"
                placeholder="https://example.com/file.zip"
                className="w-full px-3 py-2 text-sm rounded-lg border border-gray-300 dark:border-gray-600 bg-gray-50 dark:bg-gray-700 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-2 focus:ring-blue-500 mb-2"
                autoFocus
              />
              <p className="text-xs text-gray-500 dark:text-gray-400 mb-5">
                {t('filelu.remoteUrlDest')}: <span className="font-mono text-blue-600 dark:text-blue-400">{fileLuRemoteUploadDialog.destPath}</span>
              </p>
              <div className="flex gap-3 justify-end">
                <button onClick={() => setFileLuRemoteUploadDialog(null)} className="px-4 py-2 text-sm rounded-lg bg-gray-100 dark:bg-gray-700 text-gray-500 dark:text-gray-400 hover:text-gray-900 dark:hover:text-gray-100 transition-colors">
                  {t('common.cancel')}
                </button>
                <button
                  onClick={async () => {
                    const input = document.getElementById('filelu-remote-url-input') as HTMLInputElement;
                    const url = input?.value?.trim();
                    if (!url) return;
                    const d = fileLuRemoteUploadDialog;
                    setFileLuRemoteUploadDialog(null);
                    try {
                      const code = await invoke<string>('filelu_remote_url_upload', { remoteUrl: url, destPath: d.destPath });
                      notify.success(t('filelu.remoteUrlUploaded', { code }));
                      loadRemoteFiles(undefined, true);
                    } catch (err) { notify.error(t('filelu.remoteUrlError'), String(err)); }
                  }}
                  className="px-4 py-2 text-sm rounded-lg bg-blue-600 text-white hover:bg-blue-700 transition-colors"
                >
                  {t('filelu.startUpload')}
                </button>
              </div>
            </div>
          </div>
        )}

        {compressDialogState && (
          <CompressDialog
            files={compressDialogState.files}
            defaultName={compressDialogState.defaultName}
            outputDir={compressDialogState.outputDir}
            onClose={() => setCompressDialogState(null)}
            onConfirm={async (opts: CompressOptions) => {
              const ext = opts.format === 'tar.gz' ? '.tar.gz' : opts.format === 'tar.xz' ? '.tar.xz' : opts.format === 'tar.bz2' ? '.tar.bz2' : `.${opts.format}`;
              const outputPath = `${compressDialogState.outputDir}/${opts.archiveName}${ext}`;
              const paths = compressDialogState.files.map(f => f.path);
              const logId = activityLog.log('INFO', `Compressing to ${opts.archiveName}${ext}...`, 'running');
              try {
                if (opts.format === 'zip') {
                  await invoke<string>('compress_files', { paths, outputPath, password: opts.password, compressionLevel: opts.compressionLevel });
                } else if (opts.format === '7z') {
                  await invoke<string>('compress_7z', { paths, outputPath, password: opts.password, compressionLevel: opts.compressionLevel });
                } else {
                  await invoke<string>('compress_tar', { paths, outputPath, format: opts.format, compressionLevel: opts.compressionLevel });
                }
                const suffix = opts.password ? ' (AES-256)' : '';
                activityLog.updateEntry(logId, { status: 'success', message: `Created ${opts.archiveName}${ext}${suffix}` });
                notify.success(t('toast.compressed'), t('toast.compressedDesc', { name: `${opts.archiveName}${ext}${suffix}` }));
                setCompressDialogState(null);
                await loadLocalFiles(currentLocalPath);
              } catch (err) {
                activityLog.log('ERROR', `Compression failed: ${String(err)}`, 'error');
                notify.error(t('contextMenu.compressionFailed'), String(err));
              }
            }}
          />
        )}

        {cryptomatorCreateDialog && (
          <CryptomatorCreateDialog
            outputDir={cryptomatorCreateDialog.outputDir}
            onClose={() => setCryptomatorCreateDialog(null)}
            onCreated={() => loadLocalFiles(currentLocalPath)}
          />
        )}


        <main className={`flex-1 min-h-0 p-6 overflow-auto flex flex-col ${devToolsMaximized && devToolsOpen ? 'hidden' : ''}`}>
          {!isConnected && showConnectionScreen ? (
            <IntroHub
              connectionParams={connectionParams}
              quickConnectDirs={quickConnectDirs}
              loading={loading}
              onConnectionParamsChange={setConnectionParams}
              onQuickConnectDirsChange={setQuickConnectDirs}
              onConnect={connectToFtp}
              onOpenCloudPanel={() => setShowCloudPanel(true)}
              hasExistingSessions={sessions.length > 0}
              serversRefreshKey={serversRefreshKey}
              onServersChanged={() => setServersRefreshKey(k => k + 1)}
              onAeroCloud={() => {
                if (isCloudActive) {
                  // Already connected — switch to cloud tab
                  setShowConnectionScreen(false);
                } else {
                  // Not connected — open config
                  setShowCloudPanel(true);
                }
              }}
              isAeroCloudConfigured={true}
              isAeroCloudConnected={isCloudActive}
              onAeroFile={handleToggleAeroFile}
              onSavedServerConnect={async (params, initialPath, localInitialPath) => {
                // NOTE: Do NOT set connectionParams here - that would show the form
                // The form should only appear when clicking Edit, not when connecting

                const normalizedParams = normalizeProviderConnectionParams(params);

                // Check if this is an OAuth provider
                const isOAuth = normalizedParams.protocol && (isOAuthProvider(normalizedParams.protocol) || isFourSharedProvider(normalizedParams.protocol));
                logger.debug('[onSavedServerConnect] params:', { ...normalizedParams, password: normalizedParams.password ? '***' : null });
                logger.debug('[onSavedServerConnect] isOAuth:', isOAuth);

                if (isOAuth) {
                  // OAuth provider is already connected via SavedServers/FourSharedConnect component
                  // Just switch to file manager view
                  setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                  setShowConnectionScreen(false);
                  const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud', fourshared: '4shared' };
                  const providerName = normalizedParams.displayName || (normalizedParams.protocol && providerNames[normalizedParams.protocol]) || normalizedParams.protocol || 'Unknown';
                  notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));
                  // Load remote files for OAuth provider - pass protocol explicitly
                  const savedOauthResp = await loadRemoteFiles(normalizedParams.protocol);
                  // Navigate to initial local directory if specified (with fallback for invalid paths)
                  let resolvedLocalPath = currentLocalPath;
                  if (localInitialPath) {
                    resolvedLocalPath = await safeChangeLocalDirectory(localInitialPath);
                  }
                  // Create session with provider name — pass fresh files to avoid stale closure
                  createSession(
                    providerName,
                    normalizedParams,
                    savedOauthResp?.current_path || initialPath || '/',
                    resolvedLocalPath,
                    savedOauthResp?.files
                  );
                  fetchStorageQuota(normalizedParams.protocol);
                  // Reset form for next "Add New Server"
                  setConnectionParams({ server: '', username: '', password: '' });
                  setQuickConnectDirs({ remoteDir: '', localDir: '' });
                  return;
                }

                // Check if this is a non-FTP provider protocol (S3, WebDAV, MEGA, Filen use provider_connect)
                const isProvider = usesProviderApi(normalizedParams.protocol);

                if (isProvider) {
                  // S3/WebDAV connection via provider_connect
                  setLoading(true);
                  setIsSyncNavigation(false);
                  setSyncBasePaths(null);
                  // Use displayName if available - no protocol prefix, icon shows protocol
                  const providerName = normalizedParams.displayName || (normalizedParams.protocol === 's3'
                    ? normalizedParams.options?.bucket || 'S3'
                    : normalizedParams.protocol === 'azure'
                      ? normalizedParams.options?.bucket || 'Azure'
                      : normalizedParams.protocol === 'filelu'
                        ? 'FileLu'
                        : normalizedParams.protocol === 'koofr'
                          ? `Koofr ${normalizedParams.username}`
                        : normalizedParams.protocol === 'opendrive'
                          ? t('savedServers.opendriveDisplay', { username: normalizedParams.username })
                        : normalizedParams.protocol === 'yandexdisk'
                          ? `Yandex Disk ${normalizedParams.username}`
                        : normalizedParams.protocol === 'mega' || normalizedParams.protocol === 'internxt' || normalizedParams.protocol === 'filen'
                          ? normalizedParams.username
                          : normalizedParams.protocol === 'immich'
                            ? (normalizedParams.providerId === 'pixelunion' ? 'PixelUnion' : normalizedParams.server.replace(/^https?:\/\//, ''))
                            : normalizedParams.server.split(':')[0]);
                  const protocolLabel = (normalizedParams.protocol || 'FTP').toUpperCase();
                  // SEC: mask credentials in log-only provider name to prevent data leakage
                  const maskedProviderName = normalizedParams.username && providerName.includes(normalizedParams.username)
                    ? providerName.replace(normalizedParams.username, maskCredential(normalizedParams.username))
                    : providerName;
                  const logId = humanLog.logStart('CONNECT', { server: maskedProviderName, protocol: protocolLabel });

                  try {
                    // Disconnect any existing connections
                    try { await invoke('provider_disconnect'); } catch { }
                    try { await invoke('disconnect_ftp'); } catch { }

                    const providerPayload = await buildProviderParams(normalizedParams, initialPath || null);
                    const connectedParams = providerPayload.effectiveParams;
                    const providerParams = providerPayload.providerParams;

                    logger.debug('[onSavedServerConnect] provider_connect params:', { ...providerParams, password: providerParams.password ? '***' : null, key_passphrase: providerParams.key_passphrase ? '***' : null });
                    // SEC-P1-06: TOFU host key check for SFTP
                    if (normalizedParams.protocol === 'sftp') {
                      const accepted = await checkSftpHostKey(normalizedParams.server, normalizedParams.port || 22);
                      if (!accepted) return;
                    }
                    const savedConnHost = connectedParams.server || getProviderHostFallback(connectedParams.protocol, connectedParams.username);
                    const { resolvedIp: savedIp, connectingLogId: savedConnLogId } = await logConnectionSteps(savedConnHost, connectedParams.port || 443, connectedParams.protocol || 'ftp');
                    await invoke('provider_connect', { params: providerParams });
                    if (savedConnLogId) humanLog.updateEntry(savedConnLogId, { status: 'success', message: t('activity.connected_to', { ip: savedIp || savedConnHost, port: String(connectedParams.port || 443) }) });
                    logConnectionSuccess(connectedParams.protocol || 'ftp', connectedParams.username, {
                      tlsMode: connectedParams.options?.tlsMode,
                      private_key_path: connectedParams.options?.private_key_path || undefined,
                    });

                    setConnectionParams(connectedParams);

                    setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                    humanLog.logSuccess('CONNECT', { server: maskedProviderName, protocol: protocolLabel }, logId);
                    notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));

                    // Load files using provider API
                    const response = await invoke<{ files: any[]; current_path: string }>('provider_list_files', {
                      path: initialPath || null
                    });

                    const files = response.files.map(f => ({
                      name: f.name,
                      path: f.path,
                      size: f.size,
                      is_dir: f.is_dir,
                      modified: f.modified,
                      permissions: f.permissions,
                      metadata: f.metadata,
                    }));
                    setRemoteFiles(files);
                    setCurrentRemotePath(response.current_path);
                    logListingComplete(response.current_path, files.length);

                    let resolvedLocalPath2 = currentLocalPath;
                    if (localInitialPath) {
                      resolvedLocalPath2 = await safeChangeLocalDirectory(localInitialPath);
                    }

                    createSession(
                      providerName,
                      connectedParams,
                      response.current_path,
                      resolvedLocalPath2,
                      files
                    );
                    fetchStorageQuota(connectedParams.protocol);
                    // Reset form for next "Add New Server"
                    setConnectionParams({ server: '', username: '', password: '' });
                    setQuickConnectDirs({ remoteDir: '', localDir: '' });
                  } catch (error) {
                    humanLog.logError('CONNECT', { server: maskedProviderName }, logId);
                    notify.error(t('connection.connectionFailed'), String(error));
                  } finally {
                    setLoading(false);
                  }
                  return;
                }

                // Standard FTP/SFTP connection
                setLoading(true);
                // Reset navigation sync for new connection
                setIsSyncNavigation(false);
                setSyncBasePaths(null);
                const protocolLabel = (params.protocol || 'FTP').toUpperCase();
                const logId = humanLog.logStart('CONNECT', { server: params.server, protocol: protocolLabel });
                try {
                  // Disconnect any existing provider connections first (S3, WebDAV, OAuth)
                  try { await invoke('provider_disconnect'); } catch { }

                  const savedFtpProto = params.protocol || 'ftp';
                  const { resolvedIp: savedFtpIp, connectingLogId: savedFtpConnLogId } = await logConnectionSteps(params.server, params.port || 21, savedFtpProto);
                  await invoke('connect_ftp', { params });
                  if (savedFtpConnLogId) humanLog.updateEntry(savedFtpConnLogId, { status: 'success', message: t('activity.connected_to', { ip: savedFtpIp || params.server, port: String(params.port || 21) }) });
                  logConnectionSuccess(savedFtpProto, params.username, {
                    tlsMode: params.options?.tlsMode,
                    private_key_path: params.options?.private_key_path || undefined,
                  });
                  setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                  humanLog.logSuccess('CONNECT', { server: params.server, protocol: protocolLabel }, logId);
                  notify.success(t('toast.connected'), t('toast.connectedTo', { server: params.server }));

                  // Get the actual remote path after connection
                  let savedFtpResponse: FileListResponse | null = null;
                  if (initialPath) {
                    // Pass the protocol explicitly to avoid using stale state from previous session
                    await changeRemoteDirectory(initialPath, params.protocol || 'ftp');
                  } else {
                    savedFtpResponse = await loadRemoteFiles();
                  }
                  if (savedFtpResponse) {
                    logListingComplete(savedFtpResponse.current_path || '/', savedFtpResponse.files?.length || 0);
                  }

                  let resolvedLocalPath3 = currentLocalPath;
                  if (localInitialPath) {
                    resolvedLocalPath3 = await safeChangeLocalDirectory(localInitialPath);
                  }
                  // Use displayName if provided, otherwise extract from server
                  const sessionName = params.displayName || params.server.split(':')[0];
                  createSession(
                    sessionName,
                    params,
                    savedFtpResponse?.current_path || initialPath || '/',
                    resolvedLocalPath3,
                    savedFtpResponse?.files
                  );
                  // Reset form for next "Add New Server"
                  setConnectionParams({ server: '', username: '', password: '' });
                  setQuickConnectDirs({ remoteDir: '', localDir: '' });
                } catch (error) {
                  humanLog.logError('CONNECT', { server: params.server }, logId);
                  notify.error(t('connection.connectionFailed'), String(error));
                } finally {
                  setLoading(false);
                }
              }}
              onSkipToFileManager={async () => {
                // If there are existing sessions, switch back to the last active one
                if (sessions.length > 0) {
                  const lastSession = sessions[sessions.length - 1];
                  // Hide connection screen FIRST to avoid flash
                  setShowConnectionScreen(false);
                  setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                  // Then switch session (async reconnect happens in background)
                  await switchSession(lastSession.id);
                } else {
                  // No existing sessions - enter AeroFile mode with sidebar
                  setShowConnectionScreen(false);
                  setActivePanel('local');
                  setShowSidebar(true);
                  await loadLocalFiles(currentLocalPath || '/');
                }
              }}
            />
          ) : (
            <div className="bg-white dark:bg-gray-800 rounded-xl shadow-xl overflow-hidden relative z-10 flex-1 min-h-0 flex flex-col">
              {/* Session Tabs + Local Path Tabs */}
              <SessionTabs
                sessions={sessions}
                activeSessionId={activeSessionId}
                onTabClick={switchSession}
                onTabClose={closeSession}
                onCloseAll={closeAllSessions}
                onNewTab={handleNewTabFromSavedServer}
                cloudTab={isCloudActive ? {
                  enabled: true,
                  syncing: cloudSyncing,
                  active: isCloudActive,
                  serverName: cloudServerName || 'AeroCloud'
                } : undefined}
                onCloudTabClick={handleCloudTabClick}
                onReorder={setSessions}
                transferLocked={hasQueueActivity}
                localTabs={(!isConnected || !showRemotePanel) ? localTabs : undefined}
                activeLocalTabId={(!isConnected || !showRemotePanel) ? activeLocalTabId : undefined}
                onLocalTabClick={(!isConnected || !showRemotePanel) ? switchLocalTab : undefined}
                onLocalTabClose={(!isConnected || !showRemotePanel) ? closeLocalTab : undefined}
                onLocalNewTab={(!isConnected || !showRemotePanel) ? createLocalTab : undefined}
                onLocalReorder={(!isConnected || !showRemotePanel) ? setLocalTabs : undefined}
              />
              {/* Toolbar */}
              <div role="toolbar" aria-label="File operations" className="flex items-center justify-between p-3 bg-gray-50 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600">
                <div className="flex gap-2">
                  {(() => {
                    const normP = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;
                    const atSyncRemoteRoot = isSyncNavigation && syncBasePaths && normP(currentRemotePath) === normP(syncBasePaths.remote);
                    const atSyncLocalRoot = isSyncNavigation && syncBasePaths && normP(currentLocalPath) === normP(syncBasePaths.local);
                    const isDisabled = activePanel === 'remote'
                      ? (currentRemotePath === '/' || !!atSyncRemoteRoot)
                      : (currentLocalPath === '/' || !!atSyncLocalRoot);
                    return <button
                      onClick={() => !isDisabled && (activePanel === 'remote' ? changeRemoteDirectory('..') : changeLocalDirectory(currentLocalPath.split(/[\\/]/).slice(0, -1).join('/') || '/'))}
                      className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 ${isDisabled ? 'bg-gray-200 dark:bg-gray-600 opacity-50 cursor-not-allowed' : 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500'}`}
                      disabled={isDisabled}
                      aria-label={t('common.up')}
                    >
                      <FolderUp size={16} /> {t('common.up')}
                    </button>;
                  })()}
                  <button onClick={() => activePanel === 'remote' ? loadRemoteFiles() : loadLocalFiles(currentLocalPath)} className="group px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5 transition-all hover:scale-105 hover:shadow-md" aria-label={t('common.refresh')}>
                    <RefreshCw size={16} className="group-hover:rotate-180 transition-transform duration-500" /> {t('common.refresh')}
                  </button>
                  <button onClick={() => createFolder(activePanel === 'remote')} className="group px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5 transition-all hover:scale-105 hover:shadow-md" aria-label={t('common.new')}>
                    <FolderPlus size={16} className="group-hover:scale-110 transition-transform" /> {t('common.new')}
                  </button>
                  {activePanel === 'local' && (
                    <button onClick={() => openInFileManager(currentLocalPath)} className="px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5" aria-label={t('common.open')}>
                      <FolderOpen size={16} /> {t('common.open')}
                    </button>
                  )}
                  {/* Sidebar Toggle (AeroFile mode only) */}
                  {(!isConnected || !showRemotePanel) && (
                    <button
                      onClick={toggleSidebar}
                      className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 ${showSidebar ? 'bg-blue-500/20 text-blue-400 dark:text-blue-400' : 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500'}`}
                      title={showSidebar ? t('sidebar.places') : t('sidebar.places')}
                    >
                      <PanelLeft size={16} />
                    </button>
                  )}
                  {/* View Mode Toggle (3-way: list → grid → large) */}
                  <button
                    onClick={() => setViewMode(viewMode === 'list' ? 'grid' : viewMode === 'grid' ? 'large' : 'list')}
                    className="px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5"
                    title={t(`viewMode.${viewMode === 'list' ? 'grid' : viewMode === 'grid' ? 'large' : 'list'}`)}
                  >
                    {viewMode === 'list' ? <LayoutGrid size={16} /> : viewMode === 'grid' ? <Rows3 size={16} /> : <List size={16} />}
                  </button>
                  {/* Upload / Download dynamic button */}
                  {isConnected && showRemotePanel && (
                    <button
                      onClick={() => activePanel === 'local' ? uploadMultipleFiles() : downloadMultipleFiles()}
                      disabled={(activePanel === 'local' ? selectedLocalFiles.size : selectedRemoteFiles.size) === 0}
                      className={`relative px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-all ${(activePanel === 'local' ? selectedLocalFiles.size : selectedRemoteFiles.size) > 0
                        ? 'bg-green-500 hover:bg-green-600 text-white shadow-sm hover:shadow-md'
                        : 'bg-gray-200 dark:bg-gray-600 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                        }`}
                      title={activePanel === 'local' ? t('browser.uploadFiles') : t('browser.downloadFiles')}
                    >
                      {activePanel === 'local' ? <Upload size={16} /> : <Download size={16} />}
                      {activePanel === 'local' ? t('browser.uploadFiles') : t('browser.downloadFiles')}
                      {(() => {
                        const count = activePanel === 'local' ? selectedLocalFiles.size : selectedRemoteFiles.size;
                        return count > 1 ? (
                          <span className="absolute -top-1.5 -right-1.5 min-w-[18px] h-[18px] flex items-center justify-center rounded-full bg-white text-green-600 text-[10px] font-bold shadow-sm border border-green-300">
                            {count}
                          </span>
                        ) : null;
                      })()}
                    </button>
                  )}
                  {/* Delete button */}
                  <button
                    onClick={() => {
                      if (activePanel === 'remote' && selectedRemoteFiles.size > 0) {
                        deleteMultipleRemoteFiles(Array.from(selectedRemoteFiles));
                      } else if (activePanel === 'local' && selectedLocalFiles.size > 0) {
                        deleteMultipleLocalFiles(Array.from(selectedLocalFiles));
                      }
                    }}
                    disabled={(activePanel === 'remote' ? selectedRemoteFiles.size : selectedLocalFiles.size) === 0}
                    className={`relative px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-all ${(activePanel === 'remote' ? selectedRemoteFiles.size : selectedLocalFiles.size) > 0
                      ? 'bg-red-500 hover:bg-red-600 text-white shadow-sm hover:shadow-md'
                      : 'bg-gray-200 dark:bg-gray-600 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                      }`}
                    title={t('contextMenu.delete')}
                    aria-label={t('contextMenu.delete')}
                  >
                    <Trash2 size={16} />
                    {(() => {
                      const count = activePanel === 'remote' ? selectedRemoteFiles.size : selectedLocalFiles.size;
                      return count > 1 ? (
                        <span className="absolute -top-1.5 -right-1.5 min-w-[18px] h-[18px] flex items-center justify-center rounded-full bg-white text-red-600 text-[10px] font-bold shadow-sm border border-red-300">
                          {count}
                        </span>
                      ) : null;
                    })()}
                  </button>
                  {/* Separator */}
                  {isConnected && (
                    <div className="w-px h-7 bg-gray-300 dark:bg-gray-600 mx-1" />
                  )}
                  {isConnected && showRemotePanel && (
                    <>
                      <button
                        onClick={cancelTransfer}
                        disabled={!isForceStopMode && !hasActiveTransfer && !hasQueueActivity}
                        className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-all ${isForceStopMode
                          ? 'bg-orange-500 hover:bg-orange-600 text-white shadow-sm hover:shadow-md animate-pulse'
                          : (hasActiveTransfer || hasQueueActivity)
                            ? 'bg-red-500 hover:bg-red-600 text-white shadow-sm hover:shadow-md animate-pulse'
                            : 'bg-gray-200 dark:bg-gray-600 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                          }`}
                        title={isForceStopMode ? t('transfer.forceStop') : t('transfer.cancelAll')}
                      >
                        {isForceStopMode ? <Zap size={16} /> : <XCircle size={16} />}
                      </button>
                      <button
                        onClick={toggleSyncNavigation}
                        className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-colors ${isSyncNavigation
                          ? 'bg-purple-500 hover:bg-purple-600 text-white'
                          : 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500'
                          }`}
                        title={isSyncNavigation ? t('common.syncNavigationActive') : t('common.syncNavigation')}
                      >
                        {isSyncNavigation ? <Link2 size={16} /> : <Unlink size={16} />}
                        {isSyncNavigation ? t('common.synced') : t('common.sync')}
                      </button>
                      <button
                        onClick={() => setShowSyncPanel(true)}
                        className="px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5 transition-colors"
                        title={t('statusBar.syncFiles')}
                      >
                        <FolderSync size={16} /> {t('statusBar.syncFiles')}
                      </button>
                      <button
                        onClick={cycleTransferSpeedPreset}
                        disabled={!supportsParallelTransferPresets}
                        className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-colors ${
                          supportsParallelTransferPresets
                            ? effectiveTransferSpeedPreset === 'super'
                              ? 'bg-emerald-500 hover:bg-emerald-600 text-white'
                              : effectiveTransferSpeedPreset === 'fast'
                                ? 'bg-blue-500 hover:bg-blue-600 text-white'
                                : 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500'
                            : 'bg-gray-200 dark:bg-gray-600 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                        }`}
                        title={supportsParallelTransferPresets
                          ? `${t('transfer.mode')}: ${effectiveTransferSpeedLabel}`
                          : t('transfer.modeUnavailable')}
                      >
                        <Zap size={16} />
                        {effectiveTransferSpeedLabel}
                      </button>
                    </>
                  )}
                </div>
                <div className="flex gap-2">
                  {isConnected && showRemotePanel && (
                    <>
                      <PanelSwitcher
                        activePanel={activePanel}
                        swapPanels={swapPanels}
                        onPanelSelect={setActivePanel}
                        onSwap={toggleSwapPanels}
                        remoteLabel={t('browser.remote')}
                        localLabel={t('browser.local')}
                        swapTitle={t('settings.swapPanels')}
                      />
                      <div className="w-px h-6 bg-gray-300 dark:bg-gray-500 mx-1 hidden lg:block" />
                    </>
                  )}
                  {isConnected && (
                    <button
                      onClick={() => { setShowRemotePanel(p => { if (p) setActivePanel('local'); else setShowLocalPreview(false); return !p; }); }}
                      className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 ${showRemotePanel ? 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500' : 'bg-blue-500 text-white'}`}
                      title={showRemotePanel ? 'Local only' : 'Show remote'}
                    >
                      <HardDrive size={16} />
                    </button>
                  )}
                  {/* Preview Toggle - only in local-only (AeroFile) mode */}
                  {(!isConnected || !showRemotePanel) && (
                    <button
                      onClick={() => setShowLocalPreview(p => !p)}
                      className={`px-3 py-1.5 rounded-lg text-sm items-center gap-1.5 hidden md:flex ${showLocalPreview ? 'bg-blue-500 text-white' : 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500'}`}
                      title={t('common.preview')}
                    >
                      <Eye size={16} /><span className="hidden lg:inline">{t('common.preview')}</span>
                    </button>
                  )}
                </div>
              </div>

              {/* Dual Panel (or single panel when not connected) */}
              <div className="flex flex-1 min-h-0">
                {/* Remote — hidden when not connected or local-only mode */}
                {isConnected && showRemotePanel && <div
                  role="region"
                  aria-label="Remote files"
                  className={`w-1/2 ${swapPanels ? 'border-l order-2' : 'border-r order-1'} border-gray-200 dark:border-gray-700 flex flex-col transition-all duration-150 ${crossPanelTarget === 'remote' ? 'ring-2 ring-inset ring-blue-400 bg-blue-50/30 dark:bg-blue-900/10' : ''}`}
                  onDragOver={(e) => handlePanelDragOver(e, true)}
                  onDrop={(e) => handlePanelDrop(e, true)}
                  onDragLeave={handlePanelDragLeave}
                >
                  <div className="px-3 py-1.5 bg-gray-100 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600 text-sm font-medium flex items-center gap-2">
                    <div className={`flex-1 flex items-center bg-white dark:bg-gray-800 rounded-md border ${isSyncPathMismatch ? 'border-amber-400 dark:border-amber-500' : 'border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500'} focus-within:border-blue-500 dark:focus-within:border-blue-400 focus-within:ring-2 focus-within:ring-blue-500/20 transition-all overflow-hidden`}>
                      {/* Protocol icon inside address bar (like Chrome favicon) */}
                      <div className="flex-shrink-0 pl-2.5 pr-1 flex items-center" title={(() => {
                        const protocol = connectionParams.protocol || 'ftp';
                        switch (protocol) {
                          case 's3': return 'Amazon S3';
                          case 'webdav': return 'WebDAV';
                          case 'sftp': return 'SFTP (Secure)';
                          case 'ftps': return 'FTPS (Secure)';
                          case 'googledrive': return 'Google Drive';
                          case 'dropbox': return 'Dropbox';
                          case 'onedrive': return 'OneDrive';
                          case 'box': return 'Box';
                          case 'pcloud': return 'pCloud';
                          case 'azure': return 'Azure Blob';
                          case 'filen': return 'Filen';
                          case 'mega': return 'MEGA';
                          default: return 'FTP';
                        }
                      })()}>
                        {(() => {
                          const protocol = connectionParams.protocol || 'ftp';
                          const iconClass = isSyncPathMismatch ? 'text-amber-500' : isSyncNavigation ? 'text-purple-500' : isConnected ? 'text-green-500' : 'text-gray-400';
                          if (isSyncPathMismatch) return <AlertTriangle size={14} className={iconClass} />;
                          switch (protocol) {
                            case 's3': return <Cloud size={14} className={iconClass} />;
                            case 'webdav': return <Server size={14} className={iconClass} />;
                            case 'sftp': return <Lock size={14} className={iconClass} />;
                            case 'ftps': return <Shield size={14} className={iconClass} />;
                            case 'googledrive': return <Cloud size={14} className={iconClass} />;
                            case 'dropbox': return <Archive size={14} className={iconClass} />;
                            case 'onedrive': return <Cloud size={14} className={iconClass} />;
                            case 'mega': return <Shield size={14} className={iconClass} />;
                            default: return <Globe size={14} className={iconClass} />;
                          }
                        })()}
                      </div>
                      <input
                        type="text"
                        value={isConnected ? currentRemotePath : t('browser.notConnected')}
                        onChange={(e) => setCurrentRemotePath(e.target.value)}
                        onKeyDown={(e) => e.key === 'Enter' && isConnected && changeRemoteDirectory((e.target as HTMLInputElement).value)}
                        disabled={!isConnected}
                        className={`flex-1 pl-1 pr-2 py-1 bg-transparent border-none outline-none text-sm cursor-text selection:bg-blue-200 dark:selection:bg-blue-800 disabled:cursor-default disabled:text-gray-400 disabled:bg-gray-50 dark:disabled:bg-gray-900 ${isSyncPathMismatch ? 'text-amber-600 dark:text-amber-400' : ''}`}
                        title={isSyncPathMismatch ? t('browser.syncPathMismatch') : isConnected ? t('browser.editPathHint') : t('browser.notConnected')}
                        placeholder="/path/to/directory"
                      />
                    </div>
                    <button
                      onClick={(e) => {
                        const btn = e.currentTarget;
                        btn.querySelector('svg')?.classList.add('animate-spin');
                        setTimeout(() => btn.querySelector('svg')?.classList.remove('animate-spin'), 600);
                        loadRemoteFiles();
                      }}
                      className="flex-shrink-0 p-1.5 rounded text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
                      title={t('common.refresh')}
                    >
                      <RefreshCw size={13} />
                    </button>
                    {isConnected && (getActiveProviderProtocol() === 'github' || getActiveProviderProtocol() === 'gitlab') && gitHubRepoInfo && gitHubRepoInfo.writeModeKind !== 'unknown' && (
                      <GitHubBranchSelector
                        currentBranch={gitHubRepoInfo.branch}
                        branches={gitHubBranches}
                        writeMode={gitHubRepoInfo.writeModeKind}
                        workingBranch={gitHubRepoInfo.workingBranch || undefined}
                        onBranchChange={(branch: string) => void switchGitHubBranch(branch)}
                        onRefresh={() => void refreshGitHubContext(true)}
                      />
                    )}
                    {isConnected && (getActiveProviderProtocol() === 'github' || getActiveProviderProtocol() === 'gitlab') && gitHubRepoInfo && (
                      <>
                        <button
                          onClick={() => {
                            if (getActiveProviderProtocol() === 'gitlab') setShowGitLabReleaseBrowser(true);
                            else setShowGitHubReleaseBrowser(true);
                          }}
                          className={`flex-shrink-0 p-1.5 rounded transition-colors ${getActiveProviderProtocol() === 'gitlab' ? 'text-orange-400 hover:text-orange-300 hover:bg-orange-500/10' : 'text-green-400 hover:text-green-300 hover:bg-green-500/10'}`}
                          title="Releases"
                        >
                          {getActiveProviderProtocol() === 'gitlab'
                            ? <Tag size={13} className="text-orange-400" />
                            : <GitHubReleaseIcon size={13} className="text-green-400" />
                          }
                        </button>
                        {hasGitHubPages && getActiveProviderProtocol() === 'github' && (
                          <button
                            onClick={() => setShowGitHubPages(true)}
                            className="flex-shrink-0 p-1.5 rounded text-green-400 hover:text-green-300 hover:bg-green-500/10 transition-colors"
                            title="GitHub Pages"
                          >
                            <GitHubPagesIcon size={13} />
                          </button>
                        )}
                        {getActiveProviderProtocol() === 'github' && (
                        <button
                          onClick={() => setShowGitHubActions(true)}
                          className={`relative flex-shrink-0 p-1.5 rounded transition-colors ${
                            hasActiveGitHubActions
                              ? 'text-amber-400 hover:text-amber-300 hover:bg-amber-500/10'
                              : 'text-green-400 hover:text-green-300 hover:bg-green-500/10'
                          }`}
                          title={hasActiveGitHubActions ? 'GitHub Actions (running)' : 'GitHub Actions'}
                        >
                          <GitHubActionsIcon size={13} />
                          {hasActiveGitHubActions && (
                            <span className="absolute -top-0.5 -right-0.5 w-2 h-2 bg-amber-400 rounded-full animate-pulse" />
                          )}
                        </button>
                        )}
                      </>
                    )}
                    {isConnected && getActiveProviderProtocol() === 'filen' && (
                      <button
                        onClick={() => setShowFilenNotes(true)}
                        className="flex-shrink-0 p-1.5 rounded text-emerald-400 hover:text-emerald-300 hover:bg-emerald-500/10 transition-colors"
                        title={t('filenNotes.title')}
                      >
                        <FileText size={13} />
                      </button>
                    )}
                    {isConnected && (() => {
                      const proto = getActiveProviderProtocol();
                      const trashMap: Record<string, () => void> = {
                        zohoworkdrive: () => setShowZohoTrash(true),
                        jottacloud: () => setShowJottaTrash(true),
                        mega: () => setShowMegaTrash(true),
                        googledrive: () => setShowGDriveTrash(true),
                        box: () => setShowBoxTrash(true),
                        dropbox: () => setShowDropboxTrash(true),
                        filelu: () => setShowFileLuTrash(true),
                        koofr: () => setShowKoofrTrash(true),
                        opendrive: () => setShowOpenDriveTrash(true),
                        yandexdisk: () => setShowYandexTrash(true),
                        kdrive: () => setShowKDriveTrash(true),
                        pcloud: () => setShowPCloudTrash(true),
                      };
                      let handler = trashMap[proto || ''];
                      // Nextcloud trash API only works on Nextcloud/FeliCloud WebDAV servers
                      if (proto === 'webdav') {
                        const pid = connectionParams.providerId || sessions.find(s => s.id === activeSessionId)?.providerId;
                        if (pid === 'nextcloud' || pid === 'felicloud') {
                          handler = () => setShowNextcloudTrash(true);
                        }
                      }
                      return handler ? (
                        <button
                          onClick={handler}
                          className="flex-shrink-0 p-1.5 rounded text-red-400 hover:text-red-300 hover:bg-red-500/10 transition-colors"
                          title={t('contextMenu.viewTrash')}
                        >
                          <Trash2 size={13} />
                        </button>
                      ) : null;
                    })()}
                    {isConnected && (
                      <button
                        onClick={() => {
                          if (remoteSearchResults !== null) {
                            setRemoteSearchResults(null);
                            setRemoteSearchQuery('');
                          } else {
                            setShowRemoteSearchBar(prev => !prev);
                          }
                        }}
                        className={`flex-shrink-0 p-1.5 rounded transition-colors ${remoteSearchResults !== null ? 'text-blue-500' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'}`}
                        title={remoteSearchResults !== null ? 'Clear search' : 'Search files'}
                      >
                        {remoteSearching ? <RefreshCw size={13} className="animate-spin" /> : <Search size={13} />}
                      </button>
                    )}
                    {debugMode && isConnected && (
                      <button
                        onClick={() => {
                          const lines = sortedRemoteFiles.map(f =>
                            `${f.is_dir ? 'd' : '-'}\t${f.size}\t${f.modified || ''}\t${f.name}`
                          );
                          const header = `# Remote files: ${currentRemotePath} (${sortedRemoteFiles.length} entries)\n# type\tsize\tmodified\tname`;
                          navigator.clipboard.writeText(header + '\n' + lines.join('\n'));
                          notify.success(t('debug.title'), t('debug.filesCopied', { count: sortedRemoteFiles.length }));
                        }}
                        className="flex-shrink-0 p-1.5 rounded text-amber-500 hover:text-amber-600 dark:hover:text-amber-400 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
                        title={t('debug.copyFileListToClipboard')}
                      >
                        <ClipboardList size={13} />
                      </button>
                    )}
                  </div>
                  {/* Remote Search Bar */}
                  {showRemoteSearchBar && isConnected && (
                    <div className="px-3 py-1.5 bg-blue-50 dark:bg-blue-900/20 border-b border-blue-200 dark:border-blue-800 flex items-center gap-2">
                      <Search size={14} className="text-blue-500 flex-shrink-0" />
                      <input
                        autoFocus
                        type="text"
                        placeholder={t('search.remote_placeholder') || 'Search remote files...'}
                        value={remoteSearchQuery}
                        onChange={e => setRemoteSearchQuery(e.target.value)}
                        onKeyDown={e => {
                          if (e.key === 'Enter' && remoteSearchQuery.trim()) {
                            handleRemoteSearch(remoteSearchQuery);
                          } else if (e.key === 'Escape') {
                            setShowRemoteSearchBar(false);
                            setRemoteSearchQuery('');
                            setRemoteSearchResults(null);
                          }
                        }}
                        className="flex-1 text-sm bg-transparent border-none outline-none placeholder-gray-400"
                      />
                      {remoteSearching && <RefreshCw size={14} className="animate-spin text-blue-500 flex-shrink-0" />}
                      {remoteSearchQuery.trim() && (
                        <span className="text-xs text-blue-600 dark:text-blue-400 flex-shrink-0">{sortedRemoteFiles.length} results</span>
                      )}
                      <button
                        onClick={() => { setShowRemoteSearchBar(false); setRemoteSearchQuery(''); setRemoteSearchResults(null); }}
                        className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 flex-shrink-0"
                      >
                        <X size={14} />
                      </button>
                    </div>
                  )}
                  <div className="flex-1 overflow-auto" onContextMenu={(e) => {
                    // Only show empty-area menu if click target is the container itself or table background
                    const target = e.target as HTMLElement;
                    const isFileRow = target.closest('tr[data-file-row]') || target.closest('[data-file-card]');
                    if (!isFileRow && isConnected) showRemoteEmptyContextMenu(e);
                  }}>
                    {!isConnected ? (
                      <div className="flex flex-col items-center justify-center h-full text-gray-400">
                        <Cloud size={64} className="mb-4 opacity-30" />
                        <p className="text-lg font-medium">{t('browser.notConnected')}</p>
                        <p className="text-sm mt-1">{t('browser.clickConnectPrompt')}</p>
                        <button
                          onClick={() => setShowConnectionScreen(true)}
                          className="mt-4 px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg shadow-sm hover:shadow-md transition-all flex items-center gap-2"
                        >
                          <Cloud size={16} /> {t('browser.connectToServer')}
                        </button>
                      </div>
                    ) : viewMode === 'list' ? (
                      <table className="w-full text-sm" role="grid" aria-label="Remote files">
                        <thead className="bg-gray-50 dark:bg-gray-700 sticky top-0" role="rowgroup">
                          <tr role="row">
                            <SortableHeader label={t('browser.name')} field="name" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} />
                            {visibleColumns.includes('size') && <SortableHeader label={t('browser.size')} field="size" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} />}
                            {visibleColumns.includes('type') && <SortableHeader label={t('browser.type')} field="type" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} className="hidden xl:table-cell" />}
                            {visibleColumns.includes('permissions') && <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider whitespace-nowrap hidden xl:table-cell">{t('browser.permsHeader')}</th>}
                            {visibleColumns.includes('modified') && <SortableHeader label={t('browser.modified')} field="modified" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} />}
                          </tr>
                        </thead>
                        <tbody className="divide-y divide-gray-100 dark:divide-gray-700" role="rowgroup">
                          {/* Go Up Row - always visible, disabled at root or sync base */}
                          {(() => {
                            const normP = (p: string) => p.endsWith('/') && p.length > 1 ? p.slice(0, -1) : p;
                            const canGoUp = currentRemotePath !== '/' && !(isSyncNavigation && syncBasePaths && normP(currentRemotePath) === normP(syncBasePaths.remote));
                            return (
                              <tr
                                role="row"
                                className={`${canGoUp ? 'hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer' : 'opacity-50 cursor-not-allowed'}`}
                                onClick={() => canGoUp && changeRemoteDirectory('..')}
                              >
                                <td className="px-4 py-2 flex items-center gap-2 text-gray-500">
                                  {iconProvider.getFolderUpIcon(16).icon}
                                  <span className="italic">{t('browser.parentFolder')}</span>
                                </td>
                                {visibleColumns.includes('size') && <td className="px-4 py-2 text-xs text-gray-400">—</td>}
                                {visibleColumns.includes('type') && <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-400">—</td>}
                                {visibleColumns.includes('permissions') && <td className="hidden xl:table-cell px-4 py-2 text-xs text-gray-400">—</td>}
                                {visibleColumns.includes('modified') && <td className="px-4 py-2 text-xs text-gray-400">—</td>}
                              </tr>
                            );
                          })()}
                          {sortedRemoteFiles.map((file, i) => (
                            <tr
                              key={`${file.name}-${i}`}
                              data-file-row
                              role="row"
                              aria-selected={selectedRemoteFiles.has(file.name)}
                              draggable={file.name !== '..'}
                              onDragStart={(e) => handleDragStart(e, file, true, selectedRemoteFiles, sortedRemoteFiles)}
                              onDragEnd={handleDragEnd}
                              onDragOver={(e) => handleDragOver(e, file.path, file.is_dir, true)}
                              onDragLeave={handleDragLeave}
                              onDrop={(e) => file.is_dir && handleDrop(e, file.path, true)}
                              onClick={(e) => {
                                if (file.name === '..') return;
                                setActivePanel('remote');
                                if (e.shiftKey && lastSelectedRemoteIndex !== null) {
                                  // Shift+click: select range
                                  const start = Math.min(lastSelectedRemoteIndex, i);
                                  const end = Math.max(lastSelectedRemoteIndex, i);
                                  const rangeNames = sortedRemoteFiles.slice(start, end + 1).map(f => f.name);
                                  setSelectedRemoteFiles(new Set(rangeNames));
                                } else if (e.ctrlKey || e.metaKey) {
                                  // Ctrl/Cmd+click: toggle selection
                                  setSelectedRemoteFiles(prev => {
                                    const next = new Set(prev);
                                    if (next.has(file.name)) next.delete(file.name);
                                    else next.add(file.name);
                                    return next;
                                  });
                                  setLastSelectedRemoteIndex(i);
                                } else {
                                  // Normal click: toggle if already sole selection, otherwise select
                                  if (selectedRemoteFiles.size === 1 && selectedRemoteFiles.has(file.name)) {
                                    setSelectedRemoteFiles(new Set());
                                  } else {
                                    setSelectedRemoteFiles(new Set([file.name]));
                                  }
                                  setLastSelectedRemoteIndex(i);
                                }
                              }}
                              onDoubleClick={() => handleRemoteFileAction(file)}
                              onContextMenu={(e: React.MouseEvent) => showRemoteContextMenu(e, file)}
                              className={`cursor-pointer transition-colors ${dropTargetPath === file.path && file.is_dir
                                ? 'bg-green-100 dark:bg-green-900/40 ring-2 ring-green-500'
                                : selectedRemoteFiles.has(file.name)
                                  ? 'bg-blue-100 dark:bg-blue-900/40'
                                  : 'hover:bg-blue-50 dark:hover:bg-gray-700'
                                } ${dragData?.sourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                            >
                              <td className="px-4 py-2 flex items-center gap-2">
                                {file.is_dir ? iconProvider.getFolderIcon(16).icon : iconProvider.getFileIcon(file.name, 16).icon}
                                {inlineRename?.path === file.path && inlineRename?.isRemote ? (
                                  <input
                                    ref={inlineRenameRef}
                                    type="text"
                                    value={inlineRenameValue}
                                    onChange={(e) => setInlineRenameValue(e.target.value)}
                                    onKeyDown={handleInlineRenameKeyDown}
                                    onBlur={commitInlineRename}
                                    onClick={(e) => e.stopPropagation()}
                                    className="px-1 py-0.5 text-sm bg-white dark:bg-gray-900 border border-blue-500 rounded outline-none min-w-[120px]"
                                  />
                                ) : (
                                  <span
                                    className="cursor-text"
                                    onClick={(e) => {
                                      if (selectedRemoteFiles.size === 1 && selectedRemoteFiles.has(file.name) && file.name !== '..') {
                                        e.stopPropagation();
                                        startInlineRename(file.path, file.name, true);
                                      }
                                    }}
                                  >
                                    {displayName(file.name, file.is_dir)}
                                  </span>
                                )}
                                {file.metadata?.starred === 'true' && <Star size={12} className="text-yellow-400 fill-yellow-400" />}
                                {file.metadata?.box_tags && file.metadata.box_tags.split(',').slice(0, 3).map(tag => (
                                  <span key={tag} className="inline-block px-1.5 py-0 text-[9px] rounded-full bg-blue-100 text-blue-600 dark:bg-blue-900/40 dark:text-blue-300 leading-tight">{tag}</span>
                                ))}
                                {file.metadata?.box_tags && file.metadata.box_tags.split(',').length > 3 && (
                                  <span className="text-[9px] text-gray-400">+{file.metadata.box_tags.split(',').length - 3}</span>
                                )}
                                {file.metadata?.storage_class && file.metadata.storage_class !== 'STANDARD' && (
                                  <span className={`inline-block px-1.5 py-0 text-[9px] rounded-full leading-tight ${
                                    file.metadata.storage_class.includes('GLACIER') || file.metadata.storage_class === 'DEEP_ARCHIVE'
                                      ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/40 dark:text-blue-300'
                                      : file.metadata.storage_class.includes('IA') || file.metadata.storage_class === 'REDUCED_REDUNDANCY'
                                        ? 'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-300'
                                        : 'bg-green-100 text-green-700 dark:bg-green-900/40 dark:text-green-300'
                                  }`} title={file.metadata.storage_class.replace(/_/g, ' ')}>
                                    {file.metadata.storage_class === 'STANDARD_IA' ? 'IA' :
                                     file.metadata.storage_class === 'ONEZONE_IA' ? '1Z-IA' :
                                     file.metadata.storage_class === 'GLACIER' ? 'Glacier' :
                                     file.metadata.storage_class === 'GLACIER_IR' ? 'Glacier IR' :
                                     file.metadata.storage_class === 'DEEP_ARCHIVE' ? 'Deep' :
                                     file.metadata.storage_class === 'INTELLIGENT_TIERING' ? 'IT' :
                                     file.metadata.storage_class === 'REDUCED_REDUNDANCY' ? 'RR' :
                                     file.metadata.storage_class.replace(/_/g, ' ')}
                                  </span>
                                )}
                                {lockedFiles.has(file.path) && <span title={t('browser.locked')}><Lock size={12} className="text-orange-500" /></span>}
                                {getSyncBadge(file.path, file.modified || undefined, false)}
                              </td>
                              {visibleColumns.includes('size') && <td className="px-3 py-2 text-xs text-gray-500 whitespace-nowrap">{file.size ? formatBytes(file.size) : (!file.is_dir && file.size === 0 ? <span title={t('toast.zeroByteWarning')}>&#9888; 0 B</span> : '-')}</td>}
                              {visibleColumns.includes('type') && <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-500 uppercase">{file.is_dir ? t('browser.folderType') : (file.name.includes('.') ? file.name.split('.').pop() : '—')}</td>}
                              {visibleColumns.includes('permissions') && <td className="hidden xl:table-cell px-3 py-2"><FeatureBadge value={file.permissions} locked={isPasswordProtectedFile(file)} watermarked={file.metadata?.watermarked === 'true'} /></td>}
                              {visibleColumns.includes('modified') && <td className="px-3 py-2 text-xs text-gray-500 whitespace-nowrap">{formatDate(file.modified)}</td>}
                            </tr>
                          ))}
                        </tbody>
                      </table>
                    ) : viewMode === 'large' ? (
                      /* Large Icons View */
                      <LargeIconsGrid
                        files={sortedRemoteFiles as any}
                        selectedFiles={selectedRemoteFiles}
                        currentPath={currentRemotePath}
                        onFileClick={(file, e) => {
                          setActivePanel('remote');
                          const idx = sortedRemoteFiles.findIndex(f => f.name === file.name);
                          if (e.shiftKey && lastSelectedRemoteIndex !== null) {
                            const start = Math.min(lastSelectedRemoteIndex, idx);
                            const end = Math.max(lastSelectedRemoteIndex, idx);
                            const rangeNames = sortedRemoteFiles.slice(start, end + 1).map(f => f.name);
                            setSelectedRemoteFiles(new Set(rangeNames));
                          } else if (e.ctrlKey || e.metaKey) {
                            setSelectedRemoteFiles(prev => {
                              const next = new Set(prev);
                              if (next.has(file.name)) next.delete(file.name);
                              else next.add(file.name);
                              return next;
                            });
                            setLastSelectedRemoteIndex(idx);
                          } else {
                            if (selectedRemoteFiles.size === 1 && selectedRemoteFiles.has(file.name)) {
                              setSelectedRemoteFiles(new Set());
                            } else {
                              setSelectedRemoteFiles(new Set([file.name]));
                            }
                            setLastSelectedRemoteIndex(idx);
                          }
                        }}
                        onFileDoubleClick={(file) => handleRemoteFileAction(file as any)}
                        onNavigateUp={() => changeRemoteDirectory('..')}
                        isAtRoot={currentRemotePath === '/'}
                        getFileIcon={(name, isDir) => {
                          if (isDir) return iconProvider.getFolderIcon(64);
                          return iconProvider.getFileIcon(name, 48);
                        }}
                        getFolderUpIcon={() => iconProvider.getFolderUpIcon(64)}
                        onContextMenu={(e, file) => file ? showRemoteContextMenu(e, file as any) : undefined}
                        onDragStart={(e, file) => handleDragStart(e, file as any, true, selectedRemoteFiles, sortedRemoteFiles)}
                        onDragOver={(e, file) => handleDragOver(e, file.path, file.is_dir, true)}
                        onDrop={(e, file) => file.is_dir && handleDrop(e, file.path, true)}
                        onDragLeave={handleDragLeave}
                        onDragEnd={handleDragEnd}
                        dragOverTarget={dropTargetPath}
                        inlineRename={inlineRename}
                        onInlineRenameChange={setInlineRenameValue}
                        onInlineRenameCommit={commitInlineRename}
                        onInlineRenameCancel={() => setInlineRename(null)}
                        formatBytes={formatBytes}
                        showFileExtensions={true}
                      />
                    ) : (
                      /* Grid View */
                      <div className="file-grid">
                        {/* Go Up Item - always visible, disabled at root */}
                        <div
                          className={`file-grid-item file-grid-go-up ${currentRemotePath === '/' ? 'opacity-50 cursor-not-allowed' : ''}`}
                          onClick={() => currentRemotePath !== '/' && changeRemoteDirectory('..')}
                        >
                          <div className="file-grid-icon">
                            {iconProvider.getFolderUpIcon(32).icon}
                          </div>
                          <span className="file-grid-name italic text-gray-500">{t('browser.parentFolder')}</span>
                        </div>
                        {sortedRemoteFiles.map((file, i) => (
                          <div
                            key={`${file.name}-${i}`}
                            data-file-card
                            draggable={file.name !== '..'}
                            onDragStart={(e) => handleDragStart(e, file, true, selectedRemoteFiles, sortedRemoteFiles)}
                            onDragEnd={handleDragEnd}
                            onDragOver={(e) => handleDragOver(e, file.path, file.is_dir, true)}
                            onDragLeave={handleDragLeave}
                            onDrop={(e) => file.is_dir && handleDrop(e, file.path, true)}
                            className={`file-grid-item ${dropTargetPath === file.path && file.is_dir
                              ? 'ring-2 ring-green-500 bg-green-100 dark:bg-green-900/40'
                              : selectedRemoteFiles.has(file.name) ? 'selected' : ''
                              } ${dragData?.sourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                            onClick={(e) => {
                              if (file.name === '..') return;
                              setActivePanel('remote');
                              if (e.shiftKey && lastSelectedRemoteIndex !== null) {
                                const start = Math.min(lastSelectedRemoteIndex, i);
                                const end = Math.max(lastSelectedRemoteIndex, i);
                                const rangeNames = sortedRemoteFiles.slice(start, end + 1).map(f => f.name);
                                setSelectedRemoteFiles(new Set(rangeNames));
                              } else if (e.ctrlKey || e.metaKey) {
                                setSelectedRemoteFiles(prev => {
                                  const next = new Set(prev);
                                  if (next.has(file.name)) next.delete(file.name);
                                  else next.add(file.name);
                                  return next;
                                });
                                setLastSelectedRemoteIndex(i);
                              } else {
                                if (selectedRemoteFiles.size === 1 && selectedRemoteFiles.has(file.name)) {
                                  setSelectedRemoteFiles(new Set());
                                } else {
                                  setSelectedRemoteFiles(new Set([file.name]));
                                }
                                setLastSelectedRemoteIndex(i);
                              }
                            }}
                            onDoubleClick={() => handleRemoteFileAction(file)}
                            onContextMenu={(e: React.MouseEvent) => showRemoteContextMenu(e, file)}
                          >
                            {file.is_dir ? (
                              <div className="file-grid-icon">
                                {iconProvider.getFolderIcon(32).icon}
                              </div>
                            ) : providerCaps.thumbnails && isImageFile(file.name) ? (
                              <div className="file-grid-icon">
                                <ProviderThumbnail
                                  path={file.path}
                                  name={file.name}
                                  size={48}
                                />
                              </div>
                            ) : isImageFile(file.name) ? (
                              <ImageThumbnail
                                path={currentRemotePath === '/' ? `/${file.name}` : `${currentRemotePath}/${file.name}`}
                                name={file.name}
                                fallbackIcon={iconProvider.getFileIcon(file.name).icon}
                                isRemote={true}
                              />
                            ) : (
                              <div className="file-grid-icon">
                                {iconProvider.getFileIcon(file.name).icon}
                              </div>
                            )}
                            {inlineRename?.path === file.path && inlineRename?.isRemote ? (
                              <input
                                ref={inlineRenameRef}
                                type="text"
                                value={inlineRenameValue}
                                onChange={(e) => setInlineRenameValue(e.target.value)}
                                onKeyDown={handleInlineRenameKeyDown}
                                onBlur={commitInlineRename}
                                onClick={(e) => e.stopPropagation()}
                                className="file-grid-name px-1 bg-white dark:bg-gray-900 border border-blue-500 rounded outline-none text-center"
                              />
                            ) : (
                              <span
                                className="file-grid-name cursor-text"
                                onClick={(e) => {
                                  if (selectedRemoteFiles.size === 1 && selectedRemoteFiles.has(file.name) && file.name !== '..') {
                                    e.stopPropagation();
                                    startInlineRename(file.path, file.name, true);
                                  }
                                }}
                              >
                                {displayName(file.name, file.is_dir)}
                              </span>
                            )}
                            {!file.is_dir && (file.size ?? 0) > 0 && (
                              <span className="file-grid-size">{formatBytes(file.size)}</span>
                            )}
                            {!file.is_dir && file.size === 0 && (
                              <span className="file-grid-size" title={t('toast.zeroByteWarning')}>&#9888; 0 B</span>
                            )}
                          </div>
                        ))}
                      </div>
                    )}
                  </div>
                </div>}


                {/* Local — full width when remote panel is hidden */}
                <LocalFilePanel
                  isAeroFileMode={!isConnected || !showRemotePanel}
                  isConnected={isConnected}
                  className={isConnected && showRemotePanel ? (swapPanels ? 'order-1' : 'order-2') : undefined}
                  currentPath={currentLocalPath}
                  setCurrentPath={setCurrentLocalPath}
                  onNavigate={changeLocalDirectory}
                  onRefresh={loadLocalFiles}
                  isPathCoherent={isLocalPathCoherent}
                  isSyncPathMismatch={isSyncPathMismatch}
                  isSyncNavigation={isSyncNavigation}
                  syncBasePaths={syncBasePaths}
                  localFiles={localFiles}
                  sortedFiles={sortedLocalFiles}
                  selectedFiles={selectedLocalFiles}
                  setSelectedFiles={setSelectedLocalFiles}
                  lastSelectedIndex={lastSelectedLocalIndex}
                  setLastSelectedIndex={setLastSelectedLocalIndex}
                  setActivePanel={setActivePanel}
                  setPreviewFile={setPreviewFile}
                  sortField={localSortField}
                  sortOrder={localSortOrder}
                  onSort={handleLocalSort}
                  searchFilter={localSearchFilter}
                  setSearchFilter={setLocalSearchFilter}
                  showSearchBar={showLocalSearchBar}
                  setShowSearchBar={setShowLocalSearchBar}
                  searchRef={localSearchRef}
                  viewMode={viewMode}
                  visibleColumns={visibleColumns}
                  showFileExtensions={showFileExtensions}
                  debugMode={debugMode}
                  doubleClickAction={doubleClickAction}
                  inlineRename={inlineRename}
                  inlineRenameValue={inlineRenameValue}
                  setInlineRenameValue={setInlineRenameValue}
                  inlineRenameRef={inlineRenameRef}
                  onInlineRenameKeyDown={handleInlineRenameKeyDown}
                  onInlineRenameCommit={commitInlineRename}
                  onInlineRenameStart={startInlineRename}
                  onInlineRenameCancel={() => setInlineRename(null)}
                  onDragStart={handleDragStart}
                  onDragEnd={handleDragEnd}
                  onDragOver={handleDragOver}
                  onDragLeave={handleDragLeave}
                  onDrop={handleDrop}
                  dropTargetPath={dropTargetPath}
                  dragSourcePaths={dragData?.sourcePaths || []}
                  crossPanelTarget={crossPanelTarget}
                  onPanelDragOver={handlePanelDragOver}
                  onPanelDrop={handlePanelDrop}
                  onPanelDragLeave={handlePanelDragLeave}
                  onContextMenu={showLocalContextMenu}
                  onEmptyContextMenu={showLocalEmptyContextMenu}
                  onOpenUniversalPreview={openUniversalPreview}
                  onOpenDevToolsPreview={openDevToolsPreview}
                  onUploadFile={uploadFile}
                  onOpenInFileManager={openInFileManager}
                  isTrashView={isTrashView}
                  trashItems={trashItems}
                  onEmptyTrash={handleEmptyTrash}
                  onRestoreTrashItem={handleRestoreTrashItem}
                  onNavigateTrash={handleNavigateTrash}
                  showSidebar={showSidebar}
                  recentPaths={recentPaths}
                  setRecentPaths={setRecentPaths}
                  iconProvider={iconProvider}
                  displayName={displayName}
                  getSyncBadge={getSyncBadge}
                  getTagsForFile={fileTags.getTagsForFile}
                  labelCounts={fileTags.labelCounts}
                  activeTagFilter={fileTags.activeTagFilter}
                  onTagFilter={fileTags.setActiveTagFilter}
                  t={t}
                  notify={notify}
                />

                {/* Preview Panel - only in local-only (AeroFile) mode */}
                {showLocalPreview && (!isConnected || !showRemotePanel) && (
                  <div
                    className="flex flex-col bg-gray-50 dark:bg-gray-800 border-l border-gray-200 dark:border-gray-700 animate-slide-in-right relative"
                    style={{ width: previewPanelWidth, minWidth: 220, maxWidth: 500, flexShrink: 0 }}
                  >
                    {/* Resize handle */}
                    <div
                      className="absolute left-0 top-0 bottom-0 w-1 cursor-col-resize hover:bg-blue-500/40 active:bg-blue-500/60 z-10"
                      onMouseDown={(e) => {
                        e.preventDefault();
                        previewResizing.current = true;
                        const startX = e.clientX;
                        const startW = previewPanelWidth;
                        const onMove = (ev: MouseEvent) => {
                          if (!previewResizing.current) return;
                          const delta = startX - ev.clientX;
                          setPreviewPanelWidth(Math.max(220, Math.min(500, startW + delta)));
                        };
                        const onUp = () => { previewResizing.current = false; window.removeEventListener('mousemove', onMove); window.removeEventListener('mouseup', onUp); };
                        window.addEventListener('mousemove', onMove);
                        window.addEventListener('mouseup', onUp);
                      }}
                    />
                    <div className="px-3 h-[43px] bg-gray-100 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600 text-sm font-medium flex items-center gap-2">
                      <Eye size={14} className="text-blue-500" /> {t('preview.fileInfo')}
                    </div>
                    <div className="flex-1 overflow-auto p-3">
                      {previewFile ? (
                        <div className="space-y-3">
                          {/* File Icon/Thumbnail */}
                          <div className="aspect-square bg-gray-100 dark:bg-gray-700 rounded-xl flex items-center justify-center overflow-hidden shadow-inner">
                            {previewImageBase64 ? (
                              <img
                                src={previewImageBase64}
                                alt={previewFile.name}
                                className="w-full h-full object-contain"
                              />
                            ) : /\.(jpg|jpeg|png|gif|svg|webp|bmp)$/i.test(previewFile.name) ? (
                              <div className="text-gray-400 animate-pulse flex flex-col items-center">
                                <Image size={32} className="text-blue-400 mb-1" />
                                <span className="text-xs">{t('common.loading')}</span>
                              </div>
                            ) : previewFile.is_dir ? (
                              iconProvider.getFolderIcon(48).icon
                            ) : (
                              iconProvider.getFileIcon(previewFile.name, 48).icon
                            )}
                          </div>

                          {/* File Name */}
                          <div className="text-center">
                            <p className="font-medium text-sm truncate" title={previewFile.name}>{previewFile.name}</p>
                            <p className="text-xs text-gray-500 dark:text-gray-400 uppercase tracking-wide">
                              {previewFile.is_dir ? t('preview.typeDirectory') : previewFile.name.split('.').pop() || t('preview.typeFile')}
                            </p>
                          </div>

                          {/* Detailed Info */}
                          <div className="bg-gray-100 dark:bg-gray-700/50 rounded-lg p-3 space-y-2 text-xs">
                            {/* Size */}
                            {!previewFile.is_dir && (
                              <div className="flex items-center justify-between">
                                <span className="text-gray-500 flex items-center gap-1.5">
                                  <HardDrive size={12} /> {t('preview.sizeLabel')}
                                </span>
                                <span className="font-medium">{formatBytes(previewFile.size || 0)}</span>
                              </div>
                            )}

                            {/* Type */}
                            <div className="flex items-center justify-between">
                              <span className="text-gray-500 flex items-center gap-1.5">
                                <FileType size={12} /> {t('preview.typeLabel')}
                              </span>
                              <span className="font-medium">
                                {previewFile.is_dir ? t('preview.typeDirectory') : (() => {
                                  const ext = previewFile.name.split('.').pop()?.toLowerCase();
                                  if (/^(jpg|jpeg|png|gif|svg|webp|bmp)$/.test(ext || '')) return t('preview.typeImage');
                                  if (/^(mp4|webm|mov|avi|mkv)$/.test(ext || '')) return t('preview.typeVideo');
                                  if (/^(mp3|wav|ogg|flac|m4a|aac)$/.test(ext || '')) return t('preview.typeAudio');
                                  if (ext === 'pdf') return t('preview.typePdf');
                                  if (/^(js|jsx|ts|tsx|py|rs|go|java|php|rb|c|cpp|h|css|scss|html|xml|json|yaml|yml|toml|sql|sh|bash)$/.test(ext || '')) return t('preview.typeCode');
                                  if (/^(zip|tar|gz|rar|7z)$/.test(ext || '')) return t('preview.typeArchive');
                                  if (/^(txt|md|log|csv)$/.test(ext || '')) return t('preview.typeText');
                                  return ext?.toUpperCase() || t('preview.typeFile');
                                })()}
                              </span>
                            </div>

                            {/* Image Resolution */}
                            {previewImageDimensions && (
                              <div className="flex items-center justify-between">
                                <span className="text-gray-500 flex items-center gap-1.5">
                                  <Image size={12} /> {t('preview.resolutionLabel')}
                                </span>
                                <span className="font-medium">{previewImageDimensions.width} × {previewImageDimensions.height}</span>
                              </div>
                            )}

                            {/* Modified */}
                            <div className="flex items-center justify-between">
                              <span className="text-gray-500 flex items-center gap-1.5">
                                <Clock size={12} /> {t('preview.modifiedLabel')}
                              </span>
                              <span className="font-medium text-right">{previewFile.modified || '—'}</span>
                            </div>

                            {/* Extension */}
                            {!previewFile.is_dir && (
                              <div className="flex items-center justify-between">
                                <span className="text-gray-500 flex items-center gap-1.5">
                                  <Database size={12} /> {t('preview.extensionLabel')}
                                </span>
                                <span className="font-mono text-xs px-1.5 py-0.5 bg-gray-200 dark:bg-gray-600 rounded">
                                  .{previewFile.name.split('.').pop()?.toLowerCase() || '—'}
                                </span>
                              </div>
                            )}

                            {/* Path */}
                            <div className="flex items-start justify-between gap-2">
                              <span className="text-gray-500 flex items-center gap-1.5 shrink-0">
                                <FolderOpen size={12} /> {t('preview.pathLabel')}
                              </span>
                              <span className="font-medium text-right truncate" title={previewFile.path}>{previewFile.path}</span>
                            </div>
                          </div>

                          {/* Quick Actions */}
                          <div className="space-y-2">
                            {/* Open Preview button */}
                            {isMediaPreviewable(previewFile.name) && (
                              <button
                                onClick={() => openUniversalPreview(previewFile, false)}
                                className="w-full px-3 py-2 bg-blue-500 hover:bg-blue-600 text-white text-xs rounded-lg flex items-center justify-center gap-2 transition-colors"
                              >
                                <Eye size={14} /> {t('preview.openPreview')}
                              </button>
                            )}

                            {/* View Source - for text/code files */}
                            {/\.(js|jsx|ts|tsx|py|rs|go|java|php|rb|c|cpp|h|css|scss|html|xml|json|yaml|yml|toml|sql|sh|bash|txt|md|log)$/i.test(previewFile.name) && (
                              <button
                                onClick={() => openUniversalPreview(previewFile, false)}
                                className="w-full px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 text-xs rounded-lg flex items-center justify-center gap-2 transition-colors"
                              >
                                <Code size={14} /> {t('preview.viewSource')}
                              </button>
                            )}

                            {/* Copy Path */}
                            <button
                              onClick={() => {
                                navigator.clipboard.writeText(previewFile.path);
                                notify.success(t('toast.clipboardCopied'), t('toast.pathCopied'));
                              }}
                              className="w-full px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 text-xs rounded-lg flex items-center justify-center gap-2 transition-colors"
                            >
                              <Copy size={14} /> {t('preview.copyPath')}
                            </button>
                          </div>
                        </div>
                      ) : (
                        <div className="h-full flex flex-col items-center justify-center text-gray-400 text-sm">
                          <Eye size={32} className="mb-3 opacity-30" />
                          <p className="font-medium">{t('preview.noFileSelected')}</p>
                          <p className="text-xs mt-1 text-center">{t('preview.clickToViewDetails')}</p>
                        </div>
                      )}
                    </div>
                  </div>
                )}
              </div>
            </div>
          )}
        </main>

        {/* Activity Log Panel - FileZilla-style horizontal panel */}
        <ActivityLogPanel
          isVisible={showActivityLog}
          onToggle={() => setShowActivityLog(!showActivityLog)}
          initialHeight={150}
          minHeight={80}
          maxHeight={400}
          theme={getLogTheme(theme, isDark)}
        />

        {/* DevTools V2 - 3-Column Responsive Layout (at bottom, below ActivityLog) */}
        <DevToolsV2
          isOpen={devToolsOpen}
          previewFile={devToolsPreviewFile}
          localPath={currentLocalPath}
          remotePath={currentRemotePath}
          onMaximizeChange={setDevToolsMaximized}
          onClose={() => setDevToolsOpen(false)}
          onClearFile={() => setDevToolsPreviewFile(null)}
          editorTheme={getMonacoTheme(theme, isDark)}
          appTheme={getEffectiveTheme(theme, isDark)}
          providerType={connectionParams.protocol}
          isConnected={isConnected}
          selectedFiles={Array.from(selectedRemoteFiles)}
          serverHost={connectionParams.server}
          serverPort={connectionParams.port}
          serverUser={connectionParams.username}
          activeFilePanel={activePanel}
          isCloudConnection={isCloudActive}
          sshConnection={isConnected && connectionParams.protocol === 'sftp' ? {
            host: connectionParams.server.split(':')[0],
            port: connectionParams.port || 22,
            username: connectionParams.username,
            password: connectionParams.password || undefined,
            privateKeyPath: connectionParams.options?.private_key_path,
            keyPassphrase: connectionParams.options?.key_passphrase,
          } : null}
          onCheckHostKey={checkSftpHostKey}
          onFileMutation={(target) => {
            setTimeout(() => {
              if (target === 'remote' || target === 'both') {
                if (isConnected) loadRemoteFiles(undefined, true);
              }
              if (target === 'local' || target === 'both') {
                loadLocalFiles(currentLocalPath);
              }
            }, 300);
          }}
          onSaveFile={async (content, file) => {
            const logId = humanLog.logStart('UPLOAD', { filename: file.name, size: formatBytes(content.length) });
            try {
              if (file.isRemote) {
                await invoke('save_remote_file', { path: file.path, content });
                humanLog.logSuccess('UPLOAD', { filename: file.name, size: formatBytes(content.length) }, logId);
                notify.success(t('toast.fileSaved'), t('toast.fileSavedRemote', { name: file.name }));
                await loadRemoteFiles(undefined, true);
              } else {
                await invoke('save_local_file', { path: file.path, content });
                humanLog.logRaw('activity.upload_success', 'INFO', { filename: file.name, size: formatBytes(content.length), location: 'Local', time: '' }, 'success');
                notify.success(t('toast.fileSaved'), t('toast.fileSavedLocal', { name: file.name }));
                await loadLocalFiles(currentLocalPath);
              }
            } catch (error) {
              humanLog.logError('UPLOAD', { filename: file.name }, logId);
              notify.error(t('toast.saveFailed'), String(error));
            }
          }}
        />

        {showStatusBar && (
          <StatusBar
            isConnected={isConnected}
            gitHubStatus={isConnected && (getActiveProviderProtocol() === 'github' || getActiveProviderProtocol() === 'gitlab') && gitHubRepoInfo && gitHubRepoInfo.writeModeKind !== 'unknown' ? (
              <GitHubWriteModeIndicator
                writeMode={gitHubRepoInfo.writeModeKind}
                workingBranch={gitHubRepoInfo.workingBranch || undefined}
                isPrivate={gitHubRepoInfo.repoPrivate}
                onError={(title, msg) => notify.error(title, msg)}
                protocol={getActiveProviderProtocol()}
              />
            ) : undefined}
            connectionSecurity={isConnected ? (() => {
              const activeSession = sessions.find(s => s.id === activeSessionId);
              const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
              const opts = connectionParams.options ?? activeSession?.connectionParams?.options;
              const verifyCert = opts?.verifyCert ?? true;
              if (protocol === 'ftp') {
                const tlsMode = opts?.tlsMode ?? 'explicit';
                if (tlsMode === 'none') return 'insecure' as const;
                if (tlsMode === 'explicit_if_available') return 'warning' as const;
                // explicit or implicit — TLS is enforced
                return verifyCert ? 'secure' as const : 'warning' as const;
              }
              if (protocol === 'ftps') {
                return verifyCert ? 'secure' as const : 'warning' as const;
              }
              // A3-03: WebDAV over HTTP is insecure — credentials sent in plaintext
              if (protocol === 'webdav') {
                const server = connectionParams.server || activeSession?.connectionParams?.server || '';
                if (server.startsWith('http://')) return 'insecure' as const;
              }
              return 'secure' as const;
            })() : undefined}
            secureProtocol={isConnected ? (() => {
              const activeSession = sessions.find(s => s.id === activeSessionId);
              const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
              if (protocol === 'ftp') {
                const tlsMode = connectionParams.options?.tlsMode ?? activeSession?.connectionParams?.options?.tlsMode ?? 'explicit';
                if (tlsMode === 'none') return undefined;
                return 'TLS';
              }
              if (protocol === 'ftps') return 'TLS';
              if (protocol === 'sftp') return 'SSH';
              if (protocol === 'filen' || protocol === 'mega') return 'E2EE';
              return 'HTTPS';
            })() : undefined}
            serverInfo={isConnected ? (() => {
              // Get protocol from active session as fallback
              const activeSession = sessions.find(s => s.id === activeSessionId);
              const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
              if (protocol === 'googledrive') return 'Google Drive';
              if (protocol === 'dropbox') return 'Dropbox';
              if (protocol === 'onedrive') return 'OneDrive';
              if (protocol === 'box') return 'Box';
              if (protocol === 'pcloud') return 'pCloud';
              if (protocol === 'azure') return 'Azure Blob';
              if (protocol === 'filen') return 'Filen';
              if (protocol === 'mega') return 'MEGA';
              if (protocol === 's3') {
                // Show bucket name or short hostname instead of full URL
                const s3Server = connectionParams.server || activeSession?.connectionParams?.server || '';
                try {
                  const url = new URL(s3Server.startsWith('http') ? s3Server : `https://${s3Server}`);
                  const host = url.hostname;
                  // Shorten Cloudflare R2 and AWS S3 URLs
                  if (host.includes('.r2.cloudflarestorage.com')) return `R2: ${host.split('.')[0].slice(0, 8)}...`;
                  if (host.includes('.amazonaws.com')) return host.replace('.s3.amazonaws.com', '').replace('.s3.', ' (S3 ');
                  return `S3: ${host.length > 30 ? host.slice(0, 27) + '...' : host}`;
                } catch { return 'S3'; }
              }
              const server = connectionParams.server || activeSession?.connectionParams?.server;
              const username = connectionParams.username || activeSession?.connectionParams?.username;
              // For WebDAV-based protocols, extract hostname from full URL
              if (protocol === 'webdav' && server) {
                try {
                  const url = new URL(server.startsWith('http') ? server : `https://${server}`);
                  const host = url.hostname;
                  const portStr = url.port && !['80', '443'].includes(url.port) ? `:${url.port}` : '';
                  return `${username}@${host}${portStr}`;
                } catch { /* fall through */ }
              }
              return server ? `${username}@${server}` : activeSession?.serverName;
            })() : undefined}
            remotePath={currentRemotePath}
            localPath={currentLocalPath}
            remoteFileCount={remoteFiles.length}
            localFileCount={localFiles.length}
            activePanel={activePanel}
            swapPanels={swapPanels}
            devToolsOpen={devToolsOpen}
            aeroFileActive={!showConnectionScreen && (!isConnected || !showRemotePanel)}
            onToggleAeroFile={handleToggleAeroFile}
            onToggleDevTools={() => setDevToolsOpen(!devToolsOpen)}
            aeroAgentOpen={devToolsOpen}
            onToggleAeroAgent={() => {
              if (!devToolsOpen) {
                setDevToolsOpen(true);
                // Ensure agent panel is visible after DevTools opens
                setTimeout(() => window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'agent' })), 50);
              } else {
                // DevTools already open — toggle agent panel
                window.dispatchEvent(new CustomEvent('devtools-panel-toggle', { detail: 'agent' }));
              }
            }}
            onToggleSync={() => setShowSyncPanel(true)}
            onToggleCloud={() => setShowCloudPanel(true)}
            cloudEnabled={isCloudActive}
            cloudSyncing={cloudSyncing}
            transferQueueActive={transferQueue.hasActiveTransfers}
            transferQueueCount={transferQueue.items.length}
            onToggleTransferQueue={transferQueue.toggle}
            transferToastActive={hasActiveTransfer || transferQueue.hasActiveTransfers}
            onReopenTransferToast={reopenTransferToast}
            showActivityLog={showActivityLog}
            activityLogCount={activityLog.entries.length}
            onToggleActivityLog={() => setShowActivityLog(!showActivityLog)}
            updateAvailable={updateAvailable}
            onShowUpdateToast={() => setUpdateToastDismissed(false)}
            debugMode={debugMode}
            onToggleDebug={() => { setShowDebugPanel(!showDebugPanel); }}
            storageQuota={storageQuota}
          />
        )}

        {/* Debug Panel */}
        {debugMode && showDebugPanel && (
          <DebugPanel
            isVisible={true}
            onClose={() => setShowDebugPanel(false)}
            isConnected={isConnected}
            connectionParams={{
              server: connectionParams?.server || '',
              username: connectionParams?.username || '',
              protocol: connectionParams?.protocol || 'sftp',
            }}
            currentRemotePath={currentRemotePath}
            appTheme={getEffectiveTheme(theme, isDark)}
          />
        )}

        {/* Dependencies Panel */}
        <DependenciesPanel
          isVisible={showDependenciesPanel}
          onClose={() => setShowDependenciesPanel(false)}
        />
      </div>
    </>
  );
};

export default App;
