import React, { useState, useRef, useEffect, useCallback, useMemo } from 'react';
import { Send, Bot, Sparkles, Mic, MicOff, ChevronDown, Trash2, MessageSquare, Copy, Check, ImageIcon, X, GitBranch, Globe } from 'lucide-react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { GeminiIcon, OpenAIIcon, AnthropicIcon, XAIIcon, OpenRouterIcon, OllamaIcon, KimiIcon, QwenIcon, DeepSeekIcon } from './AIIcons';
import { AISettingsPanel } from '../AISettings';
import { AISettings, AIProviderType } from '../../types/ai';
import { AgentToolCall, AGENT_TOOLS, toNativeDefinitions, isSafeTool, getToolByName, getToolByNameFromAll } from '../../types/tools';
import { PluginManifest, allPluginTools, findPluginForTool } from '../../types/plugins';
import { ToolApproval } from './ToolApproval';
import { BatchToolApproval } from './BatchToolApproval';
import { MarkdownRenderer } from './MarkdownRenderer';
import { ThinkingBlock } from './ThinkingBlock';
import type { Conversation } from '../../utils/chatHistory';
import { secureGetWithFallback } from '../../utils/secureStorage';
import { useTranslation } from '../../i18n';
import { logger } from '../../utils/logger';
import { Message, AIChatProps, SelectedModel, MAX_IMAGES, MUTATION_TOOLS } from './aiChatTypes';
import { checkRateLimit, recordRequest, withRetry, estimateTokens, buildMessageWindow, detectTaskType, parseToolCalls, formatToolResult } from './aiChatUtils';
import { analyzeToolError } from './aiChatToolRetry';
import { buildExecutionLevels, executePipeline } from './aiChatToolPipeline';
import { ToolMacro, resolveMacroSteps, macrosToToolDefinitions, isMacroCall, getMacroName, DEFAULT_MACROS, MAX_TOTAL_MACRO_STEPS, createMacroStepCounter, MacroStepCounter } from './aiChatToolMacros';
import { validateToolArgs } from './aiChatToolValidation';
import { computeTokenInfo } from './aiChatTokenInfo';
import { useAIChatImages } from './useAIChatImages';
import { useAIChatConversations } from './useAIChatConversations';
import { useAgentMemory } from './useAgentMemory';
import { buildContextBlock, buildSystemPrompt } from './aiChatSystemPrompt';
import { getParameterPreset } from './aiProviderProfiles';
import { detectProjectContext, invalidateProjectCache, fetchFileImports, fetchGitContext } from './aiChatProjectContext';
import { buildSmartContext, formatSmartContextForPrompt, determineBudgetMode } from './aiChatSmartContext';
import { TokenBudgetIndicator, type TokenBudgetData } from './TokenBudgetIndicator';
import { BranchSelector } from './ConversationBranch';
import type { ProjectContext } from '../../types/contextIntelligence';
import { AIChatHeader } from './AIChatHeader';
import { DEFAULT_TEMPLATES, loadCustomTemplates, resolveTemplate } from './aiChatPromptTemplates';
import type { PromptTemplate } from './aiChatPromptTemplates';
import PromptTemplateSelector from './PromptTemplateSelector';
import { ChatSearchOverlay, type SearchMatch } from './ChatSearchOverlay';
import { useKeyboardShortcuts, getDefaultShortcuts } from './useKeyboardShortcuts';
import { initBudgetManager, checkBudget, recordSpending, getConversationCost, type BudgetCheckResult, type ConversationCost } from './CostBudgetManager';
import { CostBudgetIndicator } from './CostBudgetIndicator';

/** Maximum autonomous tool-call steps before stopping */
const MAX_AUTO_STEPS = 10;
/** Maximum steps in extreme mode */
const MAX_AUTO_STEPS_EXTREME = 50;

/** 3×3 grid-dots animated spinner */
const GridSpinner: React.FC<{ size?: number; className?: string }> = ({ size = 16, className = '' }) => (
    <svg viewBox="0 0 105 105" xmlns="http://www.w3.org/2000/svg" fill="currentColor" width={size} height={size} className={className}>
        <circle cx="12.5" cy="12.5" r="12.5"><animate attributeName="fill-opacity" begin="0s" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="12.5" cy="52.5" r="12.5"><animate attributeName="fill-opacity" begin="100ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="52.5" cy="12.5" r="12.5"><animate attributeName="fill-opacity" begin="300ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="52.5" cy="52.5" r="12.5"><animate attributeName="fill-opacity" begin="600ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="92.5" cy="12.5" r="12.5"><animate attributeName="fill-opacity" begin="800ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="92.5" cy="52.5" r="12.5"><animate attributeName="fill-opacity" begin="400ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="12.5" cy="92.5" r="12.5"><animate attributeName="fill-opacity" begin="200ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="52.5" cy="92.5" r="12.5"><animate attributeName="fill-opacity" begin="500ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
        <circle cx="92.5" cy="92.5" r="12.5"><animate attributeName="fill-opacity" begin="700ms" dur="1s" values="1;.2;1" calcMode="linear" repeatCount="indefinite"/></circle>
    </svg>
);

/** Hook: cycle through thinking messages while loading - now uses translation */
function useThinkingMessage(isActive: boolean, t: (key: string) => string, intervalMs = 3000): string {
    const [index, setIndex] = useState(0);

    // Build thinking messages array using translation function
    const thinkingMessages = [
        t('ai.thinking.thinking'),
        t('ai.thinking.analyzing'),
        t('ai.thinking.processing'),
        t('ai.thinking.reasoning'),
        t('ai.thinking.connectingDots'),
        t('ai.thinking.almostThere'),
        t('ai.thinking.craftingResponse'),
        t('ai.thinking.exploringOptions'),
        t('ai.thinking.percolating'),
        t('ai.thinking.synthesizing'),
        t('ai.thinking.evaluating'),
        t('ai.thinking.formulating'),
        t('ai.thinking.brainstorming'),
        t('ai.thinking.crunchingData'),
        t('ai.thinking.pondering'),
        t('ai.thinking.cookingUpIdeas'),
    ];

    useEffect(() => {
        if (!isActive) { setIndex(0); return; }
        const id = setInterval(() => setIndex(i => (i + 1) % thinkingMessages.length), intervalMs);
        return () => clearInterval(id);
    }, [isActive, intervalMs, thinkingMessages.length]);

    return thinkingMessages[index];
}

// Get provider icon based on type
const getProviderIcon = (type: AIProviderType, size = 12): React.ReactNode => {
    switch (type) {
        case 'google': return <GeminiIcon size={size} />;
        case 'openai': return <OpenAIIcon size={size} />;
        case 'anthropic': return <AnthropicIcon size={size} />;
        case 'xai': return <XAIIcon size={size} />;
        case 'openrouter': return <OpenRouterIcon size={size} />;
        case 'ollama': return <OllamaIcon size={size} />;
        case 'kimi': return <KimiIcon size={size} />;
        case 'qwen': return <QwenIcon size={size} />;
        case 'deepseek': return <DeepSeekIcon size={size} />;
        case 'custom': return <Bot size={size} className="text-gray-400" />;
        default: return <Bot size={size} />;
    }
};

