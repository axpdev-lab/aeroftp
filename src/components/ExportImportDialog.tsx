// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet — AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { Upload, Download, Shield, AlertCircle, CheckCircle2, X, Eye, EyeOff, Lock, Server, RefreshCw, FolderInput, AlertTriangle } from 'lucide-react';
import { ServerProfile } from '../types';
import { useTranslation } from '../i18n';
import { Checkbox } from './ui/Checkbox';

interface ExportImportDialogProps {
    servers: ServerProfile[];
    onImport: (servers: ServerProfile[]) => void;
    onClose: () => void;
}

interface ImportedServer {
    id: string;
    name: string;
    host: string;
    port: number;
    username: string;
    protocol?: string;
    initialPath?: string;
    localInitialPath?: string;
    color?: string;
    lastConnected?: string;
    options?: Record<string, unknown>;
    providerId?: string;
    credential?: string;
    hasStoredCredential?: boolean;
}

interface ImportResult {
    servers: ImportedServer[];
    metadata: {
        exportDate: string;
        aeroftpVersion: string;
        serverCount: number;
        hasCredentials: boolean;
    };
}

interface RcloneImportResult {
    servers: ImportedServer[];
    skipped: Array<{ name: string; rcloneType: string; reason: string }>;
    sourcePath: string;
    totalRemotes: number;
}

