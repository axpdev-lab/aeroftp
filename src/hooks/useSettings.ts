// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * useSettings Hook
 * Extracted from App.tsx during modularization (v1.3.1)
 *
 * Manages all application settings persisted in localStorage under 'aeroftp_settings'.
 * Provides live reload via 'storage' and custom 'aeroftp-settings-changed' events.
 *
 * Used by: App.tsx (main consumer), SettingsPanel (writes to localStorage)
 * Dependencies: invoke('toggle_menu_bar') for native menu bar visibility
 *
 * Returns: All settings as individual state values + their setters + SETTINGS_KEY constant
 */

import { useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { secureGetWithFallback, secureStoreAndClean } from '../utils/secureStorage';
import {
  DEFAULT_SFTP_DOWNLOAD_PRESET,
  normalizeSftpDownloadPreset,
  type SftpDownloadPreset,
} from '../utils/sftpDownloadPresets';

const SETTINGS_KEY = 'aeroftp_settings';
const SETTINGS_VAULT_KEY = 'app_settings';

export const MIN_APP_FONT_SIZE = 10;
export const MAX_APP_FONT_SIZE = 22;
export const DEFAULT_APP_FONT_FAMILY = "'Inter', system-ui, sans-serif";
export const MIN_INTRO_HUB_ICON_SIZE = 18;
export const MAX_INTRO_HUB_ICON_SIZE = 32;
export const DEFAULT_INTRO_HUB_ICON_SIZE = 24;

const LEGACY_FONT_SIZE_MAP: Record<string, number> = {
  small: 13,
  medium: 16,
  large: 18,
};

export const clampAppFontSize = (value: unknown): number => {
  const normalized = typeof value === 'string'
    ? LEGACY_FONT_SIZE_MAP[value] ?? Number(value)
    : Number(value);

  if (!Number.isFinite(normalized)) {
    return LEGACY_FONT_SIZE_MAP.medium;
  }

  return Math.min(MAX_APP_FONT_SIZE, Math.max(MIN_APP_FONT_SIZE, Math.round(normalized)));
};

export const normalizeAppFontFamily = (value: unknown): string => {
  return typeof value === 'string' && value.trim() ? value.trim() : DEFAULT_APP_FONT_FAMILY;
};

export const clampIntroHubIconSize = (value: unknown): number => {
  const normalized = Number(value);
  if (!Number.isFinite(normalized)) {
    return DEFAULT_INTRO_HUB_ICON_SIZE;
  }
  return Math.min(MAX_INTRO_HUB_ICON_SIZE, Math.max(MIN_INTRO_HUB_ICON_SIZE, Math.round(normalized)));
};

export interface AppSettings {
  compactMode: boolean;
  showHiddenFiles: boolean;
  showToastNotifications: boolean;
  confirmBeforeDelete: boolean;
  /**
   * Opt-in privacy hint (off by default): inside an active AeroCrypt overlay,
   * flag file/folder entries whose plaintext name also appears at a different
   * path visited this session. Deterministic name encryption means identical
   * names produce identical ciphertext, so an observer of the encrypted store
   * can tell two items share a name. Same tradeoff as rclone-crypt / Cryptomator.
   */
  warnSameNameEncrypted: boolean;
  showStatusBar: boolean;
  /** Show the floating transfer progress card (AeroProgress). Default on. */
  showTransferProgress: boolean;
  defaultLocalPath: string;
  fontSize: number;
  fontFamily: string;
  doubleClickAction: 'preview' | 'download';
  rememberLastFolder: boolean;
  systemMenuVisible: boolean;
  showMenuBar: boolean;
  showActivityLog: boolean;
  showConnectionScreen: boolean;
  debugMode: boolean;
  visibleColumns: string[];
  sortFoldersFirst: boolean;
  showFileExtensions: boolean;
  timeoutSeconds: number;
  maxConcurrentTransfers: number;
  retryCount: number;
  /** Intra-file range parallelism per download. 0 = Auto (backend decides,
   *  today: single-stream). Supported values: 0, 2, 4, 8, 16. */
  downloadSegments: number;
  /** SFTP intra-file tuning preset. The backend owns connection, cutoff, and
   * read-ahead semantics for each persisted ID. */
  sftpDownloadPreset: SftpDownloadPreset;
  fileExistsAction: 'ask' | 'overwrite' | 'skip' | 'rename' | 'resume' | 'overwrite_if_newer' | 'overwrite_if_different' | 'skip_if_identical';
  swapPanels: boolean;
  lastLocalPath?: string;
  showSystemMenu?: boolean;
  disableUpdateChecks: boolean;
  /** Layout density for My Servers / Discover cards. `detailed` shows the
   *  cached storage usage bar; `compact` keeps the legacy minimal card. */
  cardLayout: 'compact' | 'detailed';
  /** Provider logo size in the IntroHub My Servers and Discover cards. */
  introHubIconSize: number;
  /** Probe provider reachability in the Add Services list/All view. Default on. */
  discoverHealthCheck: boolean;
  /** When ON (default), single and folder transfers launch immediately on
   *  drag/drop or click, byte-identical to the legacy behaviour. When OFF,
   *  transfers are parked in the Transfer Queue panel as `staged` so the
   *  user can prune folder trees, reorder, then press Start (or Start all).
   *  Wired by TQ-4; APPENDIX-TRANSFER-QUEUE locked decision 1. */
  autoStartTransfers: boolean;
  /** Glyph used to mark favourited servers in My Servers and the CLI profiles
   *  table. 'star' = ★ (default), 'heart' = ♥ (red in the GUI). Persisted in the
   *  shared `app_settings` vault key so the CLI renders the same marker (#270). */
  favoriteMarker: 'star' | 'heart';
  /** How dates render app-wide (file browser Modified column and everywhere
   *  formatDate is used). 'localized' follows the app language (default), the
   *  rest are fixed language-neutral patterns. Mirrored to
   *  `<html data-date-format>` by App.tsx so the plain formatDate() can read it. */
  dateFormat: 'localized' | 'iso' | 'dmy' | 'mdy';
}

export const ALL_COLUMNS = ['name', 'size', 'type', 'permissions', 'modified'];

const DEFAULTS: AppSettings = {
  compactMode: false,
  showHiddenFiles: true,
  showToastNotifications: false,
  confirmBeforeDelete: true,
  warnSameNameEncrypted: false,
  showStatusBar: true,
  showTransferProgress: true,
  defaultLocalPath: '',
  fontSize: 16,
  fontFamily: DEFAULT_APP_FONT_FAMILY,
  doubleClickAction: 'preview',
  rememberLastFolder: true,
  systemMenuVisible: false,
  showMenuBar: true,
  showActivityLog: false,
  showConnectionScreen: true,
  debugMode: false,
  visibleColumns: ALL_COLUMNS,
  sortFoldersFirst: true,
  showFileExtensions: true,
  timeoutSeconds: 30,
  maxConcurrentTransfers: 5,
  retryCount: 3,
  downloadSegments: 0,
  sftpDownloadPreset: DEFAULT_SFTP_DOWNLOAD_PRESET,
  fileExistsAction: 'ask',
  swapPanels: false,
  disableUpdateChecks: false,
  cardLayout: 'compact',
  introHubIconSize: DEFAULT_INTRO_HUB_ICON_SIZE,
  discoverHealthCheck: true,
  autoStartTransfers: true,
  favoriteMarker: 'star',
  dateFormat: 'localized',
};

export const useSettings = () => {
  const [compactMode, setCompactMode] = useState(DEFAULTS.compactMode);
  const [showHiddenFiles, setShowHiddenFiles] = useState(DEFAULTS.showHiddenFiles);
  const [showToastNotifications, setShowToastNotifications] = useState(DEFAULTS.showToastNotifications);
  const [confirmBeforeDelete, setConfirmBeforeDelete] = useState(DEFAULTS.confirmBeforeDelete);
  const [warnSameNameEncrypted, setWarnSameNameEncrypted] = useState(DEFAULTS.warnSameNameEncrypted);
  const [showStatusBar, setShowStatusBar] = useState(DEFAULTS.showStatusBar);
  const [showTransferProgress, setShowTransferProgress] = useState(DEFAULTS.showTransferProgress);
  const [defaultLocalPath, setDefaultLocalPath] = useState(DEFAULTS.defaultLocalPath);
  const [fontSize, setFontSize] = useState<number>(DEFAULTS.fontSize);
  const [fontFamily, setFontFamily] = useState(DEFAULTS.fontFamily);
  const [doubleClickAction, setDoubleClickAction] = useState<'preview' | 'download'>(DEFAULTS.doubleClickAction);
  const [rememberLastFolder, setRememberLastFolder] = useState(DEFAULTS.rememberLastFolder);
  const [systemMenuVisible, setSystemMenuVisible] = useState(DEFAULTS.systemMenuVisible);
  const [showMenuBar, setShowMenuBar] = useState(DEFAULTS.showMenuBar);
  const [showActivityLog, setShowActivityLog] = useState(DEFAULTS.showActivityLog);
  const [showConnectionScreen, setShowConnectionScreen] = useState(DEFAULTS.showConnectionScreen);
  const [debugMode, setDebugMode] = useState(DEFAULTS.debugMode);
  const [visibleColumns, setVisibleColumns] = useState<string[]>(DEFAULTS.visibleColumns);
  const [sortFoldersFirst, setSortFoldersFirst] = useState(DEFAULTS.sortFoldersFirst);
  const [showFileExtensions, setShowFileExtensions] = useState(DEFAULTS.showFileExtensions);
  const [timeoutSeconds, setTimeoutSeconds] = useState(DEFAULTS.timeoutSeconds);
  const [maxConcurrentTransfers, setMaxConcurrentTransfers] = useState(DEFAULTS.maxConcurrentTransfers);
  const [retryCount, setRetryCount] = useState(DEFAULTS.retryCount);
  const [downloadSegments, setDownloadSegments] = useState(DEFAULTS.downloadSegments);
  const [sftpDownloadPreset, setSftpDownloadPreset] = useState(DEFAULTS.sftpDownloadPreset);
  const [fileExistsAction, setFileExistsAction] = useState<AppSettings['fileExistsAction']>(DEFAULTS.fileExistsAction);
  const [swapPanels, setSwapPanels] = useState(DEFAULTS.swapPanels);
  const [disableUpdateChecks, setDisableUpdateChecks] = useState(DEFAULTS.disableUpdateChecks);
  const [cardLayout, setCardLayout] = useState<AppSettings['cardLayout']>(DEFAULTS.cardLayout);
  const [introHubIconSize, setIntroHubIconSize] = useState<number>(DEFAULTS.introHubIconSize);
  const [discoverHealthCheck, setDiscoverHealthCheck] = useState(DEFAULTS.discoverHealthCheck);
  const [autoStartTransfers, setAutoStartTransfers] = useState(DEFAULTS.autoStartTransfers);
  const [favoriteMarker, setFavoriteMarker] = useState<AppSettings['favoriteMarker']>(DEFAULTS.favoriteMarker);
  const [dateFormat, setDateFormat] = useState<AppSettings['dateFormat']>(DEFAULTS.dateFormat);
  const [showSettingsPanel, setShowSettingsPanel] = useState(false);

  const applySettings = useCallback((parsed: Record<string, unknown>) => {
    if (typeof parsed.compactMode === 'boolean') setCompactMode(parsed.compactMode);
    if (typeof parsed.showHiddenFiles === 'boolean') setShowHiddenFiles(parsed.showHiddenFiles);
    if (typeof parsed.showToastNotifications === 'boolean') setShowToastNotifications(parsed.showToastNotifications);
    if (typeof parsed.confirmBeforeDelete === 'boolean') setConfirmBeforeDelete(parsed.confirmBeforeDelete);
    if (typeof parsed.warnSameNameEncrypted === 'boolean') setWarnSameNameEncrypted(parsed.warnSameNameEncrypted);
    if (typeof parsed.showStatusBar === 'boolean') setShowStatusBar(parsed.showStatusBar);
    if (typeof parsed.showTransferProgress === 'boolean') setShowTransferProgress(parsed.showTransferProgress);
    if (typeof parsed.defaultLocalPath === 'string') setDefaultLocalPath(parsed.defaultLocalPath);
    if (typeof parsed.fontSize === 'number' || typeof parsed.fontSize === 'string') {
      setFontSize(clampAppFontSize(parsed.fontSize));
    }
    if ('fontFamily' in parsed) setFontFamily(normalizeAppFontFamily(parsed.fontFamily));
    if (parsed.doubleClickAction && ['preview', 'download'].includes(parsed.doubleClickAction as string)) {
      setDoubleClickAction(parsed.doubleClickAction as 'preview' | 'download');
    }
    if (typeof parsed.rememberLastFolder === 'boolean') setRememberLastFolder(parsed.rememberLastFolder);
    if (typeof parsed.debugMode === 'boolean') setDebugMode(parsed.debugMode);
    if (Array.isArray(parsed.visibleColumns)) setVisibleColumns(parsed.visibleColumns.filter((c: unknown) => typeof c === 'string' && ALL_COLUMNS.includes(c as string)));
    if (typeof parsed.sortFoldersFirst === 'boolean') setSortFoldersFirst(parsed.sortFoldersFirst);
    if (typeof parsed.showFileExtensions === 'boolean') setShowFileExtensions(parsed.showFileExtensions);
    if (typeof parsed.timeoutSeconds === 'number') setTimeoutSeconds(parsed.timeoutSeconds);
    if (typeof parsed.maxConcurrentTransfers === 'number') setMaxConcurrentTransfers(parsed.maxConcurrentTransfers);
    if (typeof parsed.retryCount === 'number') setRetryCount(parsed.retryCount);
    if (typeof parsed.downloadSegments === 'number' && [0, 2, 4, 8, 16].includes(parsed.downloadSegments)) {
      setDownloadSegments(parsed.downloadSegments);
    }
    if ('sftpDownloadPreset' in parsed) {
      setSftpDownloadPreset(normalizeSftpDownloadPreset(parsed.sftpDownloadPreset));
    }
    if (
      typeof parsed.fileExistsAction === 'string' &&
      ['ask', 'overwrite', 'skip', 'rename', 'resume', 'overwrite_if_newer', 'overwrite_if_different', 'skip_if_identical'].includes(parsed.fileExistsAction)
    ) {
      setFileExistsAction(parsed.fileExistsAction as AppSettings['fileExistsAction']);
    }
    if (typeof parsed.swapPanels === 'boolean') setSwapPanels(parsed.swapPanels);
    if (typeof parsed.disableUpdateChecks === 'boolean') setDisableUpdateChecks(parsed.disableUpdateChecks);
    if (parsed.cardLayout === 'compact' || parsed.cardLayout === 'detailed') {
      setCardLayout(parsed.cardLayout);
    }
    if (typeof parsed.introHubIconSize === 'number' || typeof parsed.introHubIconSize === 'string') {
      setIntroHubIconSize(clampIntroHubIconSize(parsed.introHubIconSize));
    }
    if (typeof parsed.discoverHealthCheck === 'boolean') setDiscoverHealthCheck(parsed.discoverHealthCheck);
    if (typeof parsed.autoStartTransfers === 'boolean') setAutoStartTransfers(parsed.autoStartTransfers);
    if (parsed.favoriteMarker === 'star' || parsed.favoriteMarker === 'heart') {
      setFavoriteMarker(parsed.favoriteMarker);
    }
    if (parsed.dateFormat === 'localized' || parsed.dateFormat === 'iso' || parsed.dateFormat === 'dmy' || parsed.dateFormat === 'mdy') {
      setDateFormat(parsed.dateFormat);
    }
  }, []);

  // Load settings on mount + listen for changes
  useEffect(() => {
    const loadSettings = async () => {
      try {
        const parsed = await secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY);
        if (parsed) {
          applySettings(parsed);

          // System menu visibility
          const showMenu = typeof parsed.showSystemMenu === 'boolean' ? parsed.showSystemMenu : false;
          setSystemMenuVisible(showMenu);
          invoke('toggle_menu_bar', { visible: showMenu });

          // One-way idempotent migration to vault (no-op if already in vault)
          secureStoreAndClean(SETTINGS_VAULT_KEY, SETTINGS_KEY, parsed).catch(() => {});
        } else {
          // No settings saved, apply defaults for system menu
          invoke('toggle_menu_bar', { visible: false });
        }
      } catch (e) {
        console.error('Failed to init settings', e);
      }
    };

    const handleSettingsChange = (e: Event) => {
      // Use inline payload from CustomEvent for immediate sync (no async vault read)
      const detail = (e as CustomEvent)?.detail as Record<string, unknown> | undefined;
      if (detail) {
        applySettings(detail);
        const showMenu = typeof detail.showSystemMenu === 'boolean' ? detail.showSystemMenu : false;
        setSystemMenuVisible(showMenu);
        return;
      }
      // Fallback: re-read from vault (for storage events or legacy callers)
      void (async () => {
        try {
          const parsed = await secureGetWithFallback<Record<string, unknown>>(SETTINGS_VAULT_KEY, SETTINGS_KEY);
          if (parsed) {
            applySettings(parsed);
            const showMenu = typeof parsed.showSystemMenu === 'boolean' ? parsed.showSystemMenu : false;
            setSystemMenuVisible(showMenu);
          }
        } catch { /* ignore */ }
      })();
    };

    void loadSettings();

    window.addEventListener('storage', handleSettingsChange);
    window.addEventListener('aeroftp-settings-changed', handleSettingsChange);
    return () => {
      window.removeEventListener('storage', handleSettingsChange);
      window.removeEventListener('aeroftp-settings-changed', handleSettingsChange);
    };
  }, [applySettings]);

  return {
    // Settings state
    compactMode,
    showHiddenFiles,
    showToastNotifications,
    confirmBeforeDelete,
    warnSameNameEncrypted,
    showStatusBar,
    showTransferProgress,
    defaultLocalPath,
    fontSize,
    fontFamily,
    doubleClickAction,
    rememberLastFolder,
    systemMenuVisible,
    showMenuBar,
    showActivityLog,
    showConnectionScreen,
    debugMode,
    visibleColumns,
    sortFoldersFirst,
    showFileExtensions,
    timeoutSeconds,
    maxConcurrentTransfers,
    retryCount,
    downloadSegments,
    sftpDownloadPreset,
    fileExistsAction,
    swapPanels,
    disableUpdateChecks,
    cardLayout,
    introHubIconSize,
    discoverHealthCheck,
    autoStartTransfers,
    favoriteMarker,
    dateFormat,
    showSettingsPanel,

    // Setters
    setCompactMode,
    setShowHiddenFiles,
    setShowToastNotifications,
    setConfirmBeforeDelete,
    setWarnSameNameEncrypted,
    setShowStatusBar,
    setShowTransferProgress,
    setDefaultLocalPath,
    setFontSize,
    setFontFamily,
    setDoubleClickAction,
    setRememberLastFolder,
    setSystemMenuVisible,
    setShowMenuBar,
    setShowActivityLog,
    setShowConnectionScreen,
    setDebugMode,
    setVisibleColumns,
    setSortFoldersFirst,
    setShowFileExtensions,
    setTimeoutSeconds,
    setMaxConcurrentTransfers,
    setRetryCount,
    setDownloadSegments,
    setSftpDownloadPreset,
    setFileExistsAction,
    setSwapPanels,
    setDisableUpdateChecks,
    setCardLayout,
    setIntroHubIconSize,
    setDiscoverHealthCheck,
    setAutoStartTransfers,
    setFavoriteMarker,
    setDateFormat,
    setShowSettingsPanel,

    // Constants
    SETTINGS_KEY,
    SETTINGS_VAULT_KEY,
  };
};

export default useSettings;
