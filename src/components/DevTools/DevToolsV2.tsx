import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { Code, Terminal, MessageSquare, X, Maximize2, Minimize2, Columns2, Columns3, LayoutList } from 'lucide-react';
import { PreviewFile } from './types';
import { CodeEditor } from './CodeEditor';
import { SSHTerminal, SshConnectionInfo } from './SSHTerminal';
import { AIChat } from './AIChat';
import { useTranslation } from '../../i18n';
import type { EffectiveTheme } from '../../hooks/useTheme';

interface DevToolsV2Props {
    isOpen: boolean;
    previewFile: PreviewFile | null;
    localPath?: string;
    remotePath?: string;
    onClose: () => void;
    onSaveFile?: (content: string, file: PreviewFile) => Promise<void>;
    onClearFile?: () => void;
    /** Monaco editor theme: 'vs' (light), 'vs-dark', 'tokyo-night', or 'cyber' */
    editorTheme?: 'vs' | 'vs-dark' | 'tokyo-night' | 'cyber';
    /** App-level theme for DevTools panel styling */
    appTheme?: EffectiveTheme;
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
    /** Which file panel the user is currently focused on */
    activeFilePanel?: 'remote' | 'local';
    /** Whether the remote connection is via AeroCloud (vs manual server) */
    isCloudConnection?: boolean;
    /** Callback when maximize state changes */
    onMaximizeChange?: (maximized: boolean) => void;
    /** Callback to refresh file panels after AI tool mutations */
    onFileMutation?: (target: 'remote' | 'local' | 'both') => void;
    /** SEC-P1-06: TOFU host key check before SSH shell open */
    onCheckHostKey?: (host: string, port: number) => Promise<boolean>;
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
    appTheme = 'dark',
    sshConnection,
    providerType,
    isConnected,
    selectedFiles,
    serverHost,
    serverPort,
    serverUser,
    activeFilePanel,
    isCloudConnection,
    onMaximizeChange,
    onFileMutation,
    onCheckHostKey,
}) => {
    const t = useTranslation();
    const containerRef = useRef<HTMLDivElement>(null);
    const [containerWidth, setContainerWidth] = useState(window.innerWidth);
    const [isMaximized, setIsMaximized] = useState(false);
    const [height, setHeight] = useState(500);
    const isDragging = useRef(false);
    const isHDragging = useRef(false);
    const [panelRatios, setPanelRatios] = useState<number[]>([]);

    // Theme-aware classes based on appTheme
    const isLightTheme = appTheme === 'light';
    const theme = useMemo(() => {
        switch (appTheme) {
            case 'light': return {
                panel: 'bg-gray-50 text-gray-900',
                toolbar: 'bg-gray-100 border-gray-300',
                border: 'border-gray-300',
                resizeHandle: 'bg-gray-300 hover:bg-blue-500',
                resizeBar: 'bg-gray-400 group-hover:bg-white',
                buttonInactive: 'text-gray-500 hover:text-gray-900 hover:bg-gray-200',
                buttonHover: 'hover:bg-gray-200',
                text: 'text-gray-600',
                divider: 'bg-gray-300',
                panelHeader: 'bg-gray-100/50 border-gray-300',
            };
            case 'tokyo': return {
                panel: 'bg-[#1a1b26] text-[#c0caf5]',
                toolbar: 'bg-[#16161e] border-[#292e42]',
                border: 'border-[#292e42]',
                resizeHandle: 'bg-[#292e42] hover:bg-[#7aa2f7]',
                resizeBar: 'bg-[#414868] group-hover:bg-[#7aa2f7]',
                buttonInactive: 'text-[#565f89] hover:text-[#c0caf5] hover:bg-[#292e42]',
                buttonHover: 'hover:bg-[#292e42]',
                text: 'text-[#565f89]',
                divider: 'bg-[#292e42]',
                panelHeader: 'bg-[#16161e]/50 border-[#292e42]',
            };
            case 'cyber': return {
                panel: 'bg-[#0a0e17] text-emerald-100',
                toolbar: 'bg-[#0d1117] border-emerald-900/40',
                border: 'border-emerald-900/40',
                resizeHandle: 'bg-[#0d1117] hover:bg-emerald-500',
                resizeBar: 'bg-emerald-800/60 group-hover:bg-emerald-400',
                buttonInactive: 'text-gray-500 hover:text-emerald-300 hover:bg-emerald-500/10',
                buttonHover: 'hover:bg-emerald-500/10',
                text: 'text-emerald-600/80',
                divider: 'bg-emerald-900/40',
                panelHeader: 'bg-[#0d1117]/50 border-emerald-900/40',
            };
            default: return { // dark
                panel: 'bg-gray-900 text-gray-100',
                toolbar: 'bg-gray-800 border-gray-700',
                border: 'border-gray-700',
                resizeHandle: 'bg-gray-700 hover:bg-blue-500',
                resizeBar: 'bg-gray-500 group-hover:bg-blue-400',
                buttonInactive: 'text-gray-400 hover:text-white hover:bg-gray-700',
                buttonHover: 'hover:bg-gray-700',
                text: 'text-gray-400',
                divider: 'bg-gray-600',
                panelHeader: 'bg-gray-800/50 border-gray-700',
            };
        }
    }, [appTheme]);

    // Panel visibility state
    const [panels, setPanels] = useState<PanelVisibility>({
        editor: true,
        terminal: false,
        chat: false,
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
    const visiblePanelsKey = visiblePanels.join(',');

    // Reset panel width ratios when visible panels change
    useEffect(() => {
        const count = visiblePanels.length;
        setPanelRatios(count > 0 ? Array(count).fill(1 / count) : []);
    }, [visiblePanelsKey]);

    // Get CSS width for a panel based on its ratio
    const getPanelWidth = (panel: keyof PanelVisibility): string => {
        const idx = visiblePanels.indexOf(panel);
        if (idx < 0) return '0%';
        const ratio = panelRatios.length === visiblePanels.length ? panelRatios[idx] : (1 / visiblePanels.length);
        return `${(ratio ?? (1 / visiblePanels.length)) * 100}%`;
    };

    // Check if a panel is not the last visible (for placing resize handle after it)
    const isNotLastVisible = (panel: keyof PanelVisibility): boolean => {
        const idx = visiblePanels.indexOf(panel);
        return idx >= 0 && idx < visiblePanels.length - 1;
    };

    // Vertical resize handling
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

    // Horizontal panel resize handler
    const handlePanelResize = useCallback((handleIndex: number) => (e: React.MouseEvent) => {
        e.preventDefault();
        isHDragging.current = true;
        const startX = e.clientX;
        const startRatios = [...panelRatios];
        const containerW = containerRef.current?.offsetWidth || window.innerWidth;
        const MIN_RATIO = 0.15;

        document.body.style.userSelect = 'none';
        document.body.style.cursor = 'col-resize';

        const onMove = (moveEvent: MouseEvent) => {
            if (!isHDragging.current) return;
            const deltaPct = (moveEvent.clientX - startX) / containerW;

            let left = startRatios[handleIndex] + deltaPct;
            let right = startRatios[handleIndex + 1] - deltaPct;

            if (left < MIN_RATIO) { right += left - MIN_RATIO; left = MIN_RATIO; }
            if (right < MIN_RATIO) { left += right - MIN_RATIO; right = MIN_RATIO; }

            const newRatios = [...startRatios];
            newRatios[handleIndex] = left;
            newRatios[handleIndex + 1] = right;
            setPanelRatios(newRatios);
        };

        const onUp = () => {
            isHDragging.current = false;
            document.body.style.userSelect = '';
            document.body.style.cursor = '';
            document.removeEventListener('mousemove', onMove);
            document.removeEventListener('mouseup', onUp);
        };

        document.addEventListener('mousemove', onMove);
        document.addEventListener('mouseup', onUp);
    }, [panelRatios]);

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

    // Keep mounted after first open to preserve AIChat state (history, conversations)
    const hasBeenOpenedRef = useRef(false);
    if (isOpen) hasBeenOpenedRef.current = true;
    if (!hasBeenOpenedRef.current) return null;

    // Max height: leave space for header (~56px) + statusbar (~32px)
    const maxHeight = 'calc(100vh - 88px)';

    return (
        <div
            ref={containerRef}
            className={`${theme.panel} border-t ${theme.border} flex flex-col flex-shrink-0 ${!isOpen ? 'hidden' : ''}`}
            style={{
                height: isMaximized ? maxHeight : height,
                maxHeight: maxHeight
            }}
        >
            {/* Resize handle */}
            <div
                onMouseDown={handleMouseDown}
                className={`h-2 ${theme.resizeHandle} cursor-ns-resize transition-colors flex-shrink-0 flex items-center justify-center group`}
            >
                <div className={`w-10 h-0.5 rounded-full ${theme.resizeBar} transition-colors`} />
            </div>

            {/* Toolbar */}
            <div className={`flex items-center justify-between px-3 py-1.5 ${theme.toolbar} border-b flex-shrink-0`}>
                <div className="flex items-center gap-2">
                    <span className={`text-xs font-medium ${theme.text}`}>{t('devtools.title')}</span>
                    <div className={`w-px h-4 ${theme.divider}`} />

                    {/* Panel toggles */}
                    <div className="flex items-center gap-1">
                        <button
                            onClick={() => togglePanel('editor')}
                            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${panels.editor
                                ? 'bg-blue-600 text-white'
                                : theme.buttonInactive
                                }`}
                            title={t('devtools.toggleEditor')}
                        >
                            <Code size={12} />
                            {t('devtools.editor')}
                        </button>
                        <button
                            onClick={() => togglePanel('terminal')}
                            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${panels.terminal
                                ? 'bg-green-600 text-white'
                                : theme.buttonInactive
                                }`}
                            title={t('devtools.toggleTerminal')}
                        >
                            <Terminal size={12} />
                            {t('devtools.terminal')}
                        </button>
                        <button
                            onClick={() => togglePanel('chat')}
                            className={`flex items-center gap-1.5 px-2 py-1 rounded text-xs transition-colors ${panels.chat
                                ? 'bg-purple-600 text-white'
                                : theme.buttonInactive
                                }`}
                            title={t('devtools.toggleAgent')}
                        >
                            <MessageSquare size={12} />
                            {t('devtools.agent')}
                        </button>
                    </div>
                </div>

                <div className="flex items-center gap-1">
                    <button
                        onClick={() => { const next = !isMaximized; setIsMaximized(next); onMaximizeChange?.(next); }}
                        className={`p-1 ${theme.buttonHover} rounded transition-colors`}
                        title={isMaximized ? t('devtools.restore') : t('devtools.maximize')}
                    >
                        {isMaximized ? <Minimize2 size={14} /> : <Maximize2 size={14} />}
                    </button>
                    <button
                        onClick={onClose}
                        className={`p-1 ${theme.buttonHover} rounded transition-colors`}
                        title={t('devtools.closeDevTools')}
                    >
                        <X size={14} />
                    </button>
                </div>
            </div>

            {/* Resizable 3-Column Content Area */}
            <div className="flex-1 flex overflow-hidden">
                {visiblePanels.length === 0 ? (
                    <div className={`flex-1 flex items-center justify-center ${theme.text} text-sm`}>
                        <Columns3 size={24} className="mr-2 opacity-50" />
                        {t('devtools.noPanelsActive')}
                    </div>
                ) : (
                    <>
                        {visiblePanels.includes('editor') && (
                            <div
                                className="flex flex-col overflow-hidden"
                                style={{ width: getPanelWidth('editor') }}
                            >
                                <div className={`px-2 py-1 ${theme.panelHeader} border-b flex items-center gap-2`}>
                                    <Code size={12} className="text-blue-400" />
                                    <span className={`text-xs ${theme.text}`}>
                                        {previewFile?.name || t('devtools.noFileOpen')}
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
                                            setPanels(prev => ({ ...prev, chat: true }));
                                            window.dispatchEvent(new CustomEvent('aeroagent-ask', { detail: { code, fileName } }));
                                        }}
                                    />
                                </div>
                            </div>
                        )}

                        {/* Resize handle after editor */}
                        {visiblePanels.includes('editor') && isNotLastVisible('editor') && (
                            <div
                                onMouseDown={handlePanelResize(visiblePanels.indexOf('editor'))}
                                className={`w-1 cursor-col-resize ${theme.resizeHandle} transition-colors flex-shrink-0 group flex items-center justify-center`}
                            >
                                <div className={`w-0.5 h-8 rounded-full ${theme.resizeBar} transition-opacity`} />
                            </div>
                        )}

                        {visiblePanels.includes('terminal') && (
                            <div
                                className="flex flex-col overflow-hidden"
                                style={{ width: getPanelWidth('terminal') }}
                            >
                                <div className={`px-2 py-1 ${theme.panelHeader} border-b flex items-center gap-2`}>
                                    <Terminal size={12} className="text-green-400" />
                                    <span className={`text-xs ${theme.text}`}>{t('devtools.terminal')}</span>
                                </div>
                                <div className="flex-1 overflow-hidden bg-gray-900">
                                    <SSHTerminal className="h-full" localPath={localPath} sshConnection={sshConnection} appTheme={appTheme} onCheckHostKey={onCheckHostKey} />
                                </div>
                            </div>
                        )}

                        {/* Resize handle after terminal */}
                        {visiblePanels.includes('terminal') && isNotLastVisible('terminal') && (
                            <div
                                onMouseDown={handlePanelResize(visiblePanels.indexOf('terminal'))}
                                className={`w-1 cursor-col-resize ${theme.resizeHandle} transition-colors flex-shrink-0 group flex items-center justify-center`}
                            >
                                <div className={`w-0.5 h-8 rounded-full ${theme.resizeBar} transition-opacity`} />
                            </div>
                        )}

                        {/* Chat panel — always mounted to preserve AIChat state */}
                        <div
                            className={`flex flex-col overflow-hidden ${visiblePanels.includes('chat') ? '' : 'hidden'}`}
                            style={{ width: visiblePanels.includes('chat') ? getPanelWidth('chat') : undefined }}
                        >
                            <div className={`px-2 py-1 ${theme.panelHeader} border-b flex items-center gap-2`}>
                                <MessageSquare size={12} className="text-purple-400" />
                                <span className={`text-xs ${theme.text}`}>{t('devtools.agent')}</span>
                            </div>
                            <div className="flex-1 overflow-hidden">
                                <AIChat className="h-full" remotePath={remotePath} localPath={localPath} appTheme={appTheme} providerType={providerType} isConnected={isConnected} selectedFiles={selectedFiles} serverHost={serverHost} serverPort={serverPort} serverUser={serverUser} activeFilePanel={activeFilePanel} isCloudConnection={isCloudConnection} onFileMutation={onFileMutation} editorFileName={previewFile?.name} editorFilePath={previewFile?.path} />
                            </div>
                        </div>
                    </>
                )}
            </div>
        </div >
    );
};

export default DevToolsV2;
