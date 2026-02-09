import React, { useRef, useState, useEffect } from 'react';
import Editor, { OnMount, loader } from '@monaco-editor/react';
import { Save, X, RotateCcw, FileCode } from 'lucide-react';
import { PreviewFile, getFileLanguage } from './types';

// Use locally copied Monaco AMD assets (min/vs/) served by tauri-plugin-localhost.
// AMD workers are IIFE format — no ESM import issues in WebKitGTK workers.
// A Vite plugin copies node_modules/monaco-editor/min/vs → dist/vs at build time.
loader.config({ paths: { vs: '/vs' } });

// Tokyo Night theme colors - in honor of Antigravity! 🌃
const tokyoNightTheme = {
    base: 'vs-dark' as const,
    inherit: true,
    rules: [
        { token: 'comment', foreground: '565f89', fontStyle: 'italic' },
        { token: 'keyword', foreground: '9d7cd8' },
        { token: 'string', foreground: '9ece6a' },
        { token: 'number', foreground: 'ff9e64' },
        { token: 'type', foreground: '2ac3de' },
        { token: 'function', foreground: '7aa2f7' },
        { token: 'variable', foreground: 'c0caf5' },
        { token: 'constant', foreground: 'ff9e64' },
        { token: 'tag', foreground: 'f7768e' },
        { token: 'attribute.name', foreground: '73daca' },
        { token: 'attribute.value', foreground: '9ece6a' },
        { token: 'delimiter', foreground: '89ddff' },
        { token: 'operator', foreground: '89ddff' },
    ],
    colors: {
        'editor.background': '#1a1b26',
        'editor.foreground': '#c0caf5',
        'editorLineNumber.foreground': '#3b4261',
        'editorLineNumber.activeForeground': '#737aa2',
        'editor.selectionBackground': '#33467c',
        'editor.lineHighlightBackground': '#1e2030',
        'editorCursor.foreground': '#c0caf5',
        'editorWhitespace.foreground': '#3b4261',
        'editorIndentGuide.background': '#3b4261',
        'editor.selectionHighlightBackground': '#3d59a1',
        'editorBracketMatch.background': '#3d59a166',
        'editorBracketMatch.border': '#3d59a1',
    },
};

type EditorTheme = 'vs' | 'vs-dark' | 'tokyo-night';

interface CodeEditorProps {
    file: PreviewFile | null;
    onSave: (content: string) => Promise<void>;
    onClose: () => void;
    className?: string;
    /** Monaco theme: 'vs' (light), 'vs-dark', or 'tokyo-night' */
    theme?: EditorTheme;
    /** Send selected code to AeroAgent AI chat */
    onAskAgent?: (code: string, fileName: string) => void;
}

