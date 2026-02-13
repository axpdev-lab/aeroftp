/**
 * Humanized Activity Log Hook
 * Provides friendly, conversational log messages using i18n translations
 * 
 * Supports multiple languages with professional tone
 * NO EMOJIS - Professional icon-based feedback only
 */

import { useCallback, useRef } from 'react';
import { useActivityLog, OperationType } from './useActivityLog';
import { useI18n, useTranslation } from '../i18n';

// ============================================================================
// Types
// ============================================================================

export interface HumanizedLogParams {
    filename?: string;
    oldname?: string;
    newname?: string;
    foldername?: string;
    server?: string;
    path?: string;
    destination?: string; // for move operations
    count?: number;
    folders?: number;
    files?: number;
    percent?: number;
    speed?: string;
    isRemote?: boolean;
    size?: string; // string because it's usually formatted size
    time?: string;
    message?: string;
    protocol?: string;
    remote?: string; // for sync path
    local?: string; // for sync path
}

export type HumanizedOperationType =
    | 'CONNECT' | 'DISCONNECT'
    | 'UPLOAD' | 'DOWNLOAD'
    | 'DELETE' | 'RENAME' | 'MKDIR' | 'MOVE'
    | 'NAVIGATE'
    | 'DELETE_MULTIPLE' | 'UPLOAD_MULTIPLE' | 'DOWNLOAD_MULTIPLE'
    | 'SYNC' | 'RECONNECT';

// ============================================================================
// Hook
// ============================================================================

export function useHumanizedLog() {
    const activityLog = useActivityLog();
    const t = useTranslation();
    const { language } = useI18n();
    const lastNavigatePathRef = useRef<{ remote: string | null; local: string | null }>({
        remote: null,
        local: null,
    });

    const normalizePath = (value: string): string => {
        if (!value || value.trim() === '') return '/';
        const cleaned = value.replace(/\\/g, '/').replace(/\/+/g, '/');
        if (cleaned === '/') return '/';
        return cleaned.endsWith('/') ? cleaned.slice(0, -1) : cleaned;
    };

    const getParentPath = (value: string): string => {
        const normalized = normalizePath(value);
        if (normalized === '/') return '/';
        const idx = normalized.lastIndexOf('/');
        if (idx <= 0) return '/';
        return normalized.slice(0, idx);
    };

    /**
     * Helper to get location string
     */
    const getLocationString = (isRemote?: boolean): string => {
        if (isRemote === true) return t('browser.remote');
        if (isRemote === false) return t('browser.local');
        return '';
    };

    /**
     * Helper to map operation to translation key
     */
    const getTranslationKey = (operation: HumanizedOperationType, phase: string): string => {
        const opKey = operation.toLowerCase();
        // Handle specific mappings if key name differs
        if (opKey === 'delete_multiple') return `activity.delete_multiple_${phase}`;
        if (opKey === 'upload_multiple') return `activity.upload_${phase}`;
        if (opKey === 'download_multiple') return `activity.download_${phase}`;

        return `activity.${opKey}_${phase}`;
    };

    /**
     * Build variables object for i18n
     */
    const buildVars = (params: HumanizedLogParams): Record<string, string | number> => {
        // Basic params
        const vars: Record<string, string | number> = { ...params } as Record<string, string | number>;

        // Add location if specified
        if (params.isRemote !== undefined) {
            vars.location = getLocationString(params.isRemote);
        }

        // Handle path specifically if needed (often passed directly)

        return vars;
    };

    /**
     * Log a humanized operation start
     */
    const logStart = useCallback((
        operation: HumanizedOperationType,
        params: HumanizedLogParams = {}
    ): string => {
        const key = getTranslationKey(operation, 'start');
        const message = t(key, buildVars(params));

        // Determine base operation type for the icon
        const opType = operation === 'DISCONNECT' ? 'DISCONNECT' :
            operation === 'CONNECT' || operation === 'RECONNECT' ? 'CONNECT' :
                operation.includes('DELETE') ? 'DELETE' :
                    operation.includes('UPLOAD') ? 'UPLOAD' :
                        operation.includes('DOWNLOAD') ? 'DOWNLOAD' :
                            operation.includes('SYNC') ? 'INFO' :
                                operation as OperationType;

        return activityLog.log(opType, message, 'running');
    }, [t, activityLog]);

    /**
     * Log a humanized success
     */
    const logSuccess = useCallback((
        operation: HumanizedOperationType,
        params: HumanizedLogParams = {},
        existingId?: string
    ): string => {
        const key = getTranslationKey(operation, 'success');
        const message = t(key, buildVars(params));

        const opType = operation === 'DISCONNECT' ? 'DISCONNECT' :
            operation === 'CONNECT' || operation === 'RECONNECT' ? 'CONNECT' :
                operation.includes('DELETE') ? 'DELETE' :
                    operation.includes('UPLOAD') ? 'UPLOAD' :
                        operation.includes('DOWNLOAD') ? 'DOWNLOAD' :
                            operation.includes('SYNC') ? 'INFO' :
                                operation as OperationType;

        if (existingId) {
            activityLog.updateEntry(existingId, { status: 'success', message });
            return existingId;
        }
        return activityLog.log(opType, message, 'success');
    }, [t, activityLog]);

    /**
     * Log a humanized error
     */
    const logError = useCallback((
        operation: HumanizedOperationType,
        params: HumanizedLogParams = {},
        existingId?: string
    ): string => {
        const key = getTranslationKey(operation, 'error');
        const message = t(key, buildVars(params));

        const opType = 'ERROR';

        if (existingId) {
            activityLog.updateEntry(existingId, { status: 'error', message });
            return existingId;
        }
        return activityLog.log(opType, message, 'error');
    }, [t, activityLog]);

    /**
     * Log navigation (instant success)
     */
    const logNavigate = useCallback((path: string, isRemote: boolean): string => {
        const panelKey = isRemote ? 'remote' : 'local';
        const previousPath = lastNavigatePathRef.current[panelKey];
        const currentPath = normalizePath(path);

        const vars = {
            path: currentPath,
            location: getLocationString(isRemote)
        };

        let message = t('activity.navigate_success', vars);
        if (language === 'it' || language === 'en') {
            const previousNormalized = previousPath ? normalizePath(previousPath) : null;
            if (previousNormalized) {
                if (currentPath === getParentPath(previousNormalized)) {
                    message = t('activity.navigate_up_one', vars);
                } else if (previousNormalized.startsWith(`${currentPath}/`)) {
                    message = t('activity.navigate_up_to', vars);
                } else if (currentPath.startsWith(`${previousNormalized}/`)) {
                    message = t('activity.navigate_entered', vars);
                } else {
                    message = t('activity.navigate_opened', vars);
                }
            } else {
                message = t('activity.navigate_opened', vars);
            }
        }

        lastNavigatePathRef.current[panelKey] = currentPath;
        return activityLog.log('NAVIGATE', message, 'success');
    }, [t, activityLog, language]);

    /**
     * Specialized log functions using translations
     */
    const logRaw = useCallback((key: string, type: OperationType, params: Record<string, string | number> = {}, status: 'running' | 'success' | 'error' = 'running') => {
        const message = t(key, params);
        return activityLog.log(type, message, status);
    }, [t, activityLog]);

    return {
        logStart,
        logSuccess,
        logError,
        logNavigate,
        logRaw,
        // Expose raw log for legacy/custom calls if needed
        log: activityLog.log,
        updateEntry: activityLog.updateEntry,
    };
}

export default useHumanizedLog;
