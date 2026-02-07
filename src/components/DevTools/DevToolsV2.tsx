import React, { useState, useEffect, useRef, useCallback } from 'react';
import { Code, Terminal, MessageSquare, X, Maximize2, Minimize2, Columns2, Columns3, LayoutList } from 'lucide-react';
import { PreviewFile } from './types';
import { CodeEditor } from './CodeEditor';
import { SSHTerminal, SshConnectionInfo } from './SSHTerminal';
import { AIChat } from './AIChat';

interface DevToolsV2Props {
    isOpen: boolean;
    previewFile: PreviewFile | null;
    localPath?: string;
    remotePath?: string;
    onClose: () => void;
    onSaveFile?: (content: string, file: PreviewFile) => Promise<void>;
    onClearFile?: () => void;
    /** Monaco editor theme: 'vs' (light), 'vs-dark', or 'tokyo-night' */
    editorTheme?: 'vs' | 'vs-dark' | 'tokyo-night';
    /** SSH connection info for remote shell (when connected to SFTP) */
    sshConnection?: SshConnectionInfo | null;
    /** Active protocol type for AI context */
    providerType?: string;
    /** Connection status for AI context */
    isConnected?: boolean;
    /** Selected files for AI context */
    selectedFiles?: string[];
    /** Server hostname */
    serverHost?: string;
    /** Server port */
    serverPort?: number;
    /** Server username */
    serverUser?: string;
    /** Callback when maximize state changes */
    onMaximizeChange?: (maximized: boolean) => void;
    /** Callback to refresh file panels after AI tool mutations */
    onFileMutation?: (target: 'remote' | 'local' | 'both') => void;
}

type PanelVisibility = {
    editor: boolean;
    terminal: boolean;
    chat: boolean;
};

// Breakpoints for responsive layout (based on DevTools panel width)
const BREAKPOINTS = {
    THREE_COLS: 900,   // Show 3 columns above 900px
    TWO_COLS: 600,     // Show 2 columns above 600px
};

