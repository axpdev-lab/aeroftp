import React, { useState } from 'react';
import { Check, X, ChevronDown, ChevronRight, Loader2, Shield, ShieldAlert, ShieldCheck } from 'lucide-react';
import { AgentToolCall, getToolByName, DangerLevel } from '../../types/tools';

interface ToolApprovalProps {
    toolCall: AgentToolCall;
    onApprove: () => void;
    onReject: () => void;
}

const dangerConfig: Record<DangerLevel, { accent: string; label: string; icon: typeof Shield }> = {
    safe: { accent: 'border-l-blue-500', label: 'auto', icon: ShieldCheck },
    medium: { accent: 'border-l-yellow-500', label: 'confirm', icon: Shield },
    high: { accent: 'border-l-red-500', label: 'danger', icon: ShieldAlert },
};

/** Compact tool label map for common tool names */
const toolLabels: Record<string, string> = {
    local_edit: 'Edit',
    local_write: 'Write',
    local_read: 'Read',
    local_list: 'List',
    local_delete: 'Delete',
    local_mkdir: 'Mkdir',
    local_rename: 'Rename',
    local_search: 'Search',
    remote_edit: 'Remote Edit',
    remote_delete: 'Remote Delete',
    remote_mkdir: 'Remote Mkdir',
    remote_rename: 'Remote Rename',
    remote_download: 'Download',
    remote_upload: 'Upload',
    upload_files: 'Upload',
    download_files: 'Download',
    sync_preview: 'Sync Preview',
    terminal_execute: 'Terminal',
    rag_index: 'Index',
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

export const ToolApproval: React.FC<ToolApprovalProps> = ({ toolCall, onApprove, onReject }) => {
    const [expanded, setExpanded] = useState(false);
    const tool = getToolByName(toolCall.toolName);
    const dangerLevel = tool?.dangerLevel || 'medium';
    const config = dangerConfig[dangerLevel];
    const DangerIcon = config.icon;

    const isExecuting = toolCall.status === 'executing';
    const isCompleted = toolCall.status === 'completed';
    const isError = toolCall.status === 'error';
    const isPending = toolCall.status === 'pending' || toolCall.status === 'approved';

    const label = toolLabels[toolCall.toolName] || toolCall.toolName;
    const mainArg = getMainArg(toolCall.toolName, toolCall.args);

    return (
        <div className={`border-l-2 ${config.accent} bg-gray-800/40 rounded-r my-1 text-xs`}>
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
                    >
                        {expanded ? <ChevronDown size={12} /> : <ChevronRight size={12} />}
                    </button>
                )}

                {/* Status indicators */}
                {isExecuting && (
                    <span className="ml-auto flex items-center gap-1 text-yellow-400">
                        <Loader2 size={11} className="animate-spin" /> Running...
                    </span>
                )}
                {isCompleted && (
                    <span className="ml-auto flex items-center gap-1 text-green-400">
                        <Check size={11} /> Done
                    </span>
                )}
                {isError && (
                    <span className="ml-auto text-red-400 truncate max-w-[200px]">
                        Failed{toolCall.error ? `: ${toolCall.error}` : ''}
                    </span>
                )}

                {/* Inline approve/reject buttons */}
                {isPending && toolCall.status === 'pending' && (
                    <div className="ml-auto flex items-center gap-1">
                        <button
                            onClick={onApprove}
                            className={`flex items-center gap-1 px-2 py-0.5 rounded text-[11px] font-medium transition-colors ${
                                dangerLevel === 'high'
                                    ? 'bg-red-600/80 hover:bg-red-500 text-white'
                                    : 'bg-green-600/80 hover:bg-green-500 text-white'
                            }`}
                        >
                            <Check size={10} />
                            {dangerLevel === 'high' ? 'Confirm' : 'Allow'}
                        </button>
                        <button
                            onClick={onReject}
                            className="flex items-center gap-1 px-2 py-0.5 rounded text-[11px] font-medium bg-gray-700/80 hover:bg-gray-600 text-gray-300 transition-colors"
                        >
                            <X size={10} />
                        </button>
                    </div>
                )}
            </div>

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
