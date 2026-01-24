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
import { X, Download, ChevronLeft, ChevronRight, ExternalLink } from 'lucide-react';
import { UniversalPreviewProps, PreviewFileData } from './types';
import { getPreviewCategory, formatFileSize, getCategoryIcon } from './utils/fileTypes';
import { ImageViewer } from './viewers/ImageViewer';
import { AudioPlayer } from './viewers/AudioPlayer';
import { VideoPlayer } from './viewers/VideoPlayer';
import { PDFViewer } from './viewers/PDFViewer';
import { TextViewer } from './viewers/TextViewer';
// Future imports:
// import { MarkdownViewer } from './viewers/MarkdownViewer';
// import { TextViewer } from './viewers/TextViewer';

export const UniversalPreview: React.FC<UniversalPreviewProps> = ({
    isOpen,
    file,
    onClose,
    onDownload,
    onNext,
    onPrevious,
    hasNext,
    hasPrevious,
}) => {
    const [isLoading, setIsLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);

    // Reset error when file changes
    useEffect(() => {
        setError(null);
        setIsLoading(false);
    }, [file?.name, file?.path]);

    // Determine file category
    const category = file ? getPreviewCategory(file.name) : 'unknown';

    // Handle keyboard navigation
    useEffect(() => {
        if (!isOpen) return;

        const handleKeyDown = (e: KeyboardEvent) => {
            switch (e.key) {
                case 'Escape':
                    onClose();
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
    }, [isOpen, onClose, onNext, onPrevious, hasNext, hasPrevious]);

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
            onClose();
        }
    }, [onClose]);

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
                return <ImageViewer file={file} onError={handleViewerError} />;

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
                    <div className="flex items-center justify-center h-full text-gray-400">
                        <div className="text-center">
                            <div className="text-6xl mb-4">📁</div>
                            <div className="text-lg">Preview not available</div>
                            <div className="text-sm text-gray-500 mt-2">
                                This file type is not supported for preview
                            </div>
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

            {/* Modal Container */}
            <div className="relative w-[90vw] h-[90vh] max-w-7xl bg-gray-900 rounded-xl border border-gray-700 shadow-2xl flex flex-col overflow-hidden animate-scale-in">
                {/* Header */}
                <div className="flex items-center justify-between px-4 py-3 bg-gray-800 border-b border-gray-700">
                    {/* File info */}
                    <div className="flex items-center gap-3">
                        <span className="text-2xl">{getCategoryEmoji(category)}</span>
                        <div>
                            <h3 className="text-white font-medium truncate max-w-md">
                                {file.name}
                            </h3>
                            <div className="flex items-center gap-2 text-xs text-gray-400">
                                <span>{formatFileSize(file.size)}</span>
                                {file.isRemote && (
                                    <>
                                        <span>•</span>
                                        <span className="text-blue-400">Remote</span>
                                    </>
                                )}
                            </div>
                        </div>
                    </div>

                    {/* Actions */}
                    <div className="flex items-center gap-2">
                        {/* Download button */}
                        {onDownload && (
                            <button
                                onClick={onDownload}
                                className="flex items-center gap-2 px-3 py-1.5 bg-blue-600 hover:bg-blue-500 text-white text-sm rounded-lg transition-colors"
                            >
                                <Download size={16} />
                                Download
                            </button>
                        )}

                        {/* Close button */}
                        <button
                            onClick={onClose}
                            className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
                            title="Close (ESC)"
                        >
                            <X size={20} className="text-gray-400" />
                        </button>
                    </div>
                </div>

                {/* Content area */}
                <div className="flex-1 relative overflow-hidden">
                    {/* Loading overlay */}
                    {isLoading && (
                        <div className="absolute inset-0 flex items-center justify-center bg-gray-900/80 z-10">
                            <div className="flex flex-col items-center gap-3">
                                <div className="w-12 h-12 border-3 border-blue-500 border-t-transparent rounded-full animate-spin" />
                                <span className="text-gray-400">Loading...</span>
                            </div>
                        </div>
                    )}

                    {/* Error overlay */}
                    {error && (
                        <div className="absolute inset-0 flex items-center justify-center bg-gray-900/80 z-10">
                            <div className="flex flex-col items-center gap-3 text-red-400">
                                <div className="text-4xl">⚠️</div>
                                <span>{error}</span>
                            </div>
                        </div>
                    )}

                    {/* Viewer */}
                    {renderViewer()}

                    {/* Navigation arrows (for gallery mode) */}
                    {hasPrevious && (
                        <button
                            onClick={onPrevious}
                            className="absolute left-4 top-1/2 -translate-y-1/2 p-2 bg-gray-800/80 hover:bg-gray-700 rounded-full transition-colors"
                            title="Previous (←)"
                        >
                            <ChevronLeft size={24} className="text-white" />
                        </button>
                    )}
                    {hasNext && (
                        <button
                            onClick={onNext}
                            className="absolute right-4 top-1/2 -translate-y-1/2 p-2 bg-gray-800/80 hover:bg-gray-700 rounded-full transition-colors"
                            title="Next (→)"
                        >
                            <ChevronRight size={24} className="text-white" />
                        </button>
                    )}
                </div>

                {/* Footer with keyboard hints */}
                <div className="px-4 py-2 bg-gray-800 border-t border-gray-700 text-xs text-gray-500 flex justify-center gap-6">
                    <span><kbd className="px-1.5 py-0.5 bg-gray-700 rounded">ESC</kbd> Close</span>
                    {(hasNext || hasPrevious) && (
                        <span><kbd className="px-1.5 py-0.5 bg-gray-700 rounded">←</kbd> <kbd className="px-1.5 py-0.5 bg-gray-700 rounded">→</kbd> Navigate</span>
                    )}
                </div>
            </div>

            {/* CSS Animation */}
            <style>{`
                @keyframes scale-in {
                    from { opacity: 0; transform: scale(0.95); }
                    to { opacity: 1; transform: scale(1); }
                }
                .animate-scale-in {
                    animation: scale-in 0.2s ease-out;
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
