// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// PERF-02: a single chat message row, extracted from AIChat.tsx and wrapped in
// React.memo. Streaming updates rebuild only the streaming message's object
// (setMessages(prev => prev.map(...))), so a reference-based comparator lets
// every non-streaming row skip reconciliation on each streamed token, bounding
// per-token render work to the one row that actually changed.

import React from 'react';
import { Globe, Check, Copy, Wrench, GitBranch } from 'lucide-react';
import { MarkdownRenderer } from './MarkdownRenderer';
import { ThinkingBlock } from './ThinkingBlock';
import { getToolLabel } from './aiChatToolLabels';
import { CodingPlanArtifactCard } from './CodingPlanArtifactCard';
import { extractCodingPlanArtifact, getCodingPlanFromResultData } from './aiChatCodingPlan';
import { CodingPatchReview } from './CodingPatchReview';
import { getCodingPatchFromResultData } from './aiChatCodingPatch';
import { CodingCheckpointRestoreReview } from './CodingCheckpointRestoreReview';
import { getCodingCheckpointRestoreFromResultData } from './aiChatCodingCheckpointRestore';
import { CodingGitReview } from './CodingGitReview';
import { getCodingGitFromResultData } from './aiChatCodingGit';
import { CodingChecksReview } from './CodingChecksReview';
import { getCodingRunCheckFromResultData } from './aiChatCodingChecks';
import type { Message, TransferPlan, TransferPlanResultData } from './aiChatTypes';
import type { AIProviderType } from '../../types/ai';

interface ChatMessageRowProps {
    message: Message;
    ct: Record<string, string>;
    t: (key: string) => string;
    /** isLoading && this row is the active streaming message. */
    isStreamingRow: boolean;
    isExpanded: boolean;
    isCopied: boolean;
    isExecutingPlan: boolean;
    editorFilePath?: string;
    editorFileName?: string;
    onToggleExpand: (id: string, expand: boolean) => void;
    onCopy: (message: Message) => void;
    onFork: (id: string) => void;
    onExecutePlan: (message: Message, selectedOperationIds: string[]) => Promise<void>;
    getProviderIcon: (type: AIProviderType, size?: number) => React.ReactNode;
    isTransferPlanResultData: (value: unknown) => value is TransferPlanResultData;
    TransferPlanReview: React.ComponentType<{
        plan: TransferPlan;
        isExecuting: boolean;
        onExecute: (selectedOperationIds: string[]) => Promise<void>;
    }>;
}

