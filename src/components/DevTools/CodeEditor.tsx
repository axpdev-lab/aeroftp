import React, { useRef, useState, useEffect } from 'react';
import Editor, { OnMount, loader } from '@monaco-editor/react';
import { Save, X, RotateCcw, FileCode } from 'lucide-react';
import { PreviewFile, getFileLanguage } from './types';

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
}

export const CodeEditor: React.FC<CodeEditorProps> = ({
    file,
    onSave,
    onClose,
    className = '',
    theme: themeProp = 'tokyo-night',
}) => {
    const editorRef = useRef<any>(null);
    const monacoRef = useRef<any>(null);
    const containerRef = useRef<HTMLDivElement>(null);
    const [isSaving, setIsSaving] = useState(false);
    const [hasChanges, setHasChanges] = useState(false);
    const [originalContent, setOriginalContent] = useState('');
    const theme = themeProp; // Use prop directly

    const handleEditorDidMount: OnMount = (editor, monaco) => {
        editorRef.current = editor;
        monacoRef.current = monaco;

        // Define Tokyo Night theme
        monaco.editor.defineTheme('tokyo-night', tokyoNightTheme);
        monaco.editor.setTheme(theme);

        if (file) {
            setOriginalContent(file.content);
        }

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

    // ResizeObserver for container size changes
    useEffect(() => {
        const container = containerRef.current;
        const editor = editorRef.current;

        if (!container) return;

        const resizeObserver = new ResizeObserver(() => {
            if (editor) {
                // Debounce the layout call
                requestAnimationFrame(() => {
                    editor.layout();
                });
            }
        });

        resizeObserver.observe(container);
        return () => resizeObserver.disconnect();
    }, []);

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

