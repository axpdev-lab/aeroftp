/**
 * Hooks barrel export
 * Re-exports all custom hooks for easy importing
 */

// Theme and UI
export { useTheme, ThemeToggle } from './useTheme';
export type { Theme } from './useTheme';

// Keyboard
export { useKeyboardShortcuts } from './useKeyboardShortcuts';

// Drag & Drop
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

// Modularized hooks (extracted from App.tsx)
export { useSettings } from './useSettings';
export { useAutoUpdate } from './useAutoUpdate';
export type { UpdateInfo } from './useAutoUpdate';
export { usePreview } from './usePreview';
export { useOverwriteCheck } from './useOverwriteCheck';
export { useTransferEvents } from './useTransferEvents';
export { useCloudSync } from './useCloudSync';

// Component-specific hooks (used by individual components, not App.tsx)
export { useOAuth2 } from './useOAuth2';
export { useTraySync } from './useTraySync';
