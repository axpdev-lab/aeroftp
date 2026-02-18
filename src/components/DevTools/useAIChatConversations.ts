import { useState, useRef, useCallback } from 'react';
import { save } from '@tauri-apps/plugin-dialog';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import { AIProviderType } from '../../types/ai';
import {
    Conversation, ConversationMessage, ConversationBranch,
    loadHistory, loadSession, createSession, saveMessage,
    updateSessionTitle, deleteSession as deleteSessionApi,
    createBranch as createBranchApi, switchBranch as switchBranchApi,
    deleteBranch as deleteBranchApi, saveBranchMessage,
    createConversation, BranchMessage,
} from '../../utils/chatHistory';
import { Message } from './aiChatTypes';

export function useAIChatConversations() {
    const [messages, setMessages] = useState<Message[]>([]);
    const [conversations, setConversations] = useState<Conversation[]>([]);
    const [activeConversationId, setActiveConversationId] = useState<string | null>(null);
    const [showHistory, setShowHistory] = useState(false);
    const [showExportMenu, setShowExportMenu] = useState(false);
    const [expandedMessages, setExpandedMessages] = useState<Set<string>>(new Set());
    const [activeBranchId, setActiveBranchId] = useState<string | null>(null);

    const historyLoadedRef = useRef(false);
    const messagesRef = useRef(messages);
    messagesRef.current = messages;
    const conversationsRef = useRef(conversations);
    conversationsRef.current = conversations;
    const activeConversationIdRef = useRef(activeConversationId);
    activeConversationIdRef.current = activeConversationId;
    // BUG-007: Track saved content per message ID to detect streaming updates
    const savedMessageContentRef = useRef<Map<string, string>>(new Map());
    // Track whether title has been saved for current session
    const titleSavedRef = useRef(false);

    // Save conversation after messages change — uses incremental per-message saves
    const persistConversation = useCallback(async (msgs: Message[]) => {
        if (msgs.length === 0) return;

        let convId = activeConversationIdRef.current;

        // Create new session if needed
        if (!convId) {
            const newConv = createConversation(msgs[0]?.content);
            convId = newConv.id;
            setActiveConversationId(convId);
            activeConversationIdRef.current = convId;
            titleSavedRef.current = false;

            const title = msgs.find(m => m.role === 'user')?.content.slice(0, 60) || 'New Chat';
            await createSession(
                convId,
                title,
                msgs.find(m => m.modelInfo)?.modelInfo?.providerType,
                msgs.find(m => m.modelInfo)?.modelInfo?.modelName,
            );

            // Add to conversations list
            setConversations(prev => {
                const updated = [{ ...newConv, title, messages: [] }, ...prev];
                conversationsRef.current = updated;
                return updated;
            });
        }

        // BUG-007: Save new messages AND re-save messages whose content changed (streaming)
        const savedContent = savedMessageContentRef.current;
        for (const m of msgs) {
            const previousContent = savedContent.get(m.id);
            if (previousContent === m.content) continue; // Truly unchanged

            const convMsg: ConversationMessage = {
                id: m.id,
                role: m.role,
                content: m.content,
                timestamp: m.timestamp.toISOString(),
                modelInfo: m.modelInfo,
                tokenInfo: m.tokenInfo,
            };

            if (activeBranchId) {
                await saveBranchMessage(activeBranchId, convMsg);
            } else {
                await saveMessage(
                    convId,
                    convMsg,
                    m.modelInfo?.providerType,
                    m.modelInfo?.modelName,
                );
            }
            savedContent.set(m.id, m.content);
        }

        // PERF-009: Only update title once per session
        if (!titleSavedRef.current) {
            const title = msgs.find(m => m.role === 'user')?.content.slice(0, 60) || 'New Chat';
            const existingConv = conversationsRef.current.find(c => c.id === convId);
            if (existingConv && existingConv.title !== title) {
                await updateSessionTitle(convId, title);
            }
            titleSavedRef.current = true;
        }

        // Update local conversations list
        const totalTokens = msgs.reduce((sum, m) => sum + (m.tokenInfo?.totalTokens || 0), 0);
        const totalCost = msgs.reduce((sum, m) => sum + (m.tokenInfo?.cost || 0), 0);
        const title = msgs.find(m => m.role === 'user')?.content.slice(0, 60) || 'New Chat';

        setConversations(prev => {
            const idx = prev.findIndex(c => c.id === convId);
            if (idx >= 0) {
                const updated = [...prev];
                updated[idx] = {
                    ...updated[idx],
                    title,
                    updatedAt: new Date().toISOString(),
                    totalTokens,
                    totalCost,
                };
                conversationsRef.current = updated;
                return updated;
            }
            return prev;
        });
    }, [activeBranchId]);

    // New chat — resets messages and conversation ID.
    // Note: AIChat.tsx should wrap this to also clear pendingToolCalls.
    const startNewChat = useCallback(() => {
        setMessages([]);
        setActiveConversationId(null);
        setActiveBranchId(null);
        savedMessageContentRef.current = new Map();
        titleSavedRef.current = false;
    }, []);

    // Switch conversation — loads full messages from SQLite
    const switchConversation = useCallback(async (conv: Conversation) => {
        setActiveConversationId(conv.id);
        titleSavedRef.current = true; // Already has a title

        // Load full session data from SQLite
        const fullSession = await loadSession(conv.id);
        if (fullSession) {
            const loadedMessages = fullSession.messages.map(m => ({
                ...m,
                timestamp: new Date(m.timestamp),
                modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
            }));

            setActiveBranchId(fullSession.activeBranchId || null);

            // BUG-012: If active branch, load branch messages from backend
            if (fullSession.activeBranchId) {
                const branchMsgs = await switchBranchApi(conv.id, fullSession.activeBranchId);
                if (branchMsgs.length > 0) {
                    const mapped = branchMsgs.map(m => ({
                        id: m.id,
                        role: m.role as 'user' | 'assistant',
                        content: m.content,
                        timestamp: new Date(m.created_at),
                        modelInfo: m.model ? {
                            modelName: m.model,
                            providerName: '',
                            providerType: '' as AIProviderType,
                        } : undefined,
                        tokenInfo: (m.tokens_in > 0 || m.tokens_out > 0) ? {
                            inputTokens: m.tokens_in,
                            outputTokens: m.tokens_out,
                            totalTokens: m.tokens_in + m.tokens_out,
                            cost: m.cost,
                        } : undefined,
                    }));
                    setMessages(mapped);
                    savedMessageContentRef.current = new Map(mapped.map(m => [m.id, m.content]));
                } else {
                    setMessages(loadedMessages);
                    savedMessageContentRef.current = new Map(loadedMessages.map(m => [m.id, m.content]));
                }
            } else {
                setMessages(loadedMessages);
                savedMessageContentRef.current = new Map(loadedMessages.map(m => [m.id, m.content]));
            }
        } else {
            // Fallback: use what we have in-memory
            const fallbackMsgs = conv.messages.map(m => ({
                ...m,
                timestamp: new Date(m.timestamp),
                modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
            }));
            setMessages(fallbackMsgs);
            savedMessageContentRef.current = new Map(fallbackMsgs.map(m => [m.id, m.content]));
        }

        setShowHistory(false);
    }, []);

    // Delete conversation
    const handleDeleteConversation = useCallback(async (convId: string) => {
        await deleteSessionApi(convId);
        setConversations(prev => {
            const updated = prev.filter(c => c.id !== convId);
            conversationsRef.current = updated;
            return updated;
        });
        if (convId === activeConversationIdRef.current) {
            startNewChat();
        }
    }, [startNewChat]);

    // BUG-010/PERF-010: Support force reload after cleanup/delete
    const loadChatHistory = useCallback(async (force = false) => {
        if (historyLoadedRef.current && !force) return;
        try {
            const history = await loadHistory();
            setConversations(history);
            conversationsRef.current = history;

            // Only restore last session on initial load, not force-reload
            if (!historyLoadedRef.current && history.length > 0) {
                const last = history[0];
                setActiveConversationId(last.id);
                activeConversationIdRef.current = last.id;

                const fullSession = await loadSession(last.id);
                if (fullSession) {
                    const branchId = fullSession.activeBranchId || null;
                    setActiveBranchId(branchId);

                    if (branchId) {
                        // BUG-012: Load branch messages from backend
                        const branchMsgs = await switchBranchApi(last.id, branchId);
                        if (branchMsgs.length > 0) {
                            const mapped = branchMsgs.map(m => ({
                                id: m.id,
                                role: m.role as 'user' | 'assistant',
                                content: m.content,
                                timestamp: new Date(m.created_at),
                                modelInfo: m.model ? {
                                    modelName: m.model,
                                    providerName: '',
                                    providerType: '' as AIProviderType,
                                } : undefined,
                                tokenInfo: (m.tokens_in > 0 || m.tokens_out > 0) ? {
                                    inputTokens: m.tokens_in,
                                    outputTokens: m.tokens_out,
                                    totalTokens: m.tokens_in + m.tokens_out,
                                    cost: m.cost,
                                } : undefined,
                            }));
                            setMessages(mapped);
                            savedMessageContentRef.current = new Map(mapped.map(m => [m.id, m.content]));
                        } else {
                            setMessages(fullSession.messages.map(m => ({
                                ...m,
                                timestamp: new Date(m.timestamp),
                                modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                            })));
                            savedMessageContentRef.current = new Map(fullSession.messages.map(m => [m.id, m.content]));
                        }
                    } else {
                        setMessages(fullSession.messages.map(m => ({
                            ...m,
                            timestamp: new Date(m.timestamp),
                            modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                        })));
                        savedMessageContentRef.current = new Map(fullSession.messages.map(m => [m.id, m.content]));
                    }
                }
            }

            historyLoadedRef.current = true;
        } catch {
            // Don't set historyLoadedRef on error so retry is possible
        }
    }, []);

    // Export conversation
    const exportConversation = useCallback(async (format: 'markdown' | 'json') => {
        setShowExportMenu(false);
        if (messages.length === 0) return;

        try {
            const timestamp = new Date().toISOString().slice(0, 10);
            const title = conversationsRef.current.find(c => c.id === activeConversationIdRef.current)?.title || 'AeroAgent Chat';

            if (format === 'markdown') {
                const lines: string[] = [
                    `# ${title}`,
                    `*Exported on ${new Date().toLocaleString()}*`,
                    '',
                ];
                for (const msg of messages) {
                    const role = msg.role === 'user' ? 'User' : 'AeroAgent';
                    const modelTag = msg.modelInfo ? ` *(${msg.modelInfo.modelName})*` : '';
                    lines.push(`### ${role}${modelTag}`);
                    lines.push(msg.content);
                    if (msg.tokenInfo?.totalTokens) {
                        lines.push(`> ${msg.tokenInfo.totalTokens} tokens${msg.tokenInfo.cost ? ` · $${msg.tokenInfo.cost.toFixed(4)}` : ''}`);
                    }
                    lines.push('');
                }
                lines.push('---');
                lines.push('*Exported from AeroFTP AeroAgent*');

                const filePath = await save({
                    defaultPath: `aerochat-${timestamp}.md`,
                    filters: [{ name: 'Markdown', extensions: ['md'] }],
                });
                if (filePath) {
                    await writeTextFile(filePath, lines.join('\n'));
                }
            } else {
                const conv = conversationsRef.current.find(c => c.id === activeConversationIdRef.current);
                const exportData = {
                    title,
                    exportedAt: new Date().toISOString(),
                    messageCount: messages.length,
                    totalTokens: messages.reduce((sum, m) => sum + (m.tokenInfo?.totalTokens || 0), 0),
                    totalCost: messages.reduce((sum, m) => sum + (m.tokenInfo?.cost || 0), 0),
                    messages: messages.map(m => ({
                        role: m.role,
                        content: m.content,
                        timestamp: m.timestamp.toISOString(),
                        modelInfo: m.modelInfo || null,
                        tokenInfo: m.tokenInfo || null,
                    })),
                    metadata: conv ? {
                        conversationId: conv.id,
                        createdAt: conv.createdAt,
                        updatedAt: conv.updatedAt,
                    } : null,
                };

                const filePath = await save({
                    defaultPath: `aerochat-${timestamp}.json`,
                    filters: [{ name: 'JSON', extensions: ['json'] }],
                });
                if (filePath) {
                    await writeTextFile(filePath, JSON.stringify(exportData, null, 2));
                }
            }
        } catch {
            // Dialog cancelled or write error — silent
        }
    }, [messages]);

    // Fork conversation at a specific message
    const forkConversation = useCallback(async (messageId: string) => {
        if (!activeConversationIdRef.current) return;

        const messageIdx = messagesRef.current.findIndex(m => m.id === messageId);
        if (messageIdx < 0) return;

        const branchId = `branch-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
        const branchName = `Branch ${(conversationsRef.current.find(c => c.id === activeConversationIdRef.current)?.branches?.length || 0) + 1}`;

        // Messages up to fork point
        const branchMessages: BranchMessage[] = messagesRef.current
            .slice(0, messageIdx + 1)
            .map(m => ({
                id: m.id,
                branch_id: branchId,
                role: m.role,
                content: m.content,
                tool_calls: null,
                thinking: null,
                tokens_in: m.tokenInfo?.inputTokens ?? 0,
                tokens_out: m.tokenInfo?.outputTokens ?? 0,
                cost: m.tokenInfo?.cost ?? 0,
                model: m.modelInfo?.modelName ?? null,
                created_at: m.timestamp.getTime(),
            }));

        await createBranchApi(
            activeConversationIdRef.current,
            branchId,
            branchName,
            messageId,
            branchMessages,
        );

        // Update local state
        setActiveBranchId(branchId);
        const sliced = messagesRef.current.slice(0, messageIdx + 1);
        savedMessageContentRef.current = new Map(sliced.map(m => [m.id, m.content]));
        setMessages(sliced);

        // Update conversations list with new branch
        setConversations(prev => {
            const idx = prev.findIndex(c => c.id === activeConversationIdRef.current);
            if (idx >= 0) {
                const updated = [...prev];
                const conv = updated[idx];
                updated[idx] = {
                    ...conv,
                    branches: [...(conv.branches || []), {
                        id: branchId,
                        name: branchName,
                        parentMessageId: messageId,
                        messages: [],
                        createdAt: new Date().toISOString(),
                    }],
                    activeBranchId: branchId,
                };
                conversationsRef.current = updated;
                return updated;
            }
            return prev;
        });
    }, []);

    // Switch between branches
    const switchBranch = useCallback(async (branchId: string | null) => {
        if (!activeConversationIdRef.current) return;

        const branchMsgs = await switchBranchApi(activeConversationIdRef.current, branchId);
        setActiveBranchId(branchId);

        if (branchId === null) {
            // Switch to main — reload session messages
            const fullSession = await loadSession(activeConversationIdRef.current);
            if (fullSession) {
                setMessages(fullSession.messages.map(m => ({
                    ...m,
                    timestamp: new Date(m.timestamp),
                    modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                })));
                savedMessageContentRef.current = new Map(fullSession.messages.map(m => [m.id, m.content]));
            }
        } else if (branchMsgs.length > 0) {
            // Switch to branch messages
            const mapped = branchMsgs.map(m => ({
                id: m.id,
                role: m.role as 'user' | 'assistant',
                content: m.content,
                timestamp: new Date(m.created_at),
                modelInfo: m.model ? {
                    modelName: m.model,
                    providerName: '',
                    providerType: '' as AIProviderType,
                } : undefined,
                tokenInfo: (m.tokens_in > 0 || m.tokens_out > 0) ? {
                    inputTokens: m.tokens_in,
                    outputTokens: m.tokens_out,
                    totalTokens: m.tokens_in + m.tokens_out,
                    cost: m.cost,
                } : undefined,
            }));
            setMessages(mapped);
            savedMessageContentRef.current = new Map(mapped.map(m => [m.id, m.content]));
        }

        // Update conversations list
        setConversations(prev => {
            const idx = prev.findIndex(c => c.id === activeConversationIdRef.current);
            if (idx >= 0) {
                const updated = [...prev];
                updated[idx] = { ...updated[idx], activeBranchId: branchId ?? undefined };
                conversationsRef.current = updated;
                return updated;
            }
            return prev;
        });
    }, []);

    // Delete a branch
    const deleteBranch = useCallback(async (branchId: string) => {
        if (!activeConversationIdRef.current) return;

        await deleteBranchApi(activeConversationIdRef.current, branchId);

        if (activeBranchId === branchId) {
            // Switch back to main
            setActiveBranchId(null);
            const fullSession = await loadSession(activeConversationIdRef.current);
            if (fullSession) {
                setMessages(fullSession.messages.map(m => ({
                    ...m,
                    timestamp: new Date(m.timestamp),
                    modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                })));
                savedMessageContentRef.current = new Map(fullSession.messages.map(m => [m.id, m.content]));
            }
        }

        // Update conversations list
        setConversations(prev => {
            const idx = prev.findIndex(c => c.id === activeConversationIdRef.current);
            if (idx >= 0) {
                const updated = [...prev];
                const conv = updated[idx];
                updated[idx] = {
                    ...conv,
                    branches: (conv.branches || []).filter(b => b.id !== branchId),
                    activeBranchId: activeBranchId === branchId ? undefined : conv.activeBranchId,
                };
                conversationsRef.current = updated;
                return updated;
            }
            return prev;
        });
    }, [activeBranchId]);

    return {
        messages, setMessages,
        conversations, setConversations,
        activeConversationId, setActiveConversationId,
        showHistory, setShowHistory,
        showExportMenu, setShowExportMenu,
        expandedMessages, setExpandedMessages,
        activeBranchId, setActiveBranchId,
        messagesRef, conversationsRef,
        persistConversation,
        startNewChat, switchConversation, handleDeleteConversation,
        forkConversation, switchBranch, deleteBranch,
        loadChatHistory,
        exportConversation,
    };
}
