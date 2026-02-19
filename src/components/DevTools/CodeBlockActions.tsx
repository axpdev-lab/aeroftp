import React, { useState, useCallback } from 'react';
import { Copy, Check, Terminal, FileInput, AlertTriangle, FileDiff, ShieldAlert } from 'lucide-react';
import { save } from '@tauri-apps/plugin-dialog';
import { writeTextFile, readTextFile } from '@tauri-apps/plugin-fs';
import { useTranslation } from '../../i18n';
import { DiffPreview } from './DiffPreview';

// SEC: Command denylist matching backend ai_tools.rs DENIED_COMMAND_PATTERNS
// Prevents destructive commands from being dispatched to PTY via "Run in Terminal"
const DENIED_COMMAND_PATTERNS = [
    /^\s*rm\s+(-[a-zA-Z]*)?.*\s+\/\s*$/,
    /^\s*rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+)?-[a-zA-Z]*r.*\s+\/\s*$/,
    /^\s*mkfs\b/,
    /^\s*dd\s+.*of=\/dev\//,
    /^\s*shutdown\b/,
    /^\s*reboot\b/,
    /^\s*halt\b/,
    /^\s*init\s+[06]\b/,
    /^\s*:\(\)\s*\{\s*:\|:\s*&\s*\}\s*;\s*:/,
    /^\s*>\s*\/dev\/sd[a-z]/,
    /^\s*chmod\s+(-[a-zA-Z]*\s+)?777\s+\//,
    /^\s*chown\s+.*\s+\/\s*$/,
];

function isCommandDenied(command: string): boolean {
    return command.split('\n').some(line =>
        DENIED_COMMAND_PATTERNS.some(rx => rx.test(line.trim()))
    );
}

interface CodeBlockActionsProps {
    code: string;
    language: string;
    editorFilePath?: string;
    editorFileName?: string;
}