const ChatMessageRowImpl: React.FC<ChatMessageRowProps> = ({
    message,
    ct,
    t,
    isStreamingRow,
    isExpanded,
    isCopied,
    isExecutingPlan,
    editorFilePath,
    editorFileName,
    onToggleExpand,
    onCopy,
    onFork,
    onExecutePlan,
    getProviderIcon,
    isTransferPlanResultData,
    TransferPlanReview,
}) => {
    const isAssistant = message.role === 'assistant';
    const extractedCodingPlan = React.useMemo(
        () => extractCodingPlanArtifact(message.content),
        [message.content],
    );
    const codingPlan = getCodingPlanFromResultData(message.toolResultData) || extractedCodingPlan.plan;
    const codingPatch = getCodingPatchFromResultData(message.toolResultData);
    const codingCheckpointRestore = getCodingCheckpointRestoreFromResultData(message.toolResultData);
    const codingGit = getCodingGitFromResultData(message.toolResultData);
    const codingRunCheck = getCodingRunCheckFromResultData(message.toolResultData);
    const renderedContent = codingPlan ? extractedCodingPlan.content : message.content;
    const isLong = isAssistant && renderedContent.length > 500;
    return (
        <div
            data-message-id={message.id}
            className={`flex gap-3 ${message.role === 'user' ? 'justify-end' : 'justify-start'}`}
        >
            <div
                className={`max-w-[85%] rounded-lg px-4 py-2 text-sm select-text ${message.role === 'user' ? ct.userMsg : ct.assistantMsg
                    }`}
            >
                {/* Image thumbnails for vision messages */}
                {message.images && message.images.length > 0 && (
                    <div className="flex gap-1.5 mb-2 flex-wrap">
                        {message.images.map((img, i) => (
                            <img key={i} src={img.preview} alt="Attached image" className="h-16 w-16 object-cover rounded border border-white/20" />
                        ))}
                    </div>
                )}
                {/* Thinking block (Claude extended thinking) */}
                {message.thinking && (
                    <ThinkingBlock
                        content={message.thinking}
                        isComplete={!!message.thinkingDuration}
                        duration={message.thinkingDuration}
                        thinkingTokens={message.tokenInfo?.outputTokens}
                        responseTokens={message.tokenInfo?.inputTokens}
                    />
                )}
                {message.webSearchUsed && (
                    <span className="text-[10px] text-zinc-500 flex items-center gap-1 mb-1">
                        <Globe size={10} /> {t('ai.webSearchUsed')}
                    </span>
                )}
                <div className="relative">
                    <div
                        className={`select-text ${ct.prose} max-w-none ${isLong && !isExpanded ? 'max-h-[200px] overflow-hidden' : ''
                            }`}
                    >
                        <MarkdownRenderer
                            content={renderedContent}
                            isStreaming={isStreamingRow}
                            editorFilePath={editorFilePath}
                            editorFileName={editorFileName}
                        />
                        {codingPlan && (
                            <CodingPlanArtifactCard plan={codingPlan} />
                        )}
                        {isTransferPlanResultData(message.toolResultData) && (
                            <TransferPlanReview
                                plan={message.toolResultData.plan}
                                isExecuting={isExecutingPlan}
                                onExecute={async (selectedOperationIds) => {
                                    await onExecutePlan(message, selectedOperationIds);
                                }}
                            />
                        )}
                        {codingPatch && (
                            <CodingPatchReview
                                result={codingPatch.result}
                                patchText={codingPatch.patch}
                                workspaceRoot={codingPatch.workspaceRoot}
                                mode="result"
                            />
                        )}
                        {codingCheckpointRestore && (
                            <CodingCheckpointRestoreReview
                                result={codingCheckpointRestore.result}
                                paths={codingCheckpointRestore.requestedPaths}
                                mode="result"
                            />
                        )}
                        {codingGit && (
                            <CodingGitReview data={codingGit} />
                        )}
                        {codingRunCheck && (
                            <CodingChecksReview data={codingRunCheck} />
                        )}
                    </div>
                    {isLong && !isExpanded && (
                        <div className={`absolute bottom-0 left-0 right-0 h-8 bg-gradient-to-t ${ct.gradient} to-transparent flex items-end justify-center`}>
                            <button
                                onClick={() => onToggleExpand(message.id, true)}
                                className="text-xs text-purple-400 hover:text-purple-300 pb-0.5"
                            >
                                {t('ai.showMore') || 'Show more'} ▾
                            </button>
                        </div>
                    )}
                    {isLong && isExpanded && (
                        <button
                            onClick={() => onToggleExpand(message.id, false)}
                            className="text-xs text-purple-400 hover:text-purple-300 mt-1"
                        >
                            {t('ai.showLess') || 'Show less'} ▴
                        </button>
                    )}
                </div>
                <div className={`text-[10px] mt-1 flex items-center gap-2 flex-wrap ${message.role === 'user' ? ct.userMsgMeta : ct.textMuted}`}>
                    <span>{message.timestamp.toLocaleTimeString()}</span>
                    {isAssistant && (
                        <button
                            onClick={() => onCopy(message)}
                            className={`${ct.textMuted} ${ct.textHover} transition-colors`}
                            title={t('ai.copy') || 'Copy'}
                        >
                            {isCopied ? <Check size={10} className="text-green-400" /> : <Copy size={10} />}
                        </button>
                    )}
                    {message.toolName && (
                        <span className="flex items-center gap-1 text-purple-400/70">
                            <Wrench size={9} />
                            <span>{getToolLabel(message.toolName, t)}</span>
                        </span>
                    )}
                    {isAssistant && (
                        <button
                            onClick={() => onFork(message.id)}
                            className={`p-0.5 ${ct.textMuted} hover:text-purple-400 transition-colors`}
                            title={t('ai.branch.fork') || 'Fork here'}
                        >
                            <GitBranch size={10} />
                        </button>
                    )}
                    {isAssistant && message.modelInfo && (
                        <span className={`flex items-center gap-1 ${ct.textSecondary}`}>
                            • {getProviderIcon(message.modelInfo.providerType, 10)}
                            <span>{message.modelInfo.modelName}</span>
                        </span>
                    )}
                    {message.tokenInfo && (
                        <span className="flex items-center gap-1 text-gray-500">
                            • {message.tokenInfo.totalTokens ?? ((message.tokenInfo.inputTokens || 0) + (message.tokenInfo.outputTokens || 0))} tok
                            {message.tokenInfo.cost !== undefined && message.tokenInfo.cost > 0 && (
                                <span className="text-green-500/70">
                                    ${message.tokenInfo.cost < 0.01 ? message.tokenInfo.cost.toFixed(4) : message.tokenInfo.cost.toFixed(3)}
                                </span>
                            )}
                            {message.tokenInfo.cacheSavings !== undefined && message.tokenInfo.cacheSavings > 0 && (
                                <span className="text-cyan-500/70" title={`Cache: ${message.tokenInfo.cacheReadTokens || 0} read, ${message.tokenInfo.cacheCreationTokens || 0} created`}>
                                    ↓${message.tokenInfo.cacheSavings < 0.01 ? message.tokenInfo.cacheSavings.toFixed(4) : message.tokenInfo.cacheSavings.toFixed(3)}
                                </span>
                            )}
                        </span>
                    )}
                </div>
            </div>
        </div>
    );
};

function rowsEqual(prev: ChatMessageRowProps, next: ChatMessageRowProps): boolean {
    // Reference equality on `message` is the key lever: only the streaming row's
    // object is rebuilt per token, so all other rows compare equal and skip.
    // The per-row flags catch copy/expand/execute toggles, and ct/t/editor* catch
    // theme, language, and editor-context changes that affect every row.
    return (
        prev.message === next.message &&
        prev.isStreamingRow === next.isStreamingRow &&
        prev.isExpanded === next.isExpanded &&
        prev.isCopied === next.isCopied &&
        prev.isExecutingPlan === next.isExecutingPlan &&
        prev.ct === next.ct &&
        prev.t === next.t &&
        prev.editorFilePath === next.editorFilePath &&
        prev.editorFileName === next.editorFileName
    );
}

export const ChatMessageRow = React.memo(ChatMessageRowImpl, rowsEqual);
export default ChatMessageRow;
