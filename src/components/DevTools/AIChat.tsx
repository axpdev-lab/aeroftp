import React, { useState, useRef, useEffect } from 'react';
import { Send, Bot, User, Sparkles, Settings2, Mic, MicOff, ChevronDown } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { GeminiIcon, OpenAIIcon, AnthropicIcon } from './AIIcons';
import { AISettingsPanel } from '../AISettings';
import { AISettings, AIProviderType, TaskType } from '../../types/ai';
import { AgentToolCall, generateToolsPrompt, requiresApproval, getToolByName } from '../../types/tools';
import { ToolApproval } from './ToolApproval';

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
}

interface AIChatProps {
    className?: string;
    remotePath?: string;
    localPath?: string;
    /** Theme hint - AI Chat stays dark but may use for future enhancements */
    isLightTheme?: boolean;
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

// Selected model state
interface SelectedModel {
    providerId: string;
    providerName: string;
    providerType: AIProviderType;
    modelId: string;
    modelName: string;
    displayName: string;
}

export const AIChat: React.FC<AIChatProps> = ({ className = '', remotePath, localPath, isLightTheme = false }) => {
    const [messages, setMessages] = useState<Message[]>([]);
    const [input, setInput] = useState('');
    const [showModelSelector, setShowModelSelector] = useState(false);
    const [showContextMenu, setShowContextMenu] = useState(false);
    const [showSettings, setShowSettings] = useState(false);
    const [isLoading, setIsLoading] = useState(false);
    const [isListening, setIsListening] = useState(false);
    const [availableModels, setAvailableModels] = useState<SelectedModel[]>([]);
    const [selectedModel, setSelectedModel] = useState<SelectedModel | null>(null);
    const [pendingToolCall, setPendingToolCall] = useState<AgentToolCall | null>(null);
    const messagesEndRef = useRef<HTMLDivElement>(null);
    const inputRef = useRef<HTMLTextAreaElement>(null);

    const scrollToBottom = () => {
        messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
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
    }, [messages]);