export const ExportImportDialog: React.FC<ExportImportDialogProps> = ({ servers, onImport, onClose }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'export' | 'import' | 'rclone' | 'rclone-export' | null>(null);
    const [password, setPassword] = useState('');
    const [confirmPassword, setConfirmPassword] = useState('');
    const [includeCredentials, setIncludeCredentials] = useState(true);
    const [showPassword, setShowPassword] = useState(false);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    const [success, setSuccess] = useState<string | null>(null);
    const [selectedServerIds, setSelectedServerIds] = useState<Set<string>>(() => new Set(servers.map(s => s.id)));

    // Rclone-specific state
    const [rcloneDetectedPath, setRcloneDetectedPath] = useState<string | null>(null);
    const [rcloneResult, setRcloneResult] = useState<RcloneImportResult | null>(null);
    const [rcloneSelectedIds, setRcloneSelectedIds] = useState<Set<string>>(new Set());

    const allSelected = selectedServerIds.size === servers.length;
    const noneSelected = selectedServerIds.size === 0;

    const selectedServers = useMemo(
        () => servers.filter(s => selectedServerIds.has(s.id)),
        [servers, selectedServerIds]
    );

    // Auto-detect rclone config when entering rclone mode
    useEffect(() => {
        if (mode === 'rclone' && rcloneDetectedPath === null) {
            invoke<string | null>('detect_rclone_config').then(path => {
                setRcloneDetectedPath(path || '');
            }).catch(() => setRcloneDetectedPath(''));
        }
    }, [mode, rcloneDetectedPath]);

    const toggleServer = (id: string) => {
        setSelectedServerIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const toggleAll = () => {
        if (allSelected) {
            setSelectedServerIds(new Set());
        } else {
            setSelectedServerIds(new Set(servers.map(s => s.id)));
        }
    };

    const handleExport = async () => {
        if (password !== confirmPassword) {
            setError(t('settings.passwordMismatch'));
            return;
        }
        if (password.length < 8) {
            setError(t('settings.passwordTooShort'));
            return;
        }
        if (noneSelected) return;

        // Open save dialog first
        const filePath = await save({
            title: t('settings.exportServers'),
            filters: [{ name: 'AeroFTP Profile', extensions: ['aeroftp'] }],
            defaultPath: `aeroftp_backup_${new Date().toISOString().slice(0, 10)}.aeroftp`,
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const serversJson = JSON.stringify(selectedServers);
            await invoke('export_server_profiles', {
                serversJson,
                password,
                includeCredentials,
                filePath,
            });
            setSuccess(t('settings.exportSuccess').replace('{count}', String(selectedServers.length)));
            setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleImport = async () => {
        if (password.length < 1) {
            setError(t('settings.passwordRequired'));
            return;
        }

        // Open file picker first
        const filePath = await open({
            title: t('settings.importServers'),
            filters: [
                { name: 'AeroFTP Profile', extensions: ['aeroftp'] },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const result = await invoke<ImportResult>('import_server_profiles', {
                filePath,
                password,
            });

            const importedServers = result.servers;

            // Read current servers directly from localStorage (ground truth)
            // The `servers` prop may be stale or incomplete
            let currentServers: ServerProfile[] = [];
            try {
                const stored = localStorage.getItem('aeroftp-saved-servers');
                if (stored) currentServers = JSON.parse(stored);
            } catch { /* fallback to prop */ }
            if (currentServers.length === 0) currentServers = servers;

            // Merge: skip duplicates by host+port+username OR by ID
            const existingKeys = new Set(
                currentServers.map(s => `${s.host}:${s.port}:${s.username}`)
            );
            const existingIds = new Set(currentServers.map(s => s.id));

            const newServers: ServerProfile[] = importedServers
                .filter(s => !existingKeys.has(`${s.host}:${s.port}:${s.username}`) && !existingIds.has(s.id))
                .map(s => ({
                    id: s.id,
                    name: s.name,
                    host: s.host,
                    port: s.port,
                    username: s.username,
                    protocol: s.protocol as ServerProfile['protocol'],
                    initialPath: s.initialPath,
                    localInitialPath: s.localInitialPath,
                    color: s.color,
                    lastConnected: s.lastConnected,
                    options: s.options,
                    providerId: s.providerId,
                    hasStoredCredential: s.credential ? true : (s.hasStoredCredential || false),
                }));

            const skipped = importedServers.length - newServers.length;
            onImport(newServers);
            setSuccess(
                t('settings.importSuccess').replace('{count}', String(newServers.length)) +
                (skipped > 0 ? ` (${skipped} ${t('settings.duplicatesSkipped')})` : '')
            );
            setTimeout(() => onClose(), 2500);
        } catch (err) {
            const errStr = String(err);
            if (errStr.includes('Invalid password')) {
                setError(t('settings.invalidPassword'));
            } else {
                setError(errStr);
            }
        } finally {
            setLoading(false);
        }
    };

    // ---- Rclone import handlers ----

    const handleRcloneScan = async (customPath?: string) => {
        const filePath = customPath || rcloneDetectedPath;
        if (!filePath) return;

        setLoading(true);
        setError(null);
        setRcloneResult(null);
        try {
            const result = await invoke<RcloneImportResult>('import_rclone_config', { filePath });
            setRcloneResult(result);
            // Pre-select all importable servers
            setRcloneSelectedIds(new Set(result.servers.map(s => s.id)));
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const handleRcloneBrowse = async () => {
        const filePath = await open({
            title: t('settings.rcloneSelectConfig'),
            filters: [
                { name: 'rclone config', extensions: ['conf'] },
                { name: 'All Files', extensions: ['*'] },
            ],
            multiple: false,
        });
        if (!filePath) return;
        setRcloneDetectedPath(filePath);
        await handleRcloneScan(filePath);
    };

    const handleRcloneConfirm = () => {
        if (!rcloneResult) return;

        // Read current servers from localStorage
        let currentServers: ServerProfile[] = [];
        try {
            const stored = localStorage.getItem('aeroftp-saved-servers');
            if (stored) currentServers = JSON.parse(stored);
        } catch { /* fallback */ }
        if (currentServers.length === 0) currentServers = servers;

        const existingKeys = new Set(
            currentServers.map(s => `${s.host}:${s.port}:${s.username}`)
        );

        const newServers: ServerProfile[] = rcloneResult.servers
            .filter(s => rcloneSelectedIds.has(s.id))
            .filter(s => !existingKeys.has(`${s.host}:${s.port}:${s.username}`))
            .map(s => ({
                id: s.id,
                name: s.name,
                host: s.host,
                port: s.port,
                username: s.username,
                protocol: s.protocol as ServerProfile['protocol'],
                initialPath: s.initialPath,
                options: s.options as ServerProfile['options'],
                providerId: s.providerId,
                hasStoredCredential: s.hasStoredCredential || false,
            }));

        const totalSelected = rcloneResult.servers.filter(s => rcloneSelectedIds.has(s.id)).length;
        const skipped = totalSelected - newServers.length;
        onImport(newServers);
        setSuccess(
            t('settings.importSuccess').replace('{count}', String(newServers.length)) +
            (skipped > 0 ? ` (${skipped} ${t('settings.duplicatesSkipped')})` : '')
        );
        setTimeout(() => onClose(), 2500);
    };

    const toggleRcloneServer = (id: string) => {
        setRcloneSelectedIds(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    };

    const toggleAllRclone = () => {
        if (!rcloneResult) return;
        if (rcloneSelectedIds.size === rcloneResult.servers.length) {
            setRcloneSelectedIds(new Set());
        } else {
            setRcloneSelectedIds(new Set(rcloneResult.servers.map(s => s.id)));
        }
    };

    const handleRcloneExport = async () => {
        if (noneSelected) return;

        const filePath = await save({
            title: t('settings.rcloneExportTitle'),
            filters: [{ name: 'rclone config', extensions: ['conf'] }],
            defaultPath: 'rclone.conf',
        });
        if (!filePath) return;

        setLoading(true);
        setError(null);
        try {
            const serversJson = JSON.stringify(selectedServers);
            const result = await invoke<{ exported: number }>('export_rclone_config', {
                serversJson,
                includeCredentials,
                filePath,
            });
            setSuccess(t('settings.rcloneExportSuccess').replace('{count}', String(result.exported)));
            setTimeout(() => onClose(), 2000);
        } catch (err) {
            setError(String(err));
        } finally {
            setLoading(false);
        }
    };

    const resetMode = () => {
        setMode(null);
        setError(null);
        setSuccess(null);
        setPassword('');
        setConfirmPassword('');
        setRcloneResult(null);
        setRcloneSelectedIds(new Set());
    };

    // Protocol display helper
    const protocolLabel = (proto?: string) => (proto || 'ftp').toUpperCase();

    return (
        <div className="fixed inset-0 bg-black/50 flex items-center justify-center z-50" onClick={(e) => e.target === e.currentTarget && onClose()}>
            <div className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-[480px] max-h-[85vh] overflow-hidden animate-scale-in flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 flex-shrink-0">
                    <h3 className="text-lg font-semibold flex items-center gap-2">
                        <Shield size={20} className="text-blue-500" />
                        {t('settings.exportImport')}
                    </h3>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700">
                        <X size={18} />
                    </button>
                </div>

                <div className="p-5 overflow-y-auto">
                    {/* Mode selection */}
                    {!mode ? (
                        <div className="space-y-3">
                            <button
                                onClick={() => setMode('export')}
                                disabled={servers.length === 0}
                                className="w-full p-4 border border-gray-200 dark:border-gray-600 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-700/50 flex items-center gap-3 transition-colors disabled:opacity-50"
                            >
                                <div className="w-10 h-10 rounded-lg bg-green-100 dark:bg-green-900/30 flex items-center justify-center">
                                    <Download size={20} className="text-green-600 dark:text-green-400" />
                                </div>
                                <div className="text-left">
                                    <div className="font-medium">{t('settings.exportServers')}</div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">
                                        {t('settings.exportDescription').replace('{count}', String(servers.length))}
                                    </div>
                                </div>
                            </button>
                            <button
                                onClick={() => setMode('import')}
                                className="w-full p-4 border border-gray-200 dark:border-gray-600 rounded-lg hover:bg-gray-50 dark:hover:bg-gray-700/50 flex items-center gap-3 transition-colors"
                            >
                                <div className="w-10 h-10 rounded-lg bg-blue-100 dark:bg-blue-900/30 flex items-center justify-center">
                                    <Upload size={20} className="text-blue-600 dark:text-blue-400" />
                                </div>
                                <div className="text-left">
                                    <div className="font-medium">{t('settings.importServers')}</div>
                                    <div className="text-xs text-gray-500 dark:text-gray-400">
                                        {t('settings.importDescription')}
                                    </div>
                                </div>
                            </button>
                            {/* rclone section */}
                            <div className="pt-2 border-t border-gray-100 dark:border-gray-700 space-y-3">
                                <div className="text-[10px] uppercase tracking-wider text-gray-400 dark:text-gray-500 font-medium">rclone</div>
                                <button
                                    onClick={() => setMode('rclone')}
                                    className="w-full p-4 border border-orange-200 dark:border-orange-800/50 rounded-lg hover:bg-orange-50 dark:hover:bg-orange-900/20 flex items-center gap-3 transition-colors"
                                >
                                    <div className="w-10 h-10 rounded-lg bg-orange-100 dark:bg-orange-900/30 flex items-center justify-center">
                                        <FolderInput size={20} className="text-orange-600 dark:text-orange-400" />
                                    </div>
                                    <div className="text-left">
                                        <div className="font-medium">{t('settings.rcloneImport')}</div>
                                        <div className="text-xs text-gray-500 dark:text-gray-400">
                                            {t('settings.rcloneImportDesc')}
                                        </div>
                                    </div>
                                </button>
                                <button
                                    onClick={() => setMode('rclone-export')}
                                    disabled={servers.length === 0}
                                    className="w-full p-4 border border-orange-200 dark:border-orange-800/50 rounded-lg hover:bg-orange-50 dark:hover:bg-orange-900/20 flex items-center gap-3 transition-colors disabled:opacity-50"
                                >
                                    <div className="w-10 h-10 rounded-lg bg-orange-100 dark:bg-orange-900/30 flex items-center justify-center">
                                        <Download size={20} className="text-orange-600 dark:text-orange-400" />
                                    </div>
                                    <div className="text-left">
                                        <div className="font-medium">{t('settings.rcloneExport')}</div>
                                        <div className="text-xs text-gray-500 dark:text-gray-400">
                                            {t('settings.rcloneExportDesc')}
                                        </div>
                                    </div>
                                </button>
                            </div>
                        </div>
                    ) : mode === 'export' ? (
                        <div className="space-y-4">
                            {/* Server selection list */}
                            <div>
                                <div className="flex items-center justify-between mb-2">
                                    <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                        {t('settings.selectServersToExport')}
                                    </span>
                                    <button
                                        onClick={toggleAll}
                                        className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                    >
                                        {allSelected ? t('settings.deselectAll') : t('settings.selectAll')}
                                    </button>
                                </div>
                                <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                    {servers.map((server) => (
                                        <div
                                            key={server.id}
                                            className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                        >
                                            <Checkbox
                                                checked={selectedServerIds.has(server.id)}
                                                onChange={() => toggleServer(server.id)}
                                            />
                                            <div
                                                className="w-2 h-2 rounded-full flex-shrink-0"
                                                style={{ backgroundColor: server.color || '#6B7280' }}
                                            />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {server.host}:{server.port} — {server.username}
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </div>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                    {selectedServerIds.size} / {servers.length} {t('settings.selected')}
                                </div>
                            </div>

                            {/* Include credentials toggle */}
                            <div className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                                <Checkbox
                                    checked={includeCredentials}
                                    onChange={setIncludeCredentials}
                                    label={
                                        <div>
                                            <div className="text-sm font-medium flex items-center gap-1">
                                                <Lock size={14} />
                                                {t('settings.includeCredentials')}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400">
                                                {t('settings.includeCredentialsHint')}
                                            </div>
                                        </div>
                                    }
                                />
                            </div>

                            {/* Password fields */}
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    placeholder={t('settings.encryptionPassword')}
                                    value={password}
                                    onChange={(e) => setPassword(e.target.value)}
                                    className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                                />
                                <button
                                    type="button"
                                    tabIndex={-1}
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600"
                                >
                                    {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                </button>
                            </div>
                            <input
                                type={showPassword ? 'text' : 'password'}
                                placeholder={t('settings.confirmPassword')}
                                value={confirmPassword}
                                onChange={(e) => setConfirmPassword(e.target.value)}
                                className="w-full px-3 py-2 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                            />

                            {/* Password strength indicator */}
                            {password.length > 0 && password.length < 8 && (
                                <div className="text-xs text-amber-600 dark:text-amber-400">
                                    {t('settings.passwordTooShort')}
                                </div>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleExport}
                                    disabled={loading || password.length < 8 || noneSelected}
                                    className="flex-1 px-4 py-2 text-sm bg-green-500 text-white rounded-lg hover:bg-green-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Download size={16} />
                                    )}
                                    {loading ? t('settings.exporting') : `${t('settings.exportServers')} (${selectedServerIds.size})`}
                                </button>
                            </div>
                        </div>
                    ) : mode === 'import' ? (
                        <div className="space-y-4">
                            {/* Password field */}
                            <div className="relative">
                                <input
                                    type={showPassword ? 'text' : 'password'}
                                    placeholder={t('settings.decryptionPassword')}
                                    value={password}
                                    onChange={(e) => setPassword(e.target.value)}
                                    className="w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded-lg bg-white dark:bg-gray-700 text-sm"
                                />
                                <button
                                    type="button"
                                    tabIndex={-1}
                                    onClick={() => setShowPassword(!showPassword)}
                                    className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600"
                                >
                                    {showPassword ? <EyeOff size={16} /> : <Eye size={16} />}
                                </button>
                            </div>

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleImport}
                                    disabled={loading || password.length < 1}
                                    className="flex-1 px-4 py-2 text-sm bg-blue-500 text-white rounded-lg hover:bg-blue-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Upload size={16} />
                                    )}
                                    {loading ? t('settings.importing') : t('settings.importServers')}
                                </button>
                            </div>
                        </div>
                    ) : mode === 'rclone' ? (
                        /* ---- Rclone Import Mode ---- */
                        <div className="space-y-4">
                            {/* Security upgrade notice */}
                            <div className="flex items-start gap-2 p-3 bg-green-50 dark:bg-green-900/20 border border-green-200 dark:border-green-800 rounded-lg">
                                <Shield size={16} className="text-green-600 dark:text-green-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-green-700 dark:text-green-300">
                                    {t('settings.rcloneSecurityUpgrade')}
                                </div>
                            </div>

                            {!rcloneResult ? (
                                /* Step 1: Detect/select config file */
                                <>
                                    {rcloneDetectedPath === null ? (
                                        <div className="flex items-center justify-center py-6">
                                            <RefreshCw size={20} className="animate-spin text-gray-400" />
                                            <span className="ml-2 text-sm text-gray-500">{t('settings.rcloneDetecting')}</span>
                                        </div>
                                    ) : rcloneDetectedPath ? (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-gray-50 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 rounded-lg">
                                                <div className="text-xs text-gray-500 dark:text-gray-400 mb-1">{t('settings.rcloneConfigFound')}</div>
                                                <div className="text-sm font-mono truncate" title={rcloneDetectedPath}>
                                                    {rcloneDetectedPath}
                                                </div>
                                            </div>
                                            <div className="flex gap-2">
                                                <button
                                                    onClick={() => handleRcloneScan()}
                                                    disabled={loading}
                                                    className="flex-1 px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                                >
                                                    {loading ? (
                                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                                    ) : (
                                                        <FolderInput size={16} />
                                                    )}
                                                    {loading ? t('settings.rcloneScanning') : t('settings.rcloneScanConfig')}
                                                </button>
                                                <button
                                                    onClick={handleRcloneBrowse}
                                                    disabled={loading}
                                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                                >
                                                    {t('settings.rcloneBrowse')}
                                                </button>
                                            </div>
                                        </div>
                                    ) : (
                                        <div className="space-y-3">
                                            <div className="p-3 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                                <div className="text-sm text-blue-700 dark:text-blue-300 flex items-center gap-2">
                                                    <AlertCircle size={14} />
                                                    {t('settings.rcloneNotFound')}
                                                </div>
                                                <div className="text-xs text-blue-600 dark:text-blue-400 mt-1">
                                                    {t('settings.rcloneNotFoundHint')}
                                                </div>
                                            </div>
                                            <button
                                                onClick={handleRcloneBrowse}
                                                disabled={loading}
                                                className="w-full px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                            >
                                                <FolderInput size={16} />
                                                {t('settings.rcloneBrowse')}
                                            </button>
                                        </div>
                                    )}
                                </>
                            ) : (
                                /* Step 2: Preview and select remotes */
                                <>
                                    {/* Summary */}
                                    <div className="text-sm text-gray-600 dark:text-gray-300">
                                        {t('settings.rcloneFound')
                                            .replace('{total}', String(rcloneResult.totalRemotes))
                                            .replace('{supported}', String(rcloneResult.servers.length))}
                                    </div>

                                    {/* Importable servers */}
                                    {rcloneResult.servers.length > 0 && (
                                        <div>
                                            <div className="flex items-center justify-between mb-2">
                                                <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                                    {t('settings.rcloneSelectRemotes')}
                                                </span>
                                                <button
                                                    onClick={toggleAllRclone}
                                                    className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                                >
                                                    {rcloneSelectedIds.size === rcloneResult.servers.length
                                                        ? t('settings.deselectAll')
                                                        : t('settings.selectAll')}
                                                </button>
                                            </div>
                                            <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                                {rcloneResult.servers.map((server) => (
                                                    <div
                                                        key={server.id}
                                                        className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                                        onClick={() => toggleRcloneServer(server.id)}
                                                    >
                                                        <Checkbox
                                                            checked={rcloneSelectedIds.has(server.id)}
                                                            onChange={() => toggleRcloneServer(server.id)}
                                                        />
                                                        <div className="min-w-0 flex-1">
                                                            <div className="text-sm font-medium truncate">{server.name}</div>
                                                            <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                                {server.host}{server.port !== 443 ? `:${server.port}` : ''}{server.username ? ` - ${server.username}` : ''}
                                                            </div>
                                                        </div>
                                                        <div className="flex items-center gap-1.5 flex-shrink-0">
                                                            {server.hasStoredCredential && (
                                                                <Lock size={12} className="text-green-500" />
                                                            )}
                                                            <span className="text-[10px] text-gray-400 uppercase">
                                                                {protocolLabel(server.protocol)}
                                                            </span>
                                                        </div>
                                                    </div>
                                                ))}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                                {rcloneSelectedIds.size} / {rcloneResult.servers.length} {t('settings.selected')}
                                            </div>
                                        </div>
                                    )}

                                    {/* OAuth re-auth notice */}
                                    {rcloneResult.servers.some(s =>
                                        ['googledrive', 'dropbox', 'onedrive', 'box', 'pcloud', 'yandexdisk', 'jottacloud'].includes(s.protocol || '')
                                    ) && (
                                        <div className="flex items-start gap-2 p-2.5 bg-blue-50 dark:bg-blue-900/20 border border-blue-200 dark:border-blue-800 rounded-lg">
                                            <AlertCircle size={14} className="text-blue-500 mt-0.5 flex-shrink-0" />
                                            <div className="text-xs text-blue-700 dark:text-blue-300">
                                                {t('settings.rcloneOauthNotice')}
                                            </div>
                                        </div>
                                    )}

                                    {/* Skipped remotes */}
                                    {rcloneResult.skipped.length > 0 && (
                                        <div>
                                            <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                                                {t('settings.rcloneSkipped')} ({rcloneResult.skipped.length})
                                            </div>
                                            <div className="text-xs text-gray-400 dark:text-gray-500 space-y-0.5">
                                                {rcloneResult.skipped.map((s, i) => (
                                                    <div key={i} className="truncate">
                                                        <span className="font-medium">{s.name}</span>
                                                        <span className="mx-1">-</span>
                                                        <span>{s.rcloneType}</span>
                                                    </div>
                                                ))}
                                            </div>
                                            <div className="text-xs text-gray-400 dark:text-gray-500 mt-1.5 italic">
                                                {t('settings.rcloneMoreComingSoon')}
                                            </div>
                                        </div>
                                    )}
                                </>
                            )}

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                {rcloneResult && rcloneResult.servers.length > 0 && (
                                    <button
                                        onClick={handleRcloneConfirm}
                                        disabled={rcloneSelectedIds.size === 0}
                                        className="flex-1 px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                    >
                                        <Upload size={16} />
                                        {t('settings.rcloneImportSelected').replace('{count}', String(rcloneSelectedIds.size))}
                                    </button>
                                )}
                            </div>
                        </div>
                    ) : (
                        /* ---- Rclone Export Mode ---- */
                        <div className="space-y-4">
                            {/* Info notice */}
                            <div className="flex items-start gap-2 p-3 bg-orange-50 dark:bg-orange-900/20 border border-orange-200 dark:border-orange-800 rounded-lg">
                                <FolderInput size={16} className="text-orange-600 dark:text-orange-400 mt-0.5 flex-shrink-0" />
                                <div className="text-xs text-orange-700 dark:text-orange-300">
                                    {t('settings.rcloneExportNotice')}
                                </div>
                            </div>

                            {/* Server selection (reuse selectedServerIds) */}
                            <div>
                                <div className="flex items-center justify-between mb-2">
                                    <span className="text-sm font-medium text-gray-700 dark:text-gray-300">
                                        {t('settings.selectServersToExport')}
                                    </span>
                                    <button
                                        onClick={toggleAll}
                                        className="text-xs text-blue-500 hover:text-blue-600 font-medium"
                                    >
                                        {allSelected ? t('settings.deselectAll') : t('settings.selectAll')}
                                    </button>
                                </div>
                                <div className="border border-gray-200 dark:border-gray-600 rounded-lg max-h-[200px] overflow-y-auto">
                                    {servers.map((server) => (
                                        <div
                                            key={server.id}
                                            className="flex items-center gap-3 px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/50 cursor-pointer border-b border-gray-100 dark:border-gray-700 last:border-b-0"
                                            onClick={() => toggleServer(server.id)}
                                        >
                                            <Checkbox
                                                checked={selectedServerIds.has(server.id)}
                                                onChange={() => toggleServer(server.id)}
                                            />
                                            <div
                                                className="w-2 h-2 rounded-full flex-shrink-0"
                                                style={{ backgroundColor: server.color || '#6B7280' }}
                                            />
                                            <div className="min-w-0 flex-1">
                                                <div className="text-sm font-medium truncate">{server.name}</div>
                                                <div className="text-xs text-gray-500 dark:text-gray-400 truncate">
                                                    {server.host}:{server.port} — {server.username}
                                                </div>
                                            </div>
                                            <span className="text-[10px] text-gray-400 uppercase flex-shrink-0">
                                                {server.protocol || 'ftp'}
                                            </span>
                                        </div>
                                    ))}
                                </div>
                                <div className="text-xs text-gray-500 dark:text-gray-400 mt-1">
                                    {selectedServerIds.size} / {servers.length} {t('settings.selected')}
                                </div>
                            </div>

                            {/* Include credentials toggle */}
                            <div className="flex items-center gap-3 p-3 bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800 rounded-lg">
                                <Checkbox
                                    checked={includeCredentials}
                                    onChange={setIncludeCredentials}
                                    label={
                                        <div>
                                            <div className="text-sm font-medium flex items-center gap-1">
                                                <Lock size={14} />
                                                {t('settings.includeCredentials')}
                                            </div>
                                            <div className="text-xs text-gray-500 dark:text-gray-400">
                                                {t('settings.rcloneExportCredHint')}
                                            </div>
                                        </div>
                                    }
                                />
                            </div>

                            {/* Error/Success */}
                            {error && <div className="text-red-500 text-sm flex items-center gap-2"><AlertCircle size={14} />{error}</div>}
                            {success && <div className="text-green-500 text-sm flex items-center gap-2"><CheckCircle2 size={14} />{success}</div>}

                            {/* Actions */}
                            <div className="flex gap-2">
                                <button
                                    onClick={resetMode}
                                    className="px-4 py-2 text-sm border border-gray-300 dark:border-gray-600 rounded-lg hover:bg-gray-100 dark:hover:bg-gray-700"
                                >
                                    {t('common.back')}
                                </button>
                                <button
                                    onClick={handleRcloneExport}
                                    disabled={loading || noneSelected}
                                    className="flex-1 px-4 py-2 text-sm bg-orange-500 text-white rounded-lg hover:bg-orange-600 disabled:opacity-50 flex items-center justify-center gap-2"
                                >
                                    {loading ? (
                                        <span className="animate-spin w-4 h-4 border-2 border-white border-t-transparent rounded-full" />
                                    ) : (
                                        <Download size={16} />
                                    )}
                                    {loading ? t('settings.exporting') : t('settings.rcloneExportButton').replace('{count}', String(selectedServerIds.size))}
                                </button>
                            </div>
                        </div>
                    )}
                </div>
            </div>
        </div>
    );
};

export default ExportImportDialog;
