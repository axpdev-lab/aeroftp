/**
 * SyncTemplateDialog — Export/Import .aerosync sync templates
 * Portable configuration sharing between machines
 */

import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { readTextFile, writeTextFile } from '@tauri-apps/plugin-fs';
import {
    X, FileDown, FileUp, Download, Upload, Check, AlertTriangle
} from 'lucide-react';
import { SyncTemplate } from '../../types';
import { useTranslation } from '../../i18n';

interface SyncTemplateDialogProps {
    isOpen: boolean;
    onClose: () => void;
    localPath: string;
    remotePath: string;
    profileId: string;
    excludePatterns: string[];
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
    const [templateName, setTemplateName] = useState('');
    const [templateDesc, setTemplateDesc] = useState('');

    useEffect(() => {
        if (isOpen) {
            setMode('export');
            setResult(null);
            setImportPreview(null);
        }
    }, [isOpen]);

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const handleExport = async () => {
        setExporting(true);
        setResult(null);
        try {
            const { save } = await import('@tauri-apps/plugin-dialog');
            const filePath = await save({
                defaultPath: 'sync-config.aerosync',
                filters: [{ name: 'AeroSync Template', extensions: ['aerosync'] }],
            });
            if (!filePath) {
                setExporting(false);
                return;
            }
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
            const { open } = await import('@tauri-apps/plugin-dialog');
            const filePath = await open({
                filters: [{ name: 'AeroSync Template', extensions: ['aerosync'] }],
                multiple: false,
            });
            if (!filePath) {
                setImporting(false);
                return;
            }
            const jsonContent = await readTextFile(filePath as string);
            const template = await invoke<SyncTemplate>('import_sync_template_cmd', {
                jsonContent,
            });
            setImportPreview(template);
            setResult({ success: true, message: t('syncPanel.templateImported') });
        } catch {
            setResult({ success: false, message: t('common.error') });
        } finally {
            setImporting(false);
        }
    };

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 bg-black/60 z-[9999] flex items-center justify-center p-4" onClick={onClose} role="dialog" aria-modal="true" aria-label="Sync Template">
            <div
                className="bg-white dark:bg-gray-800 rounded-xl shadow-2xl w-full max-w-lg flex flex-col"
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
                        onClick={() => { setMode('export'); setResult(null); setImportPreview(null); }}
                    >
                        <Download size={14} className="inline mr-1" /> {t('syncPanel.templateExport')}
                    </button>
                    <button
                        className={`flex-1 py-2 text-xs font-medium text-center border-b-2 transition-colors ${
                            mode === 'import' ? 'border-purple-500 text-purple-400' : 'border-transparent text-gray-400 hover:text-gray-300'
                        }`}
                        onClick={() => { setMode('import'); setResult(null); setImportPreview(null); }}
                    >
                        <Upload size={14} className="inline mr-1" /> {t('syncPanel.templateImport')}
                    </button>
                </div>

                {/* Content */}
                <div className="px-5 py-4 space-y-3">
                    {mode === 'export' ? (
                        <div className="py-4 space-y-3">
                            <FileDown size={32} className="mx-auto mb-2 text-purple-400 opacity-50" />
                            <p className="text-xs text-gray-400 text-center">
                                {t('syncPanel.templateExportDesc')}
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
                            <div className="text-center">
                                <button
                                    className="px-6 py-2 rounded-lg bg-purple-500 text-white text-xs font-medium hover:bg-purple-600 disabled:opacity-50"
                                    onClick={handleExport}
                                    disabled={exporting}
                                >
                                    {exporting ? '...' : t('syncPanel.templateExport')}
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

                    {/* Import Preview */}
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
