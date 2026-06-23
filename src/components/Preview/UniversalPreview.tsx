// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Universal Preview Modal
 * 
 * Main container for the preview system. Displays a full-screen modal
 * that renders the appropriate viewer based on file type.
 * 
 * Features:
 * - Elegant modal overlay with backdrop blur
 * - Dynamic viewer selection based on file type
 * - Keyboard navigation (ESC to close, arrows for gallery)
 * - Download button
 * - Loading states for remote files
 */

import React, { useCallback, useEffect, useState } from 'react';
import { X, Download, ChevronLeft, ChevronRight, ExternalLink, Pencil, FileText } from 'lucide-react';
import { UniversalPreviewProps, PreviewFileData } from './types';
import { getPreviewCategory, formatFileSize, getCategoryIcon, isSourceViewable } from './utils/fileTypes';
import { ImageViewer } from './viewers/ImageViewer';
import { AudioPlayer } from './viewers/AudioPlayer';
import { VideoPlayer } from './viewers/VideoPlayer';
import { PDFViewer } from './viewers/PDFViewer';
import { TextViewer } from './viewers/TextViewer';
import { useI18n } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { useGuardedClose } from '../../hooks/useGuardedClose';
import { GuardedCloseConfirm } from '../GuardedCloseConfirm';

