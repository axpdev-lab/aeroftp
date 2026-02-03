/**
 * Chat History Persistence for AeroAgent
 * Saves conversations to app config directory via Tauri plugin-fs
 */

import { readTextFile, writeTextFile, mkdir, exists } from '@tauri-apps/plugin-fs';
import { appConfigDir } from '@tauri-apps/api/path';

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
}

const MAX_CONVERSATIONS = 50;
const MAX_MESSAGES_PER_CONVERSATION = 200;
const FILENAME = 'ai_history.json';

let cachedPath: string | null = null;

async function getHistoryPath(): Promise<string> {
    if (cachedPath) return cachedPath;
    const configDir = await appConfigDir();
    cachedPath = `${configDir}${FILENAME}`;
    return cachedPath;
}

export async function loadHistory(): Promise<Conversation[]> {
    try {
        const path = await getHistoryPath();
        const configDir = await appConfigDir();

        // Ensure config directory exists
        if (!(await exists(configDir))) {
            await mkdir(configDir, { recursive: true });
        }

        if (!(await exists(path))) {
            return [];
        }

        const content = await readTextFile(path);
        const data = JSON.parse(content);
        return Array.isArray(data) ? data : [];
    } catch {
        return [];
    }
}

export async function saveHistory(conversations: Conversation[]): Promise<void> {
    try {
        const path = await getHistoryPath();
        const configDir = await appConfigDir();

        if (!(await exists(configDir))) {
            await mkdir(configDir, { recursive: true });
        }

        // Enforce limits
        const trimmed = conversations.slice(0, MAX_CONVERSATIONS).map(c => ({
            ...c,
            messages: c.messages.slice(-MAX_MESSAGES_PER_CONVERSATION),
        }));

        await writeTextFile(path, JSON.stringify(trimmed, null, 2));
    } catch (e) {
        console.error('Failed to save chat history:', e);
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
