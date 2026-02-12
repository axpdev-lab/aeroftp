import React, { useState, useEffect, useCallback } from 'react';
import { Check, X, ChevronDown, ChevronRight, Loader2, Shield, ShieldAlert, ShieldCheck } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { AgentToolCall, AITool, getToolByName, getToolByNameFromAll, DangerLevel } from '../../types/tools';
import { DiffPreview } from './DiffPreview';
import { ToolProgressIndicator } from './ToolProgressIndicator';
import { useI18n } from '../../i18n';
import { getToolLabel } from './aiChatToolLabels';

interface BatchToolApprovalProps {
    toolCalls: AgentToolCall[];
    onApproveAll: () => void;
    onApproveSingle: (id: string) => void;
    onRejectAll: () => void;
    allTools?: AITool[];
}

const dangerConfig: Record<DangerLevel, { accent: string; icon: typeof Shield }> = {
    safe: { accent: 'border-l-blue-500', icon: ShieldCheck },
    medium: { accent: 'border-l-yellow-500', icon: Shield },
    high: { accent: 'border-l-red-500', icon: ShieldAlert },
};

function getMainArg(toolName: string, args: Record<string, unknown>): string {
    const path = (args.path || args.local_path || args.remote_path || args.from) as string | undefined;
    if (path) {
        const parts = path.replace(/\\/g, '/').split('/');
        return parts[parts.length - 1] || path;
    }
    const command = args.command as string | undefined;
    if (command) return command.length > 40 ? command.slice(0, 40) + '...' : command;
    return '';
}

