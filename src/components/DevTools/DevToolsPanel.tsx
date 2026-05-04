// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useState, useRef } from 'react';
import { Code, Terminal, Edit3, ChevronDown, ChevronUp, X, Maximize2, Minimize2, MessageSquare, FileX } from 'lucide-react';
import { DevToolsTab, PreviewFile } from './types';
import { FilePreview } from './FilePreview';
import { CodeEditor } from './CodeEditor';
import { SSHTerminal } from './SSHTerminal';
import { AIChat } from './AIChat';
import { useTranslation } from '../../i18n';
import { usePointerDrag } from '../../hooks/usePointerDrag';

interface DevToolsPanelProps {
    isOpen: boolean;
    previewFile: PreviewFile | null;
    localPath?: string;
    remotePath?: string;
    onClose: () => void;
    onToggle: () => void;
    onSaveFile?: (content: string, file: PreviewFile) => Promise<void>;
    onClearPreview?: () => void;
}

const DEFAULT_HEIGHT = 300;
const MIN_HEIGHT = 150;
const MAX_HEIGHT = 1200;

export const DevToolsPanel: React.FC<DevToolsPanelProps> = ({
    isOpen,
    previewFile,
    localPath,
    remotePath,
    onClose,
    onToggle,
    onSaveFile,
    onClearPreview,
}) => {
    const t = useTranslation();
    const [activeTab, setActiveTab] = useState<DevToolsTab>('preview');
    const [height, setHeight] = useState(DEFAULT_HEIGHT);
    const [isMaximized, setIsMaximized] = useState(false);
    const resizeRef = useRef<HTMLDivElement>(null);

    // Pointer-based resize drag. usePointerDrag captures on the resize handle
    // so there are no `document.addEventListener('mousemove'...)` globals to
    // leak on unmount mid-drag, and Pointer Events cover touch/stylus.
    const dragStartRef = useRef<{ y: number; startHeight: number } | null>(null);
    const { onPointerDown: onResizePointerDown } = usePointerDrag({
        onPointerMove: (e) => {
            const start = dragStartRef.current;
            if (!start) return;
            const delta = start.y - e.clientY;
            setHeight(Math.min(MAX_HEIGHT, Math.max(MIN_HEIGHT, start.startHeight + delta)));
        },
        onPointerUp: () => { dragStartRef.current = null; },
        onPointerCancel: () => { dragStartRef.current = null; },
    });
    const handlePointerDown = (e: React.PointerEvent<HTMLDivElement>) => {
        e.preventDefault();
        dragStartRef.current = { y: e.clientY, startHeight: height };
        onResizePointerDown(e);
    };

    const toggleMaximize = () => {
        if (isMaximized) {
            setHeight(DEFAULT_HEIGHT);
        } else {
            // Use almost full viewport height (leave room for header/statusbar)
            setHeight(window.innerHeight - 180);
        }
        setIsMaximized(!isMaximized);
    };

    const tabs: { id: DevToolsTab; label: string; icon: React.ReactNode; available: boolean }[] = [
        { id: 'preview', label: t('devtools.preview'), icon: <Code size={14} />, available: true },
        { id: 'editor', label: t('devtools.editor'), icon: <Edit3 size={14} />, available: true },
        { id: 'terminal', label: t('devtools.terminal'), icon: <Terminal size={14} />, available: true },
        { id: 'chat', label: t('devtools.agent'), icon: <MessageSquare size={14} />, available: true },
    ];

    if (!isOpen) {
        return null;
    }

    return (
        <div
            className="bg-gray-900 text-gray-100 border-t border-gray-700 flex flex-col"
            style={{ height: isMaximized ? MAX_HEIGHT : height }}
        >
            {/* Resize handle */}
            <div
                ref={resizeRef}
                onPointerDown={handlePointerDown}
                className="h-1 bg-gray-700 hover:bg-blue-500 cursor-ns-resize transition-colors"
            />

            {/* Header with tabs */}
            <div className="flex items-center justify-between px-2 py-1.5 bg-gray-800 border-b border-gray-700">
                <div className="flex items-center gap-1">
                    <button
                        onClick={onToggle}
                        className="p-1 hover:bg-gray-700 rounded transition-colors"
                        title={isOpen ? t('common.close') : t('devtools.title')}
                    >
                        {isOpen ? <ChevronDown size={14} /> : <ChevronUp size={14} />}
                    </button>

                    {/* Tabs */}
                    <div className="flex items-center gap-0.5 ml-2">
                        {tabs.map((tab) => (
                            <button
                                key={tab.id}
                                onClick={() => tab.available && setActiveTab(tab.id)}
                                disabled={!tab.available}
                                className={`flex items-center gap-1.5 px-3 py-1 rounded text-xs transition-colors ${activeTab === tab.id
                                    ? 'bg-gray-700 text-white'
                                    : tab.available
                                        ? 'text-gray-400 hover:text-white hover:bg-gray-700/50'
                                        : 'text-gray-600 cursor-not-allowed'
                                    }`}
                                title={!tab.available ? 'Coming soon' : tab.label}
                            >
                                {tab.icon}
                                {tab.label}
                                {!tab.available && <span className="text-[10px] text-gray-500 ml-1">Soon</span>}
                            </button>
                        ))}
                    </div>
                </div>

                <div className="flex items-center gap-1">
                    {/* Clear File Button */}
                    {previewFile && onClearPreview && (
                        <button
                            onClick={onClearPreview}
                            className="p-1 hover:bg-gray-700 rounded transition-colors text-gray-400 hover:text-white"
                            title={t('devtools.noFileSelected')}
                        >
                            <FileX size={14} />
                        </button>
                    )}
                    <button
                        onClick={toggleMaximize}
                        className="p-1 hover:bg-gray-700 rounded transition-colors"
                        title={isMaximized ? 'Restore' : 'Maximize'}
                    >
                        {isMaximized ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
                    </button>
                    <button
                        onClick={onClose}
                        className="p-1 hover:bg-gray-700 rounded transition-colors"
                        title={t('common.close')}
                    >
                        <X size={14} />
                    </button>
                </div>
            </div>

            {/* Content area */}
            <div className="flex-1 overflow-hidden">
                {activeTab === 'preview' && (
                    <FilePreview file={previewFile} className="h-full" />
                )}
                {activeTab === 'editor' && (
                    <CodeEditor
                        file={previewFile}
                        onSave={async (content) => {
                            if (onSaveFile && previewFile) {
                                await onSaveFile(content, previewFile);
                            }
                        }}
                        onClose={() => setActiveTab('preview')}
                        className="h-full"
                    />
                )}
                {activeTab === 'terminal' && (
                    <SSHTerminal className="h-full" localPath={localPath} />
                )}
                {activeTab === 'chat' && (
                    <AIChat className="h-full" remotePath={remotePath} localPath={localPath} />
                )}
            </div>
        </div>
    );
};

export default DevToolsPanel;