export const AIChat: React.FC<AIChatProps> = ({ className = '', remotePath, localPath, appTheme = 'dark', providerType, isConnected, selectedFiles, serverHost, serverPort, serverUser, activeFilePanel, isCloudConnection, onFileMutation, editorFileName, editorFilePath }) => {
    const t = useTranslation();

    // Conversation management (hook)
    const {
        messages, setMessages, conversations, activeConversationId,
        showHistory, setShowHistory, showExportMenu, setShowExportMenu,
        expandedMessages, setExpandedMessages,
        messagesRef, conversationsRef,
        persistConversation, startNewChat: startNewChatBase, switchConversation: switchConversationBase,
        handleDeleteConversation, loadChatHistory, exportConversation,
        activeBranchId, forkConversation, switchBranch, deleteBranch,
    } = useAIChatConversations();

    // Image/vision handling (hook)
    const { attachedImages, handleImagePick, handlePaste, removeImage, clearImages } = useAIChatImages();

    const isLightTheme = appTheme === 'light';

    // Theme-aware classes (light/dark/tokyo/cyber)
    const ct = useMemo(() => {
        switch (appTheme) {
            case 'light': return {
                bg: 'bg-white', bgSecondary: 'bg-gray-100', bgSecondaryHalf: 'bg-gray-100/50',
                bgSecondaryHover: 'hover:bg-gray-200', bgHalf: 'bg-gray-50/80',
                border: 'border-gray-300', borderSolid: 'border-gray-300',
                text: 'text-gray-900', textSecondary: 'text-gray-600', textMuted: 'text-gray-500',
                textHover: 'hover:text-gray-900',
                btn: 'text-gray-500 hover:text-gray-900 hover:bg-gray-200',
                userMsg: 'bg-blue-100 text-blue-900 border border-blue-300',
                userMsgMeta: 'text-blue-600/60',
                assistantMsg: 'bg-gray-100 text-gray-900',
                prose: 'prose prose-sm',
                gradient: 'from-gray-100',
                inputBg: 'bg-gray-100 border-gray-300', inputText: 'text-gray-900 placeholder-gray-400',
                dropdown: 'bg-white border-gray-300 shadow-lg',
                dropdownItem: 'hover:bg-gray-100', dropdownText: 'text-gray-900',
                sidebarBg: 'border-gray-300 bg-gray-50',
                sidebarActive: 'bg-purple-100 text-purple-700',
                sidebarInactive: 'text-gray-600 hover:bg-gray-200 hover:text-gray-900',
                modelSelected: 'bg-gray-200',
            };
            case 'tokyo': return {
                bg: 'bg-[#1a1b26]', bgSecondary: 'bg-[#16161e]', bgSecondaryHalf: 'bg-[#16161e]/50',
                bgSecondaryHover: 'hover:bg-[#292e42]', bgHalf: 'bg-[#1a1b26]/80',
                border: 'border-[#292e42]', borderSolid: 'border-[#414868]',
                text: 'text-[#c0caf5]', textSecondary: 'text-[#a9b1d6]', textMuted: 'text-[#565f89]',
                textHover: 'hover:text-[#c0caf5]',
                btn: 'text-[#565f89] hover:text-[#c0caf5] hover:bg-[#292e42]',
                userMsg: 'bg-[#24283b] text-[#7aa2f7] border border-[#7aa2f7]/30',
                userMsgMeta: 'text-[#7aa2f7]/50',
                assistantMsg: 'bg-[#16161e]/80 text-[#c0caf5]',
                prose: 'prose prose-invert prose-sm',
                gradient: 'from-[#16161e]',
                inputBg: 'bg-[#16161e] border-[#292e42]', inputText: 'text-[#c0caf5] placeholder-[#565f89]',
                dropdown: 'bg-[#16161e] border-[#292e42] shadow-xl',
                dropdownItem: 'hover:bg-[#292e42]', dropdownText: 'text-[#c0caf5]',
                sidebarBg: 'border-[#292e42] bg-[#16161e]/30',
                sidebarActive: 'bg-[#9d7cd8]/20 text-[#bb9af7]',
                sidebarInactive: 'text-[#565f89] hover:bg-[#292e42]/50 hover:text-[#a9b1d6]',
                modelSelected: 'bg-[#292e42]',
            };
            case 'cyber': return {
                bg: 'bg-[#0a0e17]', bgSecondary: 'bg-[#0d1117]', bgSecondaryHalf: 'bg-[#0d1117]/50',
                bgSecondaryHover: 'hover:bg-emerald-500/10', bgHalf: 'bg-[#0a0e17]/80',
                border: 'border-emerald-900/40', borderSolid: 'border-emerald-800/50',
                text: 'text-emerald-100', textSecondary: 'text-emerald-400/70', textMuted: 'text-emerald-600/60',
                textHover: 'hover:text-emerald-200',
                btn: 'text-gray-500 hover:text-emerald-300 hover:bg-emerald-500/10',
                userMsg: 'bg-cyan-950/40 text-cyan-100 border border-cyan-500/30',
                userMsgMeta: 'text-cyan-400/50',
                assistantMsg: 'bg-[#0d1117]/80 text-emerald-100',
                prose: 'prose prose-invert prose-sm',
                gradient: 'from-[#0d1117]',
                inputBg: 'bg-[#0d1117] border-emerald-800/50', inputText: 'text-emerald-100 placeholder-emerald-700',
                dropdown: 'bg-[#0d1117] border-emerald-800/50 shadow-xl shadow-emerald-900/20',
                dropdownItem: 'hover:bg-emerald-500/10', dropdownText: 'text-emerald-100',
                sidebarBg: 'border-emerald-900/40 bg-[#0d1117]/30',
                sidebarActive: 'bg-emerald-500/20 text-emerald-300',
                sidebarInactive: 'text-gray-500 hover:bg-emerald-500/10 hover:text-emerald-300',
                modelSelected: 'bg-emerald-900/30',
            };
            default: return { // dark
                bg: 'bg-gray-900', bgSecondary: 'bg-gray-800', bgSecondaryHalf: 'bg-gray-800/50',
                bgSecondaryHover: 'hover:bg-gray-700', bgHalf: 'bg-gray-800/80',
                border: 'border-gray-700/50', borderSolid: 'border-gray-600',
                text: 'text-gray-200', textSecondary: 'text-gray-400', textMuted: 'text-gray-500',
                textHover: 'hover:text-white',
                btn: 'text-gray-400 hover:text-white hover:bg-gray-700',
                userMsg: 'bg-[#1e2a3a] text-blue-100 border border-blue-900/40',
                userMsgMeta: 'text-blue-300/60',
                assistantMsg: 'bg-gray-800/80 text-gray-200',
                prose: 'prose prose-invert prose-sm',
                gradient: 'from-gray-800',
                inputBg: 'bg-gray-800 border-gray-600', inputText: 'text-white placeholder-gray-500',
                dropdown: 'bg-gray-800 border-gray-600 shadow-xl',
                dropdownItem: 'hover:bg-gray-700', dropdownText: 'text-gray-200',
                sidebarBg: 'border-gray-700/50 bg-gray-800/30',
                sidebarActive: 'bg-purple-600/20 text-purple-300',
                sidebarInactive: 'text-gray-400 hover:bg-gray-700/50 hover:text-gray-200',
                modelSelected: 'bg-gray-700/50',
            };
        }
    }, [appTheme]);

    const [input, setInput] = useState('');
    const [showModelSelector, setShowModelSelector] = useState(false);
    const [showContextMenu, setShowContextMenu] = useState(false);
    const [showSettings, setShowSettings] = useState(false);
    const [isLoading, setIsLoading] = useState(false);
    const thinkingMessage = useThinkingMessage(isLoading, t);
    const [isListening, setIsListening] = useState(false);
    const [copiedId, setCopiedId] = useState<string | null>(null);
    const [availableModels, setAvailableModels] = useState<SelectedModel[]>([]);
    const [selectedModel, setSelectedModel] = useState<SelectedModel | null>(null);
    const [pendingToolCalls, setPendingToolCalls] = useState<AgentToolCall[]>([]);
    const [isAutoExecuting, setIsAutoExecuting] = useState(false);
    const [autoStepCount, setAutoStepCount] = useState(0);
    const [macros] = useState<ToolMacro[]>(DEFAULT_MACROS);

    // Phase 4: Search, Templates, Cost Budget
    const [showSearch, setShowSearch] = useState(false);
    const [showTemplates, setShowTemplates] = useState(false);
    const [allTemplates, setAllTemplates] = useState<PromptTemplate[]>(DEFAULT_TEMPLATES);
    const [conversationCost, setConversationCost] = useState<ConversationCost | null>(null);
    const [budgetCheck, setBudgetCheck] = useState<BudgetCheckResult | null>(null);
    const [searchMatches, setSearchMatches] = useState<SearchMatch[]>([]);
    const [activeSearchIndex, setActiveSearchIndex] = useState(0);
    const messageListRef = useRef<HTMLDivElement>(null);

    // Wrap startNewChat to also clear pending tool calls, token budget, and cost
    const startNewChat = useCallback(() => {
        startNewChatBase();
        setPendingToolCalls([]);
        setTokenBudgetData(null);
        setConversationCost(null);
        setBudgetCheck(null);
    }, [startNewChatBase]);

    // Wrap switchConversation to also clear pending tool calls
    const switchConversation = useCallback((conv: Conversation) => {
        switchConversationBase(conv);
        setPendingToolCalls([]);
    }, [switchConversationBase]);
    // AI settings cached from vault (sync init from localStorage, then async vault refresh)
    const [cachedAiSettings, setCachedAiSettings] = useState<AISettings | null>(() => {
        try {
            const raw = localStorage.getItem('aeroftp_ai_settings');
            return raw ? JSON.parse(raw) : null;
        } catch { return null; }
    });
    // Extreme mode: auto-approve all tools, increased step limit (Cyber theme only)
    const [extremeMode, setExtremeMode] = useState(false);
    const extremeModeRef = useRef(false);
    useEffect(() => {
        extremeModeRef.current = extremeMode;
    }, [extremeMode]);
    // Load extreme mode setting from localStorage
    useEffect(() => {
        try {
            const saved = localStorage.getItem('aeroftp_ai_extreme_mode');
            if (saved === 'true') {
                // Only enable if current theme is 'cyber'
                const theme = document.documentElement.getAttribute('data-theme');
                if (theme === 'cyber') {
                    setExtremeMode(true);
                }
            }
        } catch { /* ignore */ }
    }, []);

    const autoStopRef = useRef(false);
    const multiStepContextRef = useRef<{
        aiRequest: Record<string, unknown>;
        messageHistory: Array<Record<string, unknown>>;
        modelInfo: { modelName: string; providerName: string; providerType: AIProviderType };
        modelDef: { supportsStreaming?: boolean; supportsTools?: boolean; supportsThinking?: boolean; supportsParallelTools?: boolean; maxContextTokens?: number; inputCostPer1k?: number; outputCostPer1k?: number } | undefined;
    } | null>(null);
    const streamingMsgIdRef = useRef<string | null>(null);
    const messagesEndRef = useRef<HTMLDivElement>(null);
    const inputRef = useRef<HTMLTextAreaElement>(null);
    const ragIndexRef = useRef<Record<string, unknown> | null>(null);
    const ragIndexedPathRef = useRef<string | null>(null);
    const [pluginManifests, setPluginManifests] = useState<PluginManifest[]>([]);

    // Phase 3: Context Intelligence
    const projectPath = localPath || remotePath;
    const { memory: agentMemory, appendMemory: appendAgentMemory } = useAgentMemory(projectPath);
    const projectContextRef = useRef<ProjectContext | null>(null);
    const gitSummaryRef = useRef<string | null>(null);
    const gitBranchRef = useRef<string | null>(null);
    const fileImportsRef = useRef<string[]>([]);
    const [tokenBudgetData, setTokenBudgetData] = useState<TokenBudgetData | null>(null);

    // Refresh AI settings from vault (async, with localStorage fallback)
    const refreshAiSettings = useCallback(async () => {
        const settings = await secureGetWithFallback<AISettings>('ai_settings', 'aeroftp_ai_settings');
        if (settings) setCachedAiSettings(settings);
    }, []);

    // Load AI settings from vault on mount
    useEffect(() => { refreshAiSettings(); }, [refreshAiSettings]);

    // Load plugins on mount
    useEffect(() => {
        invoke<PluginManifest[]>('list_plugins')
            .then(manifests => setPluginManifests(manifests))
            .catch(() => setPluginManifests([]));
    }, []);

    // Phase 4: Init budget manager + load custom templates on mount
    useEffect(() => {
        initBudgetManager();
        loadCustomTemplates().then(custom => {
            if (custom.length > 0) setAllTemplates([...DEFAULT_TEMPLATES, ...custom]);
        });
    }, []);

    // Merge built-in tools with plugin tools (enforce minimum medium danger for plugins)
    const pluginTools = allPluginTools(pluginManifests).map(t => ({
        ...t,
        dangerLevel: t.dangerLevel === 'safe' ? 'medium' as const : t.dangerLevel,
    }));
    const allTools = [...AGENT_TOOLS, ...pluginTools, ...macrosToToolDefinitions(macros)];

    /** In extreme mode, ALL tools are auto-approved (no confirmation dialog) */
    const isAutoApproved = useCallback((toolName: string) =>
        extremeModeRef.current || isSafeTool(toolName, allTools),
    [allTools]);

    /** Effective max steps: 50 in extreme mode, 10 normally */
    const effectiveMaxSteps = extremeMode ? MAX_AUTO_STEPS_EXTREME : MAX_AUTO_STEPS;

    const scrollToBottom = () => {
        messagesEndRef.current?.scrollIntoView({ behavior: 'smooth' });
    };

    // Phase 4: Keyboard shortcuts (#79)
    const shortcuts = getDefaultShortcuts({
        clearChat: startNewChat,
        newChat: startNewChat,
        exportChat: () => exportConversation('markdown'),
        toggleSearch: () => setShowSearch(prev => !prev),
        focusInput: () => inputRef.current?.focus(),
    });
    useKeyboardShortcuts(shortcuts);

    // Phase 4: Handle search highlight navigation
    const handleSearchHighlightMessage = useCallback((messageId: string) => {
        const msgEl = messageListRef.current?.querySelector(`[data-message-id="${messageId}"]`);
        if (msgEl) msgEl.scrollIntoView({ behavior: 'smooth', block: 'center' });
    }, []);

    const handleSearchResults = useCallback((matches: SearchMatch[], activeIndex: number) => {
        setSearchMatches(matches);
        setActiveSearchIndex(activeIndex);
    }, []);

    // Phase 4: Template selection handler (#75)
    const handleTemplateSelect = useCallback((template: PromptTemplate) => {
        const resolved = resolveTemplate(template, {
            selection: input.startsWith('/') ? '' : input,
            fileName: editorFileName,
            filePath: editorFilePath,
        });
        setInput(resolved);
        setShowTemplates(false);
        setTimeout(() => {
            if (inputRef.current) {
                inputRef.current.focus();
                inputRef.current.style.height = 'auto';
                inputRef.current.style.height = Math.min(inputRef.current.scrollHeight, 120) + 'px';
            }
        }, 50);
    }, [input, editorFileName, editorFilePath]);

    // Tool labels and markdown rendering moved to MarkdownRenderer.tsx

    useEffect(() => {
        scrollToBottom();
    }, [messages]);

    // Debounced persist (fix H-004: stale closure + rapid fire)
    // Note: persistConversation handles activeConversationId === null by creating a new ID
    useEffect(() => {
        if (messages.length === 0) return;
        const timer = setTimeout(() => persistConversation(messages), 1500);
        return () => clearTimeout(timer);
    }, [messages, persistConversation]);

    // Save conversation on unmount (component destroyed when DevTools closes)
    useEffect(() => {
        return () => {
            if (messagesRef.current.length > 0) persistConversation(messagesRef.current);
        };
    // eslint-disable-next-line react-hooks/exhaustive-deps
    }, []);

    // Load available models from settings (API keys fetched from OS Keyring)
    const loadModels = async () => {
        // Read from vault with localStorage fallback, update cache
        const settings = await secureGetWithFallback<AISettings>('ai_settings', 'aeroftp_ai_settings');
        if (settings) {
            setCachedAiSettings(settings);
            try {
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
                logger.error('[AIChat] Failed to load AI settings:', e);
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
    useEffect(() => { loadChatHistory(); }, [loadChatHistory]);

    // Listen for "Ask AeroAgent" events from Monaco editor
    useEffect(() => {
        const handleAskAgent = (e: Event) => {
            const { code, fileName } = (e as CustomEvent).detail;
            const language = fileName.split('.').pop() || 'text';
            const trimmedCode = code.length > 2000 ? code.slice(0, 2000) + '\n// ...(truncated)' : code;
            const contextText = `Regarding this code from \`${fileName}\`:\n\`\`\`${language}\n${trimmedCode}\n\`\`\`\n\n`;
            setInput(contextText);
            // Focus the input
            setTimeout(() => {
                if (inputRef.current) {
                    inputRef.current.focus();
                    inputRef.current.style.height = 'auto';
                    inputRef.current.style.height = Math.min(inputRef.current.scrollHeight, 120) + 'px';
                }
            }, 100);
        };
        window.addEventListener('aeroagent-ask', handleAskAgent);
        return () => window.removeEventListener('aeroagent-ask', handleAskAgent);
    }, []);

    // Auto-index workspace for RAG context (fix M-011: debounce + cache by path)
    useEffect(() => {
        const indexPath = localPath; // Only index local paths - remote paths are not accessible to rag_index
        if (!indexPath) { ragIndexRef.current = null; ragIndexedPathRef.current = null; return; }
        // Cache: don't re-index same path
        if (ragIndexedPathRef.current === indexPath) return;
        const timer = setTimeout(() => {
            invoke('execute_ai_tool', {
                toolName: 'rag_index',
                args: { path: indexPath, recursive: true, max_files: 100 },
                contextLocalPath: localPath || undefined,
            }).then((result: unknown) => {
                ragIndexRef.current = result as Record<string, unknown>;
                ragIndexedPathRef.current = indexPath;
            }).catch(() => {
                ragIndexRef.current = null;
                ragIndexedPathRef.current = null;
            });
        }, 1000);
        return () => clearTimeout(timer);
    }, [localPath]);

    // Phase 3: Auto-detect project context (#66)
    useEffect(() => {
        if (!projectPath) { projectContextRef.current = null; return; }
        const timer = setTimeout(() => {
            detectProjectContext(projectPath).then(ctx => {
                projectContextRef.current = ctx;
            });
        }, 500);
        return () => clearTimeout(timer);
    }, [projectPath]);

    // Phase 3: Fetch git context (#70)
    useEffect(() => {
        if (!projectPath) { gitSummaryRef.current = null; gitBranchRef.current = null; return; }
        const timer = setTimeout(() => {
            fetchGitContext(projectPath).then(result => {
                if (result) {
                    gitSummaryRef.current = result.summary;
                    gitBranchRef.current = result.branch;
                } else {
                    gitSummaryRef.current = null;
                    gitBranchRef.current = null;
                }
            });
        }, 800);
        return () => clearTimeout(timer);
    }, [projectPath]);

    // Phase 3: Scan file imports when editor file changes (#67)
    useEffect(() => {
        if (!editorFilePath) { fileImportsRef.current = []; return; }
        fetchFileImports(editorFilePath).then(imports => {
            fileImportsRef.current = imports;
        });
    }, [editorFilePath]);

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
        recognition.lang = navigator.language || 'en-US';

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

    // Execute a tool via unified provider-agnostic command (built-in or plugin)
    // SECURITY: Check built-in tools FIRST to prevent plugin name hijacking
    const executeToolByName = async (toolName: string, args: Record<string, unknown>): Promise<unknown> => {
        const isBuiltIn = AGENT_TOOLS.some(t => t.name === toolName);
        if (!isBuiltIn) {
            const plugin = findPluginForTool(pluginManifests, toolName);
            if (plugin) {
                return await invoke('execute_plugin_tool', {
                    pluginId: plugin.id,
                    toolName,
                    argsJson: JSON.stringify(args),
                });
            }
        }
        return await invoke('execute_ai_tool', { toolName, args, contextLocalPath: localPath || undefined });
    };

    // Execute a tool
    const executeTool = async (toolCall: AgentToolCall, _macroDepth = 0, _stepCounter?: MacroStepCounter): Promise<string | null> => {
        const tool = getToolByName(toolCall.toolName) || getToolByNameFromAll(toolCall.toolName, allTools);

        // Handle macro tool calls
        if (isMacroCall(toolCall.toolName)) {
            if (_macroDepth >= 5) {
                const errMsg: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: 'Macro recursion limit exceeded (max depth: 5)',
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, errMsg]);
                setPendingToolCalls([]);
                return null;
            }

            // Initialize step counter at top-level macro call
            const stepCounter = _stepCounter || createMacroStepCounter();

            const macroName = getMacroName(toolCall.toolName);
            const macro = macros.find(m => m.name === macroName);
            if (!macro) {
                const errMsg: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: `Unknown macro: ${macroName}`,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, errMsg]);
                setPendingToolCalls([]);
                return null;
            }

            // Resolve template variables and execute steps sequentially
            const steps = resolveMacroSteps(macro, toolCall.args);
            // Check for unresolved template variables
            const unresolvedVars = steps.flatMap(s =>
                Object.entries(s.args)
                    .filter(([_, v]) => /\{\{\w+\}\}/.test(v))
                    .map(([k, v]) => `${s.toolName}.${k}=${v}`)
            );
            if (unresolvedVars.length > 0) {
                const errMsg: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: `Macro "${macroName}" has unresolved variables: ${unresolvedVars.join(', ')}`,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, errMsg]);
                setPendingToolCalls([]);
                return null;
            }

            let lastResult: string | null = null;
            for (const step of steps) {
                if (++stepCounter.total > MAX_TOTAL_MACRO_STEPS) {
                    const errMsg: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: `Maximum macro execution steps (${MAX_TOTAL_MACRO_STEPS}) exceeded`,
                        timestamp: new Date(),
                    };
                    setMessages(prev => [...prev, errMsg]);
                    setPendingToolCalls([]);
                    return null;
                }
                // SECURITY: Check if step tool has high danger level — require explicit approval
                const stepTool = getToolByName(step.toolName) || getToolByNameFromAll(step.toolName, allTools);
                if (stepTool && stepTool.dangerLevel === 'high') {
                    const pendingStep: AgentToolCall = {
                        id: crypto.randomUUID(),
                        toolName: step.toolName,
                        args: step.args,
                        status: 'pending',
                    };
                    setPendingToolCalls(prev => [...prev, pendingStep]);
                    const warnMsg: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: `Macro step "${step.toolName}" requires approval (danger: high)`,
                        timestamp: new Date(),
                    };
                    setMessages(prev => [...prev, warnMsg]);
                    return lastResult; // Pause macro execution, user must approve remaining steps
                }
                const stepCall: AgentToolCall = {
                    id: crypto.randomUUID(),
                    toolName: step.toolName,
                    args: step.args,
                    status: 'approved',
                };
                lastResult = await executeTool(stepCall, _macroDepth + 1, stepCounter);
                if (!lastResult) break; // Stop on failure
            }
            return lastResult;
        }

        if (!tool) {
            // Only remove the unknown tool from pending, not the entire batch
            setPendingToolCalls(prev => prev.filter(tc => tc.toolName !== toolCall.toolName || tc.id !== toolCall.id));
            return null;
        }

        try {
            // Special case: agent_memory_write — inject project path and refresh memory
            if (toolCall.toolName === 'agent_memory_write') {
                const entry = (toolCall.args.entry as string) || '';
                const category = (toolCall.args.category as string) || 'general';
                if (!entry) throw new Error(t('ai.error.noEntrySpecified'));
                await appendAgentMemory(entry, category);
                const resultMessage: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: `Memory saved: [${category}] ${entry}`,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, resultMessage]);
                setPendingToolCalls([]);
                return `Memory saved: ${entry}`;
            }

            // Special case: terminal_execute is frontend-only (not sent to Rust backend)
            if (toolCall.toolName === 'terminal_execute') {
                const command = (toolCall.args.command as string) || '';
                if (!command) {
                    throw new Error(t('ai.error.noCommandSpecified'));
                }
                // SEC: Deny destructive commands that could cause irreversible damage
                const DENIED_COMMANDS = [
                    /^\s*rm\s+(-[a-zA-Z]*)?.*\s+\/\s*$/,     // rm -rf /
                    /^\s*rm\s+(-[a-zA-Z]*f[a-zA-Z]*\s+)?-[a-zA-Z]*r.*\s+\/\s*$/, // rm -rf /
                    /^\s*mkfs\b/,                               // mkfs (format disk)
                    /^\s*dd\s+.*of=\/dev\//,                    // dd to device
                    /^\s*shutdown\b/,                            // shutdown
                    /^\s*reboot\b/,                              // reboot
                    /^\s*halt\b/,                                // halt
                    /^\s*init\s+[06]\b/,                         // init 0/6
                    /^\s*:\(\)\s*\{\s*:\|:\s*&\s*\}\s*;\s*:/,  // fork bomb
                    /^\s*>\s*\/dev\/sd[a-z]/,                   // overwrite disk
                    /^\s*chmod\s+(-[a-zA-Z]*\s+)?777\s+\//,    // chmod 777 /
                    /^\s*chown\s+.*\s+\/\s*$/,                  // chown /
                ];
                if (DENIED_COMMANDS.some(rx => rx.test(command))) {
                    throw new Error('Command blocked: potentially destructive system command');
                }
                // Dispatch event for terminal to pick up
                window.dispatchEvent(new CustomEvent('terminal-execute', { detail: { command } }));
                // Also activate terminal panel
                window.dispatchEvent(new CustomEvent('devtools-panel-ensure', { detail: 'terminal' }));

                const resultMessage: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: `${t('ai.commandSentToTerminal')}: \`${command}\``,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, resultMessage]);
                setPendingToolCalls([]);
                return t('ai.error.commandSentToTerminal');
            }

            const result = await executeToolByName(toolCall.toolName, toolCall.args);
            const formattedResult = formatToolResult(toolCall.toolName, result);

            // Check for soft failures (tool returned success: false)
            if (result && typeof result === 'object' && 'success' in (result as Record<string, unknown>) && !(result as Record<string, unknown>).success) {
                const softError = String((result as Record<string, unknown>).message || t('ai.error.operationFailed'));
                const strategy = analyzeToolError(toolCall.toolName, toolCall.args, softError);

                const errorMessage: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: `Tool returned failure: ${softError}\n\n**Suggestion**: ${strategy.suggestion}`,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, errorMessage]);

                // Inject suggestion into multi-step context if active
                if (strategy.suggestedTool && multiStepContextRef.current) {
                    multiStepContextRef.current.messageHistory.push({
                        role: 'assistant',
                        content: `Tool "${toolCall.toolName}" failed: ${softError}. Suggested recovery: use "${strategy.suggestedTool}".`,
                    });
                }

                setPendingToolCalls([]);
                return formattedResult; // Still return so multi-step can continue with the error info
            }

            const resultMessage: Message = {
                id: crypto.randomUUID(),
                role: 'assistant',
                content: formattedResult,
                timestamp: new Date(),
            };
            setMessages(prev => [...prev, resultMessage]);

            // Refresh file panels after mutation tools
            const mutationTarget = MUTATION_TOOLS[toolCall.toolName];
            if (mutationTarget && onFileMutation) {
                onFileMutation(mutationTarget);
            }

            // Notify Monaco editor of file change for live sync
            if (toolCall.toolName === 'local_edit' || toolCall.toolName === 'local_write') {
                const changedPath = (toolCall.args.path as string) || '';
                if (changedPath) {
                    window.dispatchEvent(new CustomEvent('file-changed', { detail: { path: changedPath } }));
                    // Invalidate cached project context when config files are modified
                    const configFiles = ['package.json', 'Cargo.toml', 'composer.json', 'pyproject.toml', 'go.mod', 'pom.xml', 'build.gradle'];
                    if (configFiles.some(cf => changedPath.endsWith(cf))) {
                        invalidateProjectCache();
                    }
                }
            }

            setPendingToolCalls([]);
            return formattedResult;
        } catch (error: any) {
            const errorStr = error.message || error.toString();
            const strategy = analyzeToolError(toolCall.toolName, toolCall.args, errorStr);

            // Auto-retry for transient errors
            if (strategy.autoRetry && strategy.canRetry) {
                try {
                    const retryResult = await withRetry(
                        () => executeToolByName(toolCall.toolName, toolCall.args),
                        strategy.maxRetries || 3,
                    );
                    const retryFormatted = formatToolResult(toolCall.toolName, retryResult);
                    const retryMsg: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: retryFormatted,
                        timestamp: new Date(),
                    };
                    setMessages(prev => [...prev, retryMsg]);

                    const mutationTarget = MUTATION_TOOLS[toolCall.toolName];
                    if (mutationTarget && onFileMutation) onFileMutation(mutationTarget);

                    setPendingToolCalls([]);
                    return retryFormatted;
                } catch (retryError: any) {
                    // Retry exhausted, fall through to error display
                    const exhaustedStr = retryError.message || retryError.toString();
                    const errorMessage: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: `Tool failed after retries: ${exhaustedStr}\n\n**Suggestion**: ${strategy.suggestion}`,
                        timestamp: new Date(),
                    };
                    setMessages(prev => [...prev, errorMessage]);
                    setPendingToolCalls([]);
                    return null;
                }
            }

            // Non-retryable error
            const errorMessage: Message = {
                id: crypto.randomUUID(),
                role: 'assistant',
                content: `Tool failed: ${errorStr}\n\n**Suggestion**: ${strategy.suggestion}`,
                timestamp: new Date(),
            };
            setMessages(prev => [...prev, errorMessage]);

            // Inject suggestion into multi-step context
            if (strategy.suggestedTool && multiStepContextRef.current) {
                multiStepContextRef.current.messageHistory.push({
                    role: 'assistant',
                    content: `Tool "${toolCall.toolName}" failed: ${errorStr}. Suggested recovery: use "${strategy.suggestedTool}" with args ${JSON.stringify(strategy.suggestedArgs || {})}.`,
                });
            }

            setPendingToolCalls([]);
            return null;
        }
    };

    // Multi-step autonomous tool execution loop
    const executeMultiStep = async (
        initialToolResult: string,
        aiRequest: Record<string, unknown>,
        messageHistory: Array<Record<string, unknown>>,
        modelInfo: { modelName: string; providerName: string; providerType: AIProviderType },
        modelDef: { supportsStreaming?: boolean; supportsTools?: boolean; supportsThinking?: boolean; supportsParallelTools?: boolean; maxContextTokens?: number; inputCostPer1k?: number; outputCostPer1k?: number } | undefined,
    ) => {
        let stepCount = 1;
        let lastToolResult = initialToolResult;
        setIsAutoExecuting(true);
        setAutoStepCount(1);
        autoStopRef.current = false;

        try {
            while (stepCount < effectiveMaxSteps && !autoStopRef.current) {
                // Add tool result to message history for next AI call
                messageHistory.push({ role: 'assistant', content: `Tool result:\n${lastToolResult}` });
                messageHistory.push({ role: 'user', content: 'Continue with the next step based on the tool result above. If the task is complete, respond normally without calling a tool.' });

                // Make another AI call
                const response = await invoke<{
                    content: string;
                    model: string;
                    tokens_used?: number;
                    input_tokens?: number;
                    output_tokens?: number;
                    cache_creation_input_tokens?: number;
                    cache_read_input_tokens?: number;
                    tool_calls?: Array<{ id: string; name: string; arguments: unknown }>;
                }>('ai_chat', { request: { ...aiRequest, messages: messageHistory } });

                // Parse ALL tool calls from response (parallel support)
                let allToolsParsedMS: Array<{ tool: string; args: Record<string, unknown>; id: string }> = [];
                if (response.tool_calls && response.tool_calls.length > 0) {
                    allToolsParsedMS = response.tool_calls.map((tc: { id: string; name: string; arguments: unknown }) => ({
                        tool: tc.name,
                        args: (() => {
                            if (typeof tc.arguments === 'string') {
                                try { return JSON.parse(tc.arguments) as Record<string, unknown>; }
                                catch { return {} as Record<string, unknown>; }
                            }
                            return tc.arguments as Record<string, unknown>;
                        })(),
                        id: tc.id || crypto.randomUUID(),
                    }));
                } else {
                    const fallbacks = parseToolCalls(response.content);
                    if (fallbacks.length > 0) allToolsParsedMS = fallbacks.map(fb => ({ ...fb, id: crypto.randomUUID() }));
                }

                if (allToolsParsedMS.length === 0) {
                    // AI responded without a tool call - show final response and stop
                    const tokenInfo = computeTokenInfo(response.input_tokens, response.output_tokens, undefined, modelDef, response.cache_creation_input_tokens, response.cache_read_input_tokens);

                    const finalMsg: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: response.content,
                        timestamp: new Date(),
                        modelInfo,
                        tokenInfo,
                    };
                    setMessages(prev => [...prev, finalMsg]);
                    break;
                }

                // Separate safe vs approval-required
                const safeMS = allToolsParsedMS.filter(p => isAutoApproved(p.tool) && getToolByNameFromAll(p.tool, allTools));
                const approvalMS = allToolsParsedMS.filter(p => !isAutoApproved(p.tool) && getToolByNameFromAll(p.tool, allTools));

                if (approvalMS.length > 0) {
                    // Pause multi-step for user approval
                    const pendingMsg: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: response.content || '',
                        timestamp: new Date(),
                        modelInfo,
                    };
                    setMessages(prev => [...prev, pendingMsg]);
                    const pending = approvalMS.map(ac => ({
                        id: ac.id, toolName: ac.tool, args: ac.args, status: 'pending' as const,
                    }));
                    // Validate tool arguments in parallel
                    const pendingWithValidation = await Promise.all(
                        pending.map(async (tc) => {
                            const validation = await validateToolArgs(tc.toolName, tc.args);
                            return { ...tc, validation };
                        })
                    );
                    multiStepContextRef.current = { aiRequest, messageHistory: [...messageHistory], modelInfo, modelDef };
                    setPendingToolCalls(pendingWithValidation);
                    break; // Stop auto-loop, user must approve
                }

                // All safe tools — execute in parallel
                stepCount++;
                setAutoStepCount(stepCount);

                const safeToolCalls = safeMS.map(sc => ({
                    id: sc.id, toolName: sc.tool, args: sc.args, status: 'approved' as const,
                }));
                const levels = buildExecutionLevels(safeToolCalls);
                const results = await executePipeline(levels, executeTool);
                const combinedResult = results.filter(Boolean).join('\n---\n');
                if (!combinedResult) break;

                lastToolResult = combinedResult;
            }

            if (stepCount >= effectiveMaxSteps) {
                const maxMsg: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: `Multi-step execution completed after ${effectiveMaxSteps} steps (maximum reached).`,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, maxMsg]);
            }
        } catch (error: unknown) {
            const errMsg: Message = {
                id: crypto.randomUUID(),
                role: 'assistant',
                content: `Multi-step execution error: ${String(error)}`,
                timestamp: new Date(),
            };
            setMessages(prev => [...prev, errMsg]);
        } finally {
            setIsAutoExecuting(false);
            setAutoStepCount(0);
            setIsLoading(false);
        }
    };

    const handleSend = async () => {
        if ((!input.trim() && attachedImages.length === 0) || isLoading) return;

        // Capture attached images before clearing
        const messageImages = attachedImages.length > 0 ? [...attachedImages] : undefined;

        const userMessage: Message = {
            id: crypto.randomUUID(),
            role: 'user',
            content: input || (messageImages ? t('ai.analyzeThisImage') : ''),
            timestamp: new Date(),
            images: messageImages,
        };

        setMessages(prev => [...prev, userMessage]);
        setInput('');
        if (inputRef.current) inputRef.current.style.height = 'auto';
        clearImages();
        setIsLoading(true);

        let streamingMsgId: string | null = null;
        try {
            // Load settings from vault (with localStorage fallback)
            const settings = await secureGetWithFallback<AISettings>('ai_settings', 'aeroftp_ai_settings');
            if (!settings) {
                throw new Error('No AI providers configured. Click ⚙️ to add one.');
            }
            setCachedAiSettings(settings);

            // Auto-routing: detect task type and resolve model (fix M-010: resolve before null guard)
            let activeModel = selectedModel;
            if (!activeModel && settings.autoRouting?.enabled) {
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
                // Fallback: use default model if auto-routing didn't resolve
                if (!activeModel && settings.defaultModelId) {
                    const defaultModel = settings.models.find(m => m.id === settings.defaultModelId);
                    if (defaultModel) {
                        const defaultProvider = settings.providers.find(p => p.id === defaultModel.providerId);
                        if (defaultProvider) {
                            activeModel = {
                                providerId: defaultProvider.id,
                                providerName: defaultProvider.name,
                                providerType: defaultProvider.type,
                                modelId: defaultModel.id,
                                modelName: defaultModel.name,
                                displayName: defaultModel.displayName,
                            };
                        }
                    }
                }
            }
            // When user has explicitly selected a model (activeModel is set),
            // do NOT apply auto-routing — respect the user's choice.

            if (!activeModel) {
                throw new Error('No model selected. Click ⚙️ to configure a provider.');
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

            // Check model capabilities (needed for context budget calculation)
            const modelDef = settings.models?.find((m: { id: string }) => m.id === activeModel.modelId);
            const modelContextWindow = modelDef?.maxContextTokens || modelDef?.maxTokens || 4096;

            // Phase 3: Determine budget mode and build smart context (#70, #71)
            const budgetMode = determineBudgetMode(modelContextWindow);
            const taskType = detectTaskType(userMessage.content);

            // Build RAG summary from index
            const ragSummary = ragIndexRef.current ? (() => {
                const idx = ragIndexRef.current!;
                const extSummary = Object.entries((idx.extensions || {}) as Record<string, number>)
                    .sort((a, b) => b[1] - a[1])
                    .slice(0, 8)
                    .map(([ext, count]) => `${count} .${ext}`)
                    .join(', ');
                return `- Workspace indexed: ${idx.files_count} files (${extSummary})`;
            })() : null;

            // Build smart context with priority-based allocation
            const contextTokenBudget = Math.floor(modelContextWindow * 0.15); // 15% for smart context
            const smartCtx = buildSmartContext(
                userMessage.content,
                taskType,
                projectContextRef.current,
                gitSummaryRef.current,
                agentMemory,
                fileImportsRef.current,
                ragSummary,
                contextTokenBudget,
            );
            const smartContextBlock = formatSmartContextForPrompt(smartCtx);

            // Build context-aware system prompt with Phase 3 data
            const contextBlock = buildContextBlock({
                providerType, isConnected, serverHost, serverPort, serverUser,
                remotePath, localPath, selectedFiles,
                activeFilePanel, isCloudConnection,
                editorFileName, editorFilePath,
                ragIndex: ragIndexRef.current,
                macros,
                projectContext: projectContextRef.current,
                gitBranch: gitBranchRef.current || undefined,
                gitSummary: gitSummaryRef.current || undefined,
                agentMemory,
                fileImports: fileImportsRef.current,
                smartContextBlock: smartContextBlock || undefined,
            });
            // Build extra tool definitions for system prompt (plugin + macro, not built-in)
            const extraToolDefs = toNativeDefinitions([...pluginTools, ...macrosToToolDefinitions(macros)]);
            const systemPrompt = buildSystemPrompt(settings, contextBlock, activeModel.providerType, budgetMode, activeModel.modelName, extraToolDefs);

            // Build message history (images only on the current user message)
            const currentUserMsg: Record<string, unknown> = {
                role: 'user',
                content: userMessage.content,
            };
            if (messageImages && messageImages.length > 0) {
                currentUserMsg.images = messageImages.map(img => ({
                    data: img.data,
                    media_type: img.mediaType,
                }));
            }

            // Build context-aware message window based on model's context window
            const contextBudget = Math.floor(modelContextWindow * 0.7); // 70% for context
            const systemTokens = estimateTokens(systemPrompt);
            const currentMsgTokens = estimateTokens(userMessage.content);
            const { messages: windowMessages, historyTokens } = buildMessageWindow(
                messages, systemTokens, currentMsgTokens, contextBudget, smartCtx.totalEstimatedTokens,
            );

            // Phase 3: Update token budget indicator (#71)
            const budgetData: TokenBudgetData = {
                modelMaxTokens: modelContextWindow,
                systemPromptTokens: systemTokens,
                contextTokens: smartCtx.totalEstimatedTokens,
                historyTokens: historyTokens || 0,
                currentMessageTokens: currentMsgTokens,
                responseBuffer: Math.min(2048, Math.floor(modelContextWindow * 0.15)),
            };
            setTokenBudgetData(budgetData);

            const messageHistory = [
                { role: 'system', content: systemPrompt },
                ...windowMessages,
                currentUserMsg,
            ];

            // Rate limit check
            const rateCheck = checkRateLimit(provider.id);
            if (!rateCheck.allowed) {
                throw new Error(`Rate limit reached for ${activeModel.providerName}. Try again in ${rateCheck.waitSeconds}s.`);
            }
            recordRequest(provider.id);

            // Phase 4: Budget check before sending
            const budgetResult = checkBudget(provider.id);
            setBudgetCheck(budgetResult);
            if (!budgetResult.allowed) {
                throw new Error(budgetResult.message || 'Monthly budget exceeded.');
            }

            const useNativeTools = modelDef?.supportsTools === true;
            const useStreaming = modelDef?.supportsStreaming === true;

            // Prepare model info for message signature
            const modelInfo = {
                modelName: activeModel.displayName,
                providerName: activeModel.providerName,
                providerType: activeModel.providerType,
            };

            // Build thinking budget if model supports it
            const thinkingBudget = modelDef?.supportsThinking && settings.advancedSettings?.thinkingBudget
                ? settings.advancedSettings.thinkingBudget
                : undefined;

            // Resolve task-specific parameter preset (provider + detected task type)
            const preset = getParameterPreset(activeModel.providerType, taskType);

            const aiRequest = {
                provider_type: activeModel.providerType,
                model: activeModel.modelName,
                api_key: apiKey,
                base_url: provider.baseUrl,
                messages: messageHistory,
                max_tokens: settings.advancedSettings?.maxTokens ?? preset.maxTokens,
                temperature: settings.advancedSettings?.temperature ?? preset.temperature,
                ...(() => { const rawTopP = settings.advancedSettings?.topP ?? preset.topP; return rawTopP != null ? { top_p: Math.max(0, Math.min(1, rawTopP)) } : {}; })(),
                ...(() => { const rawTopK = settings.advancedSettings?.topK ?? preset.topK; return rawTopK != null ? { top_k: Math.max(1, Math.min(500, Math.round(rawTopK))) } : {}; })(),
                ...(useNativeTools ? { tools: toNativeDefinitions(allTools) } : {}),
                ...(thinkingBudget ? { thinking_budget: thinkingBudget } : {}),
                ...(settings.advancedSettings?.webSearchEnabled ? { web_search: true } : {}),
            };

            const webSearchActive = !!settings.advancedSettings?.webSearchEnabled;

            if (useStreaming) {
                // Streaming mode: incremental rendering
                const streamId = `stream_${crypto.randomUUID()}`;
                const msgId = crypto.randomUUID();
                streamingMsgId = msgId;
                streamingMsgIdRef.current = msgId;
                let streamContent = '';
                type ToolCallEntry = { id: string; name: string; arguments: unknown };
                const streamResult: {
                    toolCalls: ToolCallEntry[] | null;
                    inputTokens: number | undefined;
                    outputTokens: number | undefined;
                    cacheCreationTokens: number | undefined;
                    cacheReadTokens: number | undefined;
                } = { toolCalls: null, inputTokens: undefined, outputTokens: undefined, cacheCreationTokens: undefined, cacheReadTokens: undefined };

                // Add placeholder message
                const streamMsg: Message = {
                    id: msgId,
                    role: 'assistant',
                    content: '',
                    timestamp: new Date(),
                    modelInfo,
                    ...(webSearchActive ? { webSearchUsed: true } : {}),
                };
                setMessages(prev => [...prev, streamMsg]);

                // Extended thinking state
                let thinkingContent = '';
                let thinkingStartTime = 0;

                // Promise to track stream completion (fix H-003: unlisten race)
                let resolveStream: () => void;
                const streamDone = new Promise<void>(resolve => { resolveStream = resolve; });

                // Listen for stream chunks (with thinking + tool calls support)
                const unlisten: UnlistenFn = await listen<{
                    content: string;
                    done: boolean;
                    tool_calls?: Array<{ id: string; name: string; arguments: unknown }>;
                    input_tokens?: number;
                    output_tokens?: number;
                    cache_creation_input_tokens?: number;
                    cache_read_input_tokens?: number;
                    thinking?: string;
                    thinking_done?: boolean;
                }>(`ai-stream-${streamId}`, (event) => {
                    const chunk = event.payload;

                    // Handle thinking blocks (Claude extended thinking)
                    if (chunk.thinking) {
                        if (thinkingStartTime === 0) thinkingStartTime = Date.now();
                        thinkingContent += chunk.thinking;
                        setMessages(prev => prev.map(m =>
                            m.id === msgId ? { ...m, thinking: thinkingContent } : m
                        ));
                    }
                    if (chunk.thinking_done) {
                        const duration = Math.round((Date.now() - thinkingStartTime) / 1000);
                        setMessages(prev => prev.map(m =>
                            m.id === msgId ? { ...m, thinking: thinkingContent, thinkingDuration: duration } : m
                        ));
                    }

                    // Handle content
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
                        if (chunk.cache_creation_input_tokens) streamResult.cacheCreationTokens = chunk.cache_creation_input_tokens;
                        if (chunk.cache_read_input_tokens) streamResult.cacheReadTokens = chunk.cache_read_input_tokens;
                        resolveStream();
                    }
                });

                // Fire the stream command (don't await its completion for unlisten)
                invoke('ai_chat_stream', { request: aiRequest, streamId }).catch((err: unknown) => {
                    const errStr = err instanceof Error ? err.message : String(err);
                    streamContent = `**Error**: ${errStr}`;
                    setMessages(prev => prev.map(m =>
                        m.id === msgId ? { ...m, content: streamContent } : m
                    ));
                    resolveStream();
                });

                // Wait for the done event with timeout, then unlisten
                const streamTimeoutMs = (settings.advancedSettings?.streamingTimeoutSecs ?? 120) * 1000;
                const timeoutPromise = new Promise<void>((_, reject) =>
                    setTimeout(() => reject(new Error(`Stream timeout after ${Math.round(streamTimeoutMs / 1000)}s`)), streamTimeoutMs)
                );
                try {
                    await Promise.race([streamDone, timeoutPromise]);
                } catch (timeoutErr) {
                    // Timeout occurred - add error message and clean up
                    streamContent += `\n\n[Stream timeout - no response received for ${Math.round(streamTimeoutMs / 1000)} seconds]`;
                    setMessages(prev => prev.map(m =>
                        m.id === msgId ? { ...m, content: streamContent } : m
                    ));
                }
                unlisten();

                // Calculate cost
                const tokenInfo = computeTokenInfo(streamResult.inputTokens, streamResult.outputTokens, undefined, modelDef, streamResult.cacheCreationTokens, streamResult.cacheReadTokens);

                // Phase 4: Record spending for cost budget tracking
                if (tokenInfo && activeModel) {
                    const cost = tokenInfo.cost || 0;
                    const tokens = (streamResult.inputTokens || 0) + (streamResult.outputTokens || 0);
                    recordSpending(activeModel.providerId, cost, tokens, activeConversationId || undefined)
                        .then(result => setBudgetCheck(result));
                }

                // Check for tool calls from streaming — process ALL (parallel support)
                let allToolsParsed: Array<{ tool: string; args: Record<string, unknown>; id: string }> = [];
                if (streamResult.toolCalls && streamResult.toolCalls.length > 0) {
                    allToolsParsed = streamResult.toolCalls.map(tc => ({
                        tool: tc.name,
                        args: (() => {
                            if (typeof tc.arguments === 'string') {
                                try { return JSON.parse(tc.arguments as string) as Record<string, unknown>; }
                                catch { return {} as Record<string, unknown>; }
                            }
                            return tc.arguments as Record<string, unknown>;
                        })(),
                        id: tc.id || crypto.randomUUID(),
                    }));
                } else {
                    const fallbacks = parseToolCalls(streamContent);
                    if (fallbacks.length > 0) allToolsParsed = fallbacks.map(fb => ({ ...fb, id: crypto.randomUUID() }));
                }

                if (allToolsParsed.length > 0) {
                    // Separate safe (auto-execute) vs approval-required tools
                    const safeCalls = allToolsParsed.filter(p => isAutoApproved(p.tool) && getToolByNameFromAll(p.tool, allTools));
                    const approvalCalls = allToolsParsed.filter(p => !isAutoApproved(p.tool) && getToolByNameFromAll(p.tool, allTools));

                    // Execute safe tools in parallel
                    if (safeCalls.length > 0) {
                        const safeToolCalls = safeCalls.map(sc => ({
                            id: sc.id,
                            toolName: sc.tool,
                            args: sc.args,
                            status: 'approved' as const,
                        }));
                        const levels = buildExecutionLevels(safeToolCalls);
                        const results = await executePipeline(levels, executeTool);
                        const combinedResult = results.filter(Boolean).join('\n---\n');
                        if (combinedResult && !autoStopRef.current && approvalCalls.length === 0) {
                            await executeMultiStep(combinedResult, aiRequest, [...messageHistory], modelInfo, modelDef);
                            return;
                        }
                    }

                    // Queue approval-required tools
                    if (approvalCalls.length > 0) {
                        setMessages(prev => prev.map(m =>
                            m.id === msgId ? { ...m, content: streamContent || '' } : m
                        ));
                        const pending = approvalCalls.map(ac => ({
                            id: ac.id,
                            toolName: ac.tool,
                            args: ac.args,
                            status: 'pending' as const,
                        }));
                        // Validate tool arguments in parallel
                        const pendingWithValidation = await Promise.all(
                            pending.map(async (tc) => {
                                const validation = await validateToolArgs(tc.toolName, tc.args);
                                return { ...tc, validation };
                            })
                        );
                        multiStepContextRef.current = { aiRequest, messageHistory: [...messageHistory], modelInfo, modelDef };
                        setPendingToolCalls(pendingWithValidation);
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
                        cache_creation_input_tokens?: number;
                        cache_read_input_tokens?: number;
                        finish_reason?: string;
                        tool_calls?: Array<{ id: string; name: string; arguments: unknown }>;
                    }>('ai_chat', { request: aiRequest })
                );

                const tokenInfo = computeTokenInfo(response.input_tokens, response.output_tokens, response.tokens_used, modelDef, response.cache_creation_input_tokens, response.cache_read_input_tokens);

                // Phase 4: Record spending for cost budget tracking (non-streaming path)
                if (tokenInfo && activeModel) {
                    const cost = tokenInfo.cost || 0;
                    const tokens = response.tokens_used || ((response.input_tokens || 0) + (response.output_tokens || 0));
                    recordSpending(activeModel.providerId, cost, tokens, activeConversationId || undefined)
                        .then(result => setBudgetCheck(result));
                }

                // Check if AI wants to use tools — process ALL (parallel support)
                let allToolsParsedNS: Array<{ tool: string; args: Record<string, unknown>; id: string }> = [];
                if (response.tool_calls && response.tool_calls.length > 0) {
                    allToolsParsedNS = response.tool_calls.map((tc: { id: string; name: string; arguments: unknown }) => ({
                        tool: tc.name,
                        args: (() => {
                            if (typeof tc.arguments === 'string') {
                                try { return JSON.parse(tc.arguments) as Record<string, unknown>; }
                                catch { return {} as Record<string, unknown>; }
                            }
                            return tc.arguments as Record<string, unknown>;
                        })(),
                        id: tc.id || crypto.randomUUID(),
                    }));
                } else {
                    const fallbacks = parseToolCalls(response.content);
                    if (fallbacks.length > 0) allToolsParsedNS = fallbacks.map(fb => ({ ...fb, id: crypto.randomUUID() }));
                }

                if (allToolsParsedNS.length > 0) {
                    const safeCalls = allToolsParsedNS.filter(p => isAutoApproved(p.tool) && getToolByNameFromAll(p.tool, allTools));
                    const approvalCalls = allToolsParsedNS.filter(p => !isAutoApproved(p.tool) && getToolByNameFromAll(p.tool, allTools));

                    if (safeCalls.length > 0) {
                        const safeToolCalls = safeCalls.map(sc => ({
                            id: sc.id, toolName: sc.tool, args: sc.args, status: 'approved' as const,
                        }));
                        const levels = buildExecutionLevels(safeToolCalls);
                        const results = await executePipeline(levels, executeTool);
                        const combinedResult = results.filter(Boolean).join('\n---\n');
                        if (combinedResult && !autoStopRef.current && approvalCalls.length === 0) {
                            await executeMultiStep(combinedResult, aiRequest, [...messageHistory], modelInfo, modelDef);
                            return;
                        }
                    }

                    if (approvalCalls.length > 0) {
                        const pendingMessage: Message = {
                            id: crypto.randomUUID(),
                            role: 'assistant',
                            content: response.content || '',
                            timestamp: new Date(),
                            modelInfo,
                        };
                        setMessages(prev => [...prev, pendingMessage]);
                        const pending = approvalCalls.map(ac => ({
                            id: ac.id, toolName: ac.tool, args: ac.args, status: 'pending' as const,
                        }));
                        // Validate tool arguments in parallel
                        const pendingWithValidation = await Promise.all(
                            pending.map(async (tc) => {
                                const validation = await validateToolArgs(tc.toolName, tc.args);
                                return { ...tc, validation };
                            })
                        );
                        multiStepContextRef.current = { aiRequest, messageHistory: [...messageHistory], modelInfo, modelDef };
                        setPendingToolCalls(pendingWithValidation);
                    }
                } else {
                    const assistantMessage: Message = {
                        id: crypto.randomUUID(),
                        role: 'assistant',
                        content: response.content,
                        timestamp: new Date(),
                        modelInfo,
                        tokenInfo,
                        ...(webSearchActive ? { webSearchUsed: true } : {}),
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
                if (errLower.includes('tool use') || errLower.includes('tool_use') || errLower.includes('function calling')) {
                    hint = 'This model does not support tool use. Disable "Tools" for this model in AI Settings → Models.';
                } else if (errLower.includes('unauthorized') || errLower.includes('auth')) {
                    hint = 'Authentication failed. Check your API key in AI Settings.';
                } else if (errLower.includes('quota') || errLower.includes('rate limit')) {
                    hint = 'Rate limited by the provider. Wait a moment and try again.';
                } else if (errLower.includes('network') || errLower.includes('fetch') || errLower.includes('timeout')) {
                    hint = 'Network error. Check your internet connection and provider URL.';
                }
            }
            const sanitized = errStr.replace(/\/[\w\/./-]+/g, '[path]').replace(/\\[\w\\.\\-]+/g, '[path]');
            const errorContent = `**Error**: ${sanitized}\n\n${hint}`;
            if (streamingMsgId) {
                // Update the existing placeholder message instead of adding a duplicate
                setMessages(prev => prev.map(m =>
                    m.id === streamingMsgId ? { ...m, content: errorContent } : m
                ));
            } else {
                const errorMessage: Message = {
                    id: crypto.randomUUID(),
                    role: 'assistant',
                    content: errorContent,
                    timestamp: new Date(),
                };
                setMessages(prev => [...prev, errorMessage]);
            }
        } finally {
            setIsLoading(false);
            streamingMsgIdRef.current = null;
        }
    };

    // Phase 4: Record cost and update conversation cost after each message send
    // This is triggered by messages array changing (after AI responds)
    useEffect(() => {
        if (!activeConversationId) return;
        const cost = getConversationCost(activeConversationId);
        if (cost) setConversationCost(cost);
    }, [messages, activeConversationId]);

    return (
        <div className={`flex flex-col h-full ${ct.bg} ${className}`}>
            <AIChatHeader
                showHistory={showHistory}
                onToggleHistory={() => setShowHistory(!showHistory)}
                onNewChat={startNewChat}
                showExportMenu={showExportMenu}
                onToggleExportMenu={() => setShowExportMenu(!showExportMenu)}
                onExport={exportConversation}
                onOpenSettings={() => setShowSettings(true)}
                hasMessages={messages.length > 0}
                appTheme={appTheme}
            />

            {/* Phase 3: Branch selector (#69) */}
            {(() => {
                const activeConv = conversations.find(c => c.id === activeConversationId);
                const branches = activeConv?.branches || [];
                if (branches.length === 0) return null;
                return (
                    <div className={`px-3 py-1 border-b ${ct.border} ${ct.bgSecondaryHalf}`}>
                        <BranchSelector
                            branches={branches.map(b => ({
                                id: b.id,
                                name: b.name,
                                messageCount: b.messages.length,
                                createdAt: b.createdAt,
                            }))}
                            activeBranchId={activeBranchId}
                            onSwitchBranch={switchBranch}
                            onDeleteBranch={deleteBranch}
                        />
                    </div>
                );
            })()}

            <div className="flex flex-1 overflow-hidden">
            {/* History Sidebar */}
            {showHistory && (
                <div className={`w-48 flex-shrink-0 border-r ${ct.sidebarBg} flex flex-col overflow-hidden`}>
                    <div className={`p-2 text-[10px] ${ct.textMuted} uppercase tracking-wider font-medium`}>
                        History ({conversations.length})
                    </div>
                    <div className="flex-1 overflow-y-auto">
                        {conversations.map(conv => (
                            <div
                                key={conv.id}
                                className={`group flex items-center gap-1.5 px-2 py-1.5 cursor-pointer text-xs transition-colors ${
                                    conv.id === activeConversationId
                                        ? ct.sidebarActive
                                        : ct.sidebarInactive
                                }`}
                                onClick={() => switchConversation(conv)}
                            >
                                <MessageSquare size={10} className="flex-shrink-0" />
                                <span className="truncate flex-1">{conv.title}</span>
                                <button
                                    onClick={(e) => { e.stopPropagation(); handleDeleteConversation(conv.id); }}
                                    className={`opacity-0 group-hover:opacity-100 p-0.5 ${ct.textMuted} hover:text-red-400 transition-all`}
                                >
                                    <Trash2 size={10} />
                                </button>
                            </div>
                        ))}
                        {conversations.length === 0 && (
                            <div className={`px-2 py-4 text-center text-[10px] ${ct.textMuted}`}>No conversations yet</div>
                        )}
                    </div>
                </div>
            )}

            {/* Phase 4: Chat Search Overlay (#78) */}
            <ChatSearchOverlay
                messages={messages.map(m => ({ id: m.id, role: m.role, content: m.content }))}
                visible={showSearch}
                onClose={() => setShowSearch(false)}
                onHighlightMessage={handleSearchHighlightMessage}
                onSearchResults={handleSearchResults}
            />

            {/* Messages Area */}
            <div ref={messageListRef} className="flex-1 overflow-y-auto" onClick={() => showExportMenu && setShowExportMenu(false)}>
                {messages.length === 0 ? (
                    /* Empty State - System Welcome */
                    <div className="h-full flex flex-col items-center justify-center text-center px-6 py-8">
                        <div className="w-12 h-12 rounded-full bg-purple-600/20 flex items-center justify-center mb-4">
                            <Sparkles size={24} className="text-purple-400" />
                        </div>
                        <h3 className={`text-lg font-medium ${ct.text} mb-2`}>{t('ai.aeroAgent')}</h3>
                        <p className={`text-sm ${ct.textSecondary} max-w-sm mb-6`}>
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
                                <div key={i} className={`flex items-center gap-2 px-3 py-2 ${ct.bgSecondaryHalf} rounded-lg ${ct.textSecondary}`}>
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
                                data-message-id={message.id}
                                className={`flex gap-3 ${message.role === 'user' ? 'justify-end' : 'justify-start'}`}
                            >
                                <div
                                    className={`max-w-[85%] rounded-lg px-4 py-2 text-sm select-text ${message.role === 'user'
                                        ? ct.userMsg
                                        : ct.assistantMsg
                                        }`}
                                >
                                    {/* Image thumbnails for vision messages */}
                                    {message.images && message.images.length > 0 && (
                                        <div className="flex gap-1.5 mb-2 flex-wrap">
                                            {message.images.map((img, i) => (
                                                <img key={i} src={img.preview} alt="" className="h-16 w-16 object-cover rounded border border-white/20" />
                                            ))}
                                        </div>
                                    )}
                                    {/* Thinking block (Claude extended thinking) — Phase 4: token display (#74) */}
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
                                            className={`select-text ${ct.prose} max-w-none ${
                                                message.role === 'assistant' && message.content.length > 500 && !expandedMessages.has(message.id)
                                                    ? 'max-h-[200px] overflow-hidden' : ''
                                            }`}
                                        >
                                            <MarkdownRenderer
                                                content={message.content}
                                                isStreaming={isLoading && message.id === streamingMsgIdRef.current}
                                                editorFilePath={editorFilePath}
                                                editorFileName={editorFileName}
                                            />
                                        </div>
                                        {message.role === 'assistant' && message.content.length > 500 && !expandedMessages.has(message.id) && (
                                            <div className={`absolute bottom-0 left-0 right-0 h-8 bg-gradient-to-t ${ct.gradient} to-transparent flex items-end justify-center`}>
                                                <button
                                                    onClick={() => setExpandedMessages(prev => new Set(prev).add(message.id))}
                                                    className="text-xs text-purple-400 hover:text-purple-300 pb-0.5"
                                                >
                                                    {t('ai.showMore') || 'Show more'} ▾
                                                </button>
                                            </div>
                                        )}
                                        {message.role === 'assistant' && message.content.length > 500 && expandedMessages.has(message.id) && (
                                            <button
                                                onClick={() => setExpandedMessages(prev => { const s = new Set(prev); s.delete(message.id); return s; })}
                                                className="text-xs text-purple-400 hover:text-purple-300 mt-1"
                                            >
                                                {t('ai.showLess') || 'Show less'} ▴
                                            </button>
                                        )}
                                    </div>
                                    <div className={`text-[10px] mt-1 flex items-center gap-2 flex-wrap ${message.role === 'user' ? ct.userMsgMeta : ct.textMuted}`}>
                                        <span>{message.timestamp.toLocaleTimeString()}</span>
                                        {message.role === 'assistant' && (
                                            <button
                                                onClick={() => {
                                                    navigator.clipboard.writeText(message.content.replace(/<[^>]*>/g, ''));
                                                    setCopiedId(message.id);
                                                    setTimeout(() => setCopiedId(null), 1500);
                                                }}
                                                className={`${ct.textMuted} ${ct.textHover} transition-colors`}
                                                title={t('ai.copy') || 'Copy'}
                                            >
                                                {copiedId === message.id ? <Check size={10} className="text-green-400" /> : <Copy size={10} />}
                                            </button>
                                        )}
                                        {message.role === 'assistant' && (
                                            <button
                                                onClick={() => forkConversation(message.id)}
                                                className={`p-0.5 ${ct.textMuted} hover:text-purple-400 transition-colors`}
                                                title={t('ai.branch.fork') || 'Fork here'}
                                            >
                                                <GitBranch size={10} />
                                            </button>
                                        )}
                                        {message.role === 'assistant' && message.modelInfo && (
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
                        ))}
                        {isLoading && !pendingToolCalls.some(tc => tc.status === 'pending') && (
                            <div className="flex gap-3">
                                <div className={`${ct.bgHalf} rounded-lg px-4 py-2 ${ct.textSecondary} text-sm flex items-center gap-2`}>
                                    <GridSpinner size={14} className="text-purple-400" />
                                    {isAutoExecuting ? (
                                        <>
                                            {extremeMode && <span className="text-red-400 font-bold animate-pulse mr-1">EXTREME</span>}
                                            <span>Step {autoStepCount}/{effectiveMaxSteps}</span>
                                            <span className="mx-1">&mdash;</span>
                                            <button
                                                onClick={() => { autoStopRef.current = true; }}
                                                className="text-red-400 hover:text-red-300 text-xs underline"
                                            >
                                                Stop
                                            </button>
                                        </>
                                    ) : (
                                        <span className="transition-opacity duration-300">{thinkingMessage}</span>
                                    )}
                                </div>
                            </div>
                        )}
                        {/* Single tool approval (backward compat) */}
                        {pendingToolCalls.length === 1 && pendingToolCalls[0].status === 'pending' && (
                            <ToolApproval
                                toolCall={pendingToolCalls[0]}
                                allTools={allTools}
                                onApprove={async () => {
                                    setIsLoading(true);
                                    const toolResult = await executeTool(pendingToolCalls[0]);
                                    setPendingToolCalls([]);
                                    if (toolResult && multiStepContextRef.current && !autoStopRef.current) {
                                        const ctx = multiStepContextRef.current;
                                        multiStepContextRef.current = null;
                                        await executeMultiStep(toolResult, ctx.aiRequest, ctx.messageHistory, ctx.modelInfo, ctx.modelDef);
                                    } else {
                                        setIsLoading(false);
                                    }
                                }}
                                onReject={() => {
                                    multiStepContextRef.current = null;
                                    setPendingToolCalls([]);
                                    const rejectedMsg: Message = {
                                        id: crypto.randomUUID(),
                                        role: 'assistant',
                                        content: 'Operation cancelled.',
                                        timestamp: new Date(),
                                    };
                                    setMessages(prev => [...prev, rejectedMsg]);
                                }}
                            />
                        )}
                        {/* Batch tool approval (parallel tool calls) */}
                        {pendingToolCalls.length > 1 && (
                            <BatchToolApproval
                                toolCalls={pendingToolCalls}
                                allTools={allTools}
                                onApproveAll={async () => {
                                    setIsLoading(true);
                                    const levels = buildExecutionLevels(pendingToolCalls);
                                    const results = await executePipeline(levels, executeTool);
                                    setPendingToolCalls([]);
                                    const combinedResult = results.filter(Boolean).join('\n---\n');
                                    if (combinedResult && multiStepContextRef.current && !autoStopRef.current) {
                                        const ctx = multiStepContextRef.current;
                                        multiStepContextRef.current = null;
                                        await executeMultiStep(combinedResult, ctx.aiRequest, ctx.messageHistory, ctx.modelInfo, ctx.modelDef);
                                    } else {
                                        setIsLoading(false);
                                    }
                                }}
                                onApproveSingle={async (id: string) => {
                                    setIsLoading(true);
                                    const tc = pendingToolCalls.find(t => t.id === id);
                                    if (!tc) { setIsLoading(false); return; }
                                    const result = await executeTool(tc);
                                    const remaining = pendingToolCalls.filter(t => t.id !== id);
                                    setPendingToolCalls(remaining);
                                    if (remaining.length === 0 && result && multiStepContextRef.current && !autoStopRef.current) {
                                        const ctx = multiStepContextRef.current;
                                        multiStepContextRef.current = null;
                                        await executeMultiStep(result, ctx.aiRequest, ctx.messageHistory, ctx.modelInfo, ctx.modelDef);
                                    } else {
                                        setIsLoading(false);
                                    }
                                }}
                                onRejectAll={() => {
                                    multiStepContextRef.current = null;
                                    setPendingToolCalls([]);
                                    const rejectedMsg: Message = {
                                        id: crypto.randomUUID(),
                                        role: 'assistant',
                                        content: 'All operations cancelled.',
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
                <div className={`relative ${ct.inputBg} border rounded-lg focus-within:border-purple-500 transition-colors`} data-aichat-input>
                    {/* Phase 4: Prompt template selector (#75) */}
                    <PromptTemplateSelector
                        input={input}
                        templates={allTemplates}
                        onSelect={handleTemplateSelect}
                        onDismiss={() => setShowTemplates(false)}
                        visible={showTemplates}
                    />
                    {/* Phase 3: Token budget indicator (#71) */}
                    <TokenBudgetIndicator budget={tokenBudgetData} compact />
                    {/* Attached Images Preview Strip */}
                    {attachedImages.length > 0 && (
                        <div className={`flex gap-2 px-3 py-2 border-b ${ct.border} overflow-x-auto`}>
                            {attachedImages.map((img, i) => (
                                <div key={i} className="relative group shrink-0">
                                    <img src={img.preview} alt="" className={`h-12 w-12 object-cover rounded border ${ct.borderSolid}`} />
                                    <button
                                        onClick={() => removeImage(i)}
                                        className="absolute -top-1.5 -right-1.5 w-4 h-4 bg-red-500 rounded-full text-white text-[10px] flex items-center justify-center opacity-0 group-hover:opacity-100 transition-opacity"
                                        title={t('ai.removeImage')}
                                    >
                                        <X size={10} />
                                    </button>
                                </div>
                            ))}
                        </div>
                    )}
                    {/* Input Row */}
                    <div className="flex gap-2 items-start px-3 py-2">
                        <textarea
                            ref={inputRef}
                            value={input}
                            onChange={(e) => {
                                const val = e.target.value;
                                setInput(val);
                                // Phase 4: Show/hide template selector on / prefix
                                setShowTemplates(val.startsWith('/'));
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
                            onPaste={handlePaste}
                            placeholder={t('ai.askPlaceholder')}
                            className={`flex-1 bg-transparent text-sm ${ct.inputText} focus:outline-none resize-none min-h-[24px] max-h-[120px]`}
                            rows={1}
                        />
                        <button
                            onClick={handleImagePick}
                            disabled={attachedImages.length >= MAX_IMAGES}
                            className={`p-1.5 rounded transition-colors ${
                                attachedImages.length >= MAX_IMAGES
                                    ? 'text-gray-600 cursor-not-allowed'
                                    : ct.btn
                            }`}
                            title={attachedImages.length >= MAX_IMAGES ? t('ai.imageLimitReached') : t('ai.attachImage')}
                        >
                            <ImageIcon size={16} />
                        </button>
                        <button
                            onClick={toggleListening}
                            className={`p-1.5 rounded transition-colors ${isListening
                                ? 'text-red-400 bg-red-500/20'
                                : ct.btn}`}
                            title={isListening ? t('ai.stopListening') : t('ai.voiceInput')}
                        >
                            {isListening ? <MicOff size={16} /> : <Mic size={16} />}
                        </button>
                        <button
                            onClick={handleSend}
                            disabled={(!input.trim() && attachedImages.length === 0) || isLoading}
                            className="p-1.5 text-purple-400 hover:text-purple-300 disabled:text-gray-600 disabled:cursor-not-allowed transition-colors"
                        >
                            <Send size={16} />
                        </button>
                    </div>

                    {/* Bottom Row - Model Selector + Disclaimer (inside the box) */}
                    <div className={`flex items-center justify-between px-3 py-2 border-t ${ct.border} text-xs`}>
                        <div className="flex items-center gap-3">
                            {/* Context Menu for adding paths */}
                            <div className="relative">
                                <button
                                    onClick={() => setShowContextMenu(!showContextMenu)}
                                    className={`${ct.textMuted} ${ct.textHover} transition-colors`}
                                    title={t('ai.addContext')}
                                >+</button>

                                {showContextMenu && (
                                    <div className={`absolute left-0 bottom-full mb-1 ${ct.dropdown} border rounded-lg z-20 py-1 min-w-[200px]`}>
                                        <div className={`px-3 py-1.5 text-[10px] ${ct.textMuted} border-b ${ct.border}`}>{t('ai.insertPath')}</div>

                                        {remotePath && (
                                            <button
                                                onClick={() => {
                                                    setInput(prev => prev + (prev ? ' ' : '') + `@remote:${remotePath}`);
                                                    setShowContextMenu(false);
                                                    inputRef.current?.focus();
                                                }}
                                                className={`w-full px-3 py-2 text-left ${ct.dropdownItem} flex items-center gap-2`}
                                            >
                                                <span className="text-green-400">🌐</span>
                                                <div className="flex flex-col">
                                                    <span className={ct.dropdownText}>{t('ai.remotePath')}</span>
                                                    <span className={`${ct.textMuted} text-[10px] truncate max-w-[160px]`}>{remotePath}</span>
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
                                                className={`w-full px-3 py-2 text-left ${ct.dropdownItem} flex items-center gap-2`}
                                            >
                                                <span className="text-blue-400">📁</span>
                                                <div className="flex flex-col">
                                                    <span className={ct.dropdownText}>{t('ai.localPath')}</span>
                                                    <span className={`${ct.textMuted} text-[10px] truncate max-w-[160px]`}>{localPath}</span>
                                                </div>
                                            </button>
                                        )}

                                        {(!remotePath && !localPath) && (
                                            <div className={`px-3 py-2 ${ct.textMuted}`}>{t('ai.noPathsAvailable')}</div>
                                        )}

                                        <div className={`border-t ${ct.border} mt-1 pt-1`}>
                                            <button
                                                onClick={() => {
                                                    const text = `Remote: ${remotePath || 'N/A'}\nLocal: ${localPath || 'N/A'}`;
                                                    navigator.clipboard.writeText(text);
                                                    setShowContextMenu(false);
                                                }}
                                                className={`w-full px-3 py-2 text-left ${ct.dropdownItem} flex items-center gap-2 ${ct.textSecondary}`}
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
                                    className={`flex items-center gap-1.5 ${ct.textSecondary} ${ct.textHover} transition-colors`}
                                >
                                    {selectedModel ? (
                                        <>
                                            {getProviderIcon(selectedModel.providerType, 12)}
                                            <span>{selectedModel.displayName}</span>
                                        </>
                                    ) : (() => {
                                        if (cachedAiSettings?.autoRouting?.enabled) {
                                            return <><span>🤖</span><span className="text-purple-300">{t('ai.auto')}</span></>;
                                        }
                                        return <span>{t('ai.selectModel')}</span>;
                                    })()}
                                    <ChevronDown size={12} />
                                </button>

                                {showModelSelector && (
                                    <div className={`absolute left-0 bottom-full mb-1 ${ct.dropdown} border rounded-lg z-10 py-1 min-w-[260px] max-h-[300px] overflow-y-auto`}>
                                        {/* Auto option when auto-routing is enabled */}
                                        {(() => {
                                            if (cachedAiSettings?.autoRouting?.enabled) {
                                                return (
                                                    <button
                                                        onClick={() => { setSelectedModel(null); setShowModelSelector(false); }}
                                                        className={`w-full px-3 py-2 text-left text-xs ${ct.dropdownItem} flex items-center gap-2.5 border-b ${ct.border} ${!selectedModel ? 'bg-purple-600/20' : ''}`}
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
                                            <div className={`px-3 py-2 text-xs ${ct.textSecondary}`}>{t('ai.noModelsConfigured')}</div>
                                        ) : (
                                            availableModels.map(model => (
                                                <button
                                                    key={model.modelId}
                                                    onClick={() => { setSelectedModel(model); setShowModelSelector(false); }}
                                                    className={`w-full px-3 py-2 text-left text-xs ${ct.dropdownItem} flex items-center gap-2.5 ${selectedModel?.modelId === model.modelId ? ct.modelSelected : ''}`}
                                                >
                                                    <span className="w-4">{getProviderIcon(model.providerType, 14)}</span>
                                                    <div className="flex flex-col flex-1">
                                                        <span className={`font-medium ${ct.dropdownText}`}>{model.displayName}</span>
                                                        <span className={`${ct.textMuted} text-[10px]`}>{model.providerName}</span>
                                                    </div>
                                                    {selectedModel?.modelId === model.modelId && <span className="text-green-400">✓</span>}
                                                </button>
                                            ))
                                        )}
                                    </div>
                                )}
                            </div>
                        </div>

                        {/* Phase 4: Cost budget indicator (#77) */}
                        <CostBudgetIndicator conversationCost={conversationCost} budgetCheck={budgetCheck} compact />
                        {/* AI Disclaimer */}
                        <span className={`text-[10px] ${ct.textMuted}`}>{t('ai.disclaimer')}</span>
                    </div>
                </div>
            </div>

            {/* AI Settings Panel */}
            <AISettingsPanel isOpen={showSettings} onClose={() => setShowSettings(false)} />
        </div>
    );
};

export default AIChat;