export const UniversalPreview: React.FC<UniversalPreviewProps> = ({
    isOpen,
    file,
    onClose,
    onDownload,
    onEdit,
    onNext,
    onPrevious,
    hasNext,
    hasPrevious,
}) => {
    const { t } = useI18n();
    const modalDrag = useDraggableModal();
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);
    // Opt-in "view as text" for files whose extension is not recognised as a
    // known text/code format. Lets any plain-text file (e.g. a custom-suffix
    // notes file) be read as if it were a .txt, on the user's explicit
    // request, without auto-rendering binary blobs (#270 comment 17105085).
    const [forceText, setForceText] = useState(false);
    // True while AeroImage has unsaved edits: guards an accidental close.
    const [previewDirty, setPreviewDirty] = useState(false);
    const guarded = useGuardedClose({ guard: previewDirty ? 'dirty' : null, onClose });

    // Reset error + the view-as-text override when the file changes
    useEffect(() => {
        setError(null);
        setIsLoading(false);
        setForceText(false);
        setPreviewDirty(false);
    }, [file?.name, file?.path]);

    // Determine file category. When the user asked to view an otherwise
    // unrecognised file as text, treat it as text for rendering.
    const detectedCategory = file ? getPreviewCategory(file.name) : 'unknown';
    const category = forceText ? 'text' : detectedCategory;

    // Handle keyboard navigation
    useEffect(() => {
        if (!isOpen) return;

        const handleKeyDown = (e: KeyboardEvent) => {
            switch (e.key) {
                case 'Escape':
                    guarded.requestClose();
                    break;
                case 'ArrowLeft':
                    if (hasPrevious && onPrevious) onPrevious();
                    break;
                case 'ArrowRight':
                    if (hasNext && onNext) onNext();
                    break;
            }
        };

        document.addEventListener('keydown', handleKeyDown);
        return () => document.removeEventListener('keydown', handleKeyDown);
    }, [isOpen, guarded.requestClose, onNext, onPrevious, hasNext, hasPrevious]);

    // Prevent body scroll when modal is open
    useEffect(() => {
        if (isOpen) {
            document.body.style.overflow = 'hidden';
        } else {
            document.body.style.overflow = '';
        }
        return () => {
            document.body.style.overflow = '';
        };
    }, [isOpen]);

    // Handle backdrop click
    const handleBackdropClick = useCallback((e: React.MouseEvent) => {
        if (e.target === e.currentTarget) {
            guarded.requestBackdropClose();
        }
    }, [guarded]);

    // Handle error from viewers
    const handleViewerError = useCallback((errorMsg: string) => {
        setError(errorMsg);
    }, []);

    // Don't render if not open or no file
    if (!isOpen || !file) return null;

    // Render appropriate viewer based on category
    const renderViewer = () => {
        switch (category) {
            case 'image':
                return <ImageViewer file={file} onError={handleViewerError} onDirtyChange={setPreviewDirty}
                    onNext={onNext} onPrevious={onPrevious} hasNext={hasNext} hasPrevious={hasPrevious} />;

            case 'audio':
                return <AudioPlayer file={file} onError={handleViewerError} />;

            case 'video':
                return <VideoPlayer file={file} onError={handleViewerError} />;

            case 'pdf':
                return <PDFViewer file={file} onError={handleViewerError} />;

            case 'markdown':
            case 'text':
            case 'code':
                return <TextViewer file={file} onError={handleViewerError} />;

            default:
                return (
                    <div className="flex items-center justify-center h-full text-[var(--color-text-secondary)]">
                        <div className="text-center">
                            <div className="text-6xl mb-4">📁</div>
                            <div className="text-lg">{t('preview.common.notAvailable')}</div>
                            <div className="text-sm text-[var(--color-text-tertiary)] mt-2">
                                {t('preview.common.notSupported')}
                            </div>
                            {/* Let the user read any unrecognised file as plain
                                text on demand (#270 comment 17105085). */}
                            <button
                                onClick={() => setForceText(true)}
                                className="mt-5 flex items-center gap-2 mx-auto px-3 py-1.5 bg-[var(--color-bg-tertiary)] hover:bg-[var(--color-surface-hover)] text-[var(--color-text-primary)] text-sm rounded-lg transition-colors"
                            >
                                <FileText size={16} />
                                {t('preview.common.viewAsText') || 'View as text'}
                            </button>
                        </div>
                    </div>
                );
        }
    };

    return (
        <div
            className="fixed inset-0 z-50 flex items-center justify-center"
            onClick={handleBackdropClick}
        >
            {/* Backdrop */}
            <div className="absolute inset-0 bg-black/80 backdrop-blur-sm" />

            {/* Modal Container. Chrome (header / footer / frame) follows the
                active theme via CSS custom properties; the viewer paints its own
                content area (the image viewport stays black). Draggable by its
                header like the other modals (useDraggableModal). */}
            <div
                {...modalDrag.panelProps}
                className="relative w-[90vw] h-[90vh] max-w-7xl bg-[var(--color-bg-primary)] rounded-lg border border-[var(--color-border)] shadow-2xl flex flex-col overflow-hidden animate-scale-in"
            >
                {/* Header (drag handle) */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 bg-[var(--color-bg-secondary)] border-b border-[var(--color-border)] cursor-grab active:cursor-grabbing"
                >
                    {/* File info */}
                    <div className="flex items-center gap-3">
                        <span className="text-2xl">{getCategoryEmoji(category)}</span>
                        <div>
                            <h3 className="text-[var(--color-text-primary)] font-medium truncate max-w-md">
                                {file.name}
                            </h3>
                            <div className="flex items-center gap-2 text-xs text-[var(--color-text-secondary)]">
                                <span>{formatFileSize(file.size)}</span>
                                {file.isRemote && (
                                    <>
                                        <span>•</span>
                                        <span className="text-[var(--color-accent)]">{t('preview.common.remote')}</span>
                                    </>
                                )}
                            </div>
                        </div>
                    </div>

                    {/* Actions */}
                    <div className="flex items-center gap-2">
                        {/* Edit button: open plain-text/code files in the
                            AeroTools editor. Also offered once the user chose
                            to view an unrecognised file as text. */}
                        {onEdit && file && (isSourceViewable(file.name) || forceText) && (
                            <button
                                onClick={onEdit}
                                className="flex items-center gap-2 px-3 py-1.5 bg-[var(--color-bg-tertiary)] hover:bg-[var(--color-surface-hover)] text-[var(--color-text-primary)] text-sm rounded-lg transition-colors"
                                title={t('preview.common.edit') || 'Open in editor'}
                            >
                                <Pencil size={16} />
                                {t('preview.common.edit') || 'Edit'}
                            </button>
                        )}

                        {/* Download button */}
                        {onDownload && (
                            <button
                                onClick={onDownload}
                                className="flex items-center gap-2 px-3 py-1.5 bg-[var(--color-accent)] hover:bg-[var(--color-accent-hover)] text-white text-sm rounded-lg transition-colors"
                            >
                                <Download size={16} />
                                {t('preview.common.download')}
                            </button>
                        )}

                        {/* Close button */}
                        <button
                            onClick={guarded.requestClose}
                            className="p-2 hover:bg-[var(--color-bg-tertiary)] rounded-lg transition-colors"
                            title={t('preview.common.close')}
                        >
                            <X size={20} className="text-[var(--color-text-secondary)]" />
                        </button>
                    </div>
                </div>

                {/* Content area */}
                <div className="group flex-1 relative overflow-hidden">
                    {file.loading ? (
                        /* Loading skeleton: the modal opens immediately on click
                           and shows a pulsing placeholder while the bytes are
                           fetched, then swaps to the content. Far more responsive
                           than waiting for the whole download with no feedback,
                           and uses the app-wide animate-pulse idiom (#128). */
                        <div className="absolute inset-0 z-10 flex items-center justify-center p-8 bg-[var(--color-bg-primary)]">
                            <div className="w-full h-full max-w-4xl rounded-xl bg-[var(--color-bg-tertiary)] animate-pulse" />
                        </div>
                    ) : (error || file.error) ? (
                        /* Preview failure (too large / fetch error) shown inside
                           the modal, not only as a transient toast that lands in
                           the hidden activity log (#128). */
                        <div className="absolute inset-0 flex items-center justify-center bg-[var(--color-bg-primary)]/80 z-10 p-8">
                            <div className="flex flex-col items-center gap-3 text-red-400 text-center max-w-xl">
                                <div className="text-4xl">⚠️</div>
                                <span>{error || file.error}</span>
                            </div>
                        </div>
                    ) : (
                        <>
                            {/* Viewer-internal loading (e.g. decode) keeps a spinner */}
                            {isLoading && (
                                <div className="absolute inset-0 flex items-center justify-center bg-[var(--color-bg-primary)]/80 z-10">
                                    <div className="flex flex-col items-center gap-3">
                                        <div className="w-12 h-12 border-3 border-blue-500 border-t-transparent rounded-full animate-spin" />
                                        <span className="text-[var(--color-text-secondary)]">{t('preview.common.loading')}</span>
                                    </div>
                                </div>
                            )}

                            {/* Viewer. Keyed by path so each gallery slide remounts
                                fresh (zoom resets) with a light fade-in (#128). */}
                            <div key={file.path} className="absolute inset-0 animate-fade-in">
                                {renderViewer()}
                            </div>
                        </>
                    )}

                    {/* Gallery navigation: classic slideshow chevrons that fade in
                        on hover, shown only when the folder has other same-kind
                        images to page through (#128). Keyboard ← → also work. */}
                    {hasPrevious && (
                        <button
                            onClick={onPrevious}
                            className="absolute left-4 top-1/2 -translate-y-1/2 p-2 bg-[var(--color-bg-secondary)]/85 hover:bg-[var(--color-bg-tertiary)] rounded-full transition-all opacity-0 group-hover:opacity-100 z-20"
                            title={t('preview.common.previous')}
                        >
                            <ChevronLeft size={24} className="text-[var(--color-text-primary)]" />
                        </button>
                    )}
                    {hasNext && (
                        <button
                            onClick={onNext}
                            className="absolute right-4 top-1/2 -translate-y-1/2 p-2 bg-[var(--color-bg-secondary)]/85 hover:bg-[var(--color-bg-tertiary)] rounded-full transition-all opacity-0 group-hover:opacity-100 z-20"
                            title={t('preview.common.next')}
                        >
                            <ChevronRight size={24} className="text-[var(--color-text-primary)]" />
                        </button>
                    )}
                </div>

                {/* Footer with keyboard hints */}
                <div className="px-4 py-2 bg-[var(--color-bg-secondary)] border-t border-[var(--color-border)] text-xs text-[var(--color-text-tertiary)] flex justify-center gap-6">
                    <span><kbd className="px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">ESC</kbd> {t('preview.common.close')}</span>
                    {(hasNext || hasPrevious) && (
                        <span><kbd className="px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">←</kbd> <kbd className="px-1.5 py-0.5 bg-[var(--color-bg-tertiary)] rounded">→</kbd> {t('preview.common.navigate')}</span>
                    )}
                </div>
            </div>

            {guarded.confirmOpen && guarded.confirmKind && (
                <GuardedCloseConfirm
                    kind={guarded.confirmKind}
                    onKeep={guarded.cancelConfirm}
                    onConfirm={guarded.confirmAndClose}
                />
            )}

            {/* CSS Animation */}
            <style>{`
                @keyframes scale-in {
                    from { opacity: 0; transform: scale(0.95); }
                    to { opacity: 1; transform: scale(1); }
                }
                .animate-scale-in {
                    animation: scale-in 0.2s ease-out;
                }
                @keyframes fade-in {
                    from { opacity: 0; }
                    to { opacity: 1; }
                }
                .animate-fade-in {
                    animation: fade-in 0.18s ease-out;
                }
            `}</style>
        </div>
    );
};

// Helper to get emoji for category
function getCategoryEmoji(category: string): string {
    const emojis: Record<string, string> = {
        image: '🖼️',
        audio: '🎵',
        video: '🎬',
        pdf: '📄',
        markdown: '📝',
        text: '📝',
        code: '💻',
        unknown: '📁',
    };
    return emojis[category] || '📁';
}

export default UniversalPreview;
