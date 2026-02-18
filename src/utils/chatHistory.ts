/**
 * Chat History Persistence for AeroAgent — SQLite Backend
 *
 * All persistence is handled by Rust via Tauri invoke calls.
 * The old JSON flat-file approach is replaced by SQLite + FTS5.
 */

import { invoke } from '@tauri-apps/api/core';
import { logger } from './logger';

// ---------------------------------------------------------------------------
// Types (mirroring Rust structs)
// ---------------------------------------------------------------------------

export interface ChatSession {
    id: string;
    title: string;
    provider: string | null;
    model: string | null;
    message_count: number;
    total_tokens: number;
    total_cost: number;
    project_path: string | null;
    created_at: number;  // Unix timestamp ms
    updated_at: number;
}

export interface ChatMessage {
    id: string;
    session_id: string;
    role: string;
    content: string;
    tool_calls: string | null;
    thinking: string | null;
    tokens_in: number;
    tokens_out: number;
    cost: number;
    model: string | null;
    created_at: number;
}

export interface ChatBranch {
    id: string;
    session_id: string;
    name: string;
    parent_message_id: string;
    created_at: number;
}

export interface BranchMessage {
    id: string;
    branch_id: string;
    role: string;
    content: string;
    tool_calls: string | null;
    thinking: string | null;
    tokens_in: number;
    tokens_out: number;
    cost: number;
    model: string | null;
    created_at: number;
}

export interface SessionWithMessages {
    session: ChatSession;
    messages: ChatMessage[];
    branches: ChatBranch[];
    active_branch_id: string | null;
}

export interface SearchResult {
    message_id: string;
    session_id: string;
    session_title: string;
    role: string;
    content: string;
    created_at: number;
    snippet: string;
}

export interface ChatStats {
    total_sessions: number;
    total_messages: number;
    total_tokens: number;
    total_cost: number;
    db_size_bytes: number;
}

// ---------------------------------------------------------------------------
// Legacy types (kept for backward compatibility during transition)
// ---------------------------------------------------------------------------

export interface ConversationMessage {
    id: string;
    role: 'user' | 'assistant';
    content: string;
    timestamp: string;
    modelInfo?: {
        modelName: string;
        providerName: string;
        providerType: string;
    };
    tokenInfo?: {
        inputTokens?: number;
        outputTokens?: number;
        totalTokens?: number;
        cost?: number;
    };
}

export interface Conversation {
    id: string;
    title: string;
    messages: ConversationMessage[];
    createdAt: string;
    updatedAt: string;
    totalTokens: number;
    totalCost: number;
    branches?: ConversationBranch[];
    activeBranchId?: string;
}

export interface ConversationBranch {
    id: string;
    name: string;
    parentMessageId: string;
    messages: ConversationMessage[];
    createdAt: string;
}

// ---------------------------------------------------------------------------
// Conversion helpers (SQLite ↔ Legacy format)
// ---------------------------------------------------------------------------

function chatMessageToLegacy(msg: ChatMessage): ConversationMessage {
    return {
        id: msg.id,
        role: msg.role as 'user' | 'assistant',
        content: msg.content,
        timestamp: new Date(msg.created_at).toISOString(),
        modelInfo: msg.model ? {
            modelName: msg.model,
            providerName: '',
            providerType: '',
        } : undefined,
        tokenInfo: (msg.tokens_in > 0 || msg.tokens_out > 0 || msg.cost > 0) ? {
            inputTokens: msg.tokens_in,
            outputTokens: msg.tokens_out,
            totalTokens: msg.tokens_in + msg.tokens_out,
            cost: msg.cost,
        } : undefined,
    };
}

function sessionToConversation(data: SessionWithMessages): Conversation {
    return {
        id: data.session.id,
        title: data.session.title,
        messages: data.messages.map(chatMessageToLegacy),
        createdAt: new Date(data.session.created_at).toISOString(),
        updatedAt: new Date(data.session.updated_at).toISOString(),
        totalTokens: data.session.total_tokens,
        totalCost: data.session.total_cost,
        branches: data.branches.map(b => ({
            id: b.id,
            name: b.name,
            parentMessageId: b.parent_message_id,
            messages: [],  // Branch messages loaded on demand
            createdAt: new Date(b.created_at).toISOString(),
        })),
        activeBranchId: data.active_branch_id ?? undefined,
    };
}

