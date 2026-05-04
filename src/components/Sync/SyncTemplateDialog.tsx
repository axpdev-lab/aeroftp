// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * SyncTemplateDialog: Export/Import .aerosync sync templates and shell scripts
 * Portable configuration sharing between machines
 */

import React, { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open, save } from '@tauri-apps/plugin-dialog';
import { readTextFile, writeTextFile } from '@tauri-apps/plugin-fs';
import {
    X, FileDown, FileUp, Download, Upload, Check, AlertTriangle, Terminal
} from 'lucide-react';
import { SyncTemplate, SyncScriptFormat, SyncScriptMeta } from '../../types';
import { useTranslation } from '../../i18n';

interface SyncTemplateDialogProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
    profileId: string;
    excludePatterns: string[];
}

type ExportFormat = 'aerosync' | 'bash' | 'pwsh';

function detectDefaultScriptFormat(): ExportFormat {
    if (typeof navigator !== 'undefined') {
        const platform = (navigator.platform || '').toLowerCase();
        if (platform.includes('win')) return 'pwsh';
    }
    return 'bash';
}

export const SyncTemplateDialog: React.FC<SyncTemplateDialogProps> = ({
    isOpen,
    onClose,
    localPath,
    remotePath,
    profileId,
    excludePatterns,
}) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'export' | 'import'>('export');
    const [exporting, setExporting] = useState(false);
    const [importing, setImporting] = useState(false);
    const [result, setResult] = useState<{ success: boolean; message: string } | null>(null);
    const [importPreview, setImportPreview] = useState<SyncTemplate | null>(null);
    const [importedScript, setImportedScript] = useState<SyncScriptMeta | null>(null);
    const [templateName, setTemplateName] = useState('');
    const [templateDesc, setTemplateDesc] = useState('');
    const [exportFormat, setExportFormat] = useState<ExportFormat>('aerosync');

    const defaultScriptFormat = useMemo(detectDefaultScriptFormat, []);

    useEffect(() => {
        if (isOpen) {
            setMode('export');
            setResult(null);
            setImportPreview(null);
            setImportedScript(null);
            setExportFormat('aerosync');
        }
    }, [isOpen]);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const exportTemplate = async () => {
        const filePath = await save({
            defaultPath: 'sync-config.aerosync',
            filters: [{ name: 'AeroSync Template', extensions: ['aerosync'] }],
        });
        if (!filePath) return false;
        const jsonContent = await invoke<string>('export_sync_template_cmd', {
            name: templateName || 'Sync Template',
            description: templateDesc,
            profileId,
            localPath,
            remotePath,
            excludePatterns,
        });
        await writeTextFile(filePath, jsonContent);
        setResult({ success: true, message: t('syncPanel.templateExported') });
        return true;
    };

    const exportScript = async (format: SyncScriptFormat) => {
        const ext = format === 'pwsh' ? 'ps1' : 'sh';
        const defaultName = `sync-config.${ext}`;
        const filterName = format === 'pwsh' ? 'PowerShell script' : 'Shell script';
        const filePath = await save({
            defaultPath: defaultName,
            filters: [{ name: filterName, extensions: [ext] }],
        });
        if (!filePath) return false;
        const scriptContent = await invoke<string>('export_sync_script_cmd', {
            args: {
                profile_id: profileId,
                profile_display_name: templateName || 'AeroFTP Server',
                template_name: templateName,
                template_description: templateDesc,
                local_path: localPath,
                remote_path: remotePath,
                exclude_patterns: excludePatterns,
                format,
            },
        });
        await writeTextFile(filePath, scriptContent);
        setResult({ success: true, message: t('syncPanel.templateScriptExportedToast') });
        return true;
    };

    const handleExport = async () => {
        setExporting(true);
        setResult(null);
        try {
            if (exportFormat === 'aerosync') {
                await exportTemplate();
            } else {
                await exportScript(exportFormat);
            }
        } catch {
            setResult({ success: false, message: t('common.error') });
        } finally {
            setExporting(false);
        }
    };

    const handleImport = async () => {
        setImporting(true);
        setResult(null);
        try {
            const filePath = await open({
                filters: [
                    { name: 'AeroFTP sync', extensions: ['aerosync', 'sh', 'ps1'] },
                ],
                multiple: false,
            });
            if (!filePath) {
                setImporting(false);
                return;
            }
            const path = filePath as string;
            const content = await readTextFile(path);
            const lower = path.toLowerCase();
            if (lower.endsWith('.sh') || lower.endsWith('.ps1')) {
                const meta = await invoke<SyncScriptMeta>('import_sync_script_cmd', {
                    scriptContent: content,
                });
                setImportedScript(meta);
                setImportPreview(null);
                setResult({ success: true, message: t('syncPanel.templateScriptImportedToast') });
            } else {
                const template = await invoke<SyncTemplate>('import_sync_template_cmd', {
                    jsonContent: content,
                });
                setImportPreview(template);
                setImportedScript(null);
                setResult({ success: true, message: t('syncPanel.templateImported') });
            }
        } catch (err) {
            const msg = err instanceof Error
                ? err.message
                : typeof err === 'string'
                    ? err
                    : t('common.error');
            const isMetaMissing = typeof msg === 'string' && msg.includes('AEROFTP-META');
            setResult({
                success: false,
                message: isMetaMissing
                    ? t('syncPanel.templateScriptInvalidToast')
                    : t('common.error'),
            });
        } finally {
            setImporting(false);
        }
    };

    if (!isOpen) return null;

    const exportButtonLabel =
        exportFormat === 'aerosync'
            ? t('syncPanel.templateExport')
            : exportFormat === 'bash'
                ? t('syncPanel.templateFormatBash')
                : t('syncPanel.templateFormatPwsh');

    const formatRadio = (value: ExportFormat, label: string, hint?: string) => {
        const active = exportFormat === value;
        const isDefault = value !== 'aerosync' && value === defaultScriptFormat;
        return (
            <button
                type="button"
                key={value}
                className={`flex items-center gap-2 px-2 py-1.5 rounded border text-xs transition-colors w-full text-left ${
                    active
                        ? 'border-purple-500 bg-purple-500/10 text-purple-400'
                        : 'border-gray-300 dark:border-gray-600 text-gray-500 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700/40'
                }`}
                onClick={() => setExportFormat(value)}
            >
                <span
                    className={`w-3 h-3 rounded-full border ${
                        active ? 'bg-purple-500 border-purple-500' : 'border-gray-400'
                    }`}
                    aria-hidden
                />
                <span className="flex-1">
                    {label}
                    {hint ? (
                        <span className="ml-1 text-gray-400">{hint}</span>
                    ) : null}
                    {isDefault ? (
                        <span className="ml-1 text-[10px] uppercase tracking-wide text-purple-400">
                            ({t('common.default') || 'default'})
                        </span>
                    ) : null}
                </span>
            </button>
        );
    };

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="Sync Template">
            <div
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-lg flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-2">
                        <FileDown size={18} className="text-purple-500" />
                        <h3 className="font-semibold text-sm">{t('syncPanel.templates')}</h3>
                    </div>
                    <button onClick={onClose} className="text-gray-400 hover:text-gray-200">
                        <X size={18} />
                    </button>
                </div>

                {/* Mode Toggle */}
                <div className="flex border-b border-gray-200 dark:border-gray-700">
                    <button
                        className={`flex-1 py-2 text-xs font-medium text-center border-b-2 transition-colors ${
                            mode === 'export' ? 'border-purple-500 text-purple-400' : 'border-transparent text-gray-400 hover:text-gray-300'
                        }`}
                        onClick={() => { setMode('export'); setResult(null); setImportPreview(null); setImportedScript(null); }}
                    >
                        <Download size={14} className="inline mr-1" /> {t('syncPanel.templateExport')}
                    </button>
                    <button
                        className={`flex-1 py-2 text-xs font-medium text-center border-b-2 transition-colors ${
                            mode === 'import' ? 'border-purple-500 text-purple-400' : 'border-transparent text-gray-400 hover:text-gray-300'
                        }`}
                        onClick={() => { setMode('import'); setResult(null); setImportPreview(null); setImportedScript(null); }}
                    >
                        <Upload size={14} className="inline mr-1" /> {t('syncPanel.templateImport')}
                    </button>
                </div>

                {/* Content */}
                <div className="px-5 py-4 space-y-3">
                    {mode === 'export' ? (
                        <div className="py-2 space-y-3">
                            <FileDown size={32} className="mx-auto mb-1 text-purple-400 opacity-50" />
                            <p className="text-xs text-gray-400 text-center">
                                {exportFormat === 'aerosync'
                                    ? t('syncPanel.templateExportDesc')
                                    : `aeroftp-cli sync wrapper (${exportFormat === 'pwsh' ? 'PowerShell' : 'bash'})`}
                            </p>
                            <input
                                type="text"
                                className="w-full text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-2 py-1.5 placeholder-gray-400"
                                placeholder={t('syncPanel.templateName')}
                                value={templateName}
                                onChange={e => setTemplateName(e.target.value)}
                            />
                            <input
                                type="text"
                                className="w-full text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-2 py-1.5 placeholder-gray-400"
                                placeholder={t('syncPanel.templateDesc') || 'Description'}
                                value={templateDesc}
                                onChange={e => setTemplateDesc(e.target.value)}
                            />
                            <div className="space-y-1">
                                <div className="text-[11px] uppercase tracking-wide text-gray-500">
                                    {t('syncPanel.templateFormat') || 'Format'}
                                </div>
                                <div className="space-y-1">
                                    {formatRadio('aerosync', t('syncPanel.templateFormatAerosync') || '.aerosync template')}
                                    {formatRadio('bash', t('syncPanel.templateFormatBash') || 'Bash script (.sh)')}
                                    {formatRadio('pwsh', t('syncPanel.templateFormatPwsh') || 'PowerShell script (.ps1)')}
                                </div>
                            </div>
                            <div className="text-center pt-1">
                                <button
                                    className="px-6 py-2 rounded-lg bg-purple-500 text-white text-xs font-medium hover:bg-purple-600 disabled:opacity-50"
                                    onClick={handleExport}
                                    disabled={exporting}
                                >
                                    {exporting ? '...' : exportButtonLabel}
                                </button>
                            </div>
                        </div>
                    ) : (
                        <div className="text-center py-4">
                            <FileUp size={32} className="mx-auto mb-3 text-purple-400 opacity-50" />
                            <p className="text-xs text-gray-400 mb-4">
                                {t('syncPanel.templateImportDesc')}
                            </p>
                            <button
                                className="px-6 py-2 rounded-lg bg-purple-500 text-white text-xs font-medium hover:bg-purple-600 disabled:opacity-50"
                                onClick={handleImport}
                                disabled={importing}
                            >
                                {importing ? '...' : t('syncPanel.templateImport')}
                            </button>
                        </div>
                    )}

                    {/* Import Preview (template) */}
                    {importPreview && (
                        <div className="p-3 rounded-lg bg-gray-100 dark:bg-gray-700/50 text-xs space-y-1">
                            <div><strong>{t('syncPanel.templateName')}:</strong> {importPreview.name?.slice(0, 100)}</div>
                            <div><strong>{t('syncPanel.direction')}:</strong> {importPreview.profile.direction}</div>
                            <div><strong>{t('syncPanel.parallelStreams')}:</strong> {importPreview.profile.parallel_streams}</div>
                            {importPreview.exclude_patterns.length > 0 && (
                                <div><strong>Excludes:</strong> {importPreview.exclude_patterns.join(', ')}</div>
                            )}
                        </div>
                    )}

                    {/* Import Preview (script meta) */}
                    {importedScript && (
                        <div className="p-3 rounded-lg bg-gray-100 dark:bg-gray-700/50 text-xs space-y-1">
                            <div className="flex items-center gap-2">
                                <Terminal size={12} className="text-purple-400" />
                                <strong>{importedScript.profile_name}</strong>
                            </div>
                            <div><strong>{t('syncPanel.direction')}:</strong> {importedScript.direction}</div>
                            <div><strong>Local:</strong> {importedScript.local_path}</div>
                            <div><strong>Remote:</strong> {importedScript.remote_path}</div>
                            {importedScript.delete_orphans && (
                                <div className="text-amber-500">--delete</div>
                            )}
                            {importedScript.exclude_patterns.length > 0 && (
                                <div><strong>Excludes:</strong> {importedScript.exclude_patterns.join(', ')}</div>
                            )}
                            {importedScript.retries != null && (
                                <div><strong>Retries:</strong> {importedScript.retries}{importedScript.retries_sleep ? ` × ${importedScript.retries_sleep}` : ''}</div>
                            )}
                        </div>
                    )}

                    {/* Result */}
                    {result && (
                        <div className={`flex items-center gap-2 p-2 rounded-lg text-xs ${
                            result.success ? 'bg-green-500/10 text-green-400' : 'bg-red-500/10 text-red-400'
                        }`}>
                            {result.success ? <Check size={14} /> : <AlertTriangle size={14} />}
                            {result.message}
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="flex justify-end px-5 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        className="text-xs px-4 py-1.5 rounded-lg bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600"
                        onClick={onClose}
                    >
                        {t('common.close')}
                    </button>
                </div>
            </div>
        </div>
    );
};
