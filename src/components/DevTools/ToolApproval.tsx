import React, { useState, useEffect } from 'react';
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
    allTools?: AITool[];
}

const dangerConfig: Record<DangerLevel, { accent: string; icon: typeof Shield }> = {
    safe: { accent: 'border-l-blue-500', icon: ShieldCheck },
    medium: { accent: 'border-l-yellow-500', icon: Shield },
    high: { accent: 'border-l-red-500', icon: ShieldAlert },
};

/** Extract the main argument to show inline (usually path or command) */
function getMainArg(toolName: string, args: Record<string, unknown>): string {
    const path = (args.path || args.local_path || args.remote_path || args.from) as string | undefined;
    if (path) {
        // Show only filename from path
        const parts = path.replace(/\\/g, '/').split('/');
        return parts[parts.length - 1] || path;
    }
    const command = args.command as string | undefined;
    if (command) return command.length > 40 ? command.slice(0, 40) + '...' : command;
    return '';
}

export const ToolApproval: React.FC<ToolApprovalProps> = ({ toolCall, onApprove, onReject, allTools }) => {
    const [expanded, setExpanded] = useState(false);
    const [diffData, setDiffData] = useState<{ original: string; modified: string } | null>(null);
    const [diffLoading, setDiffLoading] = useState(false);
    const [diffError, setDiffError] = useState<string | null>(null);
    const { t } = useI18n();

    const tool = allTools ? getToolByNameFromAll(toolCall.toolName, allTools) : getToolByName(toolCall.toolName);
    const dangerLevel = tool?.dangerLevel || 'medium';
    const config = dangerConfig[dangerLevel];
    const DangerIcon = config.icon;

    const isExecuting = toolCall.status === 'executing';
    const isCompleted = toolCall.status === 'completed';
    const isError = toolCall.status === 'error';
    const isPending = toolCall.status === 'pending' || toolCall.status === 'approved';

    const isEditTool = toolCall.toolName === 'local_edit' || toolCall.toolName === 'remote_edit';

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

    return (
        <div className={`border-l-2 ${config.accent} bg-gray-800/40 rounded-r my-1 text-xs`} role="listitem">
            {/* Compact header row */}
            <div className="flex items-center gap-1.5 px-2 py-1.5">
                <DangerIcon size={12} className={
                    dangerLevel === 'high' ? 'text-red-400' :
                    dangerLevel === 'medium' ? 'text-yellow-400' : 'text-blue-400'
                } />

                <span className="font-mono text-gray-200 font-medium">{label}</span>

                {mainArg && (
                    <>
                        <span className="text-gray-600">&#x2039;</span>
                        <span className="text-purple-300 font-mono truncate max-w-[180px]">{mainArg}</span>
                    </>
                )}

                {/* Expand toggle for full args */}
                {Object.keys(toolCall.args).length > 1 && isPending && (
                    <button
                        onClick={() => setExpanded(!expanded)}
                        className="ml-auto text-gray-500 hover:text-gray-300 p-0.5"
                        aria-expanded={expanded}
                        aria-label={t('ai.toolApproval.toggleDetails') || 'Toggle details'}
                    >
                        {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                    </button>
                )}

                {/* Status indicators */}
                {isExecuting && (
                    <span className="ml-auto flex items-center gap-1 text-yellow-400">
                        <Loader2 size={11} className="animate-spin" /> {t('ai.tool.running') || 'Running...'}
                    </span>
                )}
                {isCompleted && (
                    <span className="ml-auto flex items-center gap-1 text-green-400">
                        <Check size={11} /> {t('ai.tool.done') || 'Done'}
                    </span>
                )}
                {isError && (
                    <span className="ml-auto text-red-400 truncate max-w-[200px]">
                        {t('ai.tool.failed') || 'Failed'}{toolCall.error ? `: ${toolCall.error}` : ''}
                    </span>
                )}

                {/* Inline approve/reject buttons */}
                {isPending && toolCall.status === 'pending' && (
                    <div className="ml-auto flex items-center gap-1">
                        <button
                            onClick={onApprove}
                            disabled={toolCall.validation && !toolCall.validation.valid}
                            className={`flex items-center gap-1 px-2 py-0.5 rounded text-[11px] font-medium transition-colors ${
                                toolCall.validation && !toolCall.validation.valid
                                    ? 'opacity-50 cursor-not-allowed bg-gray-600 text-gray-400'
                                    : dangerLevel === 'high'
                                        ? 'bg-red-600/80 hover:bg-red-500 text-white'
                                        : 'bg-green-600/80 hover:bg-green-500 text-white'
                            }`}
                            aria-label={dangerLevel === 'high' ? t('ai.toolApproval.confirmDangerous') || 'Confirm dangerous action' : t('ai.toolApproval.allowExecution') || 'Allow tool execution'}
                        >
                            <Check size={10} />
                            {dangerLevel === 'high' ? (t('ai.toolApproval.confirm') || 'Confirm') : (t('ai.toolApproval.allow') || 'Allow')}
                        </button>
                        <button
                            onClick={onReject}
                            className="flex items-center gap-1 px-2 py-0.5 rounded text-[11px] font-medium bg-gray-700/80 hover:bg-gray-600 text-gray-300 transition-colors"
                            aria-label={t('ai.toolApproval.rejectExecution') || 'Reject tool execution'}
                        >
                            <X size={10} />
                        </button>
                    </div>
                )}
            </div>

            {/* Progress indicator for long-running tools */}
            {isExecuting && (
                <ToolProgressIndicator toolName={toolCall.toolName} />
            )}

            {/* Diff preview for edit tools */}
            {isEditTool && diffLoading && (
                <div className="px-3 py-1.5 text-[11px] text-gray-500 flex items-center gap-1">
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
                <div className="px-3 py-1 text-[11px] text-yellow-400">
                    {t('ai.tool.previewUnavailable') || 'Preview unavailable:'} {diffError}
                </div>
            )}

            {/* Validation errors/warnings */}
            {toolCall.validation && !toolCall.validation.valid && (
                <div className="px-3 py-1 text-[11px]">
                    {toolCall.validation.errors.map((err, i) => (
                        <div key={`err-${i}`} className="text-red-400 flex items-center gap-1">
                            <span>{'\u2715'}</span> {err}
                        </div>
                    ))}
                </div>
            )}
            {toolCall.validation && toolCall.validation.warnings.length > 0 && (
                <div className="px-3 py-1 text-[11px]">
                    {toolCall.validation.warnings.map((warn, i) => (
                        <div key={`warn-${i}`} className="text-yellow-400 flex items-center gap-1">
                            <span>{'\u26A0'}</span> {warn}
                        </div>
                    ))}
                </div>
            )}

            {/* Expandable args detail */}
            {expanded && isPending && (
                <div className="px-3 pb-1.5 font-mono text-[11px] border-t border-gray-700/50">
                    {Object.entries(toolCall.args).map(([key, value]) => (
                        <div key={key} className="flex gap-1 py-0.5">
                            <span className="text-gray-500">{key}:</span>
                            <span className="text-gray-300 break-all">{String(value as string)}</span>
                        </div>
                    ))}
                </div>
            )}

            {/* Result display */}
            {toolCall.result != null && (
                <div className="px-3 pb-1.5 text-[11px] text-gray-400 max-h-20 overflow-auto border-t border-gray-700/50">
                    <pre className="whitespace-pre-wrap">{typeof toolCall.result === 'string' ? toolCall.result : JSON.stringify(toolCall.result as Record<string, unknown>, null, 2)}</pre>
                </div>
            )}
        </div>
    );
};

export default ToolApproval;
