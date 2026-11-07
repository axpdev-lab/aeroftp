// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useState, useCallback } from 'react';
import { Copy, Check, Terminal, FileInput, AlertTriangle, FileDiff, ShieldAlert } from 'lucide-react';
import { pickSave } from '../../utils/pickPath';
import { writeTextFile, readTextFile } from '@tauri-apps/plugin-fs';
import { useTranslation } from '../../i18n';
import { DiffPreview } from './DiffPreview';
import { copyText } from '../../utils/clipboard';
import { applyToEditorBuffer, readEditorBuffer } from '../../utils/editorBuffer';

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
    /** The open file lives on a server: its path names no local file. */
    editorFileIsRemote?: boolean;
}

export const CodeBlockActions: React.FC<CodeBlockActionsProps> = ({
    code,
    language,
    editorFilePath,
    editorFileName,
    editorFileIsRemote = false,
}) => {
    const [copied, setCopied] = useState(false);
    const [copyError, setCopyError] = useState(false);
    const [applied, setApplied] = useState(false);
    const [applyError, setApplyError] = useState(false);
    // The action that could not reach the remote file's editor buffer: the
    // editor is hidden or has another file open, and the message says so.
    const [unreachable, setUnreachable] = useState<'apply' | 'diff' | null>(null);
    const showUnreachable = useCallback((action: 'apply' | 'diff') => {
        setUnreachable(action);
        setTimeout(() => setUnreachable(current => (current === action ? null : current)), 4000);
    }, []);
    const [confirmRun, setConfirmRun] = useState(false);
    const [blocked, setBlocked] = useState(false);
    const [showDiff, setShowDiff] = useState(false);
    const [diffOriginal, setDiffOriginal] = useState<string | null>(null);
    const t = useTranslation();

    const handleCopy = useCallback(async () => {
        try {
            await copyText(code);
            setCopied(true);
            setTimeout(() => setCopied(false), 1500);
        } catch {
            setCopyError(true);
            setTimeout(() => setCopyError(false), 2000);
        }
    }, [code]);

    // Whether the code landed: the diff closes only then.
    const handleApplyToFile = useCallback(async (): Promise<boolean> => {
        try {
            if (editorFilePath && editorFileIsRemote) {
                // A remote file's path names no local file: writing it through
                // the local filesystem overwrote whatever local file had the same
                // path (deep scan item 9). The code goes into the editor's buffer,
                // left unsaved; the editor's save uploads it to its own server.
                if (!applyToEditorBuffer({ path: editorFilePath, isRemote: true }, code)) {
                    showUnreachable('apply');
                    return false;
                }
                setApplied(true);
                setTimeout(() => setApplied(false), 2000);
                return true;
            } else if (editorFilePath) {
                // Write to the currently open file in editor
                await writeTextFile(editorFilePath, code);
                // Notify Monaco editor to reload
                window.dispatchEvent(new CustomEvent('file-changed', { detail: { path: editorFilePath } }));
                setApplied(true);
                setTimeout(() => setApplied(false), 2000);
                return true;
            } else {
                // No file open: show save dialog
                const ext = language || 'txt';
                const extMap: Record<string, string> = {
                    javascript: 'js', typescript: 'ts', python: 'py', rust: 'rs',
                    bash: 'sh', yaml: 'yml', markup: 'html',
                };
                const fileExt = extMap[ext] || ext;
                const savePath = await pickSave({
                    defaultPath: `code.${fileExt}`,
                    filters: [{ name: 'Code files', extensions: [fileExt] }],
                });
                if (savePath) {
                    await writeTextFile(savePath, code);
                    setApplied(true);
                    setTimeout(() => setApplied(false), 2000);
                    return true;
                }
                return false;
            }
        } catch {
            setApplyError(true);
            setTimeout(() => setApplyError(false), 2000);
            return false;
        }
    }, [code, language, editorFilePath, editorFileIsRemote, showUnreachable]);

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
        if (editorFileIsRemote) {
            // The original of a remote file is the editor's buffer, never a
            // local file that happens to share its path.
            const original = readEditorBuffer({ path: editorFilePath, isRemote: true });
            if (original === null) {
                showUnreachable('diff');
                return;
            }
            setDiffOriginal(original);
            setShowDiff(true);
            return;
        }
        try {
            const original = await readTextFile(editorFilePath);
            setDiffOriginal(original);
            setShowDiff(true);
        } catch {
            // File might not exist yet: show diff against empty
            setDiffOriginal('');
            setShowDiff(true);
        }
    }, [editorFilePath, editorFileIsRemote, showDiff, showUnreachable]);

    const handleDiffApply = useCallback(async () => {
        // Not applied (no editor for the file, a write that failed): the diff
        // stays open, so the comparison is not lost while the user recovers.
        if (!(await handleApplyToFile())) return;
        setShowDiff(false);
        setDiffOriginal(null);
    }, [handleApplyToFile]);

    const handleDiffCancel = useCallback(() => {
        setShowDiff(false);
        setDiffOriginal(null);
    }, []);

    return (
        <>
            {/* Action buttons (visible on hover): absolutely positioned in parent group */}
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
                    className={`p-1 rounded hover:bg-gray-700 transition-colors ${applyError || unreachable === 'apply' ? 'text-red-400' : 'text-gray-400 hover:text-gray-200'}`}
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
                            : unreachable === 'diff'
                                ? 'text-red-400 hover:bg-gray-700'
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
                        ? t('ai.codeBlock.commandBlocked')
                        : confirmRun
                            ? t('ai.codeBlock.confirmRun', { command: code.length > 200 ? code.slice(0, 200) + '...' : code })
                            : t('ai.codeBlock.run')
                    }
                >
                    {blocked ? <ShieldAlert size={13} /> : confirmRun ? <AlertTriangle size={13} /> : <Terminal size={13} />}
                </button>
            </div>

            {unreachable && (
                <div role="alert" className="mt-1 text-xs text-red-400">
                    {t('ai.codeBlock.reopenEditor')}
                </div>
            )}

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