export const CodeBlockActions: React.FC<CodeBlockActionsProps> = ({
    code,
    language,
    editorFilePath,
    editorFileName,
}) => {
    const [copied, setCopied] = useState(false);
    const [copyError, setCopyError] = useState(false);
    const [applied, setApplied] = useState(false);
    const [applyError, setApplyError] = useState(false);
    const [confirmRun, setConfirmRun] = useState(false);
    const [blocked, setBlocked] = useState(false);
    const [showDiff, setShowDiff] = useState(false);
    const [diffOriginal, setDiffOriginal] = useState<string | null>(null);
    const t = useTranslation();

    const handleCopy = useCallback(async () => {
        try {
            await navigator.clipboard.writeText(code);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
        } catch {
            setCopyError(true);
            setTimeout(() => setCopyError(false), 2000);
        }
    }, [code]);

    const handleApplyToFile = useCallback(async () => {
        try {
            if (editorFilePath) {
                // Write to the currently open file in editor
                await writeTextFile(editorFilePath, code);
                // Notify Monaco editor to reload
                window.dispatchEvent(new CustomEvent('file-changed', { detail: { path: editorFilePath } }));
                setApplied(true);
                setTimeout(() => setApplied(false), 2000);
            } else {
                // No file open — show save dialog
                const ext = language || 'txt';
                const extMap: Record<string, string> = {
                    javascript: 'js', typescript: 'ts', python: 'py', rust: 'rs',
                    bash: 'sh', yaml: 'yml', markup: 'html',
                };
                const fileExt = extMap[ext] || ext;
                const savePath = await save({
                    defaultPath: `code.${fileExt}`,
                    filters: [{ name: 'Code files', extensions: [fileExt] }],
                });
                if (savePath) {
                    await writeTextFile(savePath, code);
                    setApplied(true);
                    setTimeout(() => setApplied(false), 2000);
                }
            }
        } catch {
            setApplyError(true);
            setTimeout(() => setApplyError(false), 2000);
        }
    }, [code, language, editorFilePath]);

    const handleRunClick = useCallback(() => {
        if (confirmRun) {
            // Strip Unicode bidi override characters to prevent Trojan Source attacks
            const sanitizedCode = code.replace(/[\u202A-\u202E\u2066-\u2069\u200E\u200F]/g, '');
            // SEC: Check against denylist before PTY dispatch (mirrors backend ai_tools.rs)
            if (isCommandDenied(sanitizedCode)) {
                setBlocked(true);
                setConfirmRun(false);
                setTimeout(() => setBlocked(false), 3000);
                return;
            }
            window.dispatchEvent(new CustomEvent('terminal-execute', { detail: { command: sanitizedCode } }));
            window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'terminal' }));
            setConfirmRun(false);
        } else {
            setConfirmRun(true);
            setTimeout(() => setConfirmRun(false), 3000);
        }
    }, [code, confirmRun]);

    const handleDiff = useCallback(async () => {
        if (!editorFilePath) return;
        if (showDiff) {
            // Toggle off
            setShowDiff(false);
            setDiffOriginal(null);
            return;
        }
        try {
            const original = await readTextFile(editorFilePath);
            setDiffOriginal(original);
            setShowDiff(true);
        } catch {
            // File might not exist yet — show diff against empty
            setDiffOriginal('');
            setShowDiff(true);
        }
    }, [editorFilePath, showDiff]);

    const handleDiffApply = useCallback(async () => {
        await handleApplyToFile();
        setShowDiff(false);
        setDiffOriginal(null);
    }, [handleApplyToFile]);

    const handleDiffCancel = useCallback(() => {
        setShowDiff(false);
        setDiffOriginal(null);
    }, []);

    return (
        <>
            {/* Action buttons (visible on hover) — absolutely positioned in parent group */}
            <div className="absolute top-0 right-0 flex items-center gap-0.5 p-1 opacity-0 group-hover:opacity-100 transition-opacity z-10">
                {/* Copy */}
                <button
                    onClick={handleCopy}
                    className={`p-1 rounded hover:bg-gray-700 transition-colors ${copyError ? 'text-red-400' : 'text-gray-400 hover:text-gray-200'}`}
                    title={t('ai.codeBlock.copy') || 'Copy'}
                >
                    {copied ? <Check size={13} className="text-green-400" /> : <Copy size={13} />}
                </button>

                {/* Apply to file */}
                <button
                    onClick={handleApplyToFile}
                    className={`p-1 rounded hover:bg-gray-700 transition-colors ${applyError ? 'text-red-400' : 'text-gray-400 hover:text-gray-200'}`}
                    title={editorFilePath
                        ? `${t('ai.codeBlock.apply') || 'Apply to'} ${editorFileName || 'file'}`
                        : t('ai.codeBlock.apply') || 'Save as file'
                    }
                >
                    {applied ? <Check size={13} className="text-green-400" /> : <FileInput size={13} />}
                </button>

                {/* Diff with original */}
                <button
                    onClick={handleDiff}
                    disabled={!editorFilePath}
                    className={`p-1 rounded transition-colors ${
                        !editorFilePath
                            ? 'text-gray-600 cursor-not-allowed'
                            : showDiff
                                ? 'text-purple-400 bg-gray-700'
                                : 'text-gray-400 hover:text-gray-200 hover:bg-gray-700'
                    }`}
                    title={editorFilePath
                        ? (t('ai.codeBlock.diff') || 'Diff with original')
                        : (t('ai.codeBlock.diffNoFile') || 'No file open')
                    }
                >
                    <FileDiff size={13} />
                </button>

                {/* Run in terminal (SEC-003: two-click confirmation, SEC-P4-001: command preview, GAP-B03: denylist) */}
                <button
                    onClick={handleRunClick}
                    disabled={blocked}
                    className={`p-1 rounded transition-colors ${
                        blocked ? 'bg-orange-600/80 text-white cursor-not-allowed'
                        : confirmRun ? 'bg-red-600/80 text-white hover:bg-red-500'
                        : 'hover:bg-gray-700 text-gray-400 hover:text-gray-200'
                    }`}
                    title={blocked
                        ? 'Command blocked: potentially destructive'
                        : confirmRun
                            ? `Confirm run: ${code.length > 200 ? code.slice(0, 200) + '...' : code}`
                            : (t('ai.codeBlock.run') || 'Run in terminal')
                    }
                >
                    {blocked ? <ShieldAlert size={13} /> : confirmRun ? <AlertTriangle size={13} /> : <Terminal size={13} />}
                </button>
            </div>

            {/* Diff preview (rendered below <pre> in the parent group div) */}
            {showDiff && diffOriginal !== null && (
                <div className="mt-1">
                    <DiffPreview
                        originalContent={diffOriginal}
                        modifiedContent={code}
                        fileName={editorFileName || 'file'}
                        onApply={handleDiffApply}
                        onCancel={handleDiffCancel}
                    />
                </div>
            )}
        </>
    );
};

export default CodeBlockActions;
