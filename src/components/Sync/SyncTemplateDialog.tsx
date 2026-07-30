// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * SyncTemplateDialog: Export/Import .aerosync sync templates and shell scripts
 * Portable configuration sharing between machines
 */

import React, { useState, useEffect, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { pickFile, pickSave } from '../../utils/pickPath';
import { readTextFile, writeTextFile } from '@tauri-apps/plugin-fs';
import {
    X, FileDown, FileUp, Download, Upload, Check, AlertTriangle, Terminal
} from 'lucide-react';
import {
    SyncTemplate,
    SyncScriptFormat,
    SyncScriptMeta,
    AerosyncExportScriptResult,
    AerosyncImportScriptResult,
    AerosyncScriptProfile,
    SyncProfile,
} from '../../types';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';

interface SyncTemplateDialogProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
    /**
     * Name of the connected saved server. It is what the exported script
     * connects with (`CONNECT --profile <name>`), and it is deliberately not
     * the sync preset: the two are separate objects and conflating them was
     * what broke every export path (#514).
     */
    serverProfileName: string;
    excludePatterns: string[];
}

type ExportFormat = 'aerosync' | 'aeroftp-script' | 'bash' | 'pwsh';

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
    serverProfileName,
    excludePatterns,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [mode, setMode] = useState<'export' | 'import'>('export');
    const [exporting, setExporting] = useState(false);
    const [importing, setImporting] = useState(false);
    const [applying, setApplying] = useState(false);
    const [result, setResult] = useState<{ success: boolean; message: string } | null>(null);
    const [importPreview, setImportPreview] = useState<SyncTemplate | null>(null);
    const [importedScript, setImportedScript] = useState<SyncScriptMeta | null>(null);
    const [importedAerosyncScript, setImportedAerosyncScript] =
        useState<AerosyncImportScriptResult | null>(null);
    const [templateName, setTemplateName] = useState('');
    const [templateDesc, setTemplateDesc] = useState('');
    const [exportFormat, setExportFormat] = useState<ExportFormat>('aeroftp-script');
    const [alsoGenerateWrapper, setAlsoGenerateWrapper] = useState(true);
    // The sync preset the export describes (direction, comparison keys,
    // retry policy). Every export command resolves this id against the
    // sync-preset store, which is why it must not be a saved-server id.
    const [syncProfiles, setSyncProfiles] = useState<SyncProfile[]>([]);
    const [presetId, setPresetId] = useState('');

    const defaultScriptFormat = useMemo(detectDefaultScriptFormat, []);

    useEffect(() => {
        if (isOpen) {
            setMode('export');
            setResult(null);
            setImportPreview(null);
            setImportedScript(null);
            setImportedAerosyncScript(null);
            setExportFormat('aeroftp-script');
            setAlsoGenerateWrapper(true);
        }
    }, [isOpen]);

    // Builtins first (Mirror, Two-way, Backup, Pull, Remote Backup), then any
    // custom preset saved from an imported script. Reloaded on each open so a
    // preset applied from the Import tab shows up without a remount.
    useEffect(() => {
        if (!isOpen) return;
        let cancelled = false;
        // Cleared up front, not just on success: a stale id surviving a failed
        // reload would keep Export enabled and reach the backend, which is the
        // very "not found" this dialog was fixed to stop producing.
        setSyncProfiles([]);
        setPresetId('');
        void (async () => {
            try {
                const profiles = await invoke<SyncProfile[]>('load_sync_profiles_cmd');
                if (cancelled) return;
                setSyncProfiles(profiles);
                setPresetId(profiles[0]?.id || '');
            } catch {
                // Leave the picker empty: Export stays disabled rather than
                // reaching a backend lookup that cannot succeed.
            }
        })();
        return () => { cancelled = true; };
    }, [isOpen]);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const exportTemplate = async () => {
        const filePath = await pickSave({
            defaultPath: 'sync-config.aerosync',
            filters: [{ name: 'AeroSync Template', extensions: ['aerosync'] }],
        });
        if (!filePath) return false;
        const jsonContent = await invoke<string>('export_sync_template_cmd', {
            name: templateName || 'Sync Template',
            description: templateDesc,
            profileId: presetId,
            localPath,
            remotePath,
            // Empty means "the preset's own", the same contract the
            // `.aeroftp-script` path has always used. Sending `[]` verbatim
            // exported a Mirror script with no excludes at all.
            excludePatterns: excludePatterns.length > 0 ? excludePatterns : null,
        });
        await writeTextFile(filePath, jsonContent);
        setResult({ success: true, message: t('syncPanel.templateExported') });
        return true;
    };

    /**
     * Issue #133: canonical `.aeroftp-script` export. Optionally writes
     * a sibling `.ps1` or `.sh` wrapper that just calls
     * `aeroftp-cli batch <name>.aeroftp-script` so it can be
     * double-clicked or hooked into Task Scheduler / cron.
     */
    const exportAerosyncScript = async () => {
        const defaultName = (templateName || 'aerosync-config')
            .replace(/[^A-Za-z0-9._-]/g, '-')
            .replace(/-+/g, '-')
            .replace(/^-+|-+$/g, '') || 'aerosync-config';
        const filePath = await pickSave({
            defaultPath: `${defaultName}.aeroftp-script`,
            filters: [{ name: 'AeroSync script', extensions: ['aeroftp-script'] }],
        });
        if (!filePath) return false;
        const ret = await invoke<AerosyncExportScriptResult>(
            'aerosync_export_script_cmd',
            {
                args: {
                    profile_id: presetId,
                    // The script's own label is the template name when the
                    // user typed one; what it CONNECTs with is always the
                    // saved server, resolved by name against the vault.
                    profile_display_name: templateName || serverProfileName || null,
                    local_path: localPath,
                    remote_path: remotePath,
                    connect_profile: serverProfileName || null,
                    exclude_patterns_override:
                        excludePatterns.length > 0 ? excludePatterns : null,
                    dry_run: false,
                    conflict_mode: 'newer',
                    track_renames: false,
                    skip_matching: false,
                    resync: false,
                    watch: false,
                    output_path: filePath as string,
                    also_generate_wrapper: alsoGenerateWrapper,
                },
            },
        );
        const wrapperHint = ret.wrapper_path
            ? ` (+ ${ret.wrapper_path})`
            : '';
        setResult({
            success: true,
            message:
                (t('syncPanel.aerosyncScriptExportedToast') ||
                    'AeroSync script exported') + wrapperHint,
        });
        return true;
    };

    const exportScript = async (format: SyncScriptFormat) => {
        const ext = format === 'pwsh' ? 'ps1' : 'sh';
        const defaultName = `sync-config.${ext}`;
        const filterName = format === 'pwsh' ? 'PowerShell script' : 'Shell script';
        const filePath = await pickSave({
            defaultPath: defaultName,
            filters: [{ name: filterName, extensions: [ext] }],
        });
        if (!filePath) return false;
        const scriptContent = await invoke<string>('export_sync_script_cmd', {
            args: {
                profile_id: presetId,
                profile_display_name: serverProfileName || templateName || 'AeroFTP Server',
                template_name: templateName,
                template_description: templateDesc,
                local_path: localPath,
                remote_path: remotePath,
                exclude_patterns:
                    excludePatterns.length > 0 ? excludePatterns : null,
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
            } else if (exportFormat === 'aeroftp-script') {
                await exportAerosyncScript();
            } else {
                await exportScript(exportFormat);
            }
        } catch (err) {
            const msg = err instanceof Error
                ? err.message
                : typeof err === 'string'
                    ? err
                    : t('common.error');
            setResult({ success: false, message: msg });
        } finally {
            setExporting(false);
        }
    };

    const handleImport = async () => {
        setImporting(true);
        setResult(null);
        try {
            const filePath = await pickFile({
                filters: [
                    {
                        name: 'AeroFTP sync',
                        extensions: ['aeroftp-script', 'aerosync', 'sh', 'ps1'],
                    },
                ],
                multiple: false,
            });
            if (!filePath) {
                setImporting(false);
                return;
            }
            const path = filePath as string;
            const lower = path.toLowerCase();

            // Canonical .aeroftp-script: always goes through the #133
            // pipeline. Wrappers (.ps1/.sh) try the new pipeline first
            // (resolves the sibling canonical script automatically) and
            // fall back to the legacy AEROFTP-META path when the file
            // turns out to be a standalone `aeroftp-cli sync` wrapper.
            if (lower.endsWith('.aeroftp-script')) {
                const imported = await invoke<AerosyncImportScriptResult>(
                    'aerosync_import_script_cmd',
                    { args: { input_path: path } },
                );
                setImportedAerosyncScript(imported);
                setImportPreview(null);
                setImportedScript(null);
                setResult({
                    success: true,
                    message:
                        t('syncPanel.aerosyncScriptImportedToast') ||
                        'AeroSync script imported',
                });
            } else if (lower.endsWith('.sh') || lower.endsWith('.ps1')) {
                try {
                    const imported = await invoke<AerosyncImportScriptResult>(
                        'aerosync_import_script_cmd',
                        { args: { input_path: path } },
                    );
                    setImportedAerosyncScript(imported);
                    setImportPreview(null);
                    setImportedScript(null);
                    setResult({
                        success: true,
                        message:
                            t('syncPanel.aerosyncScriptImportedToast') ||
                            'AeroSync script imported',
                    });
                } catch {
                    const content = await readTextFile(path);
                    const meta = await invoke<SyncScriptMeta>('import_sync_script_cmd', {
                        scriptContent: content,
                    });
                    setImportedScript(meta);
                    setImportedAerosyncScript(null);
                    setImportPreview(null);
                    setResult({
                        success: true,
                        message: t('syncPanel.templateScriptImportedToast'),
                    });
                }
            } else {
                const content = await readTextFile(path);
                const template = await invoke<SyncTemplate>('import_sync_template_cmd', {
                    jsonContent: content,
                });
                setImportPreview(template);
                setImportedScript(null);
                setImportedAerosyncScript(null);
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
                    : msg || t('common.error'),
            });
        } finally {
            setImporting(false);
        }
    };

    /**
     * Persist an imported .aeroftp-script profile as a saved SyncProfile
     * so the user can pick it from the AeroSync preset list at next run.
     * The original id is preserved when free, suffixed with `-imported`
     * (and a numeric tail if needed) when it collides.
     */
    const applyImportedAerosyncScript = async () => {
        if (!importedAerosyncScript) return;
        setApplying(true);
        try {
            const existing = await invoke<SyncProfile[]>('load_sync_profiles_cmd');
            const taken = new Set(existing.map((p) => p.id));
            let candidateId = importedAerosyncScript.profile.profile.id;
            if (candidateId.length === 0 || candidateId === 'imported') {
                candidateId = 'imported';
            }
            if (taken.has(candidateId) || existing.some((p) => p.builtin && p.id === candidateId)) {
                const base = `${candidateId}-imported`;
                let suffix = 0;
                candidateId = base;
                while (taken.has(candidateId)) {
                    suffix += 1;
                    candidateId = `${base}-${suffix}`;
                }
            }
            const toSave: SyncProfile = {
                ...importedAerosyncScript.profile.profile,
                id: candidateId,
                builtin: false,
            };
            await invoke('save_sync_profile_cmd', { profile: toSave });
            setResult({
                success: true,
                message:
                    t('syncPanel.aerosyncScriptAppliedToast') ||
                    `Preset "${toSave.name}" saved. Open AeroSync to use it.`,
            });
        } catch (err) {
            const msg = err instanceof Error
                ? err.message
                : typeof err === 'string'
                    ? err
                    : t('common.error');
            setResult({ success: false, message: msg });
        } finally {
            setApplying(false);
        }
    };

    if (!isOpen) return null;

    const exportButtonLabel =
        exportFormat === 'aerosync'
            ? t('syncPanel.templateExport')
            : exportFormat === 'aeroftp-script'
                ? t('syncPanel.aerosyncScriptExportButton') || 'Export script'
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
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-lg flex flex-col animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                {/* Header */}
                <div {...modalDrag.dragHandleProps} className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing">
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
                                <label
                                    htmlFor="sync-template-preset"
                                    className="block text-[11px] uppercase tracking-wide text-gray-500"
                                >
                                    {t('syncPresets.title')}
                                </label>
                                <select
                                    id="sync-template-preset"
                                    className="w-full text-xs bg-transparent border border-gray-300 dark:border-gray-600 rounded px-2 py-1.5 dark:bg-gray-800"
                                    value={presetId}
                                    onChange={e => setPresetId(e.target.value)}
                                >
                                    {syncProfiles.map(p => (
                                        <option key={p.id} value={p.id}>{p.name}</option>
                                    ))}
                                </select>
                            </div>
                            <div className="space-y-1">
                                <div className="text-[11px] uppercase tracking-wide text-gray-500">
                                    {t('syncPanel.templateFormat') || 'Format'}
                                </div>
                                <div className="space-y-1">
                                    {formatRadio(
                                        'aeroftp-script',
                                        t('syncPanel.templateFormatAeroftpScript') ||
                                            'AeroSync script (.aeroftp-script)',
                                        t('syncPanel.templateFormatAeroftpScriptHint') ||
                                            '(recommended, full round-trip)',
                                    )}
                                    {formatRadio('aerosync', t('syncPanel.templateFormatAerosync') || '.aerosync template')}
                                    {formatRadio('bash', t('syncPanel.templateFormatBash') || 'Bash script (.sh)')}
                                    {formatRadio('pwsh', t('syncPanel.templateFormatPwsh') || 'PowerShell script (.ps1)')}
                                </div>
                                {exportFormat === 'aeroftp-script' && (
                                    <label className="mt-2 flex items-center gap-2 text-xs text-gray-600 dark:text-gray-300">
                                        <input
                                            type="checkbox"
                                            checked={alsoGenerateWrapper}
                                            onChange={(e) =>
                                                setAlsoGenerateWrapper(e.target.checked)
                                            }
                                            className="rounded border-gray-300 dark:border-gray-600"
                                        />
                                        <span>
                                            {t('syncPanel.aerosyncScriptAlsoWrapper') ||
                                                'Also generate OS-native wrapper (.ps1 / .sh)'}
                                        </span>
                                    </label>
                                )}
                            </div>
                            <div className="text-center pt-1">
                                <button
                                    className="px-6 py-2 rounded-lg bg-purple-500 text-white text-xs font-medium hover:bg-purple-600 disabled:opacity-50"
                                    onClick={handleExport}
                                    disabled={exporting || !presetId}
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

                    {/* Import Preview (.aeroftp-script canonical) */}
                    {importedAerosyncScript && (
                        <div className="p-3 rounded-lg bg-gray-100 dark:bg-gray-700/50 text-xs space-y-1">
                            <div className="flex items-center gap-2">
                                <Terminal size={12} className="text-purple-400" />
                                <strong>
                                    {importedAerosyncScript.profile.profile.name}
                                </strong>
                                {importedAerosyncScript.resolved_from_wrapper && (
                                    <span className="text-[10px] uppercase tracking-wide text-gray-400">
                                        {t('syncPanel.aerosyncScriptFromWrapper') ||
                                            '(from wrapper)'}
                                    </span>
                                )}
                            </div>
                            <div>
                                <strong>{t('syncPanel.direction')}:</strong>{' '}
                                {importedAerosyncScript.profile.profile.direction}
                            </div>
                            <div>
                                <strong>Local:</strong>{' '}
                                {importedAerosyncScript.profile.local_path}
                            </div>
                            <div>
                                <strong>Remote:</strong>{' '}
                                {importedAerosyncScript.profile.remote_path}
                            </div>
                            {importedAerosyncScript.profile.profile.delete_orphans && (
                                <div className="text-amber-500">--delete</div>
                            )}
                            {importedAerosyncScript.profile.profile.exclude_patterns
                                .length > 0 && (
                                <div>
                                    <strong>Excludes:</strong>{' '}
                                    {importedAerosyncScript.profile.profile.exclude_patterns.join(
                                        ', ',
                                    )}
                                </div>
                            )}
                            <div>
                                <strong>{t('syncPanel.parallelStreams')}:</strong>{' '}
                                {importedAerosyncScript.profile.profile.parallel_streams}
                            </div>
                            {importedAerosyncScript.unmapped_fields.length > 0 && (
                                <div className="text-amber-500">
                                    {t('syncPanel.aerosyncScriptUnmappedFields') ||
                                        'Unrecognized metadata fields'}
                                    : {importedAerosyncScript.unmapped_fields.join(', ')}
                                </div>
                            )}
                            {importedAerosyncScript.warnings.length > 0 && (
                                <ul className="mt-1 list-disc pl-4 text-amber-500">
                                    {importedAerosyncScript.warnings.map((w, i) => (
                                        <li key={i}>{w}</li>
                                    ))}
                                </ul>
                            )}
                            <div className="pt-2">
                                <button
                                    className="px-3 py-1.5 rounded-lg bg-purple-500 text-white text-xs font-medium hover:bg-purple-600 disabled:opacity-50"
                                    onClick={applyImportedAerosyncScript}
                                    disabled={applying}
                                >
                                    {applying
                                        ? '...'
                                        : t('syncPanel.aerosyncScriptApplyButton') ||
                                          'Save as preset'}
                                </button>
                            </div>
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