    // Load available models from settings
    const loadModels = () => {
        const settingsJson = localStorage.getItem('aeroftp_ai_settings');
        if (settingsJson) {
            try {
                const settings: AISettings = JSON.parse(settingsJson);
                const models: SelectedModel[] = [];

                settings.providers
                    .filter(p => p.isEnabled && p.apiKey)
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
        const argsMatch = content.match(/ARGS:\s*({[^}]+})/i);

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

    // Execute a tool by calling the appropriate Tauri command
    const executeToolByName = async (toolName: string, args: Record<string, unknown>): Promise<unknown> => {
        switch (toolName) {
            case 'list_files':
                if (args.location === 'remote') {
                    return await invoke('list_directory', { path: args.path as string });
                } else {
                    return await invoke('list_local_directory', { path: args.path as string });
                }
            case 'read_file':
                if (args.location === 'remote') {
                    return await invoke('get_file_content', { path: args.path as string });
                } else {
                    return await invoke('read_local_file', { path: args.path as string });
                }
            case 'compare_directories':
                return await invoke('compare_directories', {
                    remotePath: args.remote_path as string,
                    localPath: args.local_path as string
                });
            default:
                throw new Error(`Tool ${toolName} not implemented yet`);
        }
    };

    // Format tool result for display
    const formatToolResult = (toolName: string, result: unknown): string => {
        if (toolName === 'compare_directories' && Array.isArray(result)) {
            const items = result as Array<{ relative_path: string, status: string, is_dir: boolean }>;
            if (items.length === 0) {
                return '✅ **Directories are in sync!** No differences found.';
            }

            const remoteOnly = items.filter(i => i.status === 'remote_only');
            const localOnly = items.filter(i => i.status === 'local_only');
            const different = items.filter(i => i.status === 'different');

            let output = `📊 **Directory Comparison**\n\n`;
            output += `Found **${items.length}** differences:\n\n`;

            if (remoteOnly.length > 0) {
                output += `🌐 **Remote only** (${remoteOnly.length}):\n`;
                remoteOnly.slice(0, 10).forEach(i => {
                    output += `  ${i.is_dir ? '📁' : '📄'} ${i.relative_path}\n`;
                });
                if (remoteOnly.length > 10) output += `  _...and ${remoteOnly.length - 10} more_\n`;
                output += '\n';
            }

            if (localOnly.length > 0) {
                output += `💻 **Local only** (${localOnly.length}):\n`;
                localOnly.slice(0, 10).forEach(i => {
                    output += `  ${i.is_dir ? '📁' : '📄'} ${i.relative_path}\n`;
                });
                if (localOnly.length > 10) output += `  _...and ${localOnly.length - 10} more_\n`;
                output += '\n';
            }

            if (different.length > 0) {
                output += `⚠️ **Modified** (${different.length}):\n`;
                different.slice(0, 10).forEach(i => {
                    output += `  ${i.is_dir ? '📁' : '📄'} ${i.relative_path}\n`;
                });
                if (different.length > 10) output += `  _...and ${different.length - 10} more_\n`;
            }

            return output;
        }

        // Default: show JSON
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

        try {
            if (!selectedModel) {
                throw new Error('No model selected. Click ⚙️ to configure a provider.');
            }

            // Load settings to get API key
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
                        if (routedProvider && routedProvider.apiKey) {
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

            if (!provider || !provider.apiKey) {
                throw new Error(`API key not configured for ${activeModel.providerName}`);
            }

            // Add system prompt with tools
            const systemPrompt = `You are AeroAgent, an AI assistant for AeroFTP file manager.
You can execute file operations. When you need to use a tool, respond with:
TOOL: tool_name
ARGS: {"param": "value"}

Available tools:
${generateToolsPrompt()}

Always explain what you're doing before executing tools.`;

            // Build message history
            const messageHistory = [
                { role: 'system', content: systemPrompt },
                ...messages.slice(-10).map(m => ({
                    role: m.role === 'user' ? 'user' : 'assistant',
                    content: m.content,
                })),
                { role: 'user', content: input }
            ];

            // Call the AI
            const response = await invoke<{ content: string; model: string }>('ai_chat', {
                request: {
                    provider_type: activeModel.providerType,
                    model: activeModel.modelName,
                    api_key: provider.apiKey,
                    base_url: provider.baseUrl,
                    messages: messageHistory,
                    max_tokens: settings.advancedSettings?.maxTokens || 4096,
                    temperature: settings.advancedSettings?.temperature || 0.7,
                }
            });

            // Prepare model info for message signature
            const modelInfo = {
                modelName: activeModel.displayName,
                providerName: activeModel.providerName,
                providerType: activeModel.providerType,
            };

            // Check if AI wants to use a tool
            const toolParsed = parseToolCall(response.content);

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
                            content: `🔧 I want to execute ${toolParsed.tool}. Approve or cancel:`,
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
                // Regular response without tool
                const assistantMessage: Message = {
                    id: (Date.now() + 1).toString(),
                    role: 'assistant',
                    content: response.content,
                    timestamp: new Date(),
                    modelInfo,
                };
                setMessages(prev => [...prev, assistantMessage]);
            }

        } catch (error: any) {
            const errorMessage: Message = {
                id: (Date.now() + 1).toString(),
                role: 'assistant',
                content: `⚠️ **Error**: ${error.toString()}\n\nMake sure you have configured an AI provider in settings.`,
                timestamp: new Date(),
            };
            setMessages(prev => [...prev, errorMessage]);
        } finally {
            setIsLoading(false);
        }
    };

    return (
        <div className={`flex flex-col h-full bg-gray-900 ${className}`}>
            {/* Minimal Header */}
            <div className="flex items-center justify-between px-4 py-2 bg-gray-800/50 border-b border-gray-700/50">
                <div className="flex items-center gap-2 text-sm text-gray-300">
                    <Sparkles size={14} className="text-purple-400" />
                    <span className="font-medium">AeroAgent</span>
                </div>
                <button
                    onClick={() => setShowSettings(true)}
                    className="p-1.5 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
                    title="AI Settings"
                >
                    <Settings2 size={14} />
                </button>
            </div>

            {/* Messages Area */}
            <div className="flex-1 overflow-y-auto">
                {messages.length === 0 ? (
                    /* Empty State - System Welcome */
                    <div className="h-full flex flex-col items-center justify-center text-center px-6 py-8">
                        <div className="w-12 h-12 rounded-full bg-purple-600/20 flex items-center justify-center mb-4">
                            <Sparkles size={24} className="text-purple-400" />
                        </div>
                        <h3 className="text-lg font-medium text-gray-200 mb-2">AeroAgent</h3>
                        <p className="text-sm text-gray-400 max-w-sm mb-6">
                            Your AI-powered FTP assistant. I can help you manage files, analyze code, and execute operations.
                        </p>
                        <div className="grid grid-cols-2 gap-2 text-xs max-w-md">
                            {[
                                { icon: '📂', label: 'List files & folders' },
                                { icon: '📄', label: 'Read file contents' },
                                { icon: '✏️', label: 'Create & edit files' },
                                { icon: '🔄', label: 'Upload & download' },
                                { icon: '🔍', label: 'Compare directories' },
                                { icon: '🔐', label: 'Modify permissions' },
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
                                    className={`max-w-[80%] rounded-lg px-4 py-2 text-sm ${message.role === 'user'
                                        ? 'bg-blue-600 text-white'
                                        : 'bg-gray-800 text-gray-200'
                                        }`}
                                >
                                    <div
                                        className="select-text prose prose-invert prose-sm max-w-none"
                                        dangerouslySetInnerHTML={{ __html: renderMarkdown(message.content) }}
                                    />
                                    <div className={`text-[10px] mt-1 flex items-center gap-2 ${message.role === 'user' ? 'text-blue-200' : 'text-gray-500'}`}>
                                        <span>{message.timestamp.toLocaleTimeString()}</span>
                                        {message.role === 'assistant' && message.modelInfo && (
                                            <span className="flex items-center gap-1 text-gray-400">
                                                • {getProviderIcon(message.modelInfo.providerType, 10)}
                                                <span>{message.modelInfo.modelName}</span>
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
                                    Thinking...
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
                            placeholder="Ask anything... (Shift+Enter for new line)"
                            className="flex-1 bg-transparent text-sm text-white placeholder-gray-500 focus:outline-none resize-none min-h-[24px] max-h-[120px]"
                            rows={1}
                        />
                        <button
                            onClick={toggleListening}
                            className={`p-1.5 rounded transition-colors ${isListening
                                ? 'text-red-400 bg-red-500/20'
                                : 'text-gray-400 hover:text-white hover:bg-gray-700'}`}
                            title={isListening ? 'Stop listening' : 'Voice input'}
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
                                    title="Add context (@path)"
                                >+</button>

                                {showContextMenu && (
                                    <div className="absolute left-0 bottom-full mb-1 bg-gray-800 border border-gray-600 rounded-lg shadow-xl z-20 py-1 min-w-[200px]">
                                        <div className="px-3 py-1.5 text-[10px] text-gray-500 border-b border-gray-700">Insert path</div>

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
                                                    <span className="text-gray-200">Remote Path</span>
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
                                                    <span className="text-gray-200">Local Path</span>
                                                    <span className="text-gray-500 text-[10px] truncate max-w-[160px]">{localPath}</span>
                                                </div>
                                            </button>
                                        )}

                                        {(!remotePath && !localPath) && (
                                            <div className="px-3 py-2 text-gray-500">No paths available</div>
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
                                                <span>Copy both paths</span>
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
                                            return <><span>🤖</span><span className="text-purple-300">Auto</span></>;
                                        }
                                        return <span>Select Model</span>;
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
                                                            <span className="font-medium text-purple-300">Auto (Smart Routing)</span>
                                                            <span className="text-gray-500 text-[10px]">Automatic model selection</span>
                                                        </div>
                                                        {!selectedModel && <span className="text-purple-400">✓</span>}
                                                    </button>
                                                );
                                            }
                                            return null;
                                        })()}

                                        {availableModels.length === 0 ? (
                                            <div className="px-3 py-2 text-xs text-gray-400">No models configured. Click ⚙️</div>
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
                        <span className="text-[10px] text-gray-500">AI may make mistakes. Double-check code.</span>
                    </div>
                </div>
            </div>

            {/* AI Settings Panel */}
            <AISettingsPanel isOpen={showSettings} onClose={() => setShowSettings(false)} />
        </div>
    );
};

export default AIChat;