export const DevToolsV2: React.FC<DevToolsV2Props> = ({
    isOpen,
    previewFile,
    localPath,
    remotePath,
    onClose,
    onSaveFile,
    onClearFile,
    editorTheme = 'tokyo-night',
    sshConnection,
    providerType,
    isConnected,
    selectedFiles,
    serverHost,
    serverPort,
    serverUser,
    onMaximizeChange,
    onFileMutation,
}) => {
    const containerRef = useRef<HTMLDivElement>(null);
    const [containerWidth, setContainerWidth] = useState(window.innerWidth);
    const [isMaximized, setIsMaximized] = useState(false);
    const [height, setHeight] = useState(350);
    const isDragging = useRef(false);

    // Derive light mode from editor theme
    const isLightTheme = editorTheme === 'vs';

    // Theme-aware classes
    const theme = {
        panel: isLightTheme ? 'bg-gray-50 text-gray-900' : 'bg-gray-900 text-gray-100',
        toolbar: isLightTheme ? 'bg-gray-100 border-gray-300' : 'bg-gray-800 border-gray-700',
        border: isLightTheme ? 'border-gray-300' : 'border-gray-700',
        resizeHandle: isLightTheme ? 'bg-gray-300 hover:bg-blue-500' : 'bg-gray-700 hover:bg-blue-500',
        buttonInactive: isLightTheme ? 'text-gray-500 hover:text-gray-900 hover:bg-gray-200' : 'text-gray-400 hover:text-white hover:bg-gray-700',
        buttonHover: isLightTheme ? 'hover:bg-gray-200' : 'hover:bg-gray-700',
        text: isLightTheme ? 'text-gray-600' : 'text-gray-400',
        divider: isLightTheme ? 'bg-gray-300' : 'bg-gray-600',
        panelHeader: isLightTheme ? 'bg-gray-100/50 border-gray-300' : 'bg-gray-800/50 border-gray-700',
    };

    // Panel visibility state
    const [panels, setPanels] = useState<PanelVisibility>({
        editor: true,
        terminal: false,
        chat: true,
    });

    // Track container width for responsive layout
    useEffect(() => {
        const updateWidth = () => {
            if (containerRef.current) {
                setContainerWidth(containerRef.current.offsetWidth);
            } else {
                setContainerWidth(window.innerWidth);
            }
        };

        // Initial update
        updateWidth();

        // Listen to window resize
        window.addEventListener('resize', updateWidth);

        // Listen for menu-triggered panel toggles
        const handleMenuToggle = (e: Event) => {
            const panel = (e as CustomEvent).detail as string;
            if (panel === 'editor' || panel === 'terminal' || panel === 'agent') {
                const key = panel === 'agent' ? 'chat' : panel;
                setPanels(prev => ({ ...prev, [key]: !prev[key as keyof PanelVisibility] }));
            }
        };
        window.addEventListener('devtools-panel-toggle', handleMenuToggle);

        // Listen for ensure-visible panel events (from terminal-execute)
        const handlePanelEnsure = (e: Event) => {
            const panel = (e as CustomEvent).detail as string;
            if (panel === 'editor' || panel === 'terminal' || panel === 'agent') {
                const key = panel === 'agent' ? 'chat' : panel;
                setPanels(prev => ({ ...prev, [key]: true }));
            }
        };
        window.addEventListener('devtools-panel-ensure', handlePanelEnsure);

        // Also use ResizeObserver for container-specific changes
        let observer: ResizeObserver | null = null;
        if (containerRef.current) {
            observer = new ResizeObserver(() => updateWidth());
            observer.observe(containerRef.current);
        }

        return () => {
            window.removeEventListener('resize', updateWidth);
            window.removeEventListener('devtools-panel-toggle', handleMenuToggle);
            window.removeEventListener('devtools-panel-ensure', handlePanelEnsure);
            observer?.disconnect();
        };
    }, [isOpen]);

    // Calculate visible columns based on width and panel state
    const getVisiblePanels = useCallback((): (keyof PanelVisibility)[] => {
        const activePanels: (keyof PanelVisibility)[] = [];

        // Priority order: editor > terminal > chat
        if (panels.editor) activePanels.push('editor');
        if (panels.terminal) activePanels.push('terminal');
        if (panels.chat) activePanels.push('chat');

        // Use actual width (fallback to window width if 0)
        const width = containerWidth || window.innerWidth;

        // Limit based on responsive breakpoints
        let maxPanels = 3;
        if (width < BREAKPOINTS.TWO_COLS) {
            maxPanels = 1;
        } else if (width < BREAKPOINTS.THREE_COLS) {
            maxPanels = 2;
        }

        // Return priority-based visible panels
        return activePanels.slice(0, maxPanels);
    }, [panels, containerWidth]);

    const visiblePanels = getVisiblePanels();

    // Resize handling
    const handleMouseDown = useCallback((e: React.MouseEvent) => {
        e.preventDefault();
        isDragging.current = true;
        const startY = e.clientY;
        const startHeight = height;

        const handleMouseMove = (moveEvent: MouseEvent) => {
            if (!isDragging.current) return;
            const delta = startY - moveEvent.clientY;
            const maxHeight = window.innerHeight - 120; // Leave space for header/statusbar
            const newHeight = Math.min(maxHeight, Math.max(200, startHeight + delta));
            setHeight(newHeight);
        };

        const handleMouseUp = () => {
            isDragging.current = false;
            document.removeEventListener('mousemove', handleMouseMove);
            document.removeEventListener('mouseup', handleMouseUp);
        };

        document.addEventListener('mousemove', handleMouseMove);
        document.addEventListener('mouseup', handleMouseUp);
    }, [height]);

    const togglePanel = (panel: keyof PanelVisibility) => {
        setPanels(prev => ({ ...prev, [panel]: !prev[panel] }));
    };

    // Listen for file changes from AeroAgent tools to refresh editor
    useEffect(() => {
        const handleFileChanged = async (e: Event) => {
            const { path } = (e as CustomEvent).detail;
            if (!previewFile || !path) return;

            // Check if the changed file matches the currently open file
            const normalizedChanged = path.replace(/\\/g, '/');
            const normalizedOpen = (previewFile.path || '').replace(/\\/g, '/');

            if (normalizedChanged === normalizedOpen || normalizedOpen.endsWith(normalizedChanged) || normalizedChanged.endsWith(normalizedOpen)) {
                // Re-read the file content
                try {
                    const { readTextFile } = await import('@tauri-apps/plugin-fs');
                    const content = await readTextFile(path);
                    window.dispatchEvent(new CustomEvent('editor-reload', { detail: { path, content } }));
                } catch {
                    // File might not be readable (e.g., binary)
                }
            }
        };

        window.addEventListener('file-changed', handleFileChanged);
        return () => window.removeEventListener('file-changed', handleFileChanged);
    }, [previewFile]);

    if (!isOpen) return null;

    const panelWidth = visiblePanels.length > 0 ? `${100 / visiblePanels.length}%` : '100%';

    // Max height: leave space for header (~56px) + statusbar (~32px)
    const maxHeight = 'calc(100vh - 88px)';

    return (
        <div
            ref={containerRef}
            className={`${theme.panel} border-t ${theme.border} flex flex-col flex-shrink-0`}
            style={{
                height: isMaximized ? maxHeight : height,
                maxHeight: maxHeight
            }}
        >
            {/* Resize handle */}
            <div
                onMouseDown={handleMouseDown}
                className={`h-1.5 ${theme.resizeHandle} cursor-ns-resize transition-colors flex-shrink-0`}
            />

            {/* Toolbar */}
            <div className={`flex items-center justify-between px-3 py-1.5 ${theme.toolbar} border-b flex-shrink-0`}>
                <div className="flex items-center gap-2">
                    <span className={`text-xs font-medium ${theme.text}`}>DevTools</span>
                    <div className={`w-px h-4 ${theme.divider}`} />

                    {/* Panel toggles */}
                    <div className="flex items-center gap-1">
                        <button
                            onClick={() => togglePanel('editor')}
                            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${panels.editor
                                ? 'bg-blue-600 text-white'
                                : theme.buttonInactive
                                }`}
                            title="Toggle Editor"
                        >
                            <Code size={12} />
                            Editor
                        </button>
                        <button
                            onClick={() => togglePanel('terminal')}
                            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${panels.terminal
                                ? 'bg-green-600 text-white'
                                : theme.buttonInactive
                                }`}
                            title="Toggle Terminal"
                        >
                            <Terminal size={12} />
                            Terminal
                        </button>
                        <button
                            onClick={() => togglePanel('chat')}
                            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${panels.chat
                                ? 'bg-purple-600 text-white'
                                : theme.buttonInactive
                                }`}
                            title="Toggle Agent"
                        >
                            <MessageSquare size={12} />
                            Agent
                        </button>
                    </div>
                </div>

                <div className="flex items-center gap-1">
                    <button
                        onClick={() => { const next = !isMaximized; setIsMaximized(next); onMaximizeChange?.(next); }}
                        className={`p-1 ${theme.buttonHover} rounded transition-colors`}
                        title={isMaximized ? 'Restore' : 'Maximize'}
                    >
                        {isMaximized ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
                    </button>
                    <button
                        onClick={onClose}
                        className={`p-1 ${theme.buttonHover} rounded transition-colors`}
                        title="Close DevTools"
                    >
                        <X size={14} />
                    </button>
                </div>
            </div>

            {/* 3-Column Content Area */}
            <div className="flex-1 flex overflow-hidden">
                {visiblePanels.length === 0 ? (
                    <div className={`flex-1 flex items-center justify-center ${theme.text} text-sm`}>
                        <Columns3 size={24} className="mr-2 opacity-50" />
                        No panels active. Click the buttons above to show Editor, Terminal, or AI Chat.
                    </div>
                ) : (
                    <>
                        {visiblePanels.includes('editor') && (
                            <div
                                className={`border-r ${theme.border} flex flex-col overflow-hidden`}
                                style={{ width: panelWidth }}
                            >
                                <div className={`px-2 py-1 ${theme.panelHeader} border-b flex items-center gap-2`}>
                                    <Code size={12} className="text-blue-400" />
                                    <span className={`text-xs ${theme.text}`}>
                                        {previewFile?.name || 'No file open'}
                                    </span>
                                </div>
                                <div className="flex-1 overflow-hidden">
                                    <CodeEditor
                                        file={previewFile}
                                        onSave={async (content) => {
                                            if (onSaveFile && previewFile) {
                                                await onSaveFile(content, previewFile);
                                            }
                                        }}
                                        onClose={() => onClearFile?.()}
                                        className="h-full"
                                        theme={editorTheme}
                                        onAskAgent={(code, fileName) => {
                                            // Activate chat panel
                                            setPanels(prev => ({ ...prev, chat: true }));
                                            // Dispatch event for AIChat to pick up
                                            window.dispatchEvent(new CustomEvent('aeroagent-ask', { detail: { code, fileName } }));
                                        }}
                                    />
                                </div>
                            </div>
                        )}

                        {visiblePanels.includes('terminal') && (
                            <div
                                className={`border-r ${theme.border} flex flex-col overflow-hidden`}
                                style={{ width: panelWidth }}
                            >
                                <div className={`px-2 py-1 ${theme.panelHeader} border-b flex items-center gap-2`}>
                                    <Terminal size={12} className="text-green-400" />
                                    <span className={`text-xs ${theme.text}`}>Terminal</span>
                                </div>
                                <div className="flex-1 overflow-hidden bg-gray-900">
                                    {/* Terminal always stays dark - traditional */}
                                    <SSHTerminal className="h-full" localPath={localPath} sshConnection={sshConnection} />
                                </div>
                            </div>
                        )}

                        {visiblePanels.includes('chat') && (
                            <div
                                className="flex flex-col overflow-hidden"
                                style={{ width: panelWidth }}
                            >
                                <div className={`px-2 py-1 ${theme.panelHeader} border-b flex items-center gap-2`}>
                                    <MessageSquare size={12} className="text-purple-400" />
                                    <span className={`text-xs ${theme.text}`}>AI Assistant</span>
                                </div>
                                <div className="flex-1 overflow-hidden">
                                    <AIChat className="h-full" remotePath={remotePath} localPath={localPath} isLightTheme={isLightTheme} providerType={providerType} isConnected={isConnected} selectedFiles={selectedFiles} serverHost={serverHost} serverPort={serverPort} serverUser={serverUser} onFileMutation={onFileMutation} editorFileName={previewFile?.name} editorFilePath={previewFile?.path} />
                                </div>
                            </div>
                        )}
                    </>
                )}
            </div>
        </div >
    );
};

export default DevToolsV2;
