/**
 * Hooks barrel export
 * Re-exports all custom hooks for easy importing
 */

// Theme and UI
export { useTheme, ThemeToggle } from './useTheme';
export type { Theme } from './useTheme';

// Keyboard
export { useKeyboardShortcuts } from './useKeyboardShortcuts';

// FTP Operations (Legacy)
export { useFtpConnection } from './useFtpConnection';
export { useFileTransfer } from './useFileTransfer';
export { useFileBrowser } from './useFileBrowser';
export { useSessionManager } from './useSessionManager';

// File Operations (v1.3.0 - Unified with Provider support)
export { useFileOperations } from './useFileOperations';
export { useTransferOperations } from './useTransferOperations';
export { useDragAndDrop } from './useDragAndDrop';

// Activity Log
export { useActivityLog, ActivityLogProvider } from './useActivityLog';
export { useHumanizedLog } from './useHumanizedLog';
export type { HumanizedLogParams, HumanizedOperationType } from './useHumanizedLog';

// Analytics (privacy-first, opt-in only)
export {
  useAnalytics,
  trackAppStarted,
  trackConnectionSuccess,
  trackTransferCompleted,
  trackFeatureUsed,
  Features
} from './useAnalytics';

// Re-export types for convenience
export type { SortField, SortOrder } from './useFileBrowser';
