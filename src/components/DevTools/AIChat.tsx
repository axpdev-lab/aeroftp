import React, { useState, useRef, useEffect, useCallback } from 'react';
import { Send, Bot, User, Sparkles, Settings2, Mic, MicOff, ChevronDown, Plus, Trash2, MessageSquare, PanelLeftClose, PanelLeftOpen, Copy, Check } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { GeminiIcon, OpenAIIcon, AnthropicIcon } from './AIIcons';
import { AISettingsPanel } from '../AISettings';
import { AISettings, AIProviderType, TaskType } from '../../types/ai';
import { AgentToolCall, AGENT_TOOLS, generateToolsPrompt, toNativeDefinitions, requiresApproval, getToolByName } from '../../types/tools';
import { ToolApproval } from './ToolApproval';
import { Conversation, ConversationMessage, loadHistory, saveConversation, deleteConversation, createConversation } from '../../utils/chatHistory';
import { useTranslation } from '../../i18n';

interface Message {
    id: string;
    role: 'user' | 'assistant';
    content: string;
    timestamp: Date;
    modelInfo?: {
        modelName: string;
        providerName: string;
        providerType: AIProviderType;
    };
    tokenInfo?: {
        inputTokens?: number;
        outputTokens?: number;
        totalTokens?: number;
        cost?: number;
    };
}

interface AIChatProps {
    className?: string;
    remotePath?: string;
    localPath?: string;
    /** Theme hint - AI Chat stays dark but may use for future enhancements */
    isLightTheme?: boolean;
    /** Active protocol type (e.g. 'sftp', 'ftp', 'googledrive') */
    providerType?: string;
    /** Whether currently connected to remote */
    isConnected?: boolean;
    /** Currently selected files in the file panel */
    selectedFiles?: string[];
    /** Server hostname for connection context */
    serverHost?: string;
    /** Server port for connection context */
    serverPort?: number;
    /** Username for connection context */
    serverUser?: string;
}

// Get provider icon based on type
const getProviderIcon = (type: AIProviderType, size = 12): React.ReactNode => {
    switch (type) {
        case 'google': return <GeminiIcon size={size} />;
        case 'openai': return <OpenAIIcon size={size} />;
        case 'anthropic': return <AnthropicIcon size={size} />;
        case 'xai': return <span style={{ fontSize: size }}>𝕏</span>;
        case 'openrouter': return <span style={{ fontSize: size }}>⬡</span>;
        case 'ollama': return <span style={{ fontSize: size }}>🦙</span>;
        default: return <Bot size={size} />;
    }
};

// Rate limiter: tracks request timestamps per provider
const rateLimitMap = new Map<string, number[]>();
const RATE_LIMIT_RPM = 20; // max requests per minute per provider

function checkRateLimit(providerId: string): { allowed: boolean; waitSeconds: number } {
    const now = Date.now();
    const windowMs = 60_000;
    const timestamps = (rateLimitMap.get(providerId) || []).filter(t => now - t < windowMs);
    rateLimitMap.set(providerId, timestamps);
    if (timestamps.length >= RATE_LIMIT_RPM) {
        const oldest = timestamps[0];
        const waitMs = windowMs - (now - oldest);
        return { allowed: false, waitSeconds: Math.ceil(waitMs / 1000) };
    }
    return { allowed: true, waitSeconds: 0 };
}

function recordRequest(providerId: string) {
    const timestamps = rateLimitMap.get(providerId) || [];
    timestamps.push(Date.now());
    rateLimitMap.set(providerId, timestamps);
}

// Retry with exponential backoff
async function withRetry<T>(
    fn: () => Promise<T>,
    maxAttempts: number = 3,
    baseDelayMs: number = 1000,
): Promise<T> {
    let lastError: unknown;
    for (let attempt = 0; attempt < maxAttempts; attempt++) {
        try {
            return await fn();
        } catch (error: unknown) {
            lastError = error;
            const errStr = String(error).toLowerCase();
            // Only retry on transient errors (network, rate limit, server errors)
            const isRetryable = errStr.includes('rate limit') ||
                errStr.includes('timeout') ||
                errStr.includes('429') ||
                errStr.includes('500') ||
                errStr.includes('502') ||
                errStr.includes('503') ||
                errStr.includes('network') ||
                errStr.includes('fetch');
            if (!isRetryable || attempt === maxAttempts - 1) throw error;
            const delay = baseDelayMs * Math.pow(2, attempt);
            await new Promise(resolve => setTimeout(resolve, delay));
        }
    }
    throw lastError;
}

// Selected model state
interface SelectedModel {
    providerId: string;
    providerName: string;
    providerType: AIProviderType;
    modelId: string;
    modelName: string;
    displayName: string;
}

