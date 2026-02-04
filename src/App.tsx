import * as React from 'react';
import { useState, useEffect, useCallback, useMemo, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { open } from '@tauri-apps/plugin-dialog';
import { homeDir, downloadDir } from '@tauri-apps/api/path';
import {
  FileListResponse, ConnectionParams, DownloadParams, UploadParams,
  LocalFile, TransferEvent, TransferProgress, RemoteFile, FtpSession,
  ProviderType, isOAuthProvider, isNonFtpProvider, isFtpProtocol, supportsStorageQuota, supportsNativeShareLink
} from './types';

interface DownloadFolderParams {
  remote_path: string;
  local_path: string;
}

interface UploadFolderParams {
  local_path: string;
  remote_path: string;
}
import { SessionTabs } from './components/SessionTabs';
import { PermissionsDialog } from './components/PermissionsDialog';
import { ToastContainer, useToast } from './components/Toast';
import { Logo } from './components/Logo';
import { ContextMenu, useContextMenu, ContextMenuItem } from './components/ContextMenu';
import { SavedServers } from './components/SavedServers';
import { ConnectionScreen } from './components/ConnectionScreen';
import { AboutDialog } from './components/AboutDialog';
import { SupportDialog } from './components/SupportDialog';
import { ShortcutsDialog } from './components/ShortcutsDialog';
import { SettingsPanel } from './components/SettingsPanel';
import { StatusBar } from './components/StatusBar';
import { TransferQueue, useTransferQueue } from './components/TransferQueue';
import { CustomTitlebar } from './components/CustomTitlebar';
import { DevToolsV2, PreviewFile, isPreviewable } from './components/DevTools';
import { UniversalPreview, PreviewFileData, getPreviewCategory, isPreviewable as isMediaPreviewable } from './components/Preview';
import { SyncPanel } from './components/SyncPanel';
import { VaultPanel } from './components/VaultPanel';
import { CryptomatorBrowser } from './components/CryptomatorBrowser';
import { ArchiveBrowser } from './components/ArchiveBrowser';
import { CompressDialog, CompressOptions } from './components/CompressDialog';
import { CloudPanel } from './components/CloudPanel';
import { OverwriteDialog } from './components/OverwriteDialog';
import { BatchRenameDialog, BatchRenameFile } from './components/BatchRenameDialog';
import { FileVersionsDialog } from './components/FileVersionsDialog';
import { SharePermissionsDialog } from './components/SharePermissionsDialog';
import { ProviderThumbnail } from './components/ProviderThumbnail';
import {
  FolderUp, RefreshCw, FolderPlus, FolderOpen,
  Download, Upload, Pencil, Trash2, X,
  Folder, FileText, Globe, HardDrive, Settings, Search, Eye, Link2, Unlink, PanelTop, Shield, Cloud,
  Archive, Image, Video, Music, FileType, Code, Database, Clock,
  Copy, Clipboard, ExternalLink, List, LayoutGrid, CheckCircle2, AlertTriangle, Share2, Info, Heart,
  Lock, Unlock, Server, XCircle, History, Users, FolderSync, Replace, LogOut
} from 'lucide-react';

// Utilities
import { formatBytes, formatSpeed, formatETA, formatDate, getFileIcon, getFileIconColor } from './utils';
import { useTranslation } from './i18n';

// Components
import { ConfirmDialog, InputDialog, SyncNavDialog, PropertiesDialog, FileProperties } from './components/Dialogs';
import { TransferProgressBar } from './components/Transfer';
import { ImageThumbnail } from './components/ImageThumbnail';
import { SortableHeader, SortField, SortOrder } from './components/SortableHeader';
import ActivityLogPanel from './components/ActivityLogPanel';
import DebugPanel from './components/DebugPanel';
import DependenciesPanel from './components/DependenciesPanel';
import { GoogleDriveLogo, DropboxLogo, OneDriveLogo, MegaLogo, BoxLogo, PCloudLogo, FilenLogo } from './components/ProviderLogos';

// Hooks (modularized from App.tsx - see architecture comment below)
import { useTheme, ThemeToggle, Theme, getLogTheme, getMonacoTheme } from './hooks/useTheme';
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
    showStatusBar, defaultLocalPath, fontSize, doubleClickAction, rememberLastFolder,
    systemMenuVisible, showMenuBar, showActivityLog, showConnectionScreen,
    showSettingsPanel, setShowSettingsPanel, setShowConnectionScreen,
    setShowMenuBar, setSystemMenuVisible, setShowActivityLog,
    setShowHiddenFiles, debugMode, setDebugMode,
    SETTINGS_KEY,
  } = settings;

  const [isConnected, setIsConnected] = useState(false);
  const [showRemotePanel, setShowRemotePanel] = useState(true);
  const [previewPanelWidth, setPreviewPanelWidth] = useState(280);
  const previewResizing = useRef(false);
  const [storageQuota, setStorageQuota] = useState<{ used: number; total: number; free: number } | null>(null);
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
  const [activeTransfer, setActiveTransfer] = useState<TransferProgress | null>(null);
  const [isReconnecting, setIsReconnecting] = useState(false);  // FTP reconnection in progress
  const hasActivity = activeTransfer !== null;  // Track if upload/download in progress (will be updated below)
  const [activePanel, setActivePanel] = useState<'remote' | 'local'>('remote');
  const [remoteSortField, setRemoteSortField] = useState<SortField>('name');
  const [remoteSortOrder, setRemoteSortOrder] = useState<SortOrder>('asc');
  const [localSortField, setLocalSortField] = useState<SortField>('name');
  const [localSortOrder, setLocalSortOrder] = useState<SortOrder>('asc');
  const [selectedLocalFiles, setSelectedLocalFiles] = useState<Set<string>>(new Set());
  const [selectedRemoteFiles, setSelectedRemoteFiles] = useState<Set<string>>(new Set());
  const [lastSelectedRemoteIndex, setLastSelectedRemoteIndex] = useState<number | null>(null);
  const [lastSelectedLocalIndex, setLastSelectedLocalIndex] = useState<number | null>(null);
  const [permissionsDialog, setPermissionsDialog] = useState<{ file: RemoteFile, visible: boolean } | null>(null);

  // Dialogs
  const [confirmDialog, setConfirmDialog] = useState<{ message: string; onConfirm: () => void } | null>(null);
  const [inputDialog, setInputDialog] = useState<{ title: string; defaultValue: string; onConfirm: (v: string) => void; isPassword?: boolean; placeholder?: string } | null>(null);
  const [batchRenameDialog, setBatchRenameDialog] = useState<{ files: BatchRenameFile[]; isRemote: boolean } | null>(null);
  // Inline rename state: tracks which file is being renamed directly in the list
  const [inlineRename, setInlineRename] = useState<{ path: string; name: string; isRemote: boolean } | null>(null);
  const [inlineRenameValue, setInlineRenameValue] = useState('');
  const inlineRenameRef = useRef<HTMLInputElement>(null);
  const inlineRenameClickTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [propertiesDialog, setPropertiesDialog] = useState<FileProperties | null>(null);
  const [showAboutDialog, setShowAboutDialog] = useState(false);
  const [showSupportDialog, setShowSupportDialog] = useState(false);
  const [showShortcutsDialog, setShowShortcutsDialog] = useState(false);
  // Overwrite dialog: handled by useOverwriteCheck hook
  const { overwriteDialog, setOverwriteDialog, checkOverwrite, resetOverwriteSettings } = useOverwriteCheck({ localFiles, remoteFiles });
  // showSettingsPanel provided by useSettings
  const [showDebugPanel, setShowDebugPanel] = useState(false);
  const [showDependenciesPanel, setShowDependenciesPanel] = useState(false);
  const [showSyncPanel, setShowSyncPanel] = useState(false);
  const [showVaultPanel, setShowVaultPanel] = useState(false);
  const [showCryptomatorBrowser, setShowCryptomatorBrowser] = useState(false);
  const [archiveBrowserState, setArchiveBrowserState] = useState<{ path: string; type: import('./types').ArchiveType; encrypted: boolean } | null>(null);
  const [compressDialogState, setCompressDialogState] = useState<{ files: { name: string; path: string; size: number; isDir: boolean }[]; defaultName: string; outputDir: string } | null>(null);
  // AeroCloud state + event listeners managed by useCloudSync hook (initialized below after core hooks)
  const [isSyncNavigation, setIsSyncNavigation] = useState(false); // Navigation Sync feature
  const [syncBasePaths, setSyncBasePaths] = useState<{ remote: string; local: string } | null>(null);
  const [syncNavDialog, setSyncNavDialog] = useState<{ missingPath: string; isRemote: boolean; targetPath: string } | null>(null);
  // Multi-Session Tabs (Hybrid Cache Architecture)
  const [sessions, setSessions] = useState<FtpSession[]>([]);
  const [activeSessionId, setActiveSessionId] = useState<string | null>(null);

  // Transfer Queue (unified upload + download)
  const transferQueue = useTransferQueue();

  const localSearchRef = React.useRef<HTMLInputElement>(null);

  // Track if any transfer is active (for Logo animation)
  const hasQueueActivity = transferQueue.hasActiveTransfers;

  const [localSearchFilter, setLocalSearchFilter] = useState('');
  const [showLocalSearchBar, setShowLocalSearchBar] = useState(false);

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
    const pathParts = currentLocalPath.split('/');
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


  // === Core hooks (must be before keyboard shortcuts) ===
  const { theme, setTheme, isDark } = useTheme();
  const toast = useToast();
  const contextMenu = useContextMenu();
  const humanLog = useHumanizedLog();
  const activityLog = useActivityLog();

  // Auto-Update: handled by useAutoUpdate hook
  const { updateAvailable, setUpdateAvailable, checkForUpdate } = useAutoUpdate({ activityLog });
  const [updateToastDismissed, setUpdateToastDismissed] = useState(false);
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
  } | null>(null);

  // AeroCloud state + event listeners (extracted hook)
  const {
    showCloudPanel, setShowCloudPanel,
    cloudSyncing, isCloudActive, setIsCloudActive,
    cloudServerName, setCloudServerName,
    cloudLastSync, setCloudLastSync,
    cloudLocalFolder, setCloudLocalFolder,
    cloudRemoteFolder, setCloudRemoteFolder,
    cloudPublicUrlBase, setCloudPublicUrlBase,
  } = useCloudSync({ activityLog, humanLog, t, checkForUpdate });

  // showToastNotifications provided by useSettings

  // Wrapper: Notify user with ActivityLog always, toast only if enabled
  // Returns toast ID for compatibility with removeToast
  const notify = React.useMemo(() => ({
    success: (title: string, message?: string): string | null => {
      return showToastNotifications ? toast.success(title, message) : null;
    },
    error: (title: string, message?: string): string => {
      return toast.error(title, message);
    },
    info: (title: string, message?: string): string | null => {
      activityLog.log('INFO', message ? `${title}: ${message}` : title, 'success');
      return showToastNotifications ? toast.info(title, message) : null;
    },
    warning: (title: string, message?: string): string | null => {
      activityLog.log('INFO', message ? `⚠️ ${title}: ${message}` : `⚠️ ${title}`, 'running');
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

  // Filtered files (search filter applied)
  const filteredLocalFiles = localFiles.filter(f =>
    f.name.toLowerCase().includes(localSearchFilter.toLowerCase())
  );

  // Keyboard Shortcuts
  useKeyboardShortcuts({
    'F1': () => setShowShortcutsDialog(v => !v),
    'F10': () => setShowMenuBar(v => !v),
    'Ctrl+,': () => setShowSettingsPanel(true),

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
        if (currentLocalPath !== '/') changeLocalDirectory(currentLocalPath.split('/').slice(0, -1).join('/') || '/');
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

    // Space key: Open preview for selected file
    'Space': () => {
      const selectedRemoteName = Array.from(selectedRemoteFiles)[0];
      const selectedLocalName = Array.from(selectedLocalFiles)[0];

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
      } else if (selectedLocalName) {
        const file = localFiles.find(f => f.name === selectedLocalName);
        if (file && !file.is_dir) {
          const category = getPreviewCategory(file.name);
          if (['image', 'audio', 'video', 'pdf', 'markdown', 'text'].includes(category)) {
            openUniversalPreview(file, false);
          } else if (isPreviewable(file.name)) {
            openDevToolsPreview(file, false);
          }
        }
      }
    },

    'Escape': () => {
      if (universalPreviewOpen) closeUniversalPreview();
      else if (showShortcutsDialog) setShowShortcutsDialog(false);
      else if (showAboutDialog) setShowAboutDialog(false);
      else if (showSettingsPanel) setShowSettingsPanel(false);
      else if (inputDialog) setInputDialog(null);
      else if (confirmDialog) setConfirmDialog(null);
    }
  }, [showShortcutsDialog, showAboutDialog, showSettingsPanel, inputDialog, confirmDialog,
    universalPreviewOpen, selectedRemoteFiles, selectedLocalFiles, remoteFiles, localFiles,
    activePanel, currentRemotePath, currentLocalPath, isConnected]);


  // Fetch storage quota for a given protocol (call after successful connection/reconnection)
  const fetchStorageQuota = async (protocol?: string) => {
    if (protocol && supportsStorageQuota(protocol as ProviderType)) {
      try {
        const info = await invoke<{ used: number; total: number; free: number }>('provider_storage_info');
        setStorageQuota(info);
      } catch (e) {
        console.warn('[StorageQuota] Failed to fetch:', e);
        setStorageQuota(null);
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
    const isProvider = protocol && isNonFtpProvider(protocol);

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

  // Sorting
  const sortFiles = <T extends { name: string; size: number | null; modified: string | null; is_dir: boolean }>(files: T[], field: SortField, order: SortOrder): T[] => {
    return [...files].sort((a, b) => {
      if (a.is_dir && !b.is_dir) return -1;
      if (!a.is_dir && b.is_dir) return 1;
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
  };

  const sortedRemoteFiles = useMemo(() => {
    const source = remoteSearchResults !== null ? remoteSearchResults : remoteFiles;
    return sortFiles(source, remoteSortField, remoteSortOrder);
  }, [remoteFiles, remoteSearchResults, remoteSortField, remoteSortOrder]);
  const sortedLocalFiles = useMemo(() => sortFiles(filteredLocalFiles, localSortField, localSortOrder), [filteredLocalFiles, localSortField, localSortOrder]);

  const handleRemoteSort = (field: SortField) => {
    if (remoteSortField === field) setRemoteSortOrder(remoteSortOrder === 'asc' ? 'desc' : 'asc');
    else { setRemoteSortField(field); setRemoteSortOrder('asc'); }
  };

  const handleLocalSort = (field: SortField) => {
    if (localSortField === field) setLocalSortOrder(localSortOrder === 'asc' ? 'desc' : 'asc');
    else { setLocalSortField(field); setLocalSortOrder('asc'); }
  };

  // Timeout to auto-hide transfer popup if stuck (30 seconds of no updates)
  const lastProgressUpdate = React.useRef<number>(Date.now());
  useEffect(() => {
    if (!activeTransfer) return;

    lastProgressUpdate.current = Date.now();

    const checkStuck = setInterval(() => {
      if (Date.now() - lastProgressUpdate.current > 30000) {
        console.warn('Transfer popup stuck, auto-closing');
        setActiveTransfer(null);
      }
    }, 5000);

    return () => clearInterval(checkStuck);
  }, [activeTransfer?.percentage]);

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
    return () => { unlisten.then(fn => fn()); };
  }, []);

  // Start update download
  const startUpdateDownload = useCallback(async () => {
    if (!updateAvailable?.download_url) return;
    setUpdateDownload({
      downloading: true, percentage: 0, speed_bps: 0, eta_seconds: 0,
      filename: '', downloaded: 0, total: 0,
    });
    try {
      const path = await invoke<string>('download_update', { url: updateAvailable.download_url });
      setUpdateDownload(prev => prev ? { ...prev, downloading: false, completedPath: path } : null);
      activityLog.log('INFO', `Update downloaded: ${path}`, 'success');
    } catch (error) {
      setUpdateDownload(prev => prev ? { ...prev, downloading: false, error: String(error) } : null);
      activityLog.log('ERROR', `Update download failed: ${error}`, 'error');
    }
  }, [updateAvailable, activityLog]);

  // Menu events from native menu
  useEffect(() => {
    const unlisten = listen<string>('menu-event', (event) => {
      const id = event.payload;
      switch (id) {
        case 'about': setShowAboutDialog(true); break;
        case 'support': setShowSupportDialog(true); break;
        case 'shortcuts': setShowShortcutsDialog(true); break;
        case 'settings': setShowSettingsPanel(true); break;
        case 'refresh':
          if (isConnected) loadRemoteFiles();
          loadLocalFiles(currentLocalPath);
          break;
        case 'toggle_theme':
          setTheme(theme === 'light' ? 'dark' : theme === 'dark' ? 'auto' : 'light');
          break;
        case 'new_folder':
          if (isConnected) createFolder(true);
          break;
        case 'toggle_devtools':
          setDevToolsOpen(prev => !prev);
          break;
        case 'toggle_debug_mode':
          setDebugMode(!debugMode);
          setShowDebugPanel(!debugMode);
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
        case 'check_update':
          checkForUpdate(true);
          break;
        case 'quit':
          // Will be handled by Tauri
          break;
      }
    });
    return () => { unlisten.then(fn => fn()); };
  }, [isConnected, currentLocalPath, theme, debugMode]);

  // File loading
  const loadLocalFiles = useCallback(async (path: string) => {
    try {
      const files: LocalFile[] = await invoke('get_local_files', { path, showHidden: showHiddenFiles });
      setLocalFiles(files);
      setCurrentLocalPath(path);
      setSelectedLocalFiles(new Set());
    } catch (error) {
      notify.error('Error', `Failed to list local files: ${error}`);
    }
  }, [showHiddenFiles]);

  const loadRemoteFiles = async (overrideProtocol?: string) => {
    try {
      // Check if we're connected to a Provider (OAuth, S3, WebDAV)
      // Use override protocol if provided, then connectionParams, then active session (most robust)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = (overrideProtocol || connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
      const isProvider = !!protocol && isNonFtpProvider(protocol);
      console.log('[loadRemoteFiles] protocol:', protocol, 'isProvider:', isProvider, 'override:', overrideProtocol);

      let response: FileListResponse;
      if (isProvider) {
        // Use provider API
        console.log('[loadRemoteFiles] Calling provider_list_files...');
        response = await invoke('provider_list_files', { path: null });
        console.log('[loadRemoteFiles] Provider response:', {
          fileCount: response.files?.length ?? 0,
          currentPath: response.current_path,
          files: response.files?.slice(0, 5) // Log first 5 files
        });

        // Log to activity if we got files
        if (response.files?.length > 0) {
          humanLog.logRaw('activity.loaded_items', 'INFO', { count: response.files.length, provider: protocol }, 'success');
        } else {
          activityLog.log('INFO', `No files returned from ${protocol} provider`, 'running');
        }
      } else {
        // Use FTP API
        response = await invoke('list_files');
      }
      setRemoteFiles(response.files);
      setCurrentRemotePath(response.current_path);
      setSelectedRemoteFiles(new Set());
    } catch (error) {
      console.error('[loadRemoteFiles] Error:', error);
      activityLog.log('ERROR', `Failed to list files: ${error}`, 'error');
      notify.error('Error', `Failed to list files: ${error}`);
    }
  };

  // Transfer events (backend progress updates) - handles downloads, uploads, and deletes
  const { pendingFileLogIds, pendingDeleteLogIds } = useTransferEvents({
    t, activityLog, humanLog, transferQueue, notify,
    setActiveTransfer, loadRemoteFiles, loadLocalFiles, currentLocalPath,
  });

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
    (async () => {
      // Check for saved settings with defaultLocalPath or last visited folder
      try {
        const savedSettings = localStorage.getItem(SETTINGS_KEY);
        if (savedSettings) {
          const parsed = JSON.parse(savedSettings);

          // If rememberLastFolder is enabled, try to load last visited folder
          if (parsed.rememberLastFolder && parsed.lastLocalPath) {
            try {
              await loadLocalFiles(parsed.lastLocalPath);
              return;
            } catch { /* Fall through to next option */ }
          }

          // Try defaultLocalPath from settings
          if (parsed.defaultLocalPath) {
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
    })();
  }, [loadLocalFiles]);

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

  // FTP operations
  const connectToFtp = async () => {
    // OAuth providers don't need server/username validation - they're already connected
    const protocol = connectionParams.protocol;
    console.log('[connectToFtp] connectionParams:', connectionParams);
    console.log('[connectToFtp] protocol:', protocol);
    const isOAuth = !!protocol && isOAuthProvider(protocol);
    const isProvider = protocol && ['s3', 'webdav', 'mega', 'sftp', 'filen'].includes(protocol);
    console.log('[connectToFtp] isOAuth:', isOAuth, 'isProvider:', isProvider);

    if (isOAuth) {
      // OAuth provider is already connected via OAuthConnect component
      // Just switch to file manager view
      setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
      setLoading(false);
      setShowConnectionScreen(false);
      const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud' };
      const providerName = providerNames[protocol] || protocol;
      notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));
      // Load remote files for OAuth provider - pass protocol explicitly
      await loadRemoteFiles(protocol);
      // Navigate to initial local directory if specified
      if (quickConnectDirs.localDir) {
        await changeLocalDirectory(quickConnectDirs.localDir);
      }
      // Create session with provider name
      createSession(
        providerName,
        connectionParams,
        '/',
        quickConnectDirs.localDir || currentLocalPath
      );
      fetchStorageQuota(protocol);
      return;
    }

    // S3, WebDAV and MEGA use provider_connect
    if (isProvider) {
      if ((!connectionParams.server && protocol !== 'mega') || !connectionParams.username) {
        notify.error(t('toast.missingFields'), t('toast.fillEndpointCreds'));
        return;
      }
      if (protocol === 's3' && !connectionParams.options?.bucket) {
        notify.error(t('toast.missingFields'), t('toast.fillBucket'));
        return;
      }

      setLoading(true);
      setIsSyncNavigation(false);
      setSyncBasePaths(null);
      // Use displayName if available, otherwise use server/bucket/username
      // No protocol prefix in tab name - icon distinguishes the protocol
      const providerName = connectionParams.displayName || (protocol === 's3'
        ? connectionParams.options?.bucket || 'S3'
        : protocol === 'mega'
          ? connectionParams.username
          : connectionParams.server.split(':')[0]);
      const protocolLabel = protocol.toUpperCase();
      const logId = humanLog.logStart('CONNECT', { server: providerName, protocol: protocolLabel });

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

        // Build provider connection params
        const providerParams = {
          protocol: protocol,
          server: connectionParams.server,
          port: connectionParams.port,
          username: connectionParams.username,
          password: connectionParams.password,
          initial_path: quickConnectDirs.remoteDir || null,
          bucket: connectionParams.options?.bucket,
          region: connectionParams.options?.region || 'us-east-1',
          endpoint: connectionParams.options?.endpoint || null,
          path_style: connectionParams.options?.pathStyle || false,
          save_session: connectionParams.options?.save_session,
          session_expires_at: connectionParams.options?.session_expires_at,
          // SFTP-specific options
          private_key_path: connectionParams.options?.private_key_path || null,
          key_passphrase: connectionParams.options?.key_passphrase || null,
          timeout: connectionParams.options?.timeout || 30,
          // FTP/FTPS-specific options
          tls_mode: connectionParams.options?.tlsMode || (protocol === 'ftps' ? 'implicit' : undefined),
          verify_cert: connectionParams.options?.verifyCert !== undefined ? connectionParams.options.verifyCert : true,
        };


        console.log('[connectToFtp] provider_connect params:', { ...providerParams, password: providerParams.password ? '***' : null, key_passphrase: providerParams.key_passphrase ? '***' : null });
        await invoke('provider_connect', { params: providerParams });

        setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
        humanLog.logSuccess('CONNECT', { server: providerName, protocol: protocolLabel }, logId);
        notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));

        // Load files using provider API
        console.log('[connectToFtp] Calling provider_list_files for:', protocol);
        const response = await invoke<{ files: any[]; current_path: string }>('provider_list_files', {
          path: quickConnectDirs.remoteDir || null
        });
        console.log('[connectToFtp] provider_list_files response:', {
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
        }));
        console.log('[connectToFtp] Converted files:', files.length);
        setRemoteFiles(files);
        setCurrentRemotePath(response.current_path);

        // Navigate to initial local directory if specified
        if (quickConnectDirs.localDir) {
          await changeLocalDirectory(quickConnectDirs.localDir);
        }

        // Create session with explicit S3 options preserved
        const sessionParams: ConnectionParams = {
          protocol: protocol,
          server: connectionParams.server,
          port: connectionParams.port,
          username: connectionParams.username,
          password: connectionParams.password,
          options: {
            bucket: connectionParams.options?.bucket,
            region: connectionParams.options?.region || 'us-east-1',
            endpoint: connectionParams.options?.endpoint,
            pathStyle: connectionParams.options?.pathStyle,
          },
        };
        createSession(
          providerName,
          sessionParams,
          response.current_path,
          quickConnectDirs.localDir || currentLocalPath
        );
        fetchStorageQuota(protocol);
      } catch (error) {
        humanLog.logError('CONNECT', { server: providerName }, logId);
        notify.error(t('connection.connectionFailed'), String(error));
      }
      finally { setLoading(false); }
      return;
    }

    // FTP/FTPS/SFTP - use legacy commands
    if (!connectionParams.server || !connectionParams.username) { notify.error(t('toast.missingFields'), t('toast.fillServerUser')); return; }
    setLoading(true);
    // Reset navigation sync for new connection
    setIsSyncNavigation(false);
    setSyncBasePaths(null);
    const protocolLabel = (connectionParams.protocol || 'FTP').toUpperCase();
    const logId = humanLog.logStart('CONNECT', { server: connectionParams.server, protocol: protocolLabel });
    try {
      // First disconnect any active OAuth provider to avoid conflicts
      try {
        await invoke('provider_disconnect');
      } catch {
        // Ignore if not connected to OAuth
      }
      await invoke('connect_ftp', { params: connectionParams });
      setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
      const protocol = (connectionParams.protocol || 'FTP').toUpperCase();
      humanLog.logSuccess('CONNECT', { server: connectionParams.server, protocol }, logId);
      notify.success(t('toast.connected'), t('toast.connectedTo', { server: connectionParams.server }));
      // Navigate to initial remote directory if specified
      if (quickConnectDirs.remoteDir) {
        // Pass protocol explicitly to avoid stale state from previous provider session
        await changeRemoteDirectory(quickConnectDirs.remoteDir, connectionParams.protocol || 'ftp');
      } else {
        await loadRemoteFiles();
      }
      // Navigate to initial local directory if specified
      if (quickConnectDirs.localDir) {
        await changeLocalDirectory(quickConnectDirs.localDir);
      }
      // Create session tab for FTP/FTPS/SFTP connections
      createSession(
        connectionParams.displayName || connectionParams.server,
        connectionParams,
        quickConnectDirs.remoteDir || currentRemotePath,
        quickConnectDirs.localDir || currentLocalPath
      );
    } catch (error) {
      humanLog.logError('CONNECT', { server: connectionParams.server }, logId);
      notify.error(t('connection.connectionFailed'), String(error));
    }
    finally { setLoading(false); }
  };

  const disconnectFromFtp = async () => {
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
      humanLog.logSuccess('DISCONNECT', {}, logId);
      notify.info(t('toast.disconnectedTitle'), t('toast.disconnectedFrom'));
    } catch (error) {
      humanLog.logError('DISCONNECT', {}, logId);
      notify.error(t('common.error'), t('toast.disconnectFailed', { error: String(error) }));
    }
  };

  // Session Management for Multi-Tab
  const createSession = (serverName: string, params: ConnectionParams, remotePath: string, localPath: string) => {
    // Deep copy params to prevent reference mutation when switching tabs
    const paramsCopy: ConnectionParams = JSON.parse(JSON.stringify(params));

    const newSession: FtpSession = {
      id: `session_${Date.now()}`,
      serverId: serverName,
      serverName,
      status: 'connected',
      remotePath,
      localPath,
      remoteFiles: [...remoteFiles],
      localFiles: [...localFiles],
      lastActivity: new Date(),
      connectionParams: paramsCopy,
      providerId: paramsCopy.providerId,
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

  const switchSession = async (sessionId: string) => {
    // Find the target session from current sessions state
    const targetSession = sessions.find(s => s.id === sessionId);
    if (!targetSession) return;

    // Don't switch if already on this session
    if (activeSessionId === sessionId) return;

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
    setStorageQuota(null); // Clear stale quota while reconnecting

    // Load cached data immediately (zero latency UX)
    setRemoteFiles(targetSession.remoteFiles);
    setLocalFiles(targetSession.localFiles);
    setCurrentRemotePath(targetSession.remotePath);
    setCurrentLocalPath(targetSession.localPath);
    setConnectionParams(targetSession.connectionParams);

    // Restore per-session navigation sync state
    setIsSyncNavigation(targetSession.isSyncNavigation ?? false);
    setSyncBasePaths(targetSession.syncBasePaths ?? null);

    // Determine if this is an OAuth provider session
    const protocol = targetSession.connectionParams?.protocol;
    const isOAuth = !!protocol && isOAuthProvider(protocol);
    // Treat 'mega' as a general provider like S3/WebDAV, not legacy FTP
    const isS3OrWebDAV = protocol && ['s3', 'webdav', 'mega', 'sftp', 'filen'].includes(protocol);

    // Reconnect to the new server and refresh data
    setSessions(prev => prev.map(s => s.id === sessionId ? { ...s, status: 'connecting' } : s));
    const protocolLabel = (protocol || 'FTP').toUpperCase();
    const reconnectLogId = humanLog.logRaw('activity.reconnect_start', 'CONNECT', { server: targetSession.serverName }, 'running');

    try {
      let response: FileListResponse;

      if (isOAuth) {
        // OAuth providers - need to reconnect because ProviderState may have a different provider
        console.log('[switchSession] OAuth provider, reconnecting...');

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

        // Get OAuth credentials from localStorage
        // Try new structured format first (aeroftp_oauth_settings)
        let clientId: string | null = null;
        let clientSecret: string | null = null;

        try {
          const oauthSettingsJson = localStorage.getItem('aeroftp_oauth_settings');
          if (oauthSettingsJson) {
            const oauthSettings = JSON.parse(oauthSettingsJson);
            const providerKey = protocol === 'googledrive' ? 'googledrive' : protocol;
            if (oauthSettings[providerKey]) {
              clientId = oauthSettings[providerKey].clientId;
              clientSecret = oauthSettings[providerKey].clientSecret;
            }
          }
        } catch (e) {
          console.warn('[switchSession] Failed to parse OAuth settings:', e);
        }

        // Fall back to legacy per-provider storage
        if (!clientId || !clientSecret) {
          clientId = localStorage.getItem(`oauth_${protocol}_client_id`);
          clientSecret = localStorage.getItem(`oauth_${protocol}_client_secret`);
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

        // Map protocol to OAuth provider name
        const oauthProvider = protocol === 'googledrive' ? 'google_drive' : protocol;

        // Reconnect to the OAuth provider
        await invoke('oauth2_connect', {
          params: {
            provider: oauthProvider,
            client_id: clientId,
            client_secret: clientSecret,
          }
        });

        // Now navigate to the session's path
        response = await invoke('provider_change_dir', { path: targetSession.remotePath || '/' });
      } else if (isS3OrWebDAV) {
        console.log('[switchSession] Provider (S3/WebDAV), reconnecting...');

        let connectParams = targetSession.connectionParams;
        // Safety check: recover missing S3 options
        if (protocol === 's3' && (!connectParams.options || !connectParams.options.bucket)) {
          try {
            const savedJson = localStorage.getItem('aeroftp-saved-servers');
            if (savedJson) {
              const savedServers = JSON.parse(savedJson);
              const found = savedServers.find((s: any) =>
                (s.name === targetSession.serverName) ||
                (s.host === connectParams.server && s.username === connectParams.username)
              );
              if (found && found.options && found.options.bucket) {
                console.log('[switchSession] Auto-recovered missing S3 options');
                connectParams = { ...connectParams, options: found.options };
              }
            }
          } catch (e) { console.error('Option recovery failed', e); }
        }

        // First disconnect any existing connections
        try { await invoke('provider_disconnect'); } catch { }
        try { await invoke('disconnect_ftp'); } catch { }

        // Build provider connection params in the format expected by provider_connect
        const providerParams = {
          protocol: protocol,
          server: connectParams.server,
          port: connectParams.port,
          username: connectParams.username,
          password: connectParams.password,
          initial_path: targetSession.remotePath || null,
          bucket: connectParams.options?.bucket,
          region: connectParams.options?.region || 'us-east-1',
          endpoint: connectParams.options?.endpoint || null,
          path_style: connectParams.options?.pathStyle || false,
          private_key_path: connectParams.options?.private_key_path || null,
          key_passphrase: connectParams.options?.key_passphrase || null,
          timeout: connectParams.options?.timeout || 30,
          tls_mode: connectParams.options?.tlsMode || (protocol === 'ftps' ? 'implicit' : undefined),
          verify_cert: connectParams.options?.verifyCert !== undefined ? connectParams.options.verifyCert : true,
        };

        console.log('[switchSession] provider_connect params:', { ...providerParams, password: providerParams.password ? '***' : null });
        await invoke('provider_connect', { params: providerParams });
        if (targetSession.remotePath && targetSession.remotePath !== '/') {
          try { await invoke('provider_change_dir', { path: targetSession.remotePath }); } catch (e) { console.warn('Restore path failed', e); }
        }
        response = await invoke('provider_list_files', { path: null });
      } else {
        // FTP/FTPS - reconnect and navigate
        console.log('[switchSession] FTP provider, reconnecting...');
        // First disconnect any active OAuth provider to avoid conflicts
        try {
          await invoke('provider_disconnect');
        } catch {
          // Ignore if not connected to OAuth
        }
        await invoke('connect_ftp', { params: targetSession.connectionParams });

        // Only navigate to saved path if it looks like a valid FTP path
        // Avoid using paths from previous WebDAV/S3 sessions (e.g., /wwwhome, /bucket-name)
        const savedPath = targetSession.remotePath;
        const isValidFtpPath = savedPath &&
          !savedPath.includes('wwwhome') &&
          !savedPath.includes('webdav') &&
          savedPath.startsWith('/');

        if (isValidFtpPath && savedPath !== '/') {
          try {
            await invoke('change_directory', { path: savedPath });
          } catch (pathError) {
            console.warn('[switchSession] Could not restore FTP path, using root:', pathError);
            // Path doesn't exist on this server, stay at root
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
      console.log('Reconnect error:', e);
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

    // Log the tab closure
    humanLog.logRaw('activity.disconnect_success', 'DISCONNECT', {}, 'success');

    // If closing active session, switch to another or disconnect
    if (sessionId === activeSessionId) {
      const remaining = sessions.filter(s => s.id !== sessionId);
      if (remaining.length > 0) {
        await switchSession(remaining[0].id);
      } else {
        await disconnectFromFtp();
      }
    }

    setSessions(prev => prev.filter(s => s.id !== sessionId));
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
  const handleCloudTabClick = async () => {
    console.log('Cloud Tab clicked');

    try {
      // Get cloud config to know which server profile and folders
      const cloudConfig = await invoke<{
        enabled: boolean;
        local_folder: string;
        remote_folder: string;
        server_profile: string;
      }>('get_cloud_config');

      console.log('Cloud config:', cloudConfig);

      if (!cloudConfig.enabled) {
        setShowCloudPanel(true);
        return;
      }

      // Get saved servers from localStorage (needed to check if same server)
      const savedServersStr = localStorage.getItem('aeroftp-saved-servers');
      if (!savedServersStr) {
        notify.error(t('toast.noSavedServers'), t('toast.saveServerFirst'));
        setShowCloudPanel(true);
        return;
      }

      const savedServers = JSON.parse(savedServersStr);
      const cloudServer = savedServers.find((s: { name: string }) => s.name === cloudConfig.server_profile);

      if (!cloudServer) {
        notify.error(t('toast.serverNotFound'), t('toast.serverProfileNotFound', { name: cloudConfig.server_profile }));
        setShowCloudPanel(true);
        return;
      }

      // Build connection params for cloud server
      const cloudServerString = cloudServer.port && cloudServer.port !== 21
        ? `${cloudServer.host}:${cloudServer.port}`
        : cloudServer.host;

      // Load password from OS keyring (localStorage never stores passwords)
      let cloudPassword = '';
      try {
        cloudPassword = await invoke<string>('get_credential', { account: `server_${cloudServer.id}` });
      } catch (e) {
        console.warn('Failed to load cloud server credential from keyring:', e);
      }

      // If already connected, check if it's the same server
      if (isConnected) {
        console.log('Already connected, checking if same server...');

        // Compare current connection with cloud server
        const currentServer = connectionParams.server;
        const isSameServer = currentServer === cloudServerString;

        console.log(`Current server: ${currentServer}, Cloud server: ${cloudServerString}, Same: ${isSameServer}`);

        // IMPORTANT: Save current session state before navigating to cloud
        // Capture current state values for the closure
        const capturedRemoteFiles = remoteFiles;
        const capturedLocalFiles = localFiles;
        const capturedRemotePath = currentRemotePath;
        const capturedLocalPath = currentLocalPath;
        const capturedSessionId = activeSessionId;
        const capturedSyncNav = isSyncNavigation;
        const capturedSyncPaths = syncBasePaths;

        if (capturedSessionId) {
          setSessions(prev => prev.map(s =>
            s.id === capturedSessionId
              ? {
                ...s,
                status: 'cached',  // Mark as cached since we're switching away
                remoteFiles: [...capturedRemoteFiles],
                localFiles: [...capturedLocalFiles],
                remotePath: capturedRemotePath,
                localPath: capturedLocalPath,
                isSyncNavigation: capturedSyncNav,
                syncBasePaths: capturedSyncPaths
              }
              : s
          ));
        }

        // Deselect current session tab since we're going to AeroCloud
        setActiveSessionId(null);

        // If different server, we need to reconnect to the cloud server
        if (!isSameServer) {
          console.log('Different server, reconnecting to cloud server...');
          const params = {
            server: cloudServerString,
            username: cloudServer.username || '',
            password: cloudPassword,
            protocol: cloudServer.protocol || 'ftp',
          };

          try {
            setLoading(true);
            await invoke('connect_ftp', { params });
            setConnectionParams(params);
            humanLog.logRaw('activity.connect_success', 'CONNECT', { server: `AeroCloud (${cloudServerName})`, protocol: params.protocol?.toUpperCase() || 'FTP' }, 'success');
          } catch (connError) {
            console.log('Failed to connect to cloud server:', connError);
            notify.error('Connection Failed', `Failed to connect to cloud server: ${connError}`);
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

        // Navigate to cloud folders
        try {
          const remoteResponse: FileListResponse = await invoke('change_directory', { path: cloudConfig.remote_folder });
          setRemoteFiles(remoteResponse.files);
          setCurrentRemotePath(remoteResponse.current_path);

          const localFilesData: LocalFile[] = await invoke('get_local_files', { path: cloudConfig.local_folder, showHidden: showHiddenFiles });
          setLocalFiles(localFilesData);
          setCurrentLocalPath(cloudConfig.local_folder);

          // Update cloud folder state for badge display
          setCloudRemoteFolder(cloudConfig.remote_folder);
          setCloudLocalFolder(cloudConfig.local_folder);

          humanLog.logRaw('activity.connect_success', 'CONNECT', { server: `AeroCloud (${cloudServerName})`, protocol: connectionParams.protocol || 'FTP' }, 'success');
        } catch (navError) {
          console.log('Navigation error:', navError);
        }

        // Trigger sync
        try {
          await invoke('trigger_cloud_sync');
        } catch (syncError) {
          console.log('Sync error:', syncError);
        }
        return;
      }

      // Not connected yet - connect to cloud server
      const params = {
        server: cloudServerString,
        username: cloudServer.username || '',
        password: cloudPassword,
        protocol: cloudServer.protocol || 'ftp',
      };

      // Connect
      setLoading(true);
      const logId = humanLog.logStart('CONNECT', { server: `AeroCloud (${cloudConfig.server_profile})` });
      notify.info(t('toast.connecting'), t('toast.connectingTo', { server: cloudConfig.server_profile }));

      await invoke('connect_ftp', { params });
      setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
      setConnectionParams(params);
      setShowConnectionScreen(false);  // Hide connection screen to show file browser

      // Navigate to cloud folders
      // Remote: navigate to cloud remote folder
      const remoteResponse: FileListResponse = await invoke('change_directory', { path: cloudConfig.remote_folder });
      setRemoteFiles(remoteResponse.files);
      setCurrentRemotePath(remoteResponse.current_path);

      // Local: navigate to cloud local folder
      const cloudLocalFilesData: LocalFile[] = await invoke('get_local_files', { path: cloudConfig.local_folder, showHidden: showHiddenFiles });
      setLocalFiles(cloudLocalFilesData);
      setCurrentLocalPath(cloudConfig.local_folder);

      humanLog.logSuccess('CONNECT', { server: `AeroCloud (${cloudConfig.server_profile})`, protocol: 'FTP' }, logId);
      notify.success(t('toast.connected'), t('toast.connectedTo', { server: `AeroCloud (${cloudConfig.server_profile})` }));

      // Trigger a sync after connecting to cloud
      try {
        await invoke('trigger_cloud_sync');
        notify.info(t('toast.syncStartedTitle'), t('toast.syncingCloudFiles'));
      } catch (e) {
        console.log('Sync trigger error:', e);
      }

    } catch (error) {
      notify.error(t('connection.connectionFailed'), String(error));
      setShowCloudPanel(true);
    } finally {
      setLoading(false);
    }
  };
  const changeRemoteDirectory = async (path: string, overrideProtocol?: string) => {
    try {
      // Check if we're connected to a Provider (OAuth, S3, WebDAV)
      // Use override protocol if provided, then connectionParams, then active session (most robust)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = (overrideProtocol || connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
      const isProvider = !!protocol && isNonFtpProvider(protocol);

      let response: FileListResponse;
      if (isProvider) {
        // Use provider API
        response = await invoke('provider_change_dir', { path });
      } else {
        // Use FTP API
        response = await invoke('change_directory', { path });
      }
      setRemoteFiles(response.files);
      setCurrentRemotePath(response.current_path);
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
          const files: LocalFile[] = await invoke('get_local_files', { path: newLocalPath, showHidden: showHiddenFiles });
          setLocalFiles(files);
          setCurrentLocalPath(newLocalPath);
        } catch {
          // Local directory doesn't exist - show dialog
          setSyncNavDialog({ missingPath: newLocalPath, isRemote: false, targetPath: newLocalPath });
        }
      }
    } catch (error) { notify.error('Error', `Failed to change directory: ${error}`); }
  };

  const changeLocalDirectory = async (path: string) => {
    await loadLocalFiles(path);
    humanLog.logNavigate(path, false);

    // Save last local path if remember folder is enabled
    if (rememberLastFolder) {
      try {
        const savedSettings = localStorage.getItem(SETTINGS_KEY);
        const settings = savedSettings ? JSON.parse(savedSettings) : {};
        settings.lastLocalPath = path;
        localStorage.setItem(SETTINGS_KEY, JSON.stringify(settings));
      } catch (e) {
        console.error('Failed to save last local path:', e);
      }
    }

    // Navigation Sync: mirror to remote panel if enabled
    if (isSyncNavigation && syncBasePaths && isConnected) {
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = (connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
      const isProvider = !!protocol && isNonFtpProvider(protocol);

      const relativePath = path.startsWith(syncBasePaths.local)
        ? path.slice(syncBasePaths.local.length)
        : '';
      // Join paths avoiding double slashes
      const basePath = syncBasePaths.remote.endsWith('/') ? syncBasePaths.remote.slice(0, -1) : syncBasePaths.remote;
      const relPath = relativePath.startsWith('/') ? relativePath : '/' + relativePath;
      const newRemotePath = (relativePath ? basePath + relPath : basePath) || '/';

      // Check if remote path exists
      try {
        const response: FileListResponse = isProvider
          ? await invoke('provider_change_dir', { path: newRemotePath })
          : await invoke('change_directory', { path: newRemotePath });
        setRemoteFiles(response.files);
        setCurrentRemotePath(response.current_path);
      } catch {
        // Remote directory doesn't exist - show dialog
        setSyncNavDialog({ missingPath: newRemotePath, isRemote: true, targetPath: newRemotePath });
      }
    }
  };

  // Handle sync nav dialog actions
  const handleSyncNavCreateFolder = async () => {
    if (!syncNavDialog) return;
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = (connectionParams.protocol || activeSession?.connectionParams?.protocol) as ProviderType | undefined;
    const isProviderConn = !!protocol && isNonFtpProvider(protocol);
    try {
      if (syncNavDialog.isRemote) {
        if (isProviderConn) {
          await invoke('provider_mkdir', { path: syncNavDialog.targetPath });
          const response: FileListResponse = await invoke('provider_change_dir', { path: syncNavDialog.targetPath });
          setRemoteFiles(response.files);
          setCurrentRemotePath(response.current_path);
        } else {
          await invoke('create_remote_folder', { path: syncNavDialog.targetPath });
          const response: FileListResponse = await invoke('change_directory', { path: syncNavDialog.targetPath });
          setRemoteFiles(response.files);
          setCurrentRemotePath(response.current_path);
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
      // Enabling sync: save current paths as base
      setSyncBasePaths({ remote: currentRemotePath, local: currentLocalPath });
      notify.success(t('toast.navSyncEnabled'), t('toast.syncingPaths', { remote: currentRemotePath, local: currentLocalPath }));
      humanLog.logRaw('activity.nav_sync_enabled', 'NAVIGATE', { remote: currentRemotePath, local: currentLocalPath }, 'success');
    } else {
      setSyncBasePaths(null);
      notify.info(t('toast.navSyncDisabled'));
      humanLog.logRaw('activity.nav_sync_disabled', 'NAVIGATE', {}, 'success');
    }
    setIsSyncNavigation(!isSyncNavigation);
  };

  // checkOverwrite and resetOverwriteSettings provided by useOverwriteCheck hook

  const downloadFile = async (remoteFilePath: string, fileName: string, destinationPath?: string, isDir: boolean = false, fileSize?: number) => {
    const logId = humanLog.logStart('DOWNLOAD', { filename: fileName });
    pendingFileLogIds.current.set(fileName, logId); // Dedup
    const startTime = Date.now();

    // Check if we're using a Provider (get protocol from active session as fallback)
    const activeSession = sessions.find(s => s.id === activeSessionId);
    const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
    const isProvider = !!protocol && isNonFtpProvider(protocol);

    try {
      if (isDir) {
        const downloadPath = destinationPath || await open({ directory: true, multiple: false, defaultPath: await downloadDir() });
        if (downloadPath) {
          const folderPath = `${downloadPath}/${fileName}`;
          if (isProvider) {
            // Use provider command for folder download
            await invoke('provider_download_folder', { remotePath: remoteFilePath, localPath: folderPath });
          } else {
            // Use FTP command
            const params: DownloadFolderParams = { remote_path: remoteFilePath, local_path: folderPath };
            await invoke('download_folder', { params });
          }
          const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
          humanLog.log('DOWNLOAD', `[Local] Downloaded folder ${fileName} in ${elapsed}s`, 'success');
          humanLog.updateEntry(logId, { status: 'success', message: `📥 Downloaded folder ${fileName} in ${elapsed}s` });
        } else {
          humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
        }
      } else {
        const downloadPath = destinationPath || await open({ directory: true, multiple: false, defaultPath: await downloadDir() });
        if (downloadPath) {
          const localFilePath = `${downloadPath}/${fileName}`;
          if (isProvider) {
            // Use provider command for file download
            await invoke('provider_download_file', { remotePath: remoteFilePath, localPath: localFilePath });
          } else {
            // Use FTP command
            const params: DownloadParams = { remote_path: remoteFilePath, local_path: localFilePath };
            await invoke('download_file', { params });
          }
          const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
          const sizeStr = fileSize ? formatBytes(fileSize) : '';
          const details = sizeStr ? `(${sizeStr} in ${elapsed}s)` : `(${elapsed}s)`;
          const msg = t('activity.download_success', { filename: fileName, details });
          humanLog.updateEntry(logId, { status: 'success', message: msg });
        } else {
          humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
        }
      }
    } catch (error) {
      humanLog.logError('DOWNLOAD', { filename: fileName }, logId);
      notify.error('Download Failed', String(error));
    }
  };

  const uploadFile = async (localFilePath: string, fileName: string, isDir: boolean = false, fileSize?: number) => {
    const logId = humanLog.logStart('UPLOAD', { filename: fileName });
    pendingFileLogIds.current.set(fileName, logId); // Register for adoption by backend event
    const startTime = Date.now();
    try {
      // Check if we're using a Provider (get protocol from active session as fallback)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = !!protocol && isNonFtpProvider(protocol);

      if (isDir) {
        if (isProvider) {
          const remoteRootForFolder = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;

          // Recursive upload function
          const processFolder = async (currentLocalPath: string, currentRemoteBase: string) => {
            // Create the directory itself first
            try {
              await invoke('provider_mkdir', { path: currentRemoteBase });
            } catch (e) {
              // Ignore if already exists
            }

            // Use 'get_local_files' for local directory listing!
            const entries = await invoke<LocalFile[]>('get_local_files', { path: currentLocalPath, showHidden: true });
            for (const entry of entries) {
              const newRemotePath = `${currentRemoteBase}/${entry.name}`;
              if (entry.is_dir) {
                await processFolder(entry.path, newRemotePath);
              } else {
                try {
                  humanLog.updateEntry(logId, { message: `Uploading ${entry.name}...` });
                  await invoke('provider_upload_file', { localPath: entry.path, remotePath: newRemotePath });
                } catch (e) {
                  console.error(`Failed to upload ${entry.name}:`, e);
                  // Continue with other files? Yes.
                }
              }
            }
          };

          await processFolder(localFilePath, remoteRootForFolder);

          const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
          const details = `(${elapsed}s)`;
          const msg = t('activity.upload_success', { filename: fileName, details });
          humanLog.updateEntry(logId, { message: msg });
          // Refresh list
          loadRemoteFiles();
          return;
        }
        const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;
        const params: UploadFolderParams = { local_path: localFilePath, remote_path: remotePath };
        await invoke('upload_folder', { params });
        const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
        const details = `(${elapsed}s)`;
        const msg = t('activity.upload_success', { filename: fileName, details });
        humanLog.updateEntry(logId, { status: 'success', message: msg });
      } else {
        const remotePath = `${currentRemotePath}${currentRemotePath.endsWith('/') ? '' : '/'}${fileName}`;

        if (isProvider) {
          await invoke('provider_upload_file', { localPath: localFilePath, remotePath });
        } else {
          await invoke('upload_file', { params: { local_path: localFilePath, remote_path: remotePath } as UploadParams });
        }

        const elapsed = ((Date.now() - startTime) / 1000).toFixed(1);
        const sizeStr = fileSize ? formatBytes(fileSize) : '';
        const details = sizeStr ? `(${sizeStr} in ${elapsed}s)` : `(${elapsed}s)`;
        const msg = t('activity.upload_success', { filename: fileName, details });
        humanLog.updateEntry(logId, { status: 'success', message: msg });
      }
    } catch (error) {
      humanLog.logError('UPLOAD', { filename: fileName }, logId);
      notify.error('Upload Failed', String(error));
    }
  };

  const cancelTransfer = async () => {
    setActiveTransfer(null); // Close popup immediately
    try { await invoke('cancel_transfer'); } catch { }
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
        // Queue shows progress - no toast needed

        // Add all files to queue first
        const queueItems = filesToUpload.map(({ path: filePath, file }) => {
          const fileName = filePath.split(/[/\\]/).pop() || filePath;
          const size = file?.size || 0;
          return { id: transferQueue.addItem(fileName, filePath, size, 'upload'), filePath, fileName, file };
        });

        // Upload sequentially with queue tracking and overwrite checking
        let skippedCount = 0;
        for (let i = 0; i < queueItems.length; i++) {
          const item = queueItems[i];
          const remainingInQueue = queueItems.length - i - 1;

          // Check for overwrite (only for files, not directories)
          if (!item.file.is_dir) {
            const overwriteResult = await checkOverwrite(
              item.fileName,
              item.file.size || 0,
              item.file.modified ? new Date(item.file.modified) : undefined,
              false, // sourceIsRemote = false for upload
              remainingInQueue
            );

            if (overwriteResult.action === 'cancel') {
              // Cancel entire batch
              transferQueue.failTransfer(item.id, 'Cancelled by user');
              for (let j = i + 1; j < queueItems.length; j++) {
                transferQueue.failTransfer(queueItems[j].id, 'Cancelled by user');
              }
              break;
            }

            if (overwriteResult.action === 'skip') {
              transferQueue.completeTransfer(item.id); // Mark as complete (skipped)
              humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: item.fileName }, 'success');
              skippedCount++;
              continue;
            }

            // For rename, we would need to modify the destination - for now, just proceed
            // TODO: Implement rename logic
          }

          transferQueue.startTransfer(item.id);
          try {
            await uploadFile(item.filePath, item.fileName, item.file?.is_dir || false, item.file?.size || undefined);
            transferQueue.completeTransfer(item.id);
          } catch (error) {
            transferQueue.failTransfer(item.id, String(error));
          }
        }

        // Reset apply-to-all after batch completes
        resetOverwriteSettings();

        // Queue shows completion - no toast needed
        if (skippedCount > 0) {
          notify.info(`${skippedCount} file(s) skipped`);
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
      title: 'Select Files to Upload',
    });

    if (!selected) return;
    const files = Array.isArray(selected) ? selected : [selected];

    if (files.length > 0) {
      // Reset for dialog-selected files too
      resetOverwriteSettings();
      let skippedCount = 0;

      for (let i = 0; i < files.length; i++) {
        const filePath = files[i];
        const fileName = filePath.replace(/^.*[\\\/]/, '');
        const remainingInQueue = files.length - i - 1;

        // Check for overwrite
        const overwriteResult = await checkOverwrite(
          fileName,
          0, // Size unknown from dialog
          undefined, // Modified unknown from dialog
          false, // sourceIsRemote = false for upload
          remainingInQueue
        );

        if (overwriteResult.action === 'cancel') {
          break;
        }

        if (overwriteResult.action === 'skip') {
          humanLog.logRaw('activity.upload_skipped', 'UPLOAD', { filename: fileName }, 'success');
          skippedCount++;
          continue;
        }

        await uploadFile(filePath, fileName, false);
      }

      resetOverwriteSettings();
      if (skippedCount > 0) {
        notify.info(`${skippedCount} file(s) skipped`);
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
      // Queue shows progress - no toast needed

      // Add all files to queue first
      const queueItems = filesToDownload.map(file => ({
        id: transferQueue.addItem(file.name, file.path, file.size || 0, 'download'),
        file
      }));

      // Download sequentially with queue tracking and overwrite checking
      let skippedCount = 0;
      for (let i = 0; i < queueItems.length; i++) {
        const item = queueItems[i];
        const remainingInQueue = queueItems.length - i - 1;

        // Check for overwrite (only for files, not directories)
        if (!item.file.is_dir) {
          const overwriteResult = await checkOverwrite(
            item.file.name,
            item.file.size || 0,
            item.file.modified ? new Date(item.file.modified) : undefined,
            true, // sourceIsRemote
            remainingInQueue
          );

          if (overwriteResult.action === 'cancel') {
            // Cancel entire batch
            transferQueue.failTransfer(item.id, 'Cancelled by user');
            for (let j = i + 1; j < queueItems.length; j++) {
              transferQueue.failTransfer(queueItems[j].id, 'Cancelled by user');
            }
            break;
          }

          if (overwriteResult.action === 'skip') {
            transferQueue.completeTransfer(item.id); // Mark as complete (skipped)
            humanLog.logRaw('activity.download_skipped', 'DOWNLOAD', { filename: item.file.name }, 'success');
            skippedCount++;
            continue;
          }

          // For rename, we would need to modify the destination - for now, just proceed
          // TODO: Implement rename logic
        }

        transferQueue.startTransfer(item.id);
        try {
          await downloadFile(item.file.path, item.file.name, currentLocalPath, item.file.is_dir, item.file.size || undefined);
          transferQueue.completeTransfer(item.id);
        } catch (error) {
          transferQueue.failTransfer(item.id, String(error));
        }
      }

      // Reset apply-to-all after batch completes
      resetOverwriteSettings();

      // Queue shows completion - no toast needed
      if (skippedCount > 0) {
        notify.info(`${skippedCount} file(s) skipped`);
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
      const logId = humanLog.logStart('DELETE_MULTIPLE', { count: names.length, isRemote: true });
      const deletedFiles: string[] = [];
      const deletedFolders: string[] = [];
      // Get protocol from active session as fallback (outside loop for efficiency)
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = !!protocol && isNonFtpProvider(protocol);

      for (const name of names) {
        const file = remoteFiles.find(f => f.name === name);
        if (file) {
          try {
            if (isProvider) {
              if (file.is_dir) {
                await invoke('provider_delete_dir', { path: file.path, recursive: true });
              } else {
                await invoke('provider_delete_file', { path: file.path });
              }
            } else {
              await invoke('delete_remote_file', { path: file.path, isDir: file.is_dir });
            }

            if (file.is_dir) {
              deletedFolders.push(name);
            } else {
              deletedFiles.push(name);
            }
            // Individual logs removed - summary handles all
          } catch { }
        }
      }
      await loadRemoteFiles();
      setSelectedRemoteFiles(new Set());
      // Summary message with location and item details
      // Note: For recursive folder deletes, the backend already logs individual files via
      // delete_file_complete/delete_dir_complete events. The summary here only covers the
      // top-level items selected by the user (not the files inside folders).
      const loc = t('browser.remote');
      const count = deletedFolders.length + deletedFiles.length;
      if (count === 1 && deletedFolders.length === 1) {
        // Single folder: backend already logged "Folder removed: X" via delete_dir_complete,
        // so just silently mark our start entry as success without duplicating
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.delete_dir_success', { location: loc, filename: deletedFolders[0] }) });
      } else if (count === 1 && deletedFiles.length === 1) {
        // Single file: no backend events, this is the only log entry
        humanLog.updateEntry(logId, { status: 'success', message: t('activity.delete_file_success', { location: loc, filename: deletedFiles[0] }) });
      } else {
        // Multiple items
        const allDeleted = [...deletedFolders.map(n => `📁 ${n}`), ...deletedFiles.map(n => `📄 ${n}`)];
        humanLog.updateEntry(logId, { status: 'success', message: `[${loc}] ${t('activity.delete_multiple_done', { count, items: allDeleted.join(', ') })}` });
      }
      const parts = [];
      if (deletedFolders.length > 0) parts.push(`${deletedFolders.length} folder${deletedFolders.length > 1 ? 's' : ''}`);
      if (deletedFiles.length > 0) parts.push(`${deletedFiles.length} file${deletedFiles.length > 1 ? 's' : ''}`);
      notify.success(parts.join(', '), `${parts.join(' and ')} deleted`);
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: `Delete ${names.length} selected items?`,
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
      for (const name of names) {
        const file = localFiles.find(f => f.name === name);
        if (file) {
          try {
            await invoke('delete_local_file', { path: file.path });
            if (file.is_dir) {
              deletedFolders.push(name);
            } else {
              deletedFiles.push(name);
            }
            // Individual logs removed - summary handles all
          } catch { }
        }
      }
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
        humanLog.updateEntry(logId, { status: 'success', message: `[${loc}] ${t('activity.delete_multiple_done', { count, items: allDeleted.join(', ') })}` });
      }
      const parts = [];
      if (deletedFolders.length > 0) parts.push(`${deletedFolders.length} folder${deletedFolders.length > 1 ? 's' : ''}`);
      if (deletedFiles.length > 0) parts.push(`${deletedFiles.length} file${deletedFiles.length > 1 ? 's' : ''}`);
      notify.success(parts.join(', '), `${parts.join(' and ')} deleted`);
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: `Delete ${names.length} selected items?`,
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
  const deleteRemoteFile = (path: string, isDir: boolean) => {
    const fileName = path.split('/').pop() || path;

    const performDelete = async () => {
      const logId = humanLog.logStart('DELETE', { filename: fileName });
      try {
        // Get protocol from active session as fallback
        const activeSession = sessions.find(s => s.id === activeSessionId);
        const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
        const isProvider = !!protocol && isNonFtpProvider(protocol);

        if (isProvider) {
          if (isDir) {
            await invoke('provider_delete_dir', { path, recursive: true });
          } else {
            await invoke('provider_delete_file', { path });
          }
        } else {
          await invoke('delete_remote_file', { path, isDir });
        }
        humanLog.logSuccess('DELETE', { filename: fileName }, logId);
        notify.success(t('toast.deleted'), fileName);
        await loadRemoteFiles();
      }
      catch (error) {
        humanLog.logError('DELETE', { filename: fileName }, logId);
        notify.error(t('toast.deleteFail'), String(error));
      }
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: `Delete "${fileName}"?`,
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
    const fileName = path.split('/').pop() || path;

    const performDelete = async () => {
      const logId = humanLog.logStart('DELETE', { filename: fileName });
      try {
        await invoke('delete_local_file', { path });
        humanLog.logSuccess('DELETE', { filename: fileName }, logId);
        notify.success(t('toast.deleted'), fileName);
        await loadLocalFiles(currentLocalPath);
      }
      catch (error) {
        humanLog.logError('DELETE', { filename: fileName }, logId);
        notify.error(t('toast.deleteFail'), String(error));
      }
    };

    // Check if confirmation is enabled
    if (confirmBeforeDelete) {
      setConfirmDialog({
        message: `Delete "${fileName}"?`,
        onConfirm: async () => {
          setConfirmDialog(null);
          await performDelete();
        }
      });
    } else {
      performDelete();
    }
  };

  // Legacy wrapper to maintain compatibility - removed duplicate error handling
  const deleteLocalFileInternal = async (path: string) => {
    const fileName = path.split('/').pop() || path;
    const logId = humanLog.logStart('DELETE', { filename: fileName });
    try {
      await invoke('delete_local_file', { path });
      humanLog.logSuccess('DELETE', { filename: fileName }, logId);
      notify.success(t('toast.deleted'), fileName);
      await loadLocalFiles(currentLocalPath);
    } catch (error) {
      humanLog.logError('DELETE', { filename: fileName }, logId);
      notify.error(t('toast.deleteFail'), String(error));
    }
  };

  const renameFile = (path: string, currentName: string, isRemote: boolean) => {
    setInputDialog({
      title: t('common.rename'),
      defaultValue: currentName,
      onConfirm: async (newName: string) => {
        setInputDialog(null);
        if (!newName || newName === currentName) return;
        const logId = humanLog.logStart('RENAME', { oldname: currentName, newname: newName });
        try {
          // Get parent directory from the file's path
          const parentDir = path.substring(0, path.lastIndexOf('/'));
          const newPath = parentDir + '/' + newName;

          if (isRemote) {
            // Get protocol from active session as fallback
            const activeSession = sessions.find(s => s.id === activeSessionId);
            const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
            const isProvider = !!protocol && isNonFtpProvider(protocol);

            if (isProvider) {
              await invoke('provider_rename', { from: path, to: newPath });
            } else {
              await invoke('rename_remote_file', { from: path, to: newPath });
            }
            await loadRemoteFiles();
          } else {
            await invoke('rename_local_file', { from: path, to: newPath });
            await loadLocalFiles(currentLocalPath);
          }
          humanLog.logSuccess('RENAME', { oldname: currentName, newname: newName }, logId);
          notify.success(t('toast.renamed'), newName);
        } catch (error) {
          humanLog.logError('RENAME', { oldname: currentName, newname: newName }, logId);
          notify.error(t('toast.renameFail'), String(error));
        }
      }
    });
  };

  // Inline rename: start editing directly in the file list
  const startInlineRename = (path: string, name: string, isRemote: boolean) => {
    if (name === '..') return;
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

    const logId = humanLog.logStart('RENAME', { oldname: name, newname: newName });
    try {
      const parentDir = path.substring(0, path.lastIndexOf('/'));
      const newPath = parentDir + '/' + newName;

      if (isRemote) {
        const activeSession = sessions.find(s => s.id === activeSessionId);
        const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
        const isProvider = !!protocol && isNonFtpProvider(protocol);

        if (isProvider) {
          await invoke('provider_rename', { from: path, to: newPath });
        } else {
          await invoke('rename_remote_file', { from: path, to: newPath });
        }
        await loadRemoteFiles();
      } else {
        await invoke('rename_local_file', { from: path, to: newPath });
        await loadLocalFiles(currentLocalPath);
      }
      humanLog.logSuccess('RENAME', { oldname: name, newname: newName }, logId);
      notify.success(t('toast.renamed'), newName);
    } catch (error) {
      humanLog.logError('RENAME', { oldname: name, newname: newName }, logId);
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
          const isProvider = !!protocol && isNonFtpProvider(protocol);

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
      await loadRemoteFiles();
    } else {
      await loadLocalFiles(currentLocalPath);
    }

    // Log and notify
    if (errorCount === 0) {
      humanLog.logSuccess('RENAME', { count: successCount }, logId);
      notify.success(
        t('toast.batchRenameSuccess') || 'Batch rename complete',
        `${successCount} ${t('browser.files') || 'files'}`
      );
    } else {
      humanLog.logError('RENAME', { count: successCount, message: `${errorCount} failed` }, logId);
      notify.warning(
        t('toast.batchRenamePartial') || 'Batch rename completed with errors',
        `${successCount} renamed, ${errorCount} failed`
      );
    }

    setBatchRenameDialog(null);
  };

  const createFolder = (isRemote: boolean) => {
    setInputDialog({
      title: 'New Folder',
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
            const isProvider = !!protocol && isNonFtpProvider(protocol);

            const path = currentRemotePath + (currentRemotePath.endsWith('/') ? '' : '/') + name;

            if (isProvider) {
              await invoke('provider_mkdir', { path });
            } else {
              await invoke('create_remote_folder', { path });
            }
            await loadRemoteFiles();
          } else {
            // Create local folder
            const path = currentLocalPath + '/' + name;
            await invoke('create_local_folder', { path });
            await loadLocalFiles(currentLocalPath);
          }
          humanLog.logSuccess('MKDIR', { foldername: name, isRemote }, logId);
          notify.success('Created', name);
        } catch (error) {
          humanLog.logError('MKDIR', { foldername: name, isRemote }, logId);
          notify.error('Create Failed', String(error));
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

    const items: ContextMenuItem[] = [
      { label: downloadLabel, icon: <Download size={14} />, action: () => downloadMultipleFiles(filesToUse) },
      // Media files (images, audio, video, pdf) use Universal Preview modal
      { label: t('common.preview'), icon: <Eye size={14} />, action: () => openUniversalPreview(file, true), disabled: count > 1 || file.is_dir || !isMediaPreviewable(file.name) },
      // Code files use DevTools source viewer
      { label: t('contextMenu.viewSource'), icon: <Code size={14} />, action: () => openDevToolsPreview(file, true), disabled: count > 1 || file.is_dir || !isPreviewable(file.name) },
      { label: t('common.rename'), icon: <Pencil size={14} />, action: () => renameFile(file.path, file.name, true), disabled: count > 1 },
      ...(count > 1 ? [{
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
      { label: t('contextMenu.delete'), icon: <Trash2 size={14} />, action: () => deleteMultipleRemoteFiles(filesToUse), danger: true, divider: true },
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

    // Add native Share Link for providers that support it (OAuth + S3 pre-signed URLs + MEGA)
    const hasNativeShareLink = currentProtocol && supportsNativeShareLink(currentProtocol);
    if (hasNativeShareLink) {
      const shareIcon = (() => {
        switch (currentProtocol) {
          case 'googledrive': return <GoogleDriveLogo size={14} />;
          case 'dropbox': return <DropboxLogo size={14} />;
          case 'onedrive': return <OneDriveLogo size={14} />;
          case 'mega': return <MegaLogo size={14} />;
          case 'box': return <BoxLogo size={14} />;
          case 'pcloud': return <PCloudLogo size={14} />;
          case 'filen': return <FilenLogo size={14} />;
          default: return <Share2 size={14} />;
        }
      })();
      items.push({
        label: t('contextMenu.createShareLink'),
        icon: shareIcon,
        action: async () => {
          try {
            notify.info(t('contextMenu.creatingShareLink'), t('contextMenu.shareLinkMoment'));
            const shareUrl = await invoke<string>('provider_create_share_link', { path: file.path });
            await invoke('copy_to_clipboard', { text: shareUrl });
            notify.success(t('contextMenu.shareLinkCopied'), shareUrl);
          } catch (err: unknown) {
            notify.error(t('contextMenu.shareLinkFailed'), String(err));
          }
        }
      });
    }

    // Add Import MEGA Link option (MEGA only)
    if (currentProtocol === 'mega') {
      items.push({
        label: t('contextMenu.importMegaLink'),
        icon: <MegaLogo size={14} />,
        action: async () => {
          const link = window.prompt(t('contextMenu.importMegaPrompt'));
          if (!link || !link.trim()) return;
          try {
            notify.info(t('contextMenu.importingLink'), t('contextMenu.shareLinkMoment'));
            const dest = currentRemotePath || '/';
            await invoke('provider_import_link', { link: link.trim(), dest });
            notify.success(t('contextMenu.linkImported'), t('contextMenu.importedTo', { path: dest }));
            loadRemoteFiles();
          } catch (err) {
            notify.error(t('contextMenu.importLinkFailed'), String(err));
          }
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
              notify.success('File unlocked');
            } catch (err) {
              notify.error('Failed to unlock', String(err));
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
              notify.success('File locked', `Token: ${info.token.slice(0, 20)}...`);
            } catch (err) {
              notify.error('Failed to lock', String(err));
            }
          },
        });
      }
    }

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
    const uploadLabel = count > 1 ? t('contextMenu.uploadCount', { count }) : t('common.upload');
    const filesToUpload = Array.from(selection);

    const items: ContextMenuItem[] = [
      {
        label: uploadLabel,
        icon: <Cloud size={14} />,
        action: () => uploadMultipleFiles(filesToUpload),
        disabled: !isConnected
      },
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
        label: t('contextMenu.properties'), icon: <Info size={14} />, action: () => setPropertiesDialog({
          name: file.name,
          path: file.path,
          size: file.size,
          is_dir: file.is_dir,
          modified: file.modified,
          isRemote: false,
        }), disabled: count > 1
      },
      { label: t('contextMenu.delete'), icon: <Trash2 size={14} />, action: () => deleteMultipleLocalFiles(filesToUpload), danger: true, divider: true },
      { label: t('contextMenu.copyPath'), icon: <Copy size={14} />, action: () => { navigator.clipboard.writeText(file.path); notify.success(t('contextMenu.pathCopied')); } },
      { label: t('contextMenu.copyName'), icon: <Clipboard size={14} />, action: () => { navigator.clipboard.writeText(file.name); notify.success(t('contextMenu.nameCopied')); }, divider: true },
      { label: t('contextMenu.openInFileManager'), icon: <ExternalLink size={14} />, action: () => openInFileManager(file.is_dir ? file.path : currentLocalPath) },
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
                  notify.success('Extracted!', `Files extracted to ${dest}`);
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
          notify.success('Extracted!', `Files extracted to ${dest}`);
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
        action: () => {},
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
        label: 'Copy Share Link',
        icon: <Share2 size={14} />,
        action: async () => {
          try {
            const shareUrl = await invoke<string>('generate_share_link', { localPath: file.path });
            await invoke('copy_to_clipboard', { text: shareUrl });
            notify.success('Share link copied!', shareUrl);
          } catch (err) {
            notify.error('Failed to generate share link', String(err));
          }
        }
      });
    }

    // Cryptomator vault option for folders (legacy format support)
    if (file.is_dir && count === 1) {
      items.push({
        label: t('cryptomator.openAsVault') || 'Open as Cryptomator Vault',
        icon: <Lock size={14} className="text-emerald-500" />,
        action: () => setShowCryptomatorBrowser(true),
        divider: true
      });
    }

    contextMenu.show(e, items);
  };

  const handleRemoteFileAction = async (file: RemoteFile) => {
    if (file.is_dir) {
      // Use file.path for providers (WebDAV/S3) that need absolute paths
      // file.name works for FTP which handles relative paths
      // Get protocol from active session as fallback
      const activeSession = sessions.find(s => s.id === activeSessionId);
      const protocol = connectionParams.protocol || activeSession?.connectionParams?.protocol;
      const isProvider = !!protocol && isNonFtpProvider(protocol);
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
    <div className={`h-screen bg-gradient-to-br from-gray-50 to-gray-100 dark:from-gray-900 dark:to-gray-800 text-gray-900 dark:text-gray-100 transition-colors duration-300 flex flex-col overflow-hidden ${compactMode ? 'compact-mode' : ''} font-size-${fontSize}`}>
      {/* Native System Titlebar - CustomTitlebar removed for Linux compatibility */}

      <ToastContainer toasts={toast.toasts} onRemove={toast.removeToast} />

      {/* Update Available Badge with inline download */}
      {updateAvailable?.has_update && !updateToastDismissed && (
        <div className={`fixed top-4 right-4 bg-blue-600 dark:bg-blue-700 text-white px-4 py-3 rounded-xl shadow-2xl z-50 flex flex-col gap-2 border border-blue-400/30 min-w-[300px] ${!updateDownload ? 'animate-pulse' : ''}`}>
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
              title={t('connection.dismiss')}
            >
              <X size={16} />
            </button>
          </div>

          {/* Download states */}
          {!updateDownload && (
            <button
              onClick={startUpdateDownload}
              className="bg-white text-blue-600 px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-blue-50 transition-colors shadow-sm w-full"
            >
              {t('update.downloadNow')} (.{updateAvailable.install_format || 'deb'})
            </button>
          )}

          {updateDownload?.downloading && (
            <div className="flex flex-col gap-1.5">
              <div className="w-full bg-blue-800 rounded-full h-2 overflow-hidden">
                <div
                  className="bg-white h-full rounded-full transition-all duration-200"
                  style={{ width: `${updateDownload.percentage}%` }}
                />
              </div>
              <div className="flex justify-between text-xs opacity-80">
                <span>{updateDownload.percentage}%</span>
                <span>
                  {updateDownload.speed_bps > 0
                    ? `${(updateDownload.speed_bps / 1024 / 1024).toFixed(1)} MB/s`
                    : t('update.starting')
                  }
                  {updateDownload.eta_seconds > 0 && ` — ${updateDownload.eta_seconds}s`}
                </span>
              </div>
            </div>
          )}

          {updateDownload?.completedPath && (
            <div className="flex flex-col gap-1.5">
              <span className="text-xs text-green-200 flex items-center gap-1">
                <CheckCircle2 size={12} /> {t('update.downloadComplete')}
              </span>
              <span className="text-xs opacity-70 truncate" title={updateDownload.completedPath}>
                {updateDownload.completedPath}
              </span>
              {updateAvailable.install_format === 'appimage' ? (
                <button
                  onClick={() => invoke('install_appimage_update', { downloadedPath: updateDownload.completedPath })}
                  className="bg-green-500 text-white px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-green-400 transition-colors shadow-sm w-full flex items-center justify-center gap-1.5"
                >
                  <RefreshCw size={12} /> {t('update.installRestart')}
                </button>
              ) : (
                <button
                  onClick={() => invoke('open_in_file_manager', { path: updateDownload.completedPath })}
                  className="bg-white text-blue-600 px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-blue-50 transition-colors shadow-sm w-full flex items-center justify-center gap-1.5"
                >
                  <ExternalLink size={12} /> {t('update.openFolder')}
                </button>
              )}
            </div>
          )}

          {updateDownload?.error && (
            <div className="flex flex-col gap-1.5">
              <span className="text-xs text-red-200 flex items-center gap-1">
                <AlertTriangle size={12} /> {updateDownload.error}
              </span>
              <button
                onClick={startUpdateDownload}
                className="bg-white text-blue-600 px-3 py-1.5 rounded-lg font-medium text-sm hover:bg-blue-50 transition-colors shadow-sm w-full"
              >
                {t('update.retry')}
              </button>
            </div>
          )}
        </div>
      )}
      <TransferQueue
        items={transferQueue.items}
        isVisible={transferQueue.isVisible}
        onToggle={transferQueue.toggle}
        onClear={transferQueue.clear}
      />
      {contextMenu.state.visible && <ContextMenu x={contextMenu.state.x} y={contextMenu.state.y} items={contextMenu.state.items} onClose={contextMenu.hide} />}
      {activeTransfer && <TransferProgressBar transfer={activeTransfer} onCancel={cancelTransfer} />}
      {confirmDialog && <ConfirmDialog message={confirmDialog.message} onConfirm={confirmDialog.onConfirm} onCancel={() => setConfirmDialog(null)} />}
      {inputDialog && <InputDialog title={inputDialog.title} defaultValue={inputDialog.defaultValue} onConfirm={inputDialog.onConfirm} onCancel={() => setInputDialog(null)} isPassword={inputDialog.isPassword} placeholder={inputDialog.placeholder} />}
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
          onClose={() => setPropertiesDialog(null)}
          onCalculateChecksum={async (algorithm) => {
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
              notify.error(`Checksum failed`, String(err));
              setPropertiesDialog(prev => prev ? { ...prev, checksum: { ...prev.checksum, calculating: false } } : null);
            }
          }}
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
              notify.success('Permissions Updated', `${permissionsDialog.file.name} -> ${mode}`);
              await loadRemoteFiles();
              setPermissionsDialog(null);
            } catch (e) { notify.error('Failed', String(e)); }
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
          onRestore={() => { setVersionsDialog(null); loadRemoteFiles(); }}
        />
      )}
      {sharePermissionsDialog && (
        <SharePermissionsDialog
          filePath={sharePermissionsDialog.path}
          fileName={sharePermissionsDialog.name}
          onClose={() => setSharePermissionsDialog(null)}
        />
      )}
      <AboutDialog isOpen={showAboutDialog} onClose={() => setShowAboutDialog(false)} />
      <SupportDialog isOpen={showSupportDialog} onClose={() => setShowSupportDialog(false)} />
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
      <ShortcutsDialog isOpen={showShortcutsDialog} onClose={() => setShowShortcutsDialog(false)} />
      <SettingsPanel
        isOpen={showSettingsPanel}
        onClose={() => setShowSettingsPanel(false)}
        onOpenCloudPanel={() => setShowCloudPanel(true)}
        onActivityLog={{ logRaw: humanLog.logRaw }}
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
      {showVaultPanel && <VaultPanel onClose={() => setShowVaultPanel(false)} />}
      {showCryptomatorBrowser && <CryptomatorBrowser onClose={() => setShowCryptomatorBrowser(false)} />}
      {archiveBrowserState && (
        <ArchiveBrowser
          archivePath={archiveBrowserState.path}
          archiveType={archiveBrowserState.type}
          isEncrypted={archiveBrowserState.encrypted}
          onClose={() => setArchiveBrowserState(null)}
        />
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
              notify.success('Compressed!', `Created ${opts.archiveName}${ext}${suffix}`);
              setCompressDialogState(null);
              await loadLocalFiles(currentLocalPath);
            } catch (err) {
              activityLog.log('ERROR', `Compression failed: ${String(err)}`, 'error');
              notify.error(t('contextMenu.compressionFailed'), String(err));
            }
          }}
        />
      )}

      {/* Header - can be hidden */}
      {showMenuBar && (
        <header className="sticky top-0 z-30 bg-white/80 dark:bg-gray-900/80 backdrop-blur-xl border-b border-gray-200 dark:border-gray-700">
          <div className="flex items-center justify-between px-6 py-3">
            <Logo size="sm" isConnected={isConnected} hasActivity={hasActivity || hasQueueActivity} isReconnecting={isReconnecting} />
            <div className="flex items-center gap-3">
              {/* Support button - subtle heart icon */}
              <button
                onClick={() => setShowSupportDialog(true)}
                className="p-2 rounded-lg bg-blue-500/10 hover:bg-blue-500/20 text-blue-500 dark:text-blue-400 transition-colors"
                title={t('about.supportDev')}
              >
                <Heart size={18} className="fill-current" />
              </button>
              {/* Separator */}
              <div className="w-px h-5 bg-gray-300 dark:bg-gray-600" />
              {/* AeroVault */}
              <button
                onClick={() => setShowVaultPanel(true)}
                className="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
                title="AeroVault - Military-Grade Encryption"
              >
                <Shield size={18} className="text-emerald-500 dark:text-emerald-400" />
              </button>
              {/* Separator */}
              <div className="w-px h-5 bg-gray-300 dark:bg-gray-600" />
              {/* Quick System Menu Bar Toggle */}
              <button
                onClick={async () => {
                  const newState = !systemMenuVisible;
                  setSystemMenuVisible(newState);
                  await invoke('toggle_menu_bar', { visible: newState });
                }}
                className="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
                title={systemMenuVisible ? 'Hide system menu bar' : 'Show system menu bar'}
              >
                <PanelTop size={18} className={systemMenuVisible ? 'text-blue-500' : 'text-gray-400'} />
              </button>
              {/* Theme toggle */}
              <ThemeToggle theme={theme} setTheme={setTheme} />
              {/* Settings button */}
              <button
                onClick={() => setShowSettingsPanel(true)}
                className="p-2 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-800 transition-colors"
                title={`${t('settings.title')} (Ctrl+,)`}
              >
                <Settings size={18} className="text-gray-500 dark:text-gray-400" />
              </button>
              {isConnected ? (
                <button onClick={disconnectFromFtp} className="px-4 py-2 bg-red-500 hover:bg-red-600 text-white rounded-lg transition-colors shadow-sm hover:shadow-md flex items-center gap-2">
                  <LogOut size={16} /> {t('common.disconnect')}
                </button>
              ) : !showConnectionScreen && (
                <button
                  onClick={() => setShowConnectionScreen(true)}
                  className="px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg transition-all shadow-sm hover:shadow-md flex items-center gap-2"
                >
                  <Cloud size={16} /> {t('common.connect')}
                </button>
              )}
            </div>
          </div>
        </header>
      )}

      <main className={`flex-1 min-h-0 p-6 overflow-auto ${devToolsMaximized && devToolsOpen ? 'hidden' : ''}`}>
        {!isConnected && showConnectionScreen ? (
          <ConnectionScreen
            connectionParams={connectionParams}
            quickConnectDirs={quickConnectDirs}
            loading={loading}
            onConnectionParamsChange={setConnectionParams}
            onQuickConnectDirsChange={setQuickConnectDirs}
            onConnect={connectToFtp}
            onOpenCloudPanel={() => setShowCloudPanel(true)}
            hasExistingSessions={sessions.length > 0}
            onSavedServerConnect={async (params, initialPath, localInitialPath) => {
              // NOTE: Do NOT set connectionParams here - that would show the form
              // The form should only appear when clicking Edit, not when connecting

              // Check if this is an OAuth provider
              const isOAuth = params.protocol && isOAuthProvider(params.protocol);
              console.log('[onSavedServerConnect] params:', { ...params, password: params.password ? '***' : null });
              console.log('[onSavedServerConnect] isOAuth:', isOAuth);

              if (isOAuth) {
                // OAuth provider is already connected via SavedServers component
                // Just switch to file manager view
                setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                setShowConnectionScreen(false);
                const providerNames: Record<string, string> = { googledrive: 'Google Drive', dropbox: 'Dropbox', onedrive: 'OneDrive', box: 'Box', pcloud: 'pCloud' };
                const providerName = params.displayName || (params.protocol && providerNames[params.protocol]) || params.protocol || 'Unknown';
                notify.success(t('toast.connected'), t('toast.connectedTo', { server: providerName }));
                // Load remote files for OAuth provider - pass protocol explicitly
                await loadRemoteFiles(params.protocol);
                // Navigate to initial local directory if specified
                if (localInitialPath) {
                  await changeLocalDirectory(localInitialPath);
                }
                // Create session with provider name
                createSession(
                  providerName,
                  params,
                  initialPath || '/',
                  localInitialPath || currentLocalPath
                );
                fetchStorageQuota(params.protocol);
                // Reset form for next "Add New Server"
                setConnectionParams({ server: '', username: '', password: '' });
                setQuickConnectDirs({ remoteDir: '', localDir: '' });
                return;
              }

              // Check if this is a non-FTP provider protocol (S3, WebDAV, MEGA, Filen use provider_connect)
              const isProvider = params.protocol && isNonFtpProvider(params.protocol);

              if (isProvider) {
                // S3/WebDAV connection via provider_connect
                setLoading(true);
                setIsSyncNavigation(false);
                setSyncBasePaths(null);
                // Use displayName if available - no protocol prefix, icon shows protocol
                const providerName = params.displayName || (params.protocol === 's3'
                  ? params.options?.bucket || 'S3'
                  : params.protocol === 'mega'
                    ? params.username
                    : params.server.split(':')[0]);
                const protocolLabel = (params.protocol || 'FTP').toUpperCase();
                const logId = humanLog.logStart('CONNECT', { server: providerName, protocol: protocolLabel });

                try {
                  // Disconnect any existing connections
                  try { await invoke('provider_disconnect'); } catch { }
                  try { await invoke('disconnect_ftp'); } catch { }

                  // Build provider connection params
                  const providerParams = {
                    protocol: params.protocol,
                    server: params.server,
                    port: params.port,
                    username: params.username,
                    password: params.password,
                    initial_path: initialPath || null,
                    bucket: params.options?.bucket,
                    region: params.options?.region || 'us-east-1',
                    endpoint: params.options?.endpoint || null,
                    path_style: params.options?.pathStyle || false,
                    save_session: params.options?.save_session,
                    session_expires_at: params.options?.session_expires_at,
                    // SFTP-specific options
                    private_key_path: params.options?.private_key_path || null,
                    key_passphrase: params.options?.key_passphrase || null,
                    timeout: params.options?.timeout || 30,
                    // FTP/FTPS-specific options
                    tls_mode: params.options?.tlsMode || (params.protocol === 'ftps' ? 'implicit' : undefined),
                    verify_cert: params.options?.verifyCert !== undefined ? params.options.verifyCert : true,
                  };

                  console.log('[onSavedServerConnect] provider_connect params:', { ...providerParams, password: providerParams.password ? '***' : null, key_passphrase: providerParams.key_passphrase ? '***' : null });
                  await invoke('provider_connect', { params: providerParams });

                  setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                  humanLog.logSuccess('CONNECT', { server: providerName, protocol: protocolLabel }, logId);
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
                  }));
                  setRemoteFiles(files);
                  setCurrentRemotePath(response.current_path);

                  if (localInitialPath) {
                    await changeLocalDirectory(localInitialPath);
                  }

                  createSession(
                    providerName,
                    params,
                    response.current_path,
                    localInitialPath || currentLocalPath
                  );
                  fetchStorageQuota(params.protocol);
                  // Reset form for next "Add New Server"
                  setConnectionParams({ server: '', username: '', password: '' });
                  setQuickConnectDirs({ remoteDir: '', localDir: '' });
                } catch (error) {
                  humanLog.logError('CONNECT', { server: providerName }, logId);
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

                await invoke('connect_ftp', { params });
                setIsConnected(true); setShowRemotePanel(true); setShowLocalPreview(false);
                humanLog.logSuccess('CONNECT', { server: params.server, protocol: protocolLabel }, logId);
                notify.success('Connected', `Connected to ${params.server}`);

                // Get the actual remote path after connection
                let actualRemotePath = '/';
                if (initialPath) {
                  // Pass the protocol explicitly to avoid using stale state from previous session
                  await changeRemoteDirectory(initialPath, params.protocol || 'ftp');
                  actualRemotePath = initialPath;
                } else {
                  await loadRemoteFiles();
                  // After loadRemoteFiles, currentRemotePath will be updated
                  actualRemotePath = currentRemotePath.startsWith('/') && !currentRemotePath.includes('wwwhome')
                    ? currentRemotePath : '/';
                }

                if (localInitialPath) {
                  await changeLocalDirectory(localInitialPath);
                }
                // Use displayName if provided, otherwise extract from server
                const sessionName = params.displayName || params.server.split(':')[0];
                createSession(
                  sessionName,
                  params,
                  initialPath || '/',  // Use '/' as default, not currentRemotePath from previous session
                  localInitialPath || currentLocalPath
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
                // No existing sessions - just show local file manager
                setShowConnectionScreen(false);
                setActivePanel('local');
                await loadLocalFiles(currentLocalPath || '/');
              }
            }}
          />
        ) : (
          <div className="bg-white dark:bg-gray-800 rounded-xl shadow-xl overflow-hidden">
            {/* Session Tabs - visible when there are sessions or cloud is enabled */}
            {(sessions.length > 0 || isCloudActive) && (
              <SessionTabs
                sessions={sessions}
                activeSessionId={activeSessionId}
                onTabClick={switchSession}
                onTabClose={closeSession}
                onNewTab={handleNewTabFromSavedServer}
                cloudTab={isCloudActive ? {
                  enabled: true,
                  syncing: cloudSyncing,
                  active: isCloudActive,
                  serverName: cloudServerName || 'AeroCloud'
                } : undefined}
                onCloudTabClick={handleCloudTabClick}
                onReorder={setSessions}
              />
            )}
            {/* Toolbar */}
            <div className="flex items-center justify-between p-3 bg-gray-50 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600">
              <div className="flex gap-2">
                <button onClick={() => activePanel === 'remote' ? changeRemoteDirectory('..') : changeLocalDirectory(currentLocalPath.split('/').slice(0, -1).join('/') || '/')} className="px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5">
                  <FolderUp size={16} /> {t('common.up')}
                </button>
                <button onClick={() => activePanel === 'remote' ? loadRemoteFiles() : loadLocalFiles(currentLocalPath)} className="group px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5 transition-all hover:scale-105 hover:shadow-md">
                  <RefreshCw size={16} className="group-hover:rotate-180 transition-transform duration-500" /> {t('common.refresh')}
                </button>
                <button onClick={() => createFolder(activePanel === 'remote')} className="group px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5 transition-all hover:scale-105 hover:shadow-md">
                  <FolderPlus size={16} className="group-hover:scale-110 transition-transform" /> {t('common.new')}
                </button>
                {activePanel === 'local' && (
                  <button onClick={() => openInFileManager(currentLocalPath)} className="px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5">
                    <FolderOpen size={16} /> {t('common.open')}
                  </button>
                )}
                {/* View Mode Toggle */}
                <button
                  onClick={() => setViewMode(viewMode === 'list' ? 'grid' : 'list')}
                  className="px-3 py-1.5 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 rounded-lg text-sm flex items-center gap-1.5"
                  title={viewMode === 'list' ? 'Switch to Grid View' : 'Switch to List View'}
                >
                  {viewMode === 'list' ? <LayoutGrid size={16} /> : <List size={16} />}
                </button>
                {/* Upload / Download dynamic button */}
                {isConnected && showRemotePanel && (
                  <button
                    onClick={() => activePanel === 'local' ? uploadMultipleFiles() : downloadMultipleFiles()}
                    disabled={(activePanel === 'local' ? selectedLocalFiles.size : selectedRemoteFiles.size) === 0}
                    className={`relative px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-all ${
                      (activePanel === 'local' ? selectedLocalFiles.size : selectedRemoteFiles.size) > 0
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
                  className={`relative px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-all ${
                    (activePanel === 'remote' ? selectedRemoteFiles.size : selectedLocalFiles.size) > 0
                      ? 'bg-red-500 hover:bg-red-600 text-white shadow-sm hover:shadow-md'
                      : 'bg-gray-200 dark:bg-gray-600 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                  }`}
                  title={t('contextMenu.delete')}
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
                      disabled={!activeTransfer && !hasQueueActivity}
                      className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-all ${
                        activeTransfer || hasQueueActivity
                          ? 'bg-red-500 hover:bg-red-600 text-white shadow-sm hover:shadow-md animate-pulse'
                          : 'bg-gray-200 dark:bg-gray-600 text-gray-400 dark:text-gray-500 cursor-not-allowed'
                      }`}
                      title={t('transfer.cancelAll')}
                    >
                      <XCircle size={16} />
                    </button>
                    <button
                      onClick={toggleSyncNavigation}
                      className={`px-3 py-1.5 rounded-lg text-sm flex items-center gap-1.5 transition-colors ${isSyncNavigation
                        ? 'bg-purple-500 hover:bg-purple-600 text-white'
                        : 'bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500'
                        }`}
                      title={isSyncNavigation ? t('common.synced') : t('common.sync')}
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
                  </>
                )}
              </div>
              <div className="flex gap-2">
                {isConnected && showRemotePanel && (
                  <>
                    <button onClick={() => setActivePanel('remote')} className={`px-4 py-1.5 rounded-lg text-sm flex items-center gap-1.5 ${activePanel === 'remote' ? 'bg-blue-500 text-white' : 'bg-gray-200 dark:bg-gray-600'}`}>
                      <Globe size={16} /> {t('browser.remote')}
                    </button>
                    <button onClick={() => setActivePanel('local')} className={`px-4 py-1.5 rounded-lg text-sm flex items-center gap-1.5 ${activePanel === 'local' ? 'bg-blue-500 text-white' : 'bg-gray-200 dark:bg-gray-600'}`}>
                      <HardDrive size={16} /> {t('browser.local')}
                    </button>
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
            <div className="flex h-[calc(100vh-220px)]">
              {/* Remote — hidden when not connected or local-only mode */}
              {isConnected && showRemotePanel && <div
                className={`w-1/2 border-r border-gray-200 dark:border-gray-700 flex flex-col transition-all duration-150 ${crossPanelTarget === 'remote' ? 'ring-2 ring-inset ring-blue-400 bg-blue-50/30 dark:bg-blue-900/10' : ''}`}
                onDragOver={(e) => handlePanelDragOver(e, true)}
                onDrop={(e) => handlePanelDrop(e, true)}
                onDragLeave={handlePanelDragLeave}
              >
                <div className="px-3 py-1.5 bg-gray-100 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600 text-sm font-medium flex items-center gap-2">
                  <div className="flex-1 flex items-center bg-white dark:bg-gray-800 rounded-md border border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500 focus-within:border-blue-500 dark:focus-within:border-blue-400 focus-within:ring-2 focus-within:ring-blue-500/20 transition-all overflow-hidden">
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
                        const iconClass = isSyncNavigation ? 'text-purple-500' : isConnected ? 'text-green-500' : 'text-gray-400';
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
                      value={isConnected ? currentRemotePath : 'Not Connected'}
                      onChange={(e) => setCurrentRemotePath(e.target.value)}
                      onKeyDown={(e) => e.key === 'Enter' && isConnected && changeRemoteDirectory((e.target as HTMLInputElement).value)}
                      disabled={!isConnected}
                      className="flex-1 pl-1 pr-2 py-1 bg-transparent border-none outline-none text-sm cursor-text selection:bg-blue-200 dark:selection:bg-blue-800 disabled:cursor-default disabled:text-gray-400 disabled:bg-gray-50 dark:disabled:bg-gray-900"
                      title={isConnected ? "Click to edit path, Enter to navigate" : "Not connected to server"}
                      placeholder="/path/to/directory"
                    />
                    <button
                      onClick={(e) => {
                        const btn = e.currentTarget;
                        btn.querySelector('svg')?.classList.add('animate-spin');
                        setTimeout(() => btn.querySelector('svg')?.classList.remove('animate-spin'), 600);
                        loadRemoteFiles();
                      }}
                      className="flex-shrink-0 px-2 flex items-center text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
                      title={t('common.refresh')}
                    >
                      <RefreshCw size={13} />
                    </button>
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
                        className={`flex-shrink-0 px-2 flex items-center transition-colors ${remoteSearchResults !== null ? 'text-blue-500' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300'}`}
                        title={remoteSearchResults !== null ? 'Clear search' : 'Search files'}
                      >
                        {remoteSearching ? <RefreshCw size={13} className="animate-spin" /> : <Search size={13} />}
                      </button>
                    )}
                  </div>
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
                    {remoteSearchResults !== null && (
                      <span className="text-xs text-blue-600 dark:text-blue-400 flex-shrink-0">{remoteSearchResults.length} results</span>
                    )}
                    <button
                      onClick={() => { setShowRemoteSearchBar(false); setRemoteSearchQuery(''); setRemoteSearchResults(null); }}
                      className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 flex-shrink-0"
                    >
                      <X size={14} />
                    </button>
                  </div>
                )}
                <div className="flex-1 overflow-auto">
                  {!isConnected ? (
                    <div className="flex flex-col items-center justify-center h-full text-gray-400">
                      <Cloud size={64} className="mb-4 opacity-30" />
                      <p className="text-lg font-medium">Not Connected</p>
                      <p className="text-sm mt-1">Click "Connect" to access remote files</p>
                      <button
                        onClick={() => setShowConnectionScreen(true)}
                        className="mt-4 px-4 py-2 bg-blue-500 hover:bg-blue-600 text-white rounded-lg shadow-sm hover:shadow-md transition-all flex items-center gap-2"
                      >
                        <Cloud size={16} /> Connect to Server
                      </button>
                    </div>
                  ) : viewMode === 'list' ? (
                    <table className="w-full">
                      <thead className="bg-gray-50 dark:bg-gray-700 sticky top-0">
                        <tr>
                          <SortableHeader label="Name" field="name" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} />
                          <SortableHeader label="Size" field="size" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} />
                          <SortableHeader label="Type" field="type" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} className="hidden xl:table-cell" />
                          <th className="px-3 py-2 text-left text-xs font-medium text-gray-500 dark:text-gray-400 uppercase tracking-wider whitespace-nowrap hidden xl:table-cell">Perms</th>
                          <SortableHeader label="Modified" field="modified" currentField={remoteSortField} order={remoteSortOrder} onClick={handleRemoteSort} />
                        </tr>
                      </thead>
                      <tbody className="divide-y divide-gray-100 dark:divide-gray-700">
                        {/* Go Up Row - always visible, disabled at root */}
                        <tr
                          className={`${currentRemotePath !== '/' ? 'hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer' : 'opacity-50 cursor-not-allowed'}`}
                          onClick={() => currentRemotePath !== '/' && changeRemoteDirectory('..')}
                        >
                          <td className="px-4 py-2 flex items-center gap-2 text-gray-500">
                            <FolderUp size={16} />
                            <span className="italic">Go up</span>
                          </td>
                          <td className="px-4 py-2 text-xs text-gray-400">—</td>
                          <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-400">—</td>
                          <td className="hidden xl:table-cell px-4 py-2 text-xs text-gray-400">—</td>
                          <td className="px-4 py-2 text-xs text-gray-400">—</td>
                        </tr>
                        {sortedRemoteFiles.map((file, i) => (
                          <tr
                            key={file.name}
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
                            className={`cursor-pointer transition-colors ${
                              dropTargetPath === file.path && file.is_dir
                                ? 'bg-green-100 dark:bg-green-900/40 ring-2 ring-green-500'
                                : selectedRemoteFiles.has(file.name)
                                  ? 'bg-blue-100 dark:bg-blue-900/40'
                                  : 'hover:bg-blue-50 dark:hover:bg-gray-700'
                            } ${dragData?.sourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                          >
                            <td className="px-4 py-2 flex items-center gap-2">
                              {file.is_dir ? <Folder size={16} className="text-yellow-500" /> : getFileIcon(file.name).icon}
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
                                  {file.name}
                                </span>
                              )}
                              {lockedFiles.has(file.path) && <span title="Locked"><Lock size={12} className="text-orange-500" /></span>}
                              {getSyncBadge(file.path, file.modified || undefined, false)}
                            </td>
                            <td className="px-3 py-2 text-xs text-gray-500 whitespace-nowrap">{file.size ? formatBytes(file.size) : '-'}</td>
                            <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-500 uppercase">{file.is_dir ? 'Folder' : (file.name.includes('.') ? file.name.split('.').pop() : '—')}</td>
                            <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-500 font-mono whitespace-nowrap" title={file.permissions || undefined}>{file.permissions || '-'}</td>
                            <td className="px-3 py-2 text-xs text-gray-500 whitespace-nowrap">{formatDate(file.modified)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  ) : (
                    /* Grid View */
                    <div className="file-grid">
                      {/* Go Up Item - always visible, disabled at root */}
                      <div
                        className={`file-grid-item file-grid-go-up ${currentRemotePath === '/' ? 'opacity-50 cursor-not-allowed' : ''}`}
                        onClick={() => currentRemotePath !== '/' && changeRemoteDirectory('..')}
                      >
                        <div className="file-grid-icon">
                          <FolderUp size={32} className="text-gray-400" />
                        </div>
                        <span className="file-grid-name italic text-gray-500">Go up</span>
                      </div>
                      {sortedRemoteFiles.map((file, i) => (
                        <div
                          key={file.name}
                          draggable={file.name !== '..'}
                          onDragStart={(e) => handleDragStart(e, file, true, selectedRemoteFiles, sortedRemoteFiles)}
                          onDragEnd={handleDragEnd}
                          onDragOver={(e) => handleDragOver(e, file.path, file.is_dir, true)}
                          onDragLeave={handleDragLeave}
                          onDrop={(e) => file.is_dir && handleDrop(e, file.path, true)}
                          className={`file-grid-item ${
                            dropTargetPath === file.path && file.is_dir
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
                              <Folder size={32} className="text-yellow-500" />
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
                              fallbackIcon={getFileIcon(file.name).icon}
                              isRemote={true}
                            />
                          ) : (
                            <div className="file-grid-icon">
                              {getFileIcon(file.name).icon}
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
                              {file.name}
                            </span>
                          )}
                          {!file.is_dir && file.size && (
                            <span className="file-grid-size">{formatBytes(file.size)}</span>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>}

              {/* Local — full width when remote panel is hidden */}
              <div
                className={`${(!isConnected || !showRemotePanel) ? 'flex-1 min-w-0' : 'w-1/2'} flex flex-col transition-all duration-300 ${crossPanelTarget === 'local' ? 'ring-2 ring-inset ring-blue-400 bg-blue-50/30 dark:bg-blue-900/10' : ''}`}
                onDragOver={(e) => handlePanelDragOver(e, false)}
                onDrop={(e) => handlePanelDrop(e, false)}
                onDragLeave={handlePanelDragLeave}
              >
                <div className="px-3 py-1.5 bg-gray-100 dark:bg-gray-700 border-b border-gray-200 dark:border-gray-600 text-sm font-medium flex items-center gap-2">
                  <div className={`flex-1 flex items-center bg-white dark:bg-gray-800 rounded-md border ${!isLocalPathCoherent ? 'border-amber-400 dark:border-amber-500' : 'border-gray-300 dark:border-gray-600 hover:border-blue-400 dark:hover:border-blue-500'} focus-within:border-blue-500 dark:focus-within:border-blue-400 focus-within:ring-2 focus-within:ring-blue-500/20 transition-all overflow-hidden`}>
                    {/* Local icon inside address bar (like Chrome favicon) */}
                    <div
                      className="flex-shrink-0 pl-2.5 pr-1 flex items-center"
                      title={isLocalPathCoherent ? "Local Disk" : "Local path doesn't match the connected server"}
                    >
                      {isLocalPathCoherent ? (
                        <HardDrive size={14} className={isSyncNavigation ? 'text-purple-500' : 'text-blue-500'} />
                      ) : (
                        <AlertTriangle size={14} className="text-amber-500" />
                      )}
                    </div>
                    <input
                      type="text"
                      value={currentLocalPath}
                      onChange={(e) => setCurrentLocalPath(e.target.value)}
                      onKeyDown={(e) => e.key === 'Enter' && changeLocalDirectory((e.target as HTMLInputElement).value)}
                      className={`flex-1 pl-1 pr-2 py-1 bg-transparent border-none outline-none text-sm cursor-text selection:bg-blue-200 dark:selection:bg-blue-800 ${!isLocalPathCoherent ? 'text-amber-600 dark:text-amber-400' : ''}`}
                      title={isLocalPathCoherent ? "Click to edit path, Enter to navigate" : "⚠️ Local path doesn't match the connected server"}
                      placeholder="/path/to/local/directory"
                    />
                    <button
                      onClick={(e) => {
                        const btn = e.currentTarget;
                        btn.querySelector('svg')?.classList.add('animate-spin');
                        setTimeout(() => btn.querySelector('svg')?.classList.remove('animate-spin'), 600);
                        loadLocalFiles(currentLocalPath);
                      }}
                      className="flex-shrink-0 px-2 flex items-center text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
                      title={t('common.refresh')}
                    >
                      <RefreshCw size={13} />
                    </button>
                    <button
                      onClick={() => {
                        if (localSearchFilter) {
                          setLocalSearchFilter('');
                          setShowLocalSearchBar(false);
                        } else {
                          setShowLocalSearchBar(prev => !prev);
                        }
                      }}
                      className={`flex-shrink-0 px-2 flex items-center transition-colors ${localSearchFilter ? 'text-blue-500' : 'text-gray-400 hover:text-gray-600 dark:hover:text-gray-300'}`}
                      title={localSearchFilter ? t('search.clear') || 'Clear search' : t('search.search_files') || 'Search files'}
                    >
                      <Search size={13} />
                    </button>
                  </div>
                </div>
                {showLocalSearchBar && (
                  <div className="px-3 py-1.5 bg-blue-50 dark:bg-blue-900/20 border-b border-blue-200 dark:border-blue-800 flex items-center gap-2">
                    <Search size={14} className="text-blue-500 flex-shrink-0" />
                    <input
                      autoFocus
                      ref={localSearchRef}
                      type="text"
                      placeholder={t('search.local_placeholder') || 'Filter local files...'}
                      value={localSearchFilter}
                      onChange={e => setLocalSearchFilter(e.target.value)}
                      onKeyDown={e => {
                        if (e.key === 'Escape') {
                          setShowLocalSearchBar(false);
                          setLocalSearchFilter('');
                        }
                      }}
                      className="flex-1 text-sm bg-transparent border-none outline-none placeholder-gray-400"
                    />
                    {localSearchFilter && (
                      <span className="text-xs text-blue-600 dark:text-blue-400 flex-shrink-0">
                        {localFiles.filter(f => f.name.toLowerCase().includes(localSearchFilter.toLowerCase())).length} results
                      </span>
                    )}
                    <button
                      onClick={() => { setShowLocalSearchBar(false); setLocalSearchFilter(''); }}
                      className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 flex-shrink-0"
                    >
                      <X size={14} />
                    </button>
                  </div>
                )}
                <div className="flex-1 overflow-auto">
                  {viewMode === 'list' ? (
                    <table className="w-full">
                      <thead className="bg-gray-50 dark:bg-gray-700 sticky top-0">
                        <tr>
                          <SortableHeader label="Name" field="name" currentField={localSortField} order={localSortOrder} onClick={handleLocalSort} />
                          <SortableHeader label="Size" field="size" currentField={localSortField} order={localSortOrder} onClick={handleLocalSort} />
                          <SortableHeader label="Type" field="type" currentField={localSortField} order={localSortOrder} onClick={handleLocalSort} className="hidden xl:table-cell" />
                          <SortableHeader label="Modified" field="modified" currentField={localSortField} order={localSortOrder} onClick={handleLocalSort} />
                        </tr>
                      </thead>
                      <tbody className="divide-y divide-gray-100 dark:divide-gray-700">
                        {/* Go Up Row - always visible, disabled at root */}
                        <tr
                          className={`${currentLocalPath !== '/' ? 'hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer' : 'opacity-50 cursor-not-allowed'}`}
                          onClick={() => currentLocalPath !== '/' && changeLocalDirectory(currentLocalPath.split('/').slice(0, -1).join('/') || '/')}
                        >
                          <td className="px-4 py-2 flex items-center gap-2 text-gray-500">
                            <FolderUp size={16} />
                            <span className="italic">Go up</span>
                          </td>
                          <td className="px-4 py-2 text-sm text-gray-400">—</td>
                          <td className="hidden xl:table-cell px-3 py-2 text-sm text-gray-400">—</td>
                          <td className="px-4 py-2 text-sm text-gray-400">—</td>
                        </tr>
                        {sortedLocalFiles.map((file, i) => (
                          <tr
                            key={file.name}
                            draggable={file.name !== '..'}
                            onDragStart={(e) => handleDragStart(e, file, false, selectedLocalFiles, sortedLocalFiles)}
                            onDragEnd={handleDragEnd}
                            onDragOver={(e) => handleDragOver(e, file.path, file.is_dir, false)}
                            onDragLeave={handleDragLeave}
                            onDrop={(e) => file.is_dir && handleDrop(e, file.path, false)}
                            onClick={(e) => {
                              if (file.name === '..') return;
                              setActivePanel('local');
                              if (e.shiftKey && lastSelectedLocalIndex !== null) {
                                const start = Math.min(lastSelectedLocalIndex, i);
                                const end = Math.max(lastSelectedLocalIndex, i);
                                const rangeNames = sortedLocalFiles.slice(start, end + 1).map(f => f.name);
                                setSelectedLocalFiles(new Set(rangeNames));
                              } else if (e.ctrlKey || e.metaKey) {
                                setSelectedLocalFiles(prev => {
                                  const next = new Set(prev);
                                  if (next.has(file.name)) next.delete(file.name);
                                  else next.add(file.name);
                                  return next;
                                });
                                setLastSelectedLocalIndex(i);
                              } else {
                                if (selectedLocalFiles.size === 1 && selectedLocalFiles.has(file.name)) {
                                  setSelectedLocalFiles(new Set());
                                  setPreviewFile(null);
                                } else {
                                  setSelectedLocalFiles(new Set([file.name]));
                                  setPreviewFile(file);
                                }
                                setLastSelectedLocalIndex(i);
                              }
                            }}
                            onDoubleClick={() => {
                              if (file.is_dir) {
                                changeLocalDirectory(file.path);
                              } else {
                                // Respect double-click action setting
                                if (doubleClickAction === 'preview') {
                                  const category = getPreviewCategory(file.name);
                                  if (['image', 'audio', 'video', 'pdf', 'markdown', 'text'].includes(category)) {
                                    openUniversalPreview(file, false);
                                  } else if (isPreviewable(file.name)) {
                                    openDevToolsPreview(file, false);
                                  }
                                } else {
                                  if (isConnected) {
                                    uploadFile(file.path, file.name, false);
                                  } else {
                                    openInFileManager(file.path);
                                  }
                                }
                              }
                            }}
                            onContextMenu={(e: React.MouseEvent) => showLocalContextMenu(e, file)}
                            className={`cursor-pointer transition-colors ${
                              dropTargetPath === file.path && file.is_dir
                                ? 'bg-green-100 dark:bg-green-900/40 ring-2 ring-green-500'
                                : selectedLocalFiles.has(file.name)
                                  ? 'bg-blue-100 dark:bg-blue-900/40'
                                  : 'hover:bg-blue-50 dark:hover:bg-gray-700'
                            } ${dragData?.sourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                          >
                            <td className="px-4 py-2 flex items-center gap-2">
                              {file.is_dir ? <Folder size={16} className="text-yellow-500" /> : getFileIcon(file.name).icon}
                              {inlineRename?.path === file.path && !inlineRename?.isRemote ? (
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
                                    if (selectedLocalFiles.size === 1 && selectedLocalFiles.has(file.name) && file.name !== '..') {
                                      e.stopPropagation();
                                      startInlineRename(file.path, file.name, false);
                                    }
                                  }}
                                >
                                  {file.name}
                                </span>
                              )}
                              {getSyncBadge(file.path, file.modified || undefined, true)}
                            </td>
                            <td className="px-4 py-2 text-sm text-gray-500">{file.size !== null ? formatBytes(file.size) : '-'}</td>
                            <td className="hidden xl:table-cell px-3 py-2 text-xs text-gray-500 uppercase">{file.is_dir ? 'Folder' : (file.name.includes('.') ? file.name.split('.').pop() : '—')}</td>
                            <td className="px-4 py-2 text-xs text-gray-500 whitespace-nowrap">{formatDate(file.modified)}</td>
                          </tr>
                        ))}
                      </tbody>
                    </table>
                  ) : (
                    /* Grid View */
                    <div className="file-grid">
                      {/* Go Up Item - always visible, disabled at root */}
                      <div
                        className={`file-grid-item file-grid-go-up ${currentLocalPath === '/' ? 'opacity-50 cursor-not-allowed' : ''}`}
                        onClick={() => currentLocalPath !== '/' && changeLocalDirectory(currentLocalPath.split('/').slice(0, -1).join('/') || '/')}
                      >
                        <div className="file-grid-icon">
                          <FolderUp size={32} className="text-gray-400" />
                        </div>
                        <span className="file-grid-name italic text-gray-500">Go up</span>
                      </div>
                      {sortedLocalFiles.map((file, i) => (
                        <div
                          key={file.name}
                          draggable={file.name !== '..'}
                          onDragStart={(e) => handleDragStart(e, file, false, selectedLocalFiles, sortedLocalFiles)}
                          onDragEnd={handleDragEnd}
                          onDragOver={(e) => handleDragOver(e, file.path, file.is_dir, false)}
                          onDragLeave={handleDragLeave}
                          onDrop={(e) => file.is_dir && handleDrop(e, file.path, false)}
                          className={`file-grid-item ${
                            dropTargetPath === file.path && file.is_dir
                              ? 'ring-2 ring-green-500 bg-green-100 dark:bg-green-900/40'
                              : selectedLocalFiles.has(file.name) ? 'selected' : ''
                          } ${dragData?.sourcePaths.includes(file.path) ? 'opacity-50' : ''}`}
                          onClick={(e) => {
                            if (file.name === '..') return;
                            setActivePanel('local');
                            if (e.shiftKey && lastSelectedLocalIndex !== null) {
                              const start = Math.min(lastSelectedLocalIndex, i);
                              const end = Math.max(lastSelectedLocalIndex, i);
                              const rangeNames = sortedLocalFiles.slice(start, end + 1).map(f => f.name);
                              setSelectedLocalFiles(new Set(rangeNames));
                            } else if (e.ctrlKey || e.metaKey) {
                              setSelectedLocalFiles(prev => {
                                const next = new Set(prev);
                                if (next.has(file.name)) next.delete(file.name);
                                else next.add(file.name);
                                return next;
                              });
                              setLastSelectedLocalIndex(i);
                            } else {
                              if (selectedLocalFiles.size === 1 && selectedLocalFiles.has(file.name)) {
                                setSelectedLocalFiles(new Set());
                                setPreviewFile(null);
                              } else {
                                setSelectedLocalFiles(new Set([file.name]));
                                setPreviewFile(file);
                              }
                              setLastSelectedLocalIndex(i);
                            }
                          }}
                          onDoubleClick={() => {
                            if (file.is_dir) {
                              changeLocalDirectory(file.path);
                            } else {
                              // Respect double-click action setting
                              if (doubleClickAction === 'preview') {
                                const category = getPreviewCategory(file.name);
                                if (['image', 'audio', 'video', 'pdf', 'markdown', 'text'].includes(category)) {
                                  openUniversalPreview(file, false);
                                } else if (isPreviewable(file.name)) {
                                  openDevToolsPreview(file, false);
                                }
                              } else {
                                if (isConnected) {
                                  uploadFile(file.path, file.name, false);
                                } else {
                                  openInFileManager(file.path);
                                }
                              }
                            }
                          }}
                          onContextMenu={(e: React.MouseEvent) => showLocalContextMenu(e, file)}
                        >
                          {file.is_dir ? (
                            <div className="file-grid-icon">
                              <Folder size={32} className="text-yellow-500" />
                            </div>
                          ) : isImageFile(file.name) ? (
                            <ImageThumbnail
                              path={file.path}
                              name={file.name}
                              fallbackIcon={getFileIcon(file.name).icon}
                            />
                          ) : (
                            <div className="file-grid-icon">
                              {getFileIcon(file.name).icon}
                            </div>
                          )}
                          {inlineRename?.path === file.path && !inlineRename?.isRemote ? (
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
                                if (selectedLocalFiles.size === 1 && selectedLocalFiles.has(file.name) && file.name !== '..') {
                                  e.stopPropagation();
                                  startInlineRename(file.path, file.name, false);
                                }
                              }}
                            >
                              {file.name}
                            </span>
                          )}
                          {!file.is_dir && file.size !== null && (
                            <span className="file-grid-size">{formatBytes(file.size)}</span>
                          )}
                        </div>
                      ))}
                    </div>
                  )}
                </div>
              </div>

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
                    <Eye size={14} className="text-blue-500" /> File Info
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
                              <span className="text-xs">Loading...</span>
                            </div>
                          ) : /\.(mp4|webm|mov|avi|mkv)$/i.test(previewFile.name) ? (
                            <Video size={48} className="text-purple-500" />
                          ) : /\.(mp3|wav|ogg|flac|m4a|aac)$/i.test(previewFile.name) ? (
                            <Music size={48} className="text-green-500" />
                          ) : /\.pdf$/i.test(previewFile.name) ? (
                            <FileText size={48} className="text-red-500" />
                          ) : /\.(js|jsx|ts|tsx|py|rs|go|java|php|rb|c|cpp|h|css|scss|html|xml|json|yaml|yml|toml|sql|sh|bash)$/i.test(previewFile.name) ? (
                            <Code size={48} className="text-cyan-400" />
                          ) : /\.(zip|tar|gz|rar|7z)$/i.test(previewFile.name) ? (
                            <Archive size={48} className="text-yellow-500" />
                          ) : previewFile.is_dir ? (
                            <Folder size={48} className="text-yellow-400" />
                          ) : (
                            <FileText size={48} className="text-gray-400" />
                          )}
                        </div>

                        {/* File Name */}
                        <div className="text-center">
                          <p className="font-medium text-sm truncate" title={previewFile.name}>{previewFile.name}</p>
                          <p className="text-xs text-gray-500 dark:text-gray-400 uppercase tracking-wide">
                            {previewFile.is_dir ? 'Folder' : previewFile.name.split('.').pop() || 'File'}
                          </p>
                        </div>

                        {/* Detailed Info */}
                        <div className="bg-gray-100 dark:bg-gray-700/50 rounded-lg p-3 space-y-2 text-xs">
                          {/* Size */}
                          {!previewFile.is_dir && (
                            <div className="flex items-center justify-between">
                              <span className="text-gray-500 flex items-center gap-1.5">
                                <HardDrive size={12} /> Size
                              </span>
                              <span className="font-medium">{formatBytes(previewFile.size || 0)}</span>
                            </div>
                          )}

                          {/* Type */}
                          <div className="flex items-center justify-between">
                            <span className="text-gray-500 flex items-center gap-1.5">
                              <FileType size={12} /> Type
                            </span>
                            <span className="font-medium">
                              {previewFile.is_dir ? 'Directory' : (() => {
                                const ext = previewFile.name.split('.').pop()?.toLowerCase();
                                if (/^(jpg|jpeg|png|gif|svg|webp|bmp)$/.test(ext || '')) return 'Image';
                                if (/^(mp4|webm|mov|avi|mkv)$/.test(ext || '')) return 'Video';
                                if (/^(mp3|wav|ogg|flac|m4a|aac)$/.test(ext || '')) return 'Audio';
                                if (ext === 'pdf') return 'PDF';
                                if (/^(js|jsx|ts|tsx|py|rs|go|java|php|rb|c|cpp|h|css|scss|html|xml|json|yaml|yml|toml|sql|sh|bash)$/.test(ext || '')) return 'Code';
                                if (/^(zip|tar|gz|rar|7z)$/.test(ext || '')) return 'Archive';
                                if (/^(txt|md|log|csv)$/.test(ext || '')) return 'Text';
                                return ext?.toUpperCase() || 'File';
                              })()}
                            </span>
                          </div>

                          {/* Image Resolution */}
                          {previewImageDimensions && (
                            <div className="flex items-center justify-between">
                              <span className="text-gray-500 flex items-center gap-1.5">
                                <Image size={12} /> Resolution
                              </span>
                              <span className="font-medium">{previewImageDimensions.width} × {previewImageDimensions.height}</span>
                            </div>
                          )}

                          {/* Modified */}
                          <div className="flex items-center justify-between">
                            <span className="text-gray-500 flex items-center gap-1.5">
                              <Clock size={12} /> Modified
                            </span>
                            <span className="font-medium text-right">{previewFile.modified || '—'}</span>
                          </div>

                          {/* Extension */}
                          {!previewFile.is_dir && (
                            <div className="flex items-center justify-between">
                              <span className="text-gray-500 flex items-center gap-1.5">
                                <Database size={12} /> Extension
                              </span>
                              <span className="font-mono text-xs px-1.5 py-0.5 bg-gray-200 dark:bg-gray-600 rounded">
                                .{previewFile.name.split('.').pop()?.toLowerCase() || '—'}
                              </span>
                            </div>
                          )}

                          {/* Path */}
                          <div className="flex items-start justify-between gap-2">
                            <span className="text-gray-500 flex items-center gap-1.5 shrink-0">
                              <FolderOpen size={12} /> Path
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
                              <Eye size={14} /> Open Preview
                            </button>
                          )}

                          {/* View Source - for text/code files */}
                          {/\.(js|jsx|ts|tsx|py|rs|go|java|php|rb|c|cpp|h|css|scss|html|xml|json|yaml|yml|toml|sql|sh|bash|txt|md|log)$/i.test(previewFile.name) && (
                            <button
                              onClick={() => openUniversalPreview(previewFile, false)}
                              className="w-full px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 text-xs rounded-lg flex items-center justify-center gap-2 transition-colors"
                            >
                              <Code size={14} /> View Source
                            </button>
                          )}

                          {/* Copy Path */}
                          <button
                            onClick={() => {
                              navigator.clipboard.writeText(previewFile.path);
                              notify.success('Copied', 'Path copied to clipboard');
                            }}
                            className="w-full px-3 py-2 bg-gray-200 dark:bg-gray-600 hover:bg-gray-300 dark:hover:bg-gray-500 text-xs rounded-lg flex items-center justify-center gap-2 transition-colors"
                          >
                            <Copy size={14} /> Copy Path
                          </button>
                        </div>
                      </div>
                    ) : (
                      <div className="h-full flex flex-col items-center justify-center text-gray-400 text-sm">
                        <Eye size={32} className="mb-3 opacity-30" />
                        <p className="font-medium">No file selected</p>
                        <p className="text-xs mt-1 text-center">Click on a file to view its details</p>
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
        providerType={connectionParams.protocol}
        isConnected={isConnected}
        selectedFiles={Array.from(selectedRemoteFiles)}
        serverHost={connectionParams.server}
        serverPort={connectionParams.port}
        serverUser={connectionParams.username}
        sshConnection={isConnected && connectionParams.protocol === 'sftp' ? {
          host: connectionParams.server.split(':')[0],
          port: connectionParams.port || 22,
          username: connectionParams.username,
          password: connectionParams.password || undefined,
          privateKeyPath: connectionParams.options?.private_key_path,
          keyPassphrase: connectionParams.options?.key_passphrase,
        } : null}
        onSaveFile={async (content, file) => {
          const logId = humanLog.logStart('UPLOAD', { filename: file.name, size: formatBytes(content.length) });
          try {
            if (file.isRemote) {
              await invoke('save_remote_file', { path: file.path, content });
              humanLog.logSuccess('UPLOAD', { filename: file.name, size: formatBytes(content.length) }, logId);
              notify.success('File Saved', `${file.name} saved to server`);
              await loadRemoteFiles();
            } else {
              await invoke('save_local_file', { path: file.path, content });
              humanLog.logRaw('activity.upload_success', 'INFO', { filename: file.name, size: formatBytes(content.length), location: 'Local', time: '' }, 'success');
              notify.success('File Saved', `${file.name} saved locally`);
              await loadLocalFiles(currentLocalPath);
            }
          } catch (error) {
            humanLog.logError('UPLOAD', { filename: file.name }, logId);
            notify.error('Save Failed', String(error));
          }
        }}
      />

      {showStatusBar && (
        <StatusBar
          isConnected={isConnected}
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
            // For FTP/FTPS/SFTP/etc, show username@server or session name
            const server = connectionParams.server || activeSession?.connectionParams?.server;
            const username = connectionParams.username || activeSession?.connectionParams?.username;
            return server ? `${username}@${server}` : activeSession?.serverName;
          })() : undefined}
          remotePath={currentRemotePath}
          localPath={currentLocalPath}
          remoteFileCount={remoteFiles.length}
          localFileCount={localFiles.length}
          activePanel={activePanel}
          devToolsOpen={devToolsOpen}
          onToggleDevTools={() => setDevToolsOpen(!devToolsOpen)}
          onToggleSync={() => setShowSyncPanel(true)}
          onToggleCloud={() => setShowCloudPanel(true)}
          cloudEnabled={isCloudActive}
          cloudSyncing={cloudSyncing}
          transferQueueActive={transferQueue.hasActiveTransfers}
          transferQueueCount={transferQueue.items.length}
          onToggleTransferQueue={transferQueue.toggle}
          showActivityLog={showActivityLog}
          activityLogCount={activityLog.entries.length}
          onToggleActivityLog={() => setShowActivityLog(!showActivityLog)}
          updateAvailable={updateAvailable}
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
        />
      )}

      {/* Dependencies Panel */}
      <DependenciesPanel
        isVisible={showDependenciesPanel}
        onClose={() => setShowDependenciesPanel(false)}
      />
    </div>
  );
};

export default App;