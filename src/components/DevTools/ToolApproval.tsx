import React, { useState, useEffect, useRef } from 'react';
import { Check, X, ChevronDown, ChevronRight, Loader2, Shield, ShieldAlert, ShieldCheck } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { AgentToolCall, AITool, getToolByName, getToolByNameFromAll, DangerLevel } from '../../types/tools';
import { DiffPreview } from './DiffPreview';
import { ToolProgressIndicator } from './ToolProgressIndicator';
import { useI18n } from '../../i18n';
import { getToolLabel } from './aiChatToolLabels';

interface ToolApprovalProps {
    toolCall: AgentToolCall;
    onApprove: () => void;
    onReject: () => void;
    onApproveSession?: (toolName: string) => void;
    allTools?: AITool[];
}

const dangerIcons: Record<DangerLevel, typeof Shield> = {
    safe: ShieldCheck,
    medium: Shield,
    high: ShieldAlert,
};

const dangerIconColor: Record<DangerLevel, string> = {
    safe: 'text-blue-400',
    medium: 'text-yellow-400',
    high: 'text-red-400',
};

/** Extract the main argument to show inline (usually path or command) */
function getMainArg(_toolName: string, args: Record<string, unknown>): string {
    const path = (args.path || args.local_path || args.remote_path || args.from) as string | undefined;
    if (path) {
        const parts = path.replace(/\\/g, '/').split('/');
        return parts[parts.length - 1] || path;
    }
    const command = args.command as string | undefined;
    if (command) return command.length > 40 ? command.slice(0, 40) + '...' : command;
    return '';
}

export const ToolApproval: React.FC<ToolApprovalProps> = ({ toolCall, onApprove, onReject, onApproveSession, allTools }) => {
    const [expanded, setExpanded] = useState(false);
    const [menuOpen, setMenuOpen] = useState(false);
    const [diffData, setDiffData] = useState<{ original: string; modified: string } | null>(null);
    const [diffLoading, setDiffLoading] = useState(false);
    const [diffError, setDiffError] = useState<string | null>(null);
    const menuRef = useRef<HTMLDivElement>(null);
    const { t } = useI18n();

    const tool = allTools ? getToolByNameFromAll(toolCall.toolName, allTools) : getToolByName(toolCall.toolName);
    const dangerLevel = tool?.dangerLevel || 'medium';
    const DangerIcon = dangerIcons[dangerLevel];

    const isExecuting = toolCall.status === 'executing';
    const isCompleted = toolCall.status === 'completed';
    const isError = toolCall.status === 'error';
    const isPending = toolCall.status === 'pending' || toolCall.status === 'approved';

    const isEditTool = toolCall.toolName === 'local_edit' || toolCall.toolName === 'remote_edit';

    // Close dropdown on outside click
    useEffect(() => {
        if (!menuOpen) return;
        const handleClick = (e: MouseEvent) => {
            if (menuRef.current && !menuRef.current.contains(e.target as Node)) {
                setMenuOpen(false);
            }
        };
        document.addEventListener('mousedown', handleClick);
        return () => document.removeEventListener('mousedown', handleClick);
    }, [menuOpen]);

    useEffect(() => {
        if (!isEditTool || toolCall.status !== 'pending') return;
        setDiffLoading(true);
        setDiffError(null);
        invoke('execute_ai_tool', {
            toolName: 'preview_edit',
            args: {
                path: toolCall.args.path,
                find: toolCall.args.find,
                replace: toolCall.args.replace,
                replace_all: toolCall.args.replace_all ?? true,
                remote: toolCall.toolName === 'remote_edit',
            },
        })
        .then((result: unknown) => {
            const r = result as Record<string, unknown>;
            if (r.success) {
                setDiffData({ original: r.original as string, modified: r.modified as string });
            } else {
                setDiffError((r.message as string) || 'Preview failed');
            }
        })
        .catch((err: unknown) => setDiffError(String(err)))
        .finally(() => setDiffLoading(false));
    }, [toolCall.id]);

    const label = getToolLabel(toolCall.toolName, t);
    const mainArg = getMainArg(toolCall.toolName, toolCall.args);

    const isDisabled = toolCall.validation && !toolCall.validation.valid;

    return (
        <div className="flex gap-3 justify-start" role="listitem">
            <div className="max-w-[85%] rounded-lg px-4 py-2.5 text-sm tool-approval-bubble bg-gray-800/60 dark:bg-gray-800/60 border border-gray-700/50">
                {/* Tool info row */}
                <div className="flex items-center gap-2 mb-1.5">
                    <DangerIcon size={14} className={dangerIconColor[dangerLevel]} />
                    <span className="font-mono text-gray-200 font-medium text-xs">{label}</span>
                    {mainArg && (
                        <span className="text-purple-300/80 font-mono text-xs truncate max-w-[200px]">
                            &lsaquo; {mainArg}
                        </span>
                    )}

                    {/* Expand toggle for full args */}
                    {Object.keys(toolCall.args).length > 1 && isPending && (
                        <button
                            onClick={() => setExpanded(!expanded)}
                            className="text-gray-500 hover:text-gray-300 p-0.5"
                            aria-expanded={expanded}
                            aria-label={t('ai.toolApproval.toggleDetails') || 'Toggle details'}
                        >
                            {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                        </button>
                    )}
                </div>

                {/* Diff preview for edit tools */}
                {isEditTool && diffLoading && (
                    <div className="py-1.5 text-[11px] text-gray-500 flex items-center gap-1">
                        <Loader2 size={10} className="animate-spin" />
                        {t('ai.tool.loadingDiff') || 'Loading diff preview...'}
                    </div>
                )}
                {isEditTool && diffData && isPending && (
                    <DiffPreview
                        originalContent={diffData.original}
                        modifiedContent={diffData.modified}
                        fileName={String(toolCall.args.path || '')}
                        showActions={false}
                    />
                )}
                {isEditTool && diffError && (
                    <div className="py-1 text-[11px] text-yellow-400">
                        {t('ai.tool.previewUnavailable') || 'Preview unavailable:'} {diffError}
                    </div>
                )}

                {/* Validation errors/warnings */}
                {toolCall.validation && !toolCall.validation.valid && (
                    <div className="py-1 text-[11px]">
                        {toolCall.validation.errors.map((err, i) => (
                            <div key={`err-${i}`} className="text-red-400 flex items-center gap-1">
                                <span>{'\u2715'}</span> {err}
                            </div>
                        ))}
                    </div>
                )}
                {toolCall.validation && toolCall.validation.warnings.length > 0 && (
                    <div className="py-1 text-[11px]">
                        {toolCall.validation.warnings.map((warn, i) => (
                            <div key={`warn-${i}`} className="text-yellow-400 flex items-center gap-1">
                                <span>{'\u26A0'}</span> {warn}
                            </div>
                        ))}
                    </div>
                )}

                {/* Expandable args detail */}
                {expanded && isPending && (
                    <div className="font-mono text-[11px] border-t border-gray-700/30 pt-1.5 mt-1">
                        {Object.entries(toolCall.args).map(([key, value]) => (
                            <div key={key} className="flex gap-1 py-0.5">
                                <span className="text-gray-500">{key}:</span>
                                <span className="text-gray-300 break-all">{String(value as string)}</span>
                            </div>
                        ))}
                    </div>
                )}

                {/* Action buttons / status — bottom row */}
                <div className="flex items-center gap-2 mt-2">
                    {isPending && toolCall.status === 'pending' && (
                        <>
                            {/* Split button: Allow + dropdown for session approval */}
                            <div className="relative inline-flex" ref={menuRef}>
                                <button
                                    onClick={onApprove}
                                    disabled={!!isDisabled}
                                    className={`flex items-center gap-1.5 px-3 py-1 rounded-l-md text-xs font-medium transition-colors ${
                                        isDisabled
                                            ? 'opacity-50 cursor-not-allowed bg-gray-600 text-gray-400'
                                            : dangerLevel === 'high'
                                                ? 'bg-red-600/80 hover:bg-red-500 text-white'
                                                : 'tool-approval-allow'
                                    }`}
                                    aria-label={dangerLevel === 'high' ? t('ai.toolApproval.confirmDangerous') || 'Confirm dangerous action' : t('ai.toolApproval.allowExecution') || 'Allow tool execution'}
                                >
                                    <Check size={12} />
                                    {dangerLevel === 'high' ? (t('ai.toolApproval.confirm') || 'Confirm') : (t('ai.toolApproval.allow') || 'Allow')}
                                </button>
                                {onApproveSession && dangerLevel !== 'high' && !isDisabled && (
                                    <button
                                        onClick={() => setMenuOpen(!menuOpen)}
                                        className="tool-approval-allow flex items-center px-1.5 py-1 rounded-r-md border-l border-white/20 text-xs transition-colors"
                                        aria-label="More options"
                                        aria-haspopup="true"
                                        aria-expanded={menuOpen}
                                    >
                                        <ChevronDown size={10} />
                                    </button>
                                )}
                                {/* Dropdown menu */}
                                {menuOpen && (
                                    <div className="absolute top-full left-0 mt-1 z-50 bg-gray-800 border border-gray-600 rounded-md shadow-lg py-0.5 min-w-[180px]">
                                        <button
                                            onClick={() => {
                                                setMenuOpen(false);
                                                if (onApproveSession) onApproveSession(toolCall.toolName);
                                                onApprove();
                                            }}
                                            className="w-full text-left px-3 py-1.5 text-xs text-gray-200 hover:bg-gray-700 transition-colors"
                                        >
                                            {t('ai.toolApproval.allowSession') || 'Allow for this session'}
                                        </button>
                                    </div>
                                )}
                            </div>
                            <button
                                onClick={onReject}
                                className="flex items-center gap-1 px-2.5 py-1 rounded-md text-xs font-medium bg-gray-700/60 hover:bg-gray-600/80 text-gray-400 hover:text-gray-200 transition-colors"
                                aria-label={t('ai.toolApproval.rejectExecution') || 'Reject tool execution'}
                            >
                                <X size={12} />
                            </button>
                        </>
                    )}
                    {isExecuting && (
                        <span className="flex items-center gap-1.5 text-xs text-yellow-400">
                            <Loader2 size={12} className="animate-spin" /> {t('ai.tool.running') || 'Running...'}
                        </span>
                    )}
                    {isCompleted && (
                        <span className="flex items-center gap-1.5 text-xs text-green-400">
                            <Check size={12} /> {t('ai.tool.done') || 'Done'}
                        </span>
                    )}
                    {isError && (
                        <span className="text-xs text-red-400 truncate max-w-[250px]">
                            {t('ai.tool.failed') || 'Failed'}{toolCall.error ? `: ${toolCall.error}` : ''}
                        </span>
                    )}
                </div>

                {/* Progress indicator for long-running tools */}
                {isExecuting && (
                    <ToolProgressIndicator toolName={toolCall.toolName} />
                )}

                {/* Result display */}
                {toolCall.result != null && (
                    <div className="text-[11px] text-gray-400 max-h-20 overflow-auto border-t border-gray-700/30 pt-1 mt-1.5">
                        <pre className="whitespace-pre-wrap">{typeof toolCall.result === 'string' ? toolCall.result : JSON.stringify(toolCall.result as Record<string, unknown>, null, 2)}</pre>
                    </div>
                )}
            </div>
        </div>
    );
};

export default ToolApproval;
