/**
 * ImageSaveDialog — AeroImage "Save As" modal
 *
 * Allows the user to choose filename, format, and quality before
 * saving a processed image via the Rust `process_image` command.
 */

import React, { useState, useMemo, useCallback, useEffect } from 'react';
import { X, Loader2 } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { EditState, ImageResult, OUTPUT_FORMATS, buildOperations } from '../types';
import { useI18n } from '../../../i18n';

interface ImageSaveDialogProps {
    isOpen: boolean;
    filePath: string;
    fileName: string;
    editState: EditState;
    originalDimensions: { width: number; height: number };
    onSaved: (result: ImageResult) => void;
    onClose: () => void;
}

function computeOutputDimensions(
    original: { width: number; height: number },
    state: EditState,
): { width: number; height: number } {
    let w = state.crop ? state.crop.width : original.width;
    let h = state.crop ? state.crop.height : original.height;
    if (state.resize) { w = state.resize.width; h = state.resize.height; }
    if (state.rotation === 90 || state.rotation === 270) { [w, h] = [h, w]; }
    return { width: Math.round(w), height: Math.round(h) };
}

export const ImageSaveDialog: React.FC<ImageSaveDialogProps> = ({
    isOpen, filePath, fileName, editState, originalDimensions, onSaved, onClose,
}) => {
    const { t } = useI18n();

    // Derive directory and base name from the original file path
    const directory = filePath.substring(0, filePath.lastIndexOf('/'));
    const baseName = fileName.replace(/\.[^.]+$/, '');
    const originalExt = (fileName.split('.').pop() ?? 'png').toLowerCase();
    const defaultFormat = OUTPUT_FORMATS.find(f => f.value === originalExt)?.value
        ?? OUTPUT_FORMATS.find(f => f.value === 'jpg' && originalExt === 'jpeg')?.value
        ?? 'png';

    const [name, setName] = useState(baseName);
    const [format, setFormat] = useState<string>(defaultFormat);
    const [quality, setQuality] = useState(90);
    const [saving, setSaving] = useState<'copy' | 'replace' | null>(null);
    const [error, setError] = useState<string | null>(null);

    // Reset state when dialog opens with a new file
    useEffect(() => {
        if (isOpen) {
            setName(baseName);
            setFormat(defaultFormat);
            setQuality(90);
            setSaving(null);
            setError(null);
        }
    }, [isOpen, baseName, defaultFormat]);

    // Escape key handler
    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    const outputDims = useMemo(
        () => computeOutputDimensions(originalDimensions, editState),
        [originalDimensions, editState],
    );

    const save = useCallback(async (mode: 'copy' | 'replace') => {
        setSaving(mode);
        setError(null);
        const outputPath = mode === 'replace'
            ? filePath
            : `${directory}/${name}_edited.${format}`;
        try {
            const result = await invoke<ImageResult>('process_image', {
                inputPath: filePath,
                outputPath,
                operations: buildOperations(editState),
                jpegQuality: format === 'jpg' ? quality : null,
            });
            onSaved(result);
        } catch (err) {
            setError(String(err));
        } finally {
            setSaving(null);
        }
    }, [filePath, directory, name, format, quality, editState, onSaved]);

    if (!isOpen) return null;

    const label = (key: string, fallback: string) =>
        t(`preview.image.edit.${key}` as never) || fallback;

    return (
        <div
            className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
            role="dialog"
            aria-modal="true"
            onClick={e => { if (e.target === e.currentTarget) onClose(); }}
        >
            <div className="bg-gray-800 rounded-xl shadow-2xl border border-gray-700 p-6 w-[420px] max-w-[90vw]">
                {/* Header */}
                <div className="flex items-center justify-between mb-5">
                    <h2 className="text-lg font-semibold text-gray-100">
                        {label('saveTitle', 'Save Image')}
                    </h2>
                    <button onClick={onClose} className="text-gray-400 hover:text-white transition-colors">
                        <X size={18} />
                    </button>
                </div>

                {/* Filename */}
                <div className="flex items-center gap-2 mb-3">
                    <span className="text-sm text-gray-400 w-20 shrink-0">{label('saveFilename', 'Filename')}</span>
                    <input
                        className="flex-1 bg-gray-700 border border-gray-600 rounded-lg px-3 py-2 text-sm text-gray-200 outline-none focus:border-blue-500"
                        value={name}
                        onChange={e => setName(e.target.value)}
                    />
                    <span className="text-sm text-gray-400">.{format}</span>
                </div>

                {/* Format */}
                <div className="flex items-center gap-2 mb-3">
                    <span className="text-sm text-gray-400 w-20 shrink-0">{label('saveFormat', 'Format')}</span>
                    <select
                        className="flex-1 bg-gray-700 border border-gray-600 rounded-lg px-3 py-2 text-sm text-gray-200 outline-none focus:border-blue-500"
                        value={format}
                        onChange={e => setFormat(e.target.value)}
                    >
                        {OUTPUT_FORMATS.map(f => (
                            <option key={f.value} value={f.value}>{f.label}</option>
                        ))}
                    </select>
                </div>

                {/* Quality (JPEG only) */}
                {format === 'jpg' && (
                    <div className="flex items-center gap-2 mb-3">
                        <span className="text-sm text-gray-400 w-20 shrink-0">{label('saveQuality', 'Quality')}</span>
                        <input
                            type="range" min={1} max={100} value={quality}
                            onChange={e => setQuality(Number(e.target.value))}
                            className="flex-1 accent-blue-500"
                        />
                        <span className="text-sm text-gray-300 w-8 text-right">{quality}</span>
                    </div>
                )}

                {/* Output dimensions */}
                <p className="text-sm text-gray-400 mb-5">
                    {label('saveDimensions', 'Output')}: {outputDims.width} &times; {outputDims.height} px
                </p>

                {/* Action buttons */}
                <div className="flex gap-3 mb-2">
                    <button
                        onClick={() => save('copy')}
                        disabled={saving !== null || !name.trim()}
                        className="flex-1 flex items-center justify-center gap-2 bg-blue-600 hover:bg-blue-500 disabled:opacity-50 text-white px-4 py-2 rounded-lg text-sm font-medium transition-colors"
                    >
                        {saving === 'copy' && <Loader2 size={14} className="animate-spin" />}
                        {saving === 'copy' ? label('saveSaving', 'Saving...') : label('saveAsCopy', 'Save Copy')}
                    </button>
                    <button
                        onClick={() => save('replace')}
                        disabled={saving !== null}
                        className="flex-1 flex items-center justify-center gap-2 bg-gray-600 hover:bg-gray-500 disabled:opacity-50 text-white px-4 py-2 rounded-lg text-sm font-medium transition-colors"
                    >
                        {saving === 'replace' && <Loader2 size={14} className="animate-spin" />}
                        {saving === 'replace' ? label('saveSaving', 'Saving...') : label('saveReplace', 'Replace Original')}
                    </button>
                </div>

                {/* Replace warning */}
                <p className="text-xs text-amber-400 mb-1">
                    {label('saveReplaceConfirm', 'This will overwrite the original file')}
                </p>

                {/* Error message */}
                {error && (
                    <p className="text-xs text-red-400 mt-2 break-words">{error}</p>
                )}
            </div>
        </div>
    );
};