/** Sub-component for individual batch tool items with independent diff preview state */
const BatchToolItem: React.FC<{
    tc: AgentToolCall;
    allTools?: AITool[];
    onApproveSingle: (id: string) => void;
    expandedId: string | null;
    setExpandedId: (id: string | null) => void;
    t: (key: string, params?: Record<string, string | number>) => string;
}> = ({ tc, allTools, onApproveSingle, expandedId, setExpandedId, t }) => {
    const [diffData, setDiffData] = useState<{ original: string; modified: string } | null>(null);
    const [diffLoading, setDiffLoading] = useState(false);
    const [diffError, setDiffError] = useState<string | null>(null);
    const isEditTool = tc.toolName === 'local_edit' || tc.toolName === 'remote_edit';

    const tool = allTools ? getToolByNameFromAll(tc.toolName, allTools) : getToolByName(tc.toolName);
    const dangerLevel = tool?.dangerLevel || 'medium';
    const config = dangerConfig[dangerLevel];
    const DangerIcon = config.icon;
    const label = getToolLabel(tc.toolName, t);
    const mainArg = getMainArg(tc.toolName, tc.args);
    const isExpanded = expandedId === tc.id;
    const isExecuting = tc.status === 'executing';
    const isCompleted = tc.status === 'completed';
    const isError = tc.status === 'error';

    useEffect(() => {
        if (!isEditTool || tc.status !== 'pending') return;
        setDiffLoading(true);
        invoke('execute_ai_tool', {
            toolName: 'preview_edit',
            args: {
                path: tc.args.path,
                find: tc.args.find,
                replace: tc.args.replace,
                replace_all: tc.args.replace_all ?? true,
                remote: tc.toolName === 'remote_edit',
            },
        })
        .then((result: unknown) => {
            const r = result as Record<string, unknown>;
            if (r.success) setDiffData({ original: r.original as string, modified: r.modified as string });
        })
        .catch((err: unknown) => setDiffError(String(err)))
        .finally(() => setDiffLoading(false));
    }, [tc.id]);

    return (
        <div className={`border-l-2 ${config.accent} text-xs`} role="listitem">
            <div className="flex items-center gap-1.5 px-2 py-1.5">
                <DangerIcon size={12} className={
                    dangerLevel === 'high' ? 'text-red-400' :
                    dangerLevel === 'medium' ? 'text-yellow-400' : 'text-blue-400'
                } />
                <span className="font-mono text-gray-200 font-medium">{label}</span>
                {mainArg && (
                    <>
                        <span className="text-gray-600">{'\u2039'}</span>
                        <span className="text-purple-300 font-mono truncate max-w-[160px]">{mainArg}</span>
                    </>
                )}

                {/* Expand toggle */}
                {Object.keys(tc.args).length > 1 && tc.status === 'pending' && (
                    <button
                        onClick={() => setExpandedId(isExpanded ? null : tc.id)}
                        className="text-gray-500 hover:text-gray-300 p-0.5"
                        aria-expanded={isExpanded}
                        aria-label={t('ai.toolApproval.toggleDetails') || 'Toggle details'}
                    >
                        {isExpanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                    </button>
                )}

                {/* Status */}
                {isExecuting && (
                    <span className="ml-auto flex items-center gap-1 text-yellow-400">
                        <Loader2 size={11} className="animate-spin" /> {t('ai.batch.running') || 'Running...'}
                    </span>
                )}
                {isCompleted && (
                    <span className="ml-auto flex items-center gap-1 text-green-400">
                        <Check size={11} /> {t('ai.batch.done') || 'Done'}
                    </span>
                )}
                {isError && (
                    <span className="ml-auto text-red-400 truncate max-w-[200px]">
                        {t('ai.batch.failed') || 'Failed'}{tc.error ? `: ${tc.error}` : ''}
                    </span>
                )}

                {/* Per-item approve button */}
                {tc.status === 'pending' && (
                    <button
                        onClick={() => onApproveSingle(tc.id)}
                        disabled={tc.validation && !tc.validation.valid}
                        className={`ml-auto p-1 rounded ${
                            tc.validation && !tc.validation.valid
                                ? 'opacity-50 cursor-not-allowed text-gray-500'
                                : 'hover:bg-gray-700 text-green-400 hover:text-green-300'
                        }`}
                        title={tc.validation && !tc.validation.valid ? (t('ai.tool.validationBlocked') || 'Blocked: validation failed') : (t('ai.batch.allowThis') || 'Allow this tool')}
                        aria-label={t('ai.toolApproval.approveThis') || 'Approve this tool call'}
                    >
                        <Check size={11} />
                    </button>
                )}
            </div>

            {/* Progress indicator for long-running tools */}
            {isExecuting && (
                <ToolProgressIndicator toolName={tc.toolName} />
            )}

            {/* Diff preview for edit tools */}
            {isEditTool && diffLoading && (
                <div className="px-3 py-1.5 text-[11px] text-gray-500 flex items-center gap-1">
                    <Loader2 size={10} className="animate-spin" />
                    {t('ai.tool.loadingDiff') || 'Loading diff preview...'}
                </div>
            )}
            {isEditTool && diffData && tc.status === 'pending' && (
                <DiffPreview
                    originalContent={diffData.original}
                    modifiedContent={diffData.modified}
                    fileName={String(tc.args.path || '')}
                    showActions={false}
                />
            )}
            {isEditTool && diffError && (
                <div className="px-3 py-1 text-[11px] text-yellow-400">
                    {t('ai.tool.previewUnavailable') || 'Preview unavailable:'} {diffError}
                </div>
            )}

            {/* Validation errors/warnings */}
            {tc.validation && !tc.validation.valid && (
                <div className="px-3 py-1 text-[11px]">
                    {tc.validation.errors.map((err, i) => (
                        <div key={`err-${i}`} className="text-red-400 flex items-center gap-1">
                            <span>{'\u2715'}</span> {err}
                        </div>
                    ))}
                </div>
            )}
            {tc.validation && tc.validation.warnings.length > 0 && (
                <div className="px-3 py-1 text-[11px]">
                    {tc.validation.warnings.map((warn, i) => (
                        <div key={`warn-${i}`} className="text-yellow-400 flex items-center gap-1">
                            <span>{'\u26A0'}</span> {warn}
                        </div>
                    ))}
                </div>
            )}

            {/* Expandable args */}
            {isExpanded && tc.status === 'pending' && (
                <div className="px-3 pb-1.5 font-mono text-[11px] border-t border-gray-700/30">
                    {Object.entries(tc.args).map(([key, value]) => (
                        <div key={key} className="flex gap-1 py-0.5">
                            <span className="text-gray-500">{key}:</span>
                            <span className="text-gray-300 break-all">{String(value as string)}</span>
                        </div>
                    ))}
                </div>
            )}

            {/* Result display */}
            {tc.result != null && (
                <div className="px-3 pb-1.5 text-[11px] text-gray-400 max-h-20 overflow-auto border-t border-gray-700/30">
                    <pre className="whitespace-pre-wrap">
                        {typeof tc.result === 'string' ? tc.result : JSON.stringify(tc.result as Record<string, unknown>, null, 2)}
                    </pre>
                </div>
            )}
        </div>
    );
};

export const BatchToolApproval: React.FC<BatchToolApprovalProps> = ({
    toolCalls,
    onApproveAll,
    onApproveSingle,
    onRejectAll,
    allTools,
}) => {
    const [expandedId, setExpandedId] = useState<string | null>(null);
    const [justApproved, setJustApproved] = useState(false);
    const i18n = useI18n();
    const { t } = i18n;

    const handleApproveAll = useCallback(() => {
        if (justApproved) return;
        setJustApproved(true);
        onApproveAll();
        setTimeout(() => setJustApproved(false), 500);
    }, [onApproveAll, justApproved]);

    const resolveTool = (name: string) =>
        allTools ? getToolByNameFromAll(name, allTools) : getToolByName(name);

    const hasHigh = toolCalls.some(tc => {
        const tool = resolveTool(tc.toolName);
        return tool?.dangerLevel === 'high';
    });

    const hasInvalidPending = toolCalls.some(tc => tc.status === 'pending' && tc.validation && !tc.validation.valid);

    return (
        <div className="my-2 border border-gray-700/50 rounded-lg overflow-hidden bg-gray-800/30" role="region" aria-label="Tool approval panel">
            {/* Header */}
            <div className="px-3 py-1.5 bg-gray-800/60 flex items-center gap-2 text-xs text-gray-300 border-b border-gray-700/50">
                <Shield size={12} className="text-yellow-400" />
                <span className="font-medium">
                    {toolCalls.length} {toolCalls.length === 1
                        ? (t('ai.batch.toolCall') || 'tool call')
                        : (t('ai.batch.toolCalls') || 'tool calls')
                    } {t('ai.toolApproval.pendingApproval') || 'pending approval'}
                </span>
            </div>

            {/* Tool call list */}
            <div role="list">
                {toolCalls.map((tc) => (
                    <BatchToolItem
                        key={tc.id}
                        tc={tc}
                        allTools={allTools}
                        onApproveSingle={onApproveSingle}
                        expandedId={expandedId}
                        setExpandedId={setExpandedId}
                        t={t}
                    />
                ))}
            </div>

            {/* Footer actions */}
            {toolCalls.some(tc => tc.status === 'pending') && (
                <div className="flex items-center gap-2 px-3 py-2 border-t border-gray-700/50 bg-gray-800/40">
                    <button
                        onClick={handleApproveAll}
                        disabled={justApproved || hasInvalidPending}
                        className={`flex items-center gap-1 px-3 py-1 rounded text-[11px] font-medium transition-colors ${
                            justApproved || hasInvalidPending
                                ? 'opacity-50 cursor-not-allowed bg-gray-600 text-gray-400'
                                : hasHigh
                                    ? 'bg-red-600/80 hover:bg-red-500 text-white'
                                    : 'bg-green-600/80 hover:bg-green-500 text-white'
                        }`}
                        aria-label={t('ai.toolApproval.allowAllCalls') || 'Allow all tool calls'}
                    >
                        <Check size={10} />
                        {t('ai.batch.allowAll') || 'Allow All'}
                    </button>
                    <button
                        onClick={onRejectAll}
                        className="flex items-center gap-1 px-3 py-1 rounded text-[11px] font-medium bg-gray-700/80 hover:bg-gray-600 text-gray-300 transition-colors"
                        aria-label={t('ai.toolApproval.rejectAllCalls') || 'Reject all tool calls'}
                    >
                        <X size={10} />
                        {t('ai.batch.rejectAll') || 'Reject All'}
                    </button>
                </div>
            )}
        </div>
    );
};

export default BatchToolApproval;