export const AIChat: React.FC<AIChatProps> = ({ className = '', remotePath, localPath, isLightTheme = false, providerType, isConnected, selectedFiles, serverHost, serverPort, serverUser }) => {
    const t = useTranslation();
    const [messages, setMessages] = useState<Message[]>([]);
    const [input, setInput] = useState('');
    const [showModelSelector, setShowModelSelector] = useState(false);
    const [showContextMenu, setShowContextMenu] = useState(false);
    const [showSettings, setShowSettings] = useState(false);
    const [isLoading, setIsLoading] = useState(false);
    const [isListening, setIsListening] = useState(false);
    const [copiedId, setCopiedId] = useState<string | null>(null);
    const [availableModels, setAvailableModels] = useState<SelectedModel[]>([]);
    const [selectedModel, setSelectedModel] = useState<SelectedModel | null>(null);
    const [pendingToolCall, setPendingToolCall] = useState<AgentToolCall | null>(null);
    const [conversations, setConversations] = useState<Conversation[]>([]);
    const [activeConversationId, setActiveConversationId] = useState<string | null>(null);
    const [showHistory, setShowHistory] = useState(false);
    const messagesEndRef = useRef<HTMLDivElement>(null);
    const inputRef = useRef<HTMLTextAreaElement>(null);
    const historyLoadedRef = useRef(false);

    const scrollToBottom = () => {
        messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    };

    // Friendly tool labels for display
    const toolLabels: Record<string, string> = {
        remote_list: 'Listing remote files',
        remote_read: 'Reading remote file',
        remote_write: 'Writing remote file',
        remote_rename: 'Renaming remote file',
        remote_delete: 'Deleting remote file',
        remote_mkdir: 'Creating remote folder',
        remote_upload: 'Uploading file',
        remote_download: 'Downloading file',
        remote_search: 'Searching remote files',
        remote_move: 'Moving remote file',
        local_list: 'Listing local files',
        local_read: 'Reading local file',
        local_write: 'Writing local file',
        local_search: 'Searching local files',
        local_mkdir: 'Creating local folder',
        local_delete: 'Deleting local item',
        local_rename: 'Renaming local item',
        upload_files: 'Uploading files',
        download_files: 'Downloading files',
        local_edit: 'Editing local file',
        remote_edit: 'Editing remote file',
        sync_preview: 'Comparing directories',
        archive_create: 'Creating archive',
        archive_extract: 'Extracting archive',
    };

    // Replace raw TOOL/ARGS blocks with styled chip (inline HTML)
    const formatToolCallDisplay = (text: string): string => {
        return text.replace(
            /TOOL:\s*(\w+)\s*\n\s*ARGS:\s*(\{[^}]*\})/gi,
            (_match, toolName: string, argsJson: string) => {
                const label = toolLabels[toolName] || toolName;
                let detail = '';
                try {
                    const args = JSON.parse(argsJson);
                    const path = args.path || args.remote_path || args.local_path || '';
                    if (path) detail = `<span style="opacity:0.7;margin-left:6px">${path}</span>`;
                    if (args.local_path && args.remote_path) {
                        detail = `<span style="opacity:0.7;margin-left:6px">${args.local_path} ↔ ${args.remote_path}</span>`;
                    }
                } catch { /* ignore */ }
                return `<div style="display:inline-flex;align-items:center;gap:6px;background:#374151;border-radius:6px;padding:3px 10px;margin:4px 0;font-size:12px;border-left:3px solid #8b5cf6"><svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="#a78bfa" stroke-width="2"><path d="M14.7 6.3a1 1 0 000 1.4l1.6 1.6a1 1 0 001.4 0l3.77-3.77a6 6 0 01-7.94 7.94l-6.91 6.91a2.12 2.12 0 01-3-3l6.91-6.91a6 6 0 017.94-7.94l-3.76 3.76z"/></svg><strong>${label}</strong>${detail}</div>`;
            }
        );
    };

    // Simple markdown renderer
    const renderMarkdown = (text: string): string => {
        return text
            // Code blocks (```...```)
            .replace(/```(\w+)?\n([\s\S]*?)```/g, '<pre class="bg-gray-900 rounded p-2 my-2 overflow-x-auto text-xs"><code>$2</code></pre>')
            // Inline code (`...`)
            .replace(/`([^`]+)`/g, '<code class="bg-gray-700 px-1 rounded text-purple-300">$1</code>')
            // Bold (**...**)
            .replace(/\*\*([^*]+)\*\*/g, '<strong class="font-semibold text-white">$1</strong>')
            // Italic (*...* but not **)
            .replace(/(?<!\*)\*([^*]+)\*(?!\*)/g, '<em>$1</em>')
            // Line breaks
            .replace(/\n/g, '<br/>');
    };

    useEffect(() => {
        scrollToBottom();
        // Persist after assistant replies (when messages have at least 2 entries)
        if (messages.length >= 2 && messages[messages.length - 1].role === 'assistant') {
            persistConversation(messages);
        }
    }, [messages]);

    // Load available models from settings (API keys fetched from OS Keyring)
    const loadModels = async () => {
        const settingsJson = localStorage.getItem('aeroftp_ai_settings');
        if (settingsJson) {
            try {
                const settings: AISettings = JSON.parse(settingsJson);
                const models: SelectedModel[] = [];

                // Check which providers have API keys in keyring
                const enabledProviders: string[] = [];
                for (const p of settings.providers) {
                    if (!p.isEnabled) continue;
                    // Check if API key exists (either in-memory from migration or in keyring)
                    if (p.apiKey) {
                        enabledProviders.push(p.id);
                    } else {
                        try {
                            await invoke<string>('get_credential', { account: `ai_apikey_${p.id}` });
                            enabledProviders.push(p.id);
                        } catch {
                            // No API key configured
                        }
                    }
                }

                settings.providers
                    .filter(p => enabledProviders.includes(p.id))
                    .forEach(provider => {
                        const providerModels = settings.models.filter(
                            m => m.providerId === provider.id && m.isEnabled
                        );
                        providerModels.forEach(model => {
                            models.push({
                                providerId: provider.id,
                                providerName: provider.name,
                                providerType: provider.type,
                                modelId: model.id,
                                modelName: model.name,
                                displayName: model.displayName,
                            });
                        });
                    });

                setAvailableModels(models);

                // Set default if none selected
                if (!selectedModel && models.length > 0) {
                    const defaultModel = models.find(m => {
                        const settingsModel = settings.models.find(sm => sm.id === m.modelId);
                        return settingsModel?.isDefault;
                    }) || models[0];
                    setSelectedModel(defaultModel);
                }
            } catch (e) {
                console.error('Failed to load AI settings:', e);
            }
        }
    };

    // Initial load
    useEffect(() => {
        loadModels();
    }, []);

    // Reload when settings close
    useEffect(() => {
        if (!showSettings) loadModels();
    }, [showSettings]);

    // Load chat history on mount
    useEffect(() => {
        if (historyLoadedRef.current) return;
        historyLoadedRef.current = true;
        loadHistory().then(history => {
            setConversations(history);
            // Restore last active conversation
            if (history.length > 0) {
                const last = history[0];
                setActiveConversationId(last.id);
                setMessages(last.messages.map(m => ({
                    ...m,
                    timestamp: new Date(m.timestamp),
                    modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
                })));
            }
        }).catch(() => {});
    }, []);

    // Save conversation after messages change
    const persistConversation = useCallback(async (msgs: Message[]) => {
        if (msgs.length === 0) return;
        const convId = activeConversationId || createConversation(msgs[0]?.content).id;
        if (!activeConversationId) setActiveConversationId(convId);

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

        const conv: Conversation = {
            id: convId,
            title: msgs.find(m => m.role === 'user')?.content.slice(0, 60) || 'New Chat',
            messages: convMessages,
            createdAt: conversations.find(c => c.id === convId)?.createdAt || new Date().toISOString(),
            updatedAt: new Date().toISOString(),
            totalTokens,
            totalCost,
        };

        const updated = await saveConversation(conversations, conv);
        setConversations(updated);
    }, [activeConversationId, conversations]);

    // New chat
    const startNewChat = useCallback(() => {
        setMessages([]);
        setActiveConversationId(null);
        setPendingToolCall(null);
    }, []);

    // Switch conversation
    const switchConversation = useCallback((conv: Conversation) => {
        setActiveConversationId(conv.id);
        setMessages(conv.messages.map(m => ({
            ...m,
            timestamp: new Date(m.timestamp),
            modelInfo: m.modelInfo ? { ...m.modelInfo, providerType: m.modelInfo.providerType as AIProviderType } : undefined,
        })));
        setPendingToolCall(null);
        setShowHistory(false);
    }, []);

    // Delete conversation
    const handleDeleteConversation = useCallback(async (convId: string) => {
        const updated = await deleteConversation(conversations, convId);
        setConversations(updated);
        if (convId === activeConversationId) {
            startNewChat();
        }
    }, [conversations, activeConversationId, startNewChat]);

    // Speech recognition for audio input
    const toggleListening = () => {
        if (!('webkitSpeechRecognition' in window) && !('SpeechRecognition' in window)) {
            alert('Speech recognition is not supported in this browser.');
            return;
        }

        if (isListening) {
            setIsListening(false);
            return;
        }

        const SpeechRecognition = (window as any).SpeechRecognition || (window as any).webkitSpeechRecognition;
        const recognition = new SpeechRecognition();
        recognition.continuous = false;
        recognition.interimResults = false;
        recognition.lang = 'en-US';

        recognition.onstart = () => setIsListening(true);
        recognition.onend = () => setIsListening(false);
        recognition.onerror = () => setIsListening(false);
        recognition.onresult = (event: any) => {
            const transcript = event.results[0][0].transcript;
            setInput(prev => prev + (prev ? ' ' : '') + transcript);
            inputRef.current?.focus();
        };

        recognition.start();
    };

    // Detect task type from user input for auto-routing
    const detectTaskType = (input: string): TaskType => {
        // Code generation patterns
        if (/\b(create|write|generate|build|implement|make|add)\b.*\b(function|class|component|code|file|script)\b/i.test(input) ||
            /\b(new|create)\b.*\b(file|folder|directory)\b/i.test(input)) {
            return 'code_generation';
        }

        // Code review patterns
        if (/\b(review|refactor|improve|optimize|fix|debug|check)\b.*\b(code|function|class|file)\b/i.test(input) ||
            /\bwhat('s| is)\b.*\b(wrong|issue|bug|problem)\b/i.test(input)) {
            return 'code_review';
        }

        // File analysis patterns
        if (/\b(read|show|display|analyze|explain|what)\b.*\b(file|content|code)\b/i.test(input) ||
            /\b(list|show|display)\b.*\b(files|folders|directory)\b/i.test(input)) {
            return 'file_analysis';
        }

        // Terminal command patterns
        if (/\b(run|execute|terminal|command|shell|bash|npm|git|chmod)\b/i.test(input) ||
            /\b(how to|how do i)\b.*\b(install|run|start|build)\b/i.test(input)) {
            return 'terminal_command';
        }

        // Quick answer patterns
        if (/^(what|how|why|when|where|who|is|are|can|could|would|should)\b/i.test(input) &&
            input.length < 100) {
            return 'quick_answer';
        }

        return 'general';
    };

    // Parse tool calls from AI response
    const parseToolCall = (content: string): { tool: string; args: Record<string, unknown> } | null => {
        const toolMatch = content.match(/TOOL:\s*(\w+)/i);
        // Match ARGS with nested braces and multiline JSON
        const argsMatch = content.match(/ARGS:\s*(\{[\s\S]*\})/i);

        if (toolMatch) {
            try {
                const args = argsMatch ? JSON.parse(argsMatch[1]) : {};
                return { tool: toolMatch[1], args };
            } catch {
                return { tool: toolMatch[1], args: {} };
            }
        }
        return null;
    };

    // Execute a tool via unified provider-agnostic command
    const executeToolByName = async (toolName: string, args: Record<string, unknown>): Promise<unknown> => {
        return await invoke('execute_ai_tool', { toolName, args });
    };

    // Format tool result for display
    const formatToolResult = (_toolName: string, result: unknown): string => {
        if (result && typeof result === 'object') {
            const r = result as Record<string, unknown>;
            // List results
            if (r.entries && Array.isArray(r.entries)) {
                const entries = r.entries as Array<{ name: string; is_dir: boolean; size: number }>;
                const lines = entries.map(e => `${e.is_dir ? '/' : ' '} ${e.name}${e.is_dir ? '' : ` (${e.size} bytes)`}`);
                let output = lines.join('\n');
                if (r.truncated) output += `\n_...truncated (${r.total} total)_`;
                return `\`\`\`\n${output}\n\`\`\``;
            }
            // Read results
            if (typeof r.content === 'string') {
                let output = r.content as string;
                if (r.truncated) output += `\n\n_...truncated (${r.size} bytes total)_`;
                return `\`\`\`\n${output}\n\`\`\``;
            }
            // Sync preview results
            if (r.synced !== undefined) {
                const lines: string[] = [];
                lines.push(`**Local:** ${r.local_files} files | **Remote:** ${r.remote_files} files | **Identical:** ${r.identical}`);
                if (r.synced) {
                    lines.push('\n**Folders are in sync.**');
                } else {
                    const onlyLocal = r.only_local as Array<{ name: string; size: number }>;
                    const onlyRemote = r.only_remote as Array<{ name: string; size: number }>;
                    const sizeDiff = r.size_different as Array<{ name: string; local_size: number; remote_size: number }>;
                    if (onlyLocal?.length) {
                        lines.push(`\n**Only local** (${onlyLocal.length}):`);
                        onlyLocal.forEach(f => lines.push(`  + ${f.name} (${f.size} bytes)`));
                    }
                    if (onlyRemote?.length) {
                        lines.push(`\n**Only remote** (${onlyRemote.length}):`);
                        onlyRemote.forEach(f => lines.push(`  - ${f.name} (${f.size} bytes)`));
                    }
                    if (sizeDiff?.length) {
                        lines.push(`\n**Size differs** (${sizeDiff.length}):`);
                        sizeDiff.forEach(f => lines.push(`  ~ ${f.name} (local: ${f.local_size}, remote: ${f.remote_size})`));
                    }
                }
                return lines.join('\n');
            }
            // Batch upload/download results
            if (typeof r.uploaded === 'number' || typeof r.downloaded === 'number') {
                const count = (r.uploaded ?? r.downloaded) as number;
                const action = r.uploaded !== undefined ? 'Uploaded' : 'Downloaded';
                const files = r.files as string[] | undefined;
                const errors = r.errors as Array<{ file: string; error: string }> | undefined;
                const lines: string[] = [];
                lines.push(`**${action} ${count} file(s)**`);
                if (files?.length) lines.push(files.map(f => `  + ${f}`).join('\n'));
                if (errors?.length) {
                    lines.push(`\n**Failed (${errors.length}):**`);
                    errors.forEach(e => lines.push(`  - ${e.file}: ${e.error}`));
                }
                return lines.join('\n');
            }
            // Edit results
            if (r.replaced !== undefined) {
                return r.success
                    ? `**Replaced ${r.replaced} occurrence(s)** in \`${(r as any).message?.split(' in ').pop() || 'file'}\``
                    : String(r.message || 'String not found in file');
            }
            // Success message
            if (r.message) return String(r.message);
            // Search results
            if (r.results && Array.isArray(r.results)) {
                const results = r.results as Array<{ name: string; path: string; is_dir: boolean }>;
                return results.map(e => `${e.is_dir ? '/' : ' '} ${e.path}`).join('\n') || 'No results found.';
            }
        }
        return `\`\`\`json\n${JSON.stringify(result, null, 2)}\n\`\`\``;
    };

    // Execute a tool
    const executeTool = async (toolCall: AgentToolCall) => {
        const tool = getToolByName(toolCall.toolName);
        if (!tool) {
            setPendingToolCall(null);
            return;
        }

        try {
            const result = await executeToolByName(toolCall.toolName, toolCall.args);
            const formattedResult = formatToolResult(toolCall.toolName, result);
            const resultMessage: Message = {
                id: Date.now().toString(),
                role: 'assistant',
                content: formattedResult,
                timestamp: new Date(),
            };
            setMessages(prev => [...prev, resultMessage]);
        } catch (error: any) {
            const errorMessage: Message = {
                id: Date.now().toString(),
                role: 'assistant',
                content: `❌ Tool failed: ${error.message || error.toString()}`,
                timestamp: new Date(),
            };
            setMessages(prev => [...prev, errorMessage]);
        }
        setPendingToolCall(null);
    };

    const handleSend = async () => {
        if (!input.trim() || isLoading) return;

        const userMessage: Message = {
            id: Date.now().toString(),
            role: 'user',
            content: input,
            timestamp: new Date(),
        };

        setMessages(prev => [...prev, userMessage]);
        setInput('');
        setIsLoading(true);

        let streamingMsgId: string | null = null;
        try {
            if (!selectedModel) {
                throw new Error('No model selected. Click ⚙️ to configure a provider.');
            }

            // Load settings to get provider config
            const settingsJson = localStorage.getItem('aeroftp_ai_settings');
            if (!settingsJson) {
                throw new Error('No AI providers configured. Click ⚙️ to add one.');
            }

            const settings: AISettings = JSON.parse(settingsJson);

            // Auto-routing: detect task type and potentially override model
            let activeModel = selectedModel;
            if (settings.autoRouting.enabled) {
                const taskType = detectTaskType(input);
                const rule = settings.autoRouting.rules.find(r => r.taskType === taskType);
                if (rule) {
                    const routedModel = settings.models.find(m => m.id === rule.preferredModelId);
                    if (routedModel) {
                        const routedProvider = settings.providers.find(p => p.id === routedModel.providerId);
                        if (routedProvider) {
                            activeModel = {
                                providerId: routedProvider.id,
                                providerName: routedProvider.name,
                                providerType: routedProvider.type,
                                modelId: routedModel.id,
                                modelName: routedModel.name,
                                displayName: routedModel.displayName,
                            };
                        }
                    }
                }
            }

            const provider = settings.providers.find(p => p.id === activeModel.providerId);
            if (!provider) {
                throw new Error(`Provider not configured for ${activeModel.providerName}`);
            }

            // Fetch API key from OS Keyring
            let apiKey: string;
            try {
                apiKey = await invoke<string>('get_credential', { account: `ai_apikey_${provider.id}` });
            } catch {
                throw new Error(`API key not configured for ${activeModel.providerName}. Open AI Settings to add one.`);
            }

            // Build context block
            const contextLines: string[] = [];
            if (providerType) contextLines.push(`- Protocol: ${providerType.toUpperCase()} (${isConnected ? 'connected' : 'disconnected'})`);
            if (serverHost) contextLines.push(`- Server: ${serverHost}${serverPort ? ':' + serverPort : ''}`);
            if (serverUser) contextLines.push(`- User: ${serverUser}`);
            if (remotePath) contextLines.push(`- Remote path: ${remotePath}`);
            if (localPath) contextLines.push(`- Local path: ${localPath}`);
            if (selectedFiles && selectedFiles.length > 0) contextLines.push(`- Selected files: ${selectedFiles.slice(0, 10).join(', ')}${selectedFiles.length > 10 ? ` (+${selectedFiles.length - 10} more)` : ''}`);
            const contextBlock = contextLines.length > 0
                ? `\n\nCURRENT CONTEXT:\n${contextLines.join('\n')}`
                : '';

            // Add system prompt with tools + context
            const systemPrompt = `You are AeroAgent, the built-in AI assistant for AeroFTP — a multi-protocol file manager supporting FTP, FTPS, SFTP, S3, WebDAV, Google Drive, Dropbox, OneDrive, MEGA, Box, pCloud, Azure Blob, and Filen.

## Identity & Tone
- Professional yet approachable. Be concise and action-oriented.
- Respond in the same language the user writes in.
- Use short paragraphs. Use bold for key terms. Use bullet lists for multiple items.
- Use minimal emoji (one per section header max) for readability, never excessive.

## Capabilities
You can browse, search, upload, download, rename, delete, move, and sync files across all connected providers. You can also create and extract archives (ZIP, 7z, TAR).

When you need to use a tool, respond with:
TOOL: tool_name
ARGS: {"param": "value"}

Available tools:
${generateToolsPrompt()}

## Protocol & Provider Expertise
You are an expert on every protocol and cloud provider AeroFTP supports. When users ask how to configure or troubleshoot a connection, provide accurate, step-by-step guidance.

### FTP / FTPS
- **Port**: 21 (FTP), 21 or 990 (FTPS explicit/implicit)
- **TLS**: AeroFTP defaults to FTPS (explicit TLS). Implicit TLS uses port 990.
- **Passive mode**: enabled by default; required behind NAT/firewalls.
- **Features**: MLSD/MLST for accurate listings, FEAT negotiation, UTF-8.

### SFTP
- **Port**: 22 (SSH)
- **Auth**: password or SSH key (OpenSSH format). Key passphrase supported.
- **Host key verification**: first-connect trust with fingerprint stored locally.
- **Differs from FTPS**: SFTP runs over SSH, not FTP+TLS. Different protocol entirely.

### S3 (Amazon S3 & compatible)
- **Required fields**: Endpoint URL, Access Key ID, Secret Access Key, Bucket name, Region.
- **Compatible services**: MinIO, Backblaze B2, Wasabi, DigitalOcean Spaces, Cloudflare R2.
- **Endpoint examples**: \`https://s3.amazonaws.com\`, \`https://s3.eu-west-1.amazonaws.com\`, \`https://play.min.io\`.
- **Path style**: enable for MinIO/self-hosted. Virtual-hosted style for AWS default.

### WebDAV
- **URL format**: full URL including path, e.g. \`https://cloud.example.com/remote.php/dav/files/username/\`
- **Nextcloud/ownCloud**: use the DAV endpoint above with your username/password or app password.
- **Auth**: Basic or Digest. HTTPS strongly recommended.

### Google Drive
- **Auth**: OAuth 2.0 with PKCE. Click "Connect", authorize in browser, token stored securely.
- **Scopes**: full drive access for file management.
- **Shared drives**: accessible after authorization.

### Dropbox
- **Auth**: OAuth 2.0 with PKCE. Authorize via browser.
- **Scopes**: files.content.read, files.content.write, sharing.write.
- **Limits**: 150MB per single upload, chunked for larger files.

### OneDrive
- **Auth**: OAuth 2.0 via Microsoft identity platform.
- **Endpoint**: Microsoft Graph API.
- **Personal vs Business**: both supported.

### MEGA
- **Auth**: email + password. No OAuth.
- **Encryption**: client-side AES encryption (MEGA's own protocol).
- **2FA**: not yet supported in AeroFTP.

### Box
- **Auth**: OAuth 2.0 with PKCE.
- **Upload limit**: 150MB single upload (chunked upload for larger files planned).

### pCloud
- **Auth**: OAuth 2.0 with PKCE.
- **Regions**: US (api.pcloud.com) or EU (eapi.pcloud.com). Choose based on account region.

### Azure Blob Storage
- **Required**: Account Name, Access Key, Container name.
- **Endpoint**: \`https://<account>.blob.core.windows.net\`
- **Block size**: 256MB max per block.

### Filen
- **Auth**: email + password (encrypted).
- **Encryption**: zero-knowledge, client-side AES-256.
- **2FA**: not yet supported in AeroFTP.

### Archives & Encryption
- **ZIP**: AES-256 encryption, compression levels 0-9.
- **7z**: LZMA2 compression, AES-256 encryption.
- **TAR**: no encryption, combined with GZ/XZ/BZ2 for compression.
- **AeroVault**: AES-256 encrypted containers (.aerovault files). Create, add, extract, change password.
- **Cryptomator**: format 8 support. Unlock, browse, decrypt, encrypt files.

## Behavior Rules
1. **Explain before acting**: briefly state what you will do, then execute the tool.
2. **Summarize after**: report the result clearly (file count, sizes, errors).
3. **Never delete or overwrite without confirmation**: if a tool would destroy data, ask the user first.
4. **Suggest next steps**: after completing a task, suggest related actions when useful.
5. **Handle errors gracefully**: if a tool fails, explain why and suggest alternatives.
6. **Stay in scope**: you are a file management and protocol configuration assistant. Politely decline unrelated requests.
7. **Be honest about limits**: if you cannot do something, say so clearly.
8. **Configuration help**: when a user asks how to set up a provider, give the exact fields needed, common pitfalls, and example values.

## Response Format
- For file listings: use a compact table or numbered list with name, size, date.
- For comparisons (sync_preview): highlight differences clearly with +/−/~ markers.
- For errors: quote the error message and explain in plain language.
- For configuration help: list required fields, then optional fields, with examples.
- Keep responses under 500 words unless the user asks for detail.${contextBlock}`;

            // Build message history
            const messageHistory = [
                { role: 'system', content: systemPrompt },
                ...messages.slice(-10).map(m => ({
                    role: m.role === 'user' ? 'user' : 'assistant',
                    content: m.content,
                })),
                { role: 'user', content: input }
            ];

            // Rate limit check
            const rateCheck = checkRateLimit(provider.id);
            if (!rateCheck.allowed) {
                throw new Error(`Rate limit reached for ${activeModel.providerName}. Try again in ${rateCheck.waitSeconds}s.`);
            }
            recordRequest(provider.id);

            // Check model capabilities
            const modelDef = settings.models?.find((m: { id: string }) => m.id === activeModel.modelId);
            const useNativeTools = modelDef?.supportsTools === true;
            const useStreaming = modelDef?.supportsStreaming === true;

            // Prepare model info for message signature
            const modelInfo = {
                modelName: activeModel.displayName,
                providerName: activeModel.providerName,
                providerType: activeModel.providerType,
            };

            const aiRequest = {
                provider_type: activeModel.providerType,
                model: activeModel.modelName,
                api_key: apiKey,
                base_url: provider.baseUrl,
                messages: messageHistory,
                max_tokens: settings.advancedSettings?.maxTokens || 4096,
                temperature: settings.advancedSettings?.temperature || 0.7,
                ...(useNativeTools ? { tools: toNativeDefinitions(AGENT_TOOLS) } : {}),
            };

            if (useStreaming) {
                // Streaming mode: incremental rendering
                const streamId = `stream_${Date.now()}`;
                const msgId = (Date.now() + 1).toString();
                streamingMsgId = msgId;
                let streamContent = '';
                type ToolCallEntry = { id: string; name: string; arguments: unknown };
                const streamResult: {
                    toolCalls: ToolCallEntry[] | null;
                    inputTokens: number | undefined;
                    outputTokens: number | undefined;
                } = { toolCalls: null, inputTokens: undefined, outputTokens: undefined };

                // Add placeholder message
                const streamMsg: Message = {
                    id: msgId,
                    role: 'assistant',
                    content: '',
                    timestamp: new Date(),
                    modelInfo,
                };
                setMessages(prev => [...prev, streamMsg]);

                // Listen for stream chunks
                const unlisten: UnlistenFn = await listen<{
                    content: string;
                    done: boolean;
                    tool_calls?: Array<{ id: string; name: string; arguments: unknown }>;
                    input_tokens?: number;
                    output_tokens?: number;
                }>(`ai-stream-${streamId}`, (event) => {
                    const chunk = event.payload;
                    if (chunk.content) {
                        streamContent += chunk.content;
                        setMessages(prev => prev.map(m =>
                            m.id === msgId ? { ...m, content: streamContent } : m
                        ));
                    }
                    if (chunk.done) {
                        if (chunk.tool_calls) streamResult.toolCalls = chunk.tool_calls;
                        if (chunk.input_tokens) streamResult.inputTokens = chunk.input_tokens;
                        if (chunk.output_tokens) streamResult.outputTokens = chunk.output_tokens;
                    }
                });

                // Start streaming
                await invoke('ai_chat_stream', { request: aiRequest, streamId });
                unlisten();

                // Calculate cost
                const tokenInfo: Message['tokenInfo'] = streamResult.inputTokens || streamResult.outputTokens ? {
                    inputTokens: streamResult.inputTokens,
                    outputTokens: streamResult.outputTokens,
                    totalTokens: (streamResult.inputTokens || 0) + (streamResult.outputTokens || 0),
                    cost: modelDef?.inputCostPer1k && modelDef?.outputCostPer1k
                        ? ((streamResult.inputTokens || 0) / 1000) * modelDef.inputCostPer1k +
                          ((streamResult.outputTokens || 0) / 1000) * modelDef.outputCostPer1k
                        : undefined,
                } : undefined;

                // Check for tool calls from streaming
                let toolParsed: { tool: string; args: Record<string, unknown> } | null = null;
                if (streamResult.toolCalls && streamResult.toolCalls.length > 0) {
                    const tc = streamResult.toolCalls[0];
                    const args = typeof tc.arguments === 'string' ? JSON.parse(tc.arguments as string) : tc.arguments;
                    toolParsed = { tool: tc.name, args: args as Record<string, unknown> };
                } else {
                    toolParsed = parseToolCall(streamContent);
                }

                if (toolParsed) {
                    const tool = getToolByName(toolParsed.tool);
                    if (tool) {
                        const toolCall: AgentToolCall = {
                            id: Date.now().toString(),
                            toolName: toolParsed.tool,
                            args: toolParsed.args,
                            status: requiresApproval(toolParsed.tool) ? 'pending' : 'approved',
                        };
                        // Update the streamed message or add approval
                        if (toolCall.status === 'pending') {
                            setMessages(prev => prev.map(m =>
                                m.id === msgId ? { ...m, content: streamContent || `I want to execute **${toolLabels[toolParsed!.tool] || toolParsed!.tool}**${toolParsed!.args.path ? ` → \`${toolParsed!.args.path}\`` : ''}. Approve or cancel:` } : m
                            ));
                            setPendingToolCall(toolCall);
                        } else {
                            await executeTool(toolCall);
                        }
                    }
                } else {
                    // Update final message with token info
                    setMessages(prev => prev.map(m =>
                        m.id === msgId ? { ...m, content: streamContent, tokenInfo } : m
                    ));
                }
            } else {
                // Non-streaming mode: single response
                const response = await withRetry(() =>
                    invoke<{
                        content: string;
                        model: string;
                        tokens_used?: number;
                        input_tokens?: number;
                        output_tokens?: number;
                        finish_reason?: string;
                        tool_calls?: Array<{ id: string; name: string; arguments: unknown }>;
                    }>('ai_chat', { request: aiRequest })
                );

                const tokenInfo: Message['tokenInfo'] = response.input_tokens || response.output_tokens ? {
                    inputTokens: response.input_tokens,
                    outputTokens: response.output_tokens,
                    totalTokens: response.tokens_used,
                    cost: modelDef?.inputCostPer1k && modelDef?.outputCostPer1k
                        ? ((response.input_tokens || 0) / 1000) * modelDef.inputCostPer1k +
                          ((response.output_tokens || 0) / 1000) * modelDef.outputCostPer1k
                        : undefined,
                } : undefined;

                // Check if AI wants to use a tool
                let toolParsed: { tool: string; args: Record<string, unknown> } | null = null;
                if (response.tool_calls && response.tool_calls.length > 0) {
                    const tc = response.tool_calls[0];
                    const args = typeof tc.arguments === 'string' ? JSON.parse(tc.arguments) : tc.arguments;
                    toolParsed = { tool: tc.name, args: args as Record<string, unknown> };
                } else {
                    toolParsed = parseToolCall(response.content);
                }

                if (toolParsed) {
                    const tool = getToolByName(toolParsed.tool);
                    if (tool) {
                        const toolCall: AgentToolCall = {
                            id: Date.now().toString(),
                            toolName: toolParsed.tool,
                            args: toolParsed.args,
                            status: requiresApproval(toolParsed.tool) ? 'pending' : 'approved',
                        };
                        if (toolCall.status === 'pending') {
                            const pendingMessage: Message = {
                                id: (Date.now() + 1).toString(),
                                role: 'assistant',
                                content: response.content || `I want to execute **${toolLabels[toolParsed.tool] || toolParsed.tool}**${toolParsed.args.path ? ` → \`${toolParsed.args.path}\`` : ''}. Approve or cancel:`,
                                timestamp: new Date(),
                                modelInfo,
                            };
                            setMessages(prev => [...prev, pendingMessage]);
                            setPendingToolCall(toolCall);
                        } else {
                            await executeTool(toolCall);
                        }
                    }
                } else {
                    const assistantMessage: Message = {
                        id: (Date.now() + 1).toString(),
                        role: 'assistant',
                        content: response.content,
                        timestamp: new Date(),
                        modelInfo,
                        tokenInfo,
                    };
                    setMessages(prev => [...prev, assistantMessage]);
                }
            }

        } catch (error: unknown) {
            const rawErr = String(error);
            let errStr = rawErr;
            let httpCode = 0;
            // Try to extract human-readable message from JSON error bodies
            // e.g. HTTP 429 — {"error":{"code":429,"message":"You exceeded..."}}
            const httpCodeMatch = rawErr.match(/HTTP (\d{3})/);
            if (httpCodeMatch) httpCode = parseInt(httpCodeMatch[1], 10);
            const jsonMatch = rawErr.match(/\{[\s\S]*\}/);
            if (jsonMatch) {
                try {
                    const parsed = JSON.parse(jsonMatch[0]);
                    if (parsed?.error?.code) httpCode = parsed.error.code;
                    const msg = parsed?.error?.message || parsed?.message || parsed?.error?.status || null;
                    if (msg) errStr = `HTTP error — ${msg}`;
                } catch { /* keep original */ }
            }
            // Select hint based on HTTP status code first, then text patterns
            let hint = 'Make sure you have configured an AI provider in settings.';
            if (httpCode === 401 || httpCode === 403) {
                hint = 'Authentication failed. Check your API key in AI Settings.';
            } else if (httpCode === 429) {
                hint = 'Rate limited by the provider. Wait a moment and try again.';
            } else if (httpCode === 404) {
                hint = 'Model not found. Check your model name in AI Settings.';
            } else {
                const errLower = rawErr.toLowerCase();
                if (errLower.includes('unauthorized') || errLower.includes('auth')) {
                    hint = 'Authentication failed. Check your API key in AI Settings.';
                } else if (errLower.includes('quota') || errLower.includes('rate limit')) {
                    hint = 'Rate limited by the provider. Wait a moment and try again.';
                } else if (errLower.includes('network') || errLower.includes('fetch') || errLower.includes('timeout')) {
                    hint = 'Network error. Check your internet connection and provider URL.';
                }
            }
            const errorContent = `**Error**: ${errStr}\n\n${hint}`;
            if (streamingMsgId) {
                // Update the existing placeholder message instead of adding a duplicate
                setMessages(prev => prev.map(m =>
                    m.id === streamingMsgId ? { ...m, content: errorContent } : m
                ));
            } else {
                const errorMessage: Message = {
                    id: (Date.now() + 1).toString(),
                    role: 'assistant',
                    content: errorContent,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, errorMessage]);
            }
        } finally {
            setIsLoading(false);
        }
    };

    return (
        <div className={`flex flex-col h-full bg-gray-900 ${className}`}>
            {/* Minimal Header */}
            <div className="flex items-center justify-between px-4 py-2 bg-gray-800/50 border-b border-gray-700/50">
                <div className="flex items-center gap-2 text-sm text-gray-300">
                    <button
                        onClick={() => setShowHistory(!showHistory)}
                        className="p-1 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
                        title={showHistory ? t('ai.hideHistory') : t('ai.chatHistory')}
                    >
                        {showHistory ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
                    </button>
                    <Sparkles size={14} className="text-purple-400" />
                    <span className="font-medium">{t('ai.aeroAgent')}</span>
                </div>
                <div className="flex items-center gap-1">
                    <button
                        onClick={startNewChat}
                        className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
                        title={t('ai.newChat')}
                    >
                        <Plus size={14} />
                    </button>
                    <button
                        onClick={() => setShowSettings(true)}
                        className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
                        title={t('ai.aiSettings')}
                    >
                        <Settings2 size={14} />
                    </button>
                </div>
            </div>

            <div className="flex flex-1 overflow-hidden">
            {/* History Sidebar */}
            {showHistory && (
                <div className="w-48 flex-shrink-0 border-r border-gray-700/50 bg-gray-800/30 flex flex-col overflow-hidden">
                    <div className="p-2 text-[10px] text-gray-500 uppercase tracking-wider font-medium">
                        History ({conversations.length})
                    </div>
                    <div className="flex-1 overflow-y-auto">
                        {conversations.map(conv => (
                            <div
                                key={conv.id}
                                className={`group flex items-center gap-1.5 px-2 py-1.5 cursor-pointer text-xs transition-colors ${
                                    conv.id === activeConversationId
                                        ? 'bg-purple-600/20 text-purple-300'
                                        : 'text-gray-400 hover:bg-gray-700/50 hover:text-gray-200'
                                }`}
                                onClick={() => switchConversation(conv)}
                            >
                                <MessageSquare size={10} className="flex-shrink-0" />
                                <span className="truncate flex-1">{conv.title}</span>
                                <button
                                    onClick={(e) => { e.stopPropagation(); handleDeleteConversation(conv.id); }}
                                    className="opacity-0 group-hover:opacity-100 p-0.5 text-gray-500 hover:text-red-400 transition-all"
                                >
                                    <Trash2 size={10} />
                                </button>
                            </div>
                        ))}
                        {conversations.length === 0 && (
                            <div className="px-2 py-4 text-center text-[10px] text-gray-600">No conversations yet</div>
                        )}
                    </div>
                </div>
            )}

            {/* Messages Area */}
            <div className="flex-1 overflow-y-auto">
                {messages.length === 0 ? (
                    /* Empty State - System Welcome */
                    <div className="h-full flex flex-col items-center justify-center text-center px-6 py-8">
                        <div className="w-12 h-12 rounded-full bg-purple-600/20 flex items-center justify-center mb-4">
                            <Sparkles size={24} className="text-purple-400" />
                        </div>
                        <h3 className="text-lg font-medium text-gray-200 mb-2">{t('ai.aeroAgent')}</h3>
                        <p className="text-sm text-gray-400 max-w-sm mb-6">
                            {t('ai.welcome')}
                        </p>
                        <div className="grid grid-cols-2 gap-2 text-xs max-w-md">
                            {[
                                { icon: '📂', label: t('ai.listFiles') },
                                { icon: '📄', label: t('ai.readFiles') },
                                { icon: '✏️', label: t('ai.createEditFiles') },
                                { icon: '🔄', label: t('ai.uploadDownload') },
                                { icon: '🔍', label: t('ai.compareDirectories') },
                                { icon: '🔐', label: t('ai.modifyPermissions') },
                            ].map((item, i) => (
                                <div key={i} className="flex items-center gap-2 px-3 py-2 bg-gray-800/50 rounded-lg text-gray-400">
                                    <span>{item.icon}</span>
                                    <span>{item.label}</span>
                                </div>
                            ))}
                        </div>
                    </div>
                ) : (
                    /* Messages List */
                    <div className="p-4 space-y-4">
                        {messages.map((message) => (
                            <div
                                key={message.id}
                                className={`flex gap-3 ${message.role === 'user' ? 'justify-end' : 'justify-start'}`}
                            >
                                {message.role === 'assistant' && (
                                    <div className="w-7 h-7 rounded-full bg-purple-600/20 flex items-center justify-center shrink-0">
                                        <Sparkles size={14} className="text-purple-400" />
                                    </div>
                                )}
                                <div
                                    className={`max-w-[80%] rounded-lg px-4 py-2 text-sm select-text ${message.role === 'user'
                                        ? 'bg-blue-600 text-white'
                                        : 'bg-gray-800 text-gray-200'
                                        }`}
                                >
                                    <div
                                        className="select-text prose prose-invert prose-sm max-w-none"
                                        dangerouslySetInnerHTML={{ __html: renderMarkdown(formatToolCallDisplay(message.content)) }}
                                    />
                                    <div className={`text-[10px] mt-1 flex items-center gap-2 flex-wrap ${message.role === 'user' ? 'text-blue-200' : 'text-gray-500'}`}>
                                        <span>{message.timestamp.toLocaleTimeString()}</span>
                                        {message.role === 'assistant' && (
                                            <button
                                                onClick={() => {
                                                    navigator.clipboard.writeText(message.content.replace(/<[^>]*>/g, ''));
                                                    setCopiedId(message.id);
                                                    setTimeout(() => setCopiedId(null), 1500);
                                                }}
                                                className="text-gray-500 hover:text-gray-300 transition-colors"
                                                title={t('ai.copy') || 'Copy'}
                                            >
                                                {copiedId === message.id ? <Check size={10} className="text-green-400" /> : <Copy size={10} />}
                                            </button>
                                        )}
                                        {message.role === 'assistant' && message.modelInfo && (
                                            <span className="flex items-center gap-1 text-gray-400">
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
                                            </span>
                                        )}
                                    </div>
                                </div>
                                {message.role === 'user' && (
                                    <div className="w-7 h-7 rounded-full bg-blue-600/20 flex items-center justify-center shrink-0">
                                        <User size={14} className="text-blue-400" />
                                    </div>
                                )}
                            </div>
                        ))}
                        {isLoading && (
                            <div className="flex gap-3">
                                <div className="w-7 h-7 rounded-full bg-purple-600/20 flex items-center justify-center shrink-0">
                                    <Sparkles size={14} className="text-purple-400 animate-pulse" />
                                </div>
                                <div className="bg-gray-800 rounded-lg px-4 py-2 text-gray-400 text-sm">
                                    {t('ai.thinking')}
                                </div>
                            </div>
                        )}
                        {pendingToolCall && pendingToolCall.status === 'pending' && (
                            <ToolApproval
                                toolCall={pendingToolCall}
                                onApprove={async () => {
                                    await executeTool(pendingToolCall);
                                }}
                                onReject={() => {
                                    setPendingToolCall(null);
                                    const rejectedMsg: Message = {
                                        id: Date.now().toString(),
                                        role: 'assistant',
                                        content: '❌ Operation cancelled.',
                                        timestamp: new Date(),
                                    };
                                    setMessages(prev => [...prev, rejectedMsg]);
                                }}
                            />
                        )}
                        <div ref={messagesEndRef} />
                    </div>
                )}
            </div>

            </div>{/* end flex row (sidebar + messages) */}

            {/* Input Area - Antigravity Style - All inside one box */}
            <div className="p-3">
                <div className="bg-gray-800 border border-gray-600 rounded-lg focus-within:border-purple-500 transition-colors">
                    {/* Input Row */}
                    <div className="flex gap-2 items-start px-3 py-2">
                        <textarea
                            ref={inputRef}
                            value={input}
                            onChange={(e) => {
                                setInput(e.target.value);
                                // Auto-resize
                                e.target.style.height = 'auto';
                                e.target.style.height = Math.min(e.target.scrollHeight, 120) + 'px';
                            }}
                            onKeyDown={(e) => {
                                if (e.key === 'Enter' && !e.shiftKey) {
                                    e.preventDefault();
                                    handleSend();
                                }
                            }}
                            placeholder={t('ai.askPlaceholder')}
                            className="flex-1 bg-transparent text-sm text-white placeholder-gray-500 focus:outline-none resize-none min-h-[24px] max-h-[120px]"
                            rows={1}
                        />
                        <button
                            onClick={toggleListening}
                            className={`p-1.5 rounded transition-colors ${isListening
                                ? 'text-red-400 bg-red-500/20'
                                : 'text-gray-400 hover:text-white hover:bg-gray-700'}`}
                            title={isListening ? t('ai.stopListening') : t('ai.voiceInput')}
                        >
                            {isListening ? <MicOff size={16} /> : <Mic size={16} />}
                        </button>
                        <button
                            onClick={handleSend}
                            disabled={!input.trim() || isLoading}
                            className="p-1.5 text-purple-400 hover:text-purple-300 disabled:text-gray-600 disabled:cursor-not-allowed transition-colors"
                        >
                            <Send size={16} />
                        </button>
                    </div>

                    {/* Bottom Row - Model Selector + Disclaimer (inside the box) */}
                    <div className="flex items-center justify-between px-3 py-2 border-t border-gray-700/50 text-xs">
                        <div className="flex items-center gap-3">
                            {/* Context Menu for adding paths */}
                            <div className="relative">
                                <button
                                    onClick={() => setShowContextMenu(!showContextMenu)}
                                    className="text-gray-500 hover:text-white transition-colors"
                                    title={t('ai.addContext')}
                                >+</button>

                                {showContextMenu && (
                                    <div className="absolute left-0 bottom-full mb-1 bg-gray-800 border border-gray-600 rounded-lg shadow-xl z-20 py-1 min-w-[200px]">
                                        <div className="px-3 py-1.5 text-[10px] text-gray-500 border-b border-gray-700">{t('ai.insertPath')}</div>

                                        {remotePath && (
                                            <button
                                                onClick={() => {
                                                    setInput(prev => prev + (prev ? ' ' : '') + `@remote:${remotePath}`);
                                                    setShowContextMenu(false);
                                                    inputRef.current?.focus();
                                                }}
                                                className="w-full px-3 py-2 text-left hover:bg-gray-700 flex items-center gap-2"
                                            >
                                                <span className="text-green-400">🌐</span>
                                                <div className="flex flex-col">
                                                    <span className="text-gray-200">{t('ai.remotePath')}</span>
                                                    <span className="text-gray-500 text-[10px] truncate max-w-[160px]">{remotePath}</span>
                                                </div>
                                            </button>
                                        )}

                                        {localPath && (
                                            <button
                                                onClick={() => {
                                                    setInput(prev => prev + (prev ? ' ' : '') + `@local:${localPath}`);
                                                    setShowContextMenu(false);
                                                    inputRef.current?.focus();
                                                }}
                                                className="w-full px-3 py-2 text-left hover:bg-gray-700 flex items-center gap-2"
                                            >
                                                <span className="text-blue-400">📁</span>
                                                <div className="flex flex-col">
                                                    <span className="text-gray-200">{t('ai.localPath')}</span>
                                                    <span className="text-gray-500 text-[10px] truncate max-w-[160px]">{localPath}</span>
                                                </div>
                                            </button>
                                        )}

                                        {(!remotePath && !localPath) && (
                                            <div className="px-3 py-2 text-gray-500">{t('ai.noPathsAvailable')}</div>
                                        )}

                                        <div className="border-t border-gray-700 mt-1 pt-1">
                                            <button
                                                onClick={() => {
                                                    const text = `Remote: ${remotePath || 'N/A'}\nLocal: ${localPath || 'N/A'}`;
                                                    navigator.clipboard.writeText(text);
                                                    setShowContextMenu(false);
                                                }}
                                                className="w-full px-3 py-2 text-left hover:bg-gray-700 flex items-center gap-2 text-gray-400"
                                            >
                                                <span>📋</span>
                                                <span>{t('ai.copyBothPaths')}</span>
                                            </button>
                                        </div>
                                    </div>
                                )}
                            </div>

                            <div className="relative">
                                <button
                                    onClick={() => { loadModels(); setShowModelSelector(!showModelSelector); }}
                                    className="flex items-center gap-1.5 text-gray-400 hover:text-white transition-colors"
                                >
                                    {selectedModel ? (
                                        <>
                                            {getProviderIcon(selectedModel.providerType, 12)}
                                            <span>{selectedModel.displayName}</span>
                                        </>
                                    ) : (() => {
                                        const settingsJson = localStorage.getItem('aeroftp_ai_settings');
                                        const settings = settingsJson ? JSON.parse(settingsJson) : null;
                                        if (settings?.autoRouting?.enabled) {
                                            return <><span>🤖</span><span className="text-purple-300">{t('ai.auto')}</span></>;
                                        }
                                        return <span>{t('ai.selectModel')}</span>;
                                    })()}
                                    <ChevronDown size={12} />
                                </button>

                                {showModelSelector && (
                                    <div className="absolute left-0 bottom-full mb-1 bg-gray-800 border border-gray-600 rounded-lg shadow-xl z-10 py-1 min-w-[260px] max-h-[300px] overflow-y-auto">
                                        {/* Auto option when auto-routing is enabled */}
                                        {(() => {
                                            const settingsJson = localStorage.getItem('aeroftp_ai_settings');
                                            const settings = settingsJson ? JSON.parse(settingsJson) : null;
                                            if (settings?.autoRouting?.enabled) {
                                                return (
                                                    <button
                                                        onClick={() => { setSelectedModel(null); setShowModelSelector(false); }}
                                                        className={`w-full px-3 py-2 text-left text-xs hover:bg-gray-700 flex items-center gap-2.5 border-b border-gray-700 ${!selectedModel ? 'bg-purple-600/20' : ''}`}
                                                    >
                                                        <span className="w-4">🤖</span>
                                                        <div className="flex flex-col flex-1">
                                                            <span className="font-medium text-purple-300">{t('ai.autoSmartRouting')}</span>
                                                            <span className="text-gray-500 text-[10px]">{t('ai.automaticModelSelection')}</span>
                                                        </div>
                                                        {!selectedModel && <span className="text-purple-400">✓</span>}
                                                    </button>
                                                );
                                            }
                                            return null;
                                        })()}

                                        {availableModels.length === 0 ? (
                                            <div className="px-3 py-2 text-xs text-gray-400">{t('ai.noModelsConfigured')}</div>
                                        ) : (
                                            availableModels.map(model => (
                                                <button
                                                    key={model.modelId}
                                                    onClick={() => { setSelectedModel(model); setShowModelSelector(false); }}
                                                    className={`w-full px-3 py-2 text-left text-xs hover:bg-gray-700 flex items-center gap-2.5 ${selectedModel?.modelId === model.modelId ? 'bg-gray-700/50' : ''}`}
                                                >
                                                    <span className="w-4">{getProviderIcon(model.providerType, 14)}</span>
                                                    <div className="flex flex-col flex-1">
                                                        <span className="font-medium text-gray-200">{model.displayName}</span>
                                                        <span className="text-gray-500 text-[10px]">{model.providerName}</span>
                                                    </div>
                                                    {selectedModel?.modelId === model.modelId && <span className="text-green-400">✓</span>}
                                                </button>
                                            ))
                                        )}
                                    </div>
                                )}
                            </div>
                        </div>

                        {/* AI Disclaimer */}
                        <span className="text-[10px] text-gray-500">{t('ai.disclaimer')}</span>
                    </div>
                </div>
            </div>

            {/* AI Settings Panel */}
            <AISettingsPanel isOpen={showSettings} onClose={() => setShowSettings(false)} />
        </div>
    );
};

export default AIChat;