// ---------------------------------------------------------------------------
// API functions
// ---------------------------------------------------------------------------

export async function initChatHistory(): Promise<string> {
    try {
        return await invoke<string>('chat_history_init');
    } catch (e) {
        logger.error('Chat history init failed:', e);
        return 'Init failed';
    }
}

export async function loadHistory(): Promise<Conversation[]> {
    try {
        const sessions = await invoke<ChatSession[]>('chat_history_list_sessions', {
            limit: 200,
            offset: 0,
        });

        // For the session list, we only need lightweight data
        // Full messages are loaded when switching to a conversation
        return sessions.map(s => ({
            id: s.id,
            title: s.title,
            messages: [],  // Loaded on demand
            createdAt: new Date(s.created_at).toISOString(),
            updatedAt: new Date(s.updated_at).toISOString(),
            totalTokens: s.total_tokens,
            totalCost: s.total_cost,
        }));
    } catch (e) {
        logger.error('Failed to load chat history:', e);
        return [];
    }
}

export async function loadSession(sessionId: string): Promise<Conversation | null> {
    try {
        const data = await invoke<SessionWithMessages>('chat_history_get_session', {
            sessionId,
        });
        return sessionToConversation(data);
    } catch (e) {
        logger.error('Failed to load session:', e);
        return null;
    }
}

export async function createSession(
    id: string,
    title: string,
    provider?: string,
    model?: string,
): Promise<ChatSession | null> {
    try {
        return await invoke<ChatSession>('chat_history_create_session', {
            id,
            title,
            provider: provider ?? null,
            model: model ?? null,
            projectPath: null,
        });
    } catch (e) {
        logger.error('Failed to create session:', e);
        return null;
    }
}

export async function saveMessage(
    sessionId: string,
    msg: ConversationMessage,
    provider?: string,
    model?: string,
): Promise<void> {
    try {
        const chatMsg: ChatMessage = {
            id: msg.id,
            session_id: sessionId,
            role: msg.role,
            content: msg.content,
            tool_calls: null,
            thinking: null,
            tokens_in: msg.tokenInfo?.inputTokens ?? 0,
            tokens_out: msg.tokenInfo?.outputTokens ?? 0,
            cost: msg.tokenInfo?.cost ?? 0,
            model: msg.modelInfo?.modelName ?? model ?? null,
            created_at: new Date(msg.timestamp).getTime(),
        };

        await invoke('chat_history_save_message', {
            sessionId,
            message: chatMsg,
            provider: provider ?? msg.modelInfo?.providerType ?? null,
            model: model ?? msg.modelInfo?.modelName ?? null,
        });
    } catch (e) {
        logger.error('Failed to save message:', e);
    }
}

export async function updateSessionTitle(sessionId: string, title: string): Promise<void> {
    try {
        await invoke('chat_history_update_session_title', { sessionId, title });
    } catch (e) {
        logger.error('Failed to update session title:', e);
    }
}

export async function deleteSession(sessionId: string): Promise<void> {
    try {
        await invoke('chat_history_delete_session', { sessionId });
    } catch (e) {
        logger.error('Failed to delete session:', e);
    }
}

export async function deleteSessionsBulk(
    sessionIds?: string[],
    olderThanDays?: number,
): Promise<number> {
    try {
        return await invoke<number>('chat_history_delete_sessions_bulk', {
            sessionIds: sessionIds ?? null,
            olderThanDays: olderThanDays ?? null,
        });
    } catch (e) {
        logger.error('Failed to bulk delete sessions:', e);
        return 0;
    }
}

export async function searchHistory(query: string, limit?: number): Promise<SearchResult[]> {
    try {
        return await invoke<SearchResult[]>('chat_history_search', {
            query,
            limit: limit ?? 50,
        });
    } catch (e) {
        logger.error('Failed to search chat history:', e);
        return [];
    }
}

export async function cleanupHistory(retentionDays: number): Promise<number> {
    try {
        return await invoke<number>('chat_history_cleanup', { retentionDays });
    } catch (e) {
        logger.error('Failed to cleanup chat history:', e);
        return 0;
    }
}

