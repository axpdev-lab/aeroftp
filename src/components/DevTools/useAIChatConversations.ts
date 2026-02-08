import { useState, useRef, useCallback } from 'react';
import { save } from '@tauri-apps/plugin-dialog';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import { AIProviderType } from '../../types/ai';
import { Conversation, ConversationMessage, ConversationBranch, loadHistory, saveConversation, deleteConversation, createConversation } from '../../utils/chatHistory';
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

    // Save conversation after messages change
    const persistConversation = useCallback(async (msgs: Message[]) => {
        if (msgs.length === 0) return;
        const convId = activeConversationIdRef.current || createConversation(msgs[0]?.content).id;
        if (!activeConversationIdRef.current) setActiveConversationId(convId);

        const convMessages: ConversationMessage[] = msgs.map(m => ({
            id: m.id,
            role: m.role,
            content: m.content,
            timestamp: m.timestamp.toISOString(),
            modelInfo: m.modelInfo,
            tokenInfo: m.tokenInfo,
        }));

        const totalTokens = msgs.reduce((sum, m) => sum + (m.tokenInfo?.totalTokens || 0), 0);
        const totalCost = msgs.reduce((sum, m) => sum + (m.tokenInfo?.cost || 0), 0);

        const latestConversations = conversationsRef.current;
        const existingConv = latestConversations.find(c => c.id === convId);

        const conv: Conversation = {
            id: convId,
            title: msgs.find(m => m.role === 'user')?.content.slice(0, 60) || 'New Chat',
            messages: activeBranchId ? (existingConv?.messages || []) : convMessages,
            createdAt: existingConv?.createdAt || new Date().toISOString(),
            updatedAt: new Date().toISOString(),
            totalTokens,
            totalCost,
            branches: activeBranchId
                ? (existingConv?.branches || []).map(b =>
                    b.id === activeBranchId ? { ...b, messages: convMessages } : b
                  )
                : existingConv?.branches,
            activeBranchId: existingConv?.activeBranchId,
        };

        const updated = await saveConversation(latestConversations, conv);
        setConversations(updated);
    }, [activeBranchId]);

    // New chat — resets messages and conversation ID.
    // Note: AIChat.tsx should wrap this to also clear pendingToolCalls.
    const startNewChat = useCallback(() => {
        setMessages([]);
        setActiveConversationId(null);
        setActiveBranchId(null);
    }, []);

    // Switch conversation
    const switchConversation = useCallback((conv: Conversation) => {
        setActiveConversationId(conv.id);
        setMessages(conv.messages.map(m => ({
            ...m,
            timestamp: new Date(m.timestamp),
            modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
        })));
        setActiveBranchId(conv.activeBranchId || null);
        setShowHistory(false);
    }, []);

    // Delete conversation
    const handleDeleteConversation = useCallback(async (convId: string) => {
        const updated = await deleteConversation(conversationsRef.current, convId);
        setConversations(updated);
        if (convId === activeConversationId) {
            startNewChat();
        }
    }, [activeConversationId, startNewChat]);

    // Load chat history on mount (call this from a useEffect in AIChat.tsx)
    const loadChatHistory = useCallback(async () => {
        if (historyLoadedRef.current) return;
        historyLoadedRef.current = true;
        try {
            const history = await loadHistory();
            setConversations(history);
            // Restore last active conversation
            if (history.length > 0) {
                const last = history[0];
                setActiveConversationId(last.id);

                // Restore active branch if exists
                if (last.activeBranchId) {
                    setActiveBranchId(last.activeBranchId);
                    const branch = last.branches?.find(b => b.id === last.activeBranchId);
                    if (branch) {
                        setMessages(branch.messages.map(m => ({
                            ...m,
                            timestamp: new Date(m.timestamp),
                            modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                        })));
                    } else {
                        // Branch not found, fallback to main
                        setMessages(last.messages.map(m => ({
                            ...m,
                            timestamp: new Date(m.timestamp),
                            modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                        })));
                    }
                } else {
                    setMessages(last.messages.map(m => ({
                        ...m,
                        timestamp: new Date(m.timestamp),
                        modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                    })));
                }
            }
        } catch { /* silent */ }
    }, []);

    // Export conversation
    const exportConversation = useCallback(async (format: 'markdown' | 'json') => {
        setShowExportMenu(false);
        if (messages.length === 0) return;

        try {
            const timestamp = new Date().toISOString().slice(0, 10);
            const title = conversationsRef.current.find(c => c.id === activeConversationId)?.title || 'AeroAgent Chat';

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
                const conv = conversationsRef.current.find(c => c.id === activeConversationId);
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
    }, [messages, activeConversationId]);

    // Fork conversation at a specific message
    const forkConversation = useCallback(async (messageId: string) => {
        if (!activeConversationId) return;

        const messageIdx = messagesRef.current.findIndex(m => m.id === messageId);
        if (messageIdx < 0) return;

        const branchId = `branch-${Date.now()}-${Math.random().toString(36).slice(2, 6)}`;
        const branchName = `Branch ${(conversationsRef.current.find(c => c.id === activeConversationId)?.branches?.length || 0) + 1}`;

        // Messages up to fork point go to the branch
        const branchMessages: ConversationMessage[] = messagesRef.current
            .slice(0, messageIdx + 1)
            .map(m => ({
                id: m.id,
                role: m.role,
                content: m.content,
                timestamp: m.timestamp.toISOString(),
                modelInfo: m.modelInfo,
                tokenInfo: m.tokenInfo,
            }));

        const newBranch: ConversationBranch = {
            id: branchId,
            name: branchName,
            parentMessageId: messageId,
            messages: branchMessages,
            createdAt: new Date().toISOString(),
        };

        // Update conversation with new branch
        const conv = conversationsRef.current.find(c => c.id === activeConversationId);
        if (!conv) return;

        const updatedConv: Conversation = {
            ...conv,
            branches: [...(conv.branches || []), newBranch],
            activeBranchId: branchId,
        };

        const updated = await saveConversation(conversationsRef.current, updatedConv);
        setConversations(updated);
        setActiveBranchId(branchId);

        // Load branch messages (up to fork point)
        setMessages(messagesRef.current.slice(0, messageIdx + 1));
    }, [activeConversationId]);

    // Switch between branches
    const switchBranch = useCallback(async (branchId: string | null) => {
        if (!activeConversationId) return;
        const conv = conversationsRef.current.find(c => c.id === activeConversationId);
        if (!conv) return;

        setActiveBranchId(branchId);

        if (branchId === null) {
            // Switch to main conversation
            setMessages(conv.messages.map(m => ({
                ...m,
                timestamp: new Date(m.timestamp),
                modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
            })));
        } else {
            // Switch to branch
            const branch = conv.branches?.find(b => b.id === branchId);
            if (branch) {
                setMessages(branch.messages.map(m => ({
                    ...m,
                    timestamp: new Date(m.timestamp),
                    modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                })));
            }
        }

        // Persist active branch
        const updatedConv: Conversation = { ...conv, activeBranchId: branchId ?? undefined };
        const updated = await saveConversation(conversationsRef.current, updatedConv);
        setConversations(updated);
    }, [activeConversationId]);

    // Delete a branch
    const deleteBranch = useCallback(async (branchId: string) => {
        if (!activeConversationId) return;
        const conv = conversationsRef.current.find(c => c.id === activeConversationId);
        if (!conv) return;

        const updatedBranches = (conv.branches || []).filter(b => b.id !== branchId);
        const updatedConv: Conversation = {
            ...conv,
            branches: updatedBranches,
            activeBranchId: activeBranchId === branchId ? undefined : conv.activeBranchId,
        };

        if (activeBranchId === branchId) {
            // Switch back to main
            setActiveBranchId(null);
            setMessages(conv.messages.map(m => ({
                ...m,
                timestamp: new Date(m.timestamp),
                modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
            })));
        }

        const updated = await saveConversation(conversationsRef.current, updatedConv);
        setConversations(updated);
    }, [activeConversationId, activeBranchId]);

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
