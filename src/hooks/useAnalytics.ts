/**
 * Analytics hook for privacy-first telemetry with Aptabase
 *
 * Features:
 * - Opt-in only (respects user preference)
 * - No PII collected
 * - EU data residency
 *
 * Events tracked:
 * - app_started: App launch (version, language, OS)
 * - connection_success: Protocol type only (no server info)
 * - transfer_completed: Direction and size range only
 * - feature_used: Which features are popular
 *
 * NOTE: Aptabase plugin temporarily disabled (v1.3.0) due to Tokio runtime issue.
 * Events are logged to console in dev mode. Will be enabled in v1.3.1.
 */

// Aptabase plugin disabled - use stub that logs to console in dev mode
// eslint-disable-next-line @typescript-eslint/no-unused-vars
const trackEvent = async (_name: string, _props?: Record<string, string | number>): Promise<void> => {
  // TODO: Enable when Aptabase Tokio runtime issue is resolved (v1.3.1)
  // import { trackEvent } from '@aptabase/tauri';
  // For now, silently no-op (events are ignored)
};

// Check if analytics is enabled
const isAnalyticsEnabled = (): boolean => {
  const saved = localStorage.getItem('analytics_enabled');
  return saved === 'true';
};

// Size ranges for transfer events (no exact sizes to preserve privacy)
const getSizeRange = (bytes: number): string => {
  if (bytes < 1024) return 'tiny'; // < 1KB
  if (bytes < 1024 * 1024) return 'small'; // < 1MB
  if (bytes < 100 * 1024 * 1024) return 'medium'; // < 100MB
  if (bytes < 1024 * 1024 * 1024) return 'large'; // < 1GB
  return 'huge'; // >= 1GB
};

// Track app started event
export const trackAppStarted = async (version: string, language: string): Promise<void> => {
  if (!isAnalyticsEnabled()) return;

  try {
    await trackEvent('app_started', {
      version,
      language,
      platform: navigator.platform || 'unknown'
    });
  } catch {
    // Silently fail - analytics should never break the app
  }
};

// Track successful connection (protocol type only, no server info)
export const trackConnectionSuccess = async (protocol: string): Promise<void> => {
  if (!isAnalyticsEnabled()) return;

  try {
    await trackEvent('connection_success', {
      protocol: protocol.toLowerCase() // ftp, sftp, webdav, gdrive, dropbox, onedrive, mega
    });
  } catch {
    // Silently fail
  }
};

// Track completed transfer (direction and size range only)
export const trackTransferCompleted = async (direction: 'upload' | 'download', bytes: number): Promise<void> => {
  if (!isAnalyticsEnabled()) return;

  try {
    await trackEvent('transfer_completed', {
      direction,
      size_range: getSizeRange(bytes)
    });
  } catch {
    // Silently fail
  }
};

// Track feature usage (helps prioritize development)
export const trackFeatureUsed = async (feature: string): Promise<void> => {
  if (!isAnalyticsEnabled()) return;

  try {
    await trackEvent('feature_used', {
      feature
    });
  } catch {
    // Silently fail
  }
};

// Feature names (use these for consistency)
export const Features = {
  COMPRESS_ZIP: 'compress_zip',
  COMPRESS_7Z: 'compress_7z',
  EXTRACT_ZIP: 'extract_zip',
  EXTRACT_7Z: 'extract_7z',
  TERMINAL: 'terminal',
  QUICK_CONNECT: 'quick_connect',
  SYNC_FOLDERS: 'sync_folders',
  AEROCLOUD: 'aerocloud',
  DUAL_PANE: 'dual_pane',
  BOOKMARKS: 'bookmarks',
  SHARE_LINK: 'share_link',
  SEARCH: 'search',
  PREVIEW_FILE: 'preview_file',
  EDIT_FILE: 'edit_file'
} as const;

// Hook for components
export const useAnalytics = () => {
  return {
    trackAppStarted,
    trackConnectionSuccess,
    trackTransferCompleted,
    trackFeatureUsed,
    Features
  };
};

export default useAnalytics;