// F4: Dedicated clear-all (replaces semantic overload of deleteSessionsBulk(undefined, 0))
export async function clearAllHistory(): Promise<number> {
    try {
        return await invoke<number>('chat_history_clear_all');
    } catch (e) {
        logger.error('Failed to clear all chat history:', e);
        return 0;
    }
}

export async function getChatStats(): Promise<ChatStats | null> {
    try {
        return await invoke<ChatStats>('chat_history_stats');
    } catch (e) {
        logger.error('Failed to get chat stats:', e);
        return null;
    }
}

export async function exportSession(sessionId: string, format: 'json' | 'markdown'): Promise<string | null> {
    try {
        return await invoke<string>('chat_history_export_session', { sessionId, format });
    } catch (e) {
        logger.error('Failed to export session:', e);
        return null;
    }
}

export async function importSession(jsonData: string): Promise<string | null> {
    try {
        return await invoke<string>('chat_history_import', { jsonData });
    } catch (e) {
        logger.error('Failed to import session:', e);
        return null;
    }
}

// Branch management

export async function createBranch(
    sessionId: string,
    branchId: string,
    name: string,
    parentMessageId: string,
    messages: BranchMessage[],
): Promise<void> {
    try {
        await invoke('chat_history_create_branch', {
            sessionId,
            branchId,
            name,
            parentMessageId,
            messages,
        });
    } catch (e) {
        logger.error('Failed to create branch:', e);
    }
}

export async function switchBranch(
    sessionId: string,
    branchId: string | null,
): Promise<BranchMessage[]> {
    try {
        return await invoke<BranchMessage[]>('chat_history_switch_branch', {
            sessionId,
            branchId,
        });
    } catch (e) {
        logger.error('Failed to switch branch:', e);
        return [];
    }
}

export async function deleteBranch(sessionId: string, branchId: string): Promise<void> {
    try {
        await invoke('chat_history_delete_branch', { sessionId, branchId });
    } catch (e) {
        logger.error('Failed to delete branch:', e);
    }
}

export async function saveBranchMessage(branchId: string, msg: ConversationMessage): Promise<void> {
    try {
        const branchMsg: BranchMessage = {
            id: msg.id,
            branch_id: branchId,
            role: msg.role,
            content: msg.content,
            tool_calls: null,
            thinking: null,
            tokens_in: msg.tokenInfo?.inputTokens ?? 0,
            tokens_out: msg.tokenInfo?.outputTokens ?? 0,
            cost: msg.tokenInfo?.cost ?? 0,
            model: msg.modelInfo?.modelName ?? null,
            created_at: new Date(msg.timestamp).getTime(),
        };

        await invoke('chat_history_save_branch_message', {
            branchId,
            message: branchMsg,
        });
    } catch (e) {
        logger.error('Failed to save branch message:', e);
    }
}

// Legacy compatibility — createConversation for hook usage
export function createConversation(firstMessage?: string): Conversation {
    return {
        id: `conv-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`,
        title: firstMessage ? firstMessage.slice(0, 60) : 'New Chat',
        messages: [],
        createdAt: new Date().toISOString(),
        updatedAt: new Date().toISOString(),
        totalTokens: 0,
        totalCost: 0,
    };
}

// Legacy compat — saveConversation (wraps incremental saves)
export async function saveConversation(
    conversations: Conversation[],
    conversation: Conversation,
): Promise<Conversation[]> {
    // Save session metadata
    const existing = conversations.find(c => c.id === conversation.id);
    if (!existing) {
        await createSession(
            conversation.id,
            conversation.title,
        );
    } else {
        await updateSessionTitle(conversation.id, conversation.title);
    }

    // Save each message incrementally
    for (const msg of conversation.messages) {
        await saveMessage(conversation.id, msg);
    }

    // Update local list
    const idx = conversations.findIndex(c => c.id === conversation.id);
    const updated = [...conversations];
    if (idx >= 0) {
        updated[idx] = conversation;
    } else {
        updated.unshift(conversation);
    }
    return updated;
}

// Legacy compat — deleteConversation
export async function deleteConversation(
    conversations: Conversation[],
    conversationId: string,
): Promise<Conversation[]> {
    await deleteSession(conversationId);
    return conversations.filter(c => c.id !== conversationId);
}
