/**
 * Chat History Persistence for AeroAgent
 * Saves conversations to app config directory via Tauri plugin-fs
 */

import { readTextFile, writeTextFile, BaseDirectory } from '@tauri-apps/plugin-fs';
import { logger } from './logger';

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

const MAX_CONVERSATIONS = 50;
const MAX_MESSAGES_PER_CONVERSATION = 200;
const FILENAME = 'ai_history.json';

const FS_OPTS = { baseDir: BaseDirectory.AppConfig };

export async function loadHistory(): Promise<Conversation[]> {
    try {
        const content = await readTextFile(FILENAME, FS_OPTS);
        const data = JSON.parse(content);
        return Array.isArray(data) ? data : [];
    } catch {
        return [];
    }
}

export async function saveHistory(conversations: Conversation[]): Promise<void> {
    try {
        // Enforce limits
        const trimmed = conversations.slice(0, MAX_CONVERSATIONS).map(c => ({
            ...c,
            messages: c.messages.slice(-MAX_MESSAGES_PER_CONVERSATION),
        }));

        // AppConfig dir is created by Rust setup in lib.rs
        await writeTextFile(FILENAME, JSON.stringify(trimmed, null, 2), FS_OPTS);
    } catch (e) {
        logger.error('Failed to save chat history:', e);
    }
}

export async function saveConversation(
    conversations: Conversation[],
    conversation: Conversation
): Promise<Conversation[]> {
    const idx = conversations.findIndex(c => c.id === conversation.id);
    const updated = [...conversations];
    if (idx >= 0) {
        updated[idx] = conversation;
    } else {
        updated.unshift(conversation);
    }
    await saveHistory(updated);
    return updated;
}

export async function deleteConversation(
    conversations: Conversation[],
    conversationId: string
): Promise<Conversation[]> {
    const updated = conversations.filter(c => c.id !== conversationId);
    await saveHistory(updated);
    return updated;
}

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
