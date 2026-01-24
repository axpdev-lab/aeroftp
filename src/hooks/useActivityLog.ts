/**
 * Activity Log Hook
 * Centralized logging service for all app operations
 * 
 * Features:
 * - Type-safe operation logging
 * - Auto-cleanup (max 500 entries)
 * - Timestamp and status tracking
 * - Context provider for app-wide access
 */

import React, { useState, useCallback, createContext, useContext, useMemo, ReactNode } from 'react';

// ============================================================================
// Types
// ============================================================================

export type OperationType =
    | 'CONNECT'
    | 'DISCONNECT'
    | 'UPLOAD'
    | 'DOWNLOAD'
    | 'DELETE'
    | 'RENAME'
    | 'MKDIR'
    | 'NAVIGATE'
    | 'ERROR'
    | 'INFO'
    | 'SUCCESS';

export type OperationStatus = 'pending' | 'running' | 'success' | 'error';

export interface LogEntry {
    id: string;
    timestamp: Date;
    operation: OperationType;
    status: OperationStatus;
    message: string;
    details?: string;
}

export interface ActivityLogContextValue {
    /** All log entries */
    entries: LogEntry[];

    /** Add a new log entry */
    log: (operation: OperationType, message: string, status?: OperationStatus, details?: string) => string;

    /** Update an existing log entry */
    updateEntry: (id: string, updates: Partial<Pick<LogEntry, 'status' | 'message' | 'details'>>) => void;

    /** Clear all log entries */
    clear: () => void;

    /** Clear entries by operation type */
    clearByType: (operation: OperationType) => void;

    /** Get count of running operations */
    runningCount: number;

    /** Check if any operation is running */
    hasRunning: boolean;
}

// ============================================================================
// Constants
// ============================================================================

const MAX_ENTRIES = 500;
const ENTRY_ID_PREFIX = 'log_';
let entryCounter = 0;

// ============================================================================
// Context
// ============================================================================

const ActivityLogContext = createContext<ActivityLogContextValue | null>(null);

// ============================================================================
// Hook
// ============================================================================

/**
 * Hook to access the Activity Log context
 * Must be used within an ActivityLogProvider
 */
export function useActivityLog(): ActivityLogContextValue {
    const context = useContext(ActivityLogContext);
    if (!context) {
        throw new Error('useActivityLog must be used within an ActivityLogProvider');
    }
    return context;
}

/**
 * Internal hook for managing the activity log state
 */
function useActivityLogState(): ActivityLogContextValue {
    const [entries, setEntries] = useState<LogEntry[]>([]);

    // Generate unique entry ID
    const generateId = useCallback((): string => {
        entryCounter += 1;
        return `${ENTRY_ID_PREFIX}${Date.now()}_${entryCounter}`;
    }, []);

    // Add a new log entry
    const log = useCallback((
        operation: OperationType,
        message: string,
        status: OperationStatus = 'success',
        details?: string
    ): string => {
        const id = generateId();
        const entry: LogEntry = {
            id,
            timestamp: new Date(),
            operation,
            status,
            message,
            details,
        };

        setEntries(prev => {
            const newEntries = [...prev, entry];
            // Auto-cleanup: keep only the last MAX_ENTRIES
            if (newEntries.length > MAX_ENTRIES) {
                return newEntries.slice(-MAX_ENTRIES);
            }
            return newEntries;
        });

        return id;
    }, [generateId]);

    // Update an existing entry
    const updateEntry = useCallback((
        id: string,
        updates: Partial<Pick<LogEntry, 'status' | 'message' | 'details'>>
    ): void => {
        setEntries(prev => prev.map(entry =>
            entry.id === id
                ? { ...entry, ...updates }
                : entry
        ));
    }, []);

    // Clear all entries
    const clear = useCallback((): void => {
        setEntries([]);
    }, []);

    // Clear entries by operation type
    const clearByType = useCallback((operation: OperationType): void => {
        setEntries(prev => prev.filter(entry => entry.operation !== operation));
    }, []);

    // Computed values
    const runningCount = useMemo(() =>
        entries.filter(e => e.status === 'running' || e.status === 'pending').length,
        [entries]
    );

    const hasRunning = runningCount > 0;

    return {
        entries,
        log,
        updateEntry,
        clear,
        clearByType,
        runningCount,
        hasRunning,
    };
}

// ============================================================================
// Provider Component
// ============================================================================

interface ActivityLogProviderProps {
    children: ReactNode;
}

export function ActivityLogProvider({ children }: ActivityLogProviderProps): React.ReactElement {
    const value = useActivityLogState();

    return React.createElement(
        ActivityLogContext.Provider,
        { value },
        children
    );
}

// ============================================================================
// Utility Functions
// ============================================================================

/**
 * Get the Lucide icon name for an operation type
 * Icons are rendered in ActivityLogPanel component
 */
export function getOperationIcon(operation: OperationType): string {
    const icons: Record<OperationType, string> = {
        CONNECT: 'plug',
        DISCONNECT: 'unplug',
        UPLOAD: 'upload',
        DOWNLOAD: 'download',
        DELETE: 'trash-2',
        RENAME: 'pencil',
        MKDIR: 'folder-plus',
        NAVIGATE: 'folder-open',
        ERROR: 'alert-circle',
        INFO: 'info',
        SUCCESS: 'check-circle',
    };
    return icons[operation];
}

/**
 * Get the CSS color class for an operation status
 */
export function getStatusColorClass(status: OperationStatus): string {
    const colors: Record<OperationStatus, string> = {
        pending: 'text-gray-400',
        running: 'text-blue-400',
        success: 'text-green-400',
        error: 'text-red-400',
    };
    return colors[status];
}

/**
 * Format timestamp for display
 */
export function formatTimestamp(date: Date): string {
    return date.toLocaleTimeString('it-IT', {
        hour: '2-digit',
        minute: '2-digit',
        second: '2-digit',
    });
}

export default useActivityLog;