export const CodeEditor: React.FC<CodeEditorProps> = ({
    file,
    onSave,
    onClose,
    className = '',
    theme: themeProp = 'tokyo-night',
    onAskAgent,
}) => {
    const editorRef = useRef<any>(null);
    const monacoRef = useRef<any>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const onAskAgentRef = useRef(onAskAgent);
    const fileRef = useRef(file);
    const [isSaving, setIsSaving] = useState(false);
    const [hasChanges, setHasChanges] = useState(false);
    const [originalContent, setOriginalContent] = useState('');
    const theme = themeProp; // Use prop directly

    // Keep refs in sync so Monaco closure always has latest values
    useEffect(() => {
        onAskAgentRef.current = onAskAgent;
        fileRef.current = file;
    }, [onAskAgent, file]);

    const handleEditorDidMount: OnMount = (editor, monaco) => {
        editorRef.current = editor;
        monacoRef.current = monaco;

        // Define Tokyo Night theme
        monaco.editor.defineTheme('tokyo-night', tokyoNightTheme);
        monaco.editor.setTheme(theme);

        if (file) {
            setOriginalContent(file.content);
        }

        // "Ask AeroAgent" context menu action
        editor.addAction({
            id: 'ask-aeroagent',
            label: 'Ask AeroAgent',
            contextMenuGroupId: '9_aeroagent',
            contextMenuOrder: 1,
            keybindings: [monaco.KeyMod.CtrlCmd | monaco.KeyMod.Shift | monaco.KeyCode.KeyA],
            run: (ed) => {
                const selection = ed.getSelection();
                const selectedText = selection ? ed.getModel()?.getValueInRange(selection) : '';
                const code = selectedText || ed.getValue();
                const fileName = fileRef.current?.name || 'unknown';
                if (onAskAgentRef.current) {
                    onAskAgentRef.current(code, fileName);
                }
            },
        });

        // Force layout update after mount to fix rendering issues
        setTimeout(() => {
            editor.layout();
            editor.focus();
        }, 100);
    };

    // Force layout update when container might have changed
    useEffect(() => {
        const editor = editorRef.current;
        if (editor) {
            // Small delay to ensure container is fully rendered
            const timer = setTimeout(() => {
                editor.layout();
            }, 50);
            return () => clearTimeout(timer);
        }
    }, [file]);

    // Apply theme when changed with layout refresh
    useEffect(() => {
        if (monacoRef.current && editorRef.current) {
            monacoRef.current.editor.setTheme(theme);
            // Force layout update after theme change
            setTimeout(() => {
                editorRef.current?.layout();
            }, 50);
        }
    }, [theme]);

    // Listen for editor-insert events from AeroAgent code block actions
    useEffect(() => {
        const handleEditorInsert = (e: Event) => {
            const { code } = (e as CustomEvent).detail;
            const editor = editorRef.current;
            const monaco = monacoRef.current;
            if (!editor || !monaco || !code) return;

            const position = editor.getPosition();
            if (position) {
                editor.executeEdits('aeroagent-insert', [{
                    range: new monaco.Range(position.lineNumber, position.column, position.lineNumber, position.column),
                    text: code,
                    forceMoveMarkers: true,
                }]);
                editor.focus();
            }
        };

        window.addEventListener('editor-insert', handleEditorInsert);
        return () => window.removeEventListener('editor-insert', handleEditorInsert);
    }, []);

    // ResizeObserver for container size changes
    useEffect(() => {
        const container = containerRef.current;
        if (!container) return;

        const resizeObserver = new ResizeObserver(() => {
            // Read ref inside callback — not the stale closure value
            const ed = editorRef.current;
            if (ed) {
                requestAnimationFrame(() => {
                    ed.layout();
                });
            }
        });

        resizeObserver.observe(container);
        return () => resizeObserver.disconnect();
    }, []);

    // Listen for editor-reload events (from Agent -> Monaco live sync)
    useEffect(() => {
        const handleReload = (e: Event) => {
            const { path, content } = (e as CustomEvent).detail;
            if (!file || !path) return;

            // Check if this reload is for the currently open file
            const normalizedPath = path.replace(/\\/g, '/');
            const normalizedFile = (file.path || file.name).replace(/\\/g, '/');

            if (normalizedPath === normalizedFile || normalizedFile.endsWith(normalizedPath.split('/').pop() || '') || normalizedPath.endsWith(normalizedFile.split('/').pop() || '')) {
                const editor = editorRef.current;
                if (editor) {
                    // Save cursor position
                    const position = editor.getPosition();
                    // Update content
                    editor.setValue(content);
                    setOriginalContent(content);
                    setHasChanges(false);
                    // Restore cursor position
                    if (position) {
                        editor.setPosition(position);
                        editor.revealPositionInCenter(position);
                    }
                }
            }
        };

        window.addEventListener('editor-reload', handleReload);
        return () => window.removeEventListener('editor-reload', handleReload);
    }, [file]);

    const handleChange = (value: string | undefined) => {
        if (value !== undefined && value !== originalContent) {
            setHasChanges(true);
        } else {
            setHasChanges(false);
        }
    };

    const handleSave = async () => {
        if (!editorRef.current) return;

        const content = editorRef.current.getValue();
        setIsSaving(true);

        try {
            await onSave(content);
            setOriginalContent(content);
            setHasChanges(false);
        } finally {
            setIsSaving(false);
        }
    };

    const handleReset = () => {
        if (editorRef.current && originalContent) {
            editorRef.current.setValue(originalContent);
            setHasChanges(false);
        }
    };

    const language = file ? getFileLanguage(file.name) : 'text';

    // Map our language names to Monaco's
    const monacoLanguage = (lang: string): string => {
        const map: Record<string, string> = {
            'text': 'plaintext',
            'js': 'javascript',
            'ts': 'typescript',
            'jsx': 'javascript',
            'tsx': 'typescript',
            'bash': 'shell',
            'sh': 'shell',
        };
        return map[lang] || lang;
    };

    if (!file) {
        return (
            <div className={`flex flex-col items-center justify-center h-full text-gray-400 ${className}`}>
                <FileCode size={48} className="mb-3 opacity-30" />
                <p>No file selected for editing</p>
                <p className="text-sm mt-1">Right-click on a file and choose "Edit"</p>
            </div>
        );
    }

    // Theme-aware header styling
    const isLightTheme = theme === 'vs';
    const headerBg = isLightTheme ? 'bg-gray-100' : 'bg-gray-800';
    const headerBorder = isLightTheme ? 'border-gray-300' : 'border-gray-700';
    const headerText = isLightTheme ? 'text-gray-700' : 'text-gray-300';
    const buttonBg = isLightTheme ? 'bg-gray-200 hover:bg-gray-300' : 'bg-gray-700 hover:bg-gray-600';
    const hoverBg = isLightTheme ? 'hover:bg-gray-200' : 'hover:bg-gray-700';

    return (
        <div className={`flex flex-col h-full ${className}`}>
            {/* Editor Header */}
            <div className={`flex items-center justify-between px-4 py-2 ${headerBg} border-b ${headerBorder}`}>
                <div className={`flex items-center gap-2 text-sm ${headerText}`}>
                    <FileCode size={14} />
                    <span className="font-medium">{file.name}</span>
                    {hasChanges && <span className="text-yellow-500">• Modified</span>}
                    {file.isRemote && <span className="text-blue-500 ml-2">Remote</span>}
                </div>

                <div className="flex items-center gap-2">
                    <button
                        onClick={handleReset}
                        disabled={!hasChanges}
                        className={`flex items-center gap-1 px-2 py-1 text-xs ${buttonBg} disabled:opacity-50 disabled:cursor-not-allowed rounded transition-colors`}
                        title="Reset changes"
                    >
                        <RotateCcw size={12} />
                        Reset
                    </button>
                    <button
                        onClick={handleSave}
                        disabled={!hasChanges || isSaving}
                        className="flex items-center gap-1 px-3 py-1 text-xs bg-green-600 hover:bg-green-500 disabled:opacity-50 disabled:cursor-not-allowed text-white rounded transition-colors"
                        title="Save file (Ctrl+S)"
                    >
                        <Save size={12} />
                        {isSaving ? 'Saving...' : 'Save'}
                    </button>
                    <button
                        onClick={onClose}
                        className={`p-1 ${hoverBg} rounded transition-colors`}
                        title="Close editor"
                    >
                        <X size={14} />
                    </button>
                </div>
            </div>

            {/* Monaco Editor */}
            <div ref={containerRef} className="flex-1" style={{ minHeight: 0 }}>
                <Editor
                    height="100%"
                    language={monacoLanguage(language)}
                    value={file.content}
                    theme={theme}
                    onMount={handleEditorDidMount}
                    onChange={handleChange}
                    options={{
                        fontSize: 13,
                        fontFamily: "'JetBrains Mono', 'Fira Code', 'Cascadia Code', 'Consolas', 'Courier New', monospace",
                        minimap: { enabled: true, scale: 0.8 },
                        scrollBeyondLastLine: false,
                        wordWrap: 'on',
                        lineNumbers: 'on',
                        renderWhitespace: 'selection',
                        tabSize: 2,
                        insertSpaces: true,
                        automaticLayout: true,
                        folding: true,
                        formatOnPaste: true,
                        bracketPairColorization: { enabled: true },
                        padding: { top: 8, bottom: 8 },
                    }}
                />
            </div>
        </div>
    );
};

export default CodeEditor;

