import React, { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Plus, Trash2, Edit2, Check, AlertCircle,
    Zap, Server, Key, Globe, Cpu, ChevronDown, ChevronRight, Sliders, MessageSquare, Puzzle, Layers,
    Eye, EyeOff, List
} from 'lucide-react';
import type { PluginManifest } from '../../types/plugins';
import { DEFAULT_MACROS } from '../DevTools/aiChatToolMacros';
import { detectOllamaModelFamily } from '../DevTools/aiProviderProfiles';
import { OllamaGpuMonitor } from '../DevTools/OllamaGpuMonitor';
import { GeminiIcon, OpenAIIcon, AnthropicIcon, XAIIcon, OpenRouterIcon, OllamaIcon, KimiIcon, QwenIcon, DeepSeekIcon } from '../DevTools/AIIcons';
import {
    AIProvider, AIModel, AISettings, AIProviderType,
    PROVIDER_PRESETS, DEFAULT_MODELS, generateId, getDefaultAISettings
} from '../../types/ai';
import { logger } from '../../utils/logger';
import { secureGetWithFallback, secureStoreAndClean } from '../../utils/secureStorage';
import { applyRegistryDefaults, lookupModelSpec } from '../../types/aiModelRegistry';
import { useTranslation } from '../../i18n';

interface AISettingsPanelProps {
    isOpen: boolean;
    onClose: () => void;
}

// Provider type icons
const getProviderIcon = (type: AIProviderType): React.ReactNode => {
    switch (type) {
        case 'google': return <GeminiIcon size={16} />;
        case 'openai': return <OpenAIIcon size={16} />;
        case 'anthropic': return <AnthropicIcon size={16} />;
        case 'xai': return <XAIIcon size={16} />;
        case 'openrouter': return <OpenRouterIcon size={16} />;
        case 'ollama': return <OllamaIcon size={16} />;
        case 'kimi': return <KimiIcon size={16} />;
        case 'qwen': return <QwenIcon size={16} />;
        case 'deepseek': return <DeepSeekIcon size={16} />;
        case 'custom': return <Server size={14} className="text-gray-400" />;
        default: return <Server size={14} />;
    }
};

// Local storage key
const AI_SETTINGS_KEY = 'aeroftp_ai_settings';

// Model Edit Modal Component
interface ModelEditModalProps {
    model: AIModel | null;
    providerId: string;
    isNew: boolean;
    onSave: (model: AIModel) => void;
    onClose: () => void;
}

const ModelEditModal: React.FC<ModelEditModalProps> = ({ model, providerId, isNew, onSave, onClose }) => {
    const [formData, setFormData] = useState({
        name: model?.name || '',
        displayName: model?.displayName || '',
        maxTokens: model?.maxTokens || 4096,
        maxContextTokens: model?.maxContextTokens ?? 0,
        supportsStreaming: model?.supportsStreaming ?? true,
        supportsTools: model?.supportsTools ?? true,
        supportsVision: model?.supportsVision ?? false,
        supportsThinking: model?.supportsThinking ?? false,
        isEnabled: model?.isEnabled ?? true,
    });

    // Auto-fill from registry when model name changes (new models only)
    const handleNameChange = (name: string) => {
        setFormData(prev => {
            const spec = lookupModelSpec(name);
            if (!spec || !isNew) return { ...prev, name };
            return {
                ...prev,
                name,
                displayName: prev.displayName || spec.displayName,
                maxTokens: spec.maxTokens,
                maxContextTokens: spec.maxContextTokens,
                supportsStreaming: spec.supportsStreaming,
                supportsTools: spec.supportsTools,
                supportsVision: spec.supportsVision,
                supportsThinking: spec.supportsThinking,
            };
        });
    };

    const handleSave = () => {
        if (!formData.name.trim()) return;

        const baseModel = {
            ...(model || {}),  // preserve fields not in the form (toolCallQuality, bestFor, costs, etc.)
            id: model?.id || generateId(),
            providerId,
            name: formData.name.trim(),
            displayName: formData.displayName.trim() || formData.name.trim(),
            maxTokens: formData.maxTokens,
            maxContextTokens: formData.maxContextTokens || undefined,
            supportsStreaming: formData.supportsStreaming,
            supportsTools: formData.supportsTools,
            supportsVision: formData.supportsVision,
            supportsThinking: formData.supportsThinking,
            isEnabled: formData.isEnabled,
            isDefault: model?.isDefault ?? false,
        };
        onSave(isNew ? applyRegistryDefaults(baseModel) as AIModel : baseModel);
    };

    return (
        <div className="fixed inset-0 z-[60] flex items-center justify-center">
            <div className="absolute inset-0 bg-black/60" onClick={onClose} />
            <div className="relative bg-gray-800 rounded-xl shadow-2xl w-full max-w-md p-6">
                <h3 className="text-lg font-semibold mb-4 flex items-center gap-2">
                    <Cpu size={20} className="text-purple-400" />
                    {isNew ? 'Add Model' : 'Edit Model'}
                </h3>

                <div className="space-y-4">
                    {/* Model Name */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">Model Name (API)</label>
                        <input
                            type="text"
                            value={formData.name}
                            onChange={e => handleNameChange(e.target.value)}
                            placeholder="e.g., gpt-4-turbo, gemini-2.0-flash"
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                            autoFocus
                        />
                        {isNew && lookupModelSpec(formData.name) && (
                            <span className="text-xs text-green-400 mt-1 block">Known model — specs auto-filled</span>
                        )}
                    </div>

                    {/* Display Name */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">Display Name</label>
                        <input
                            type="text"
                            value={formData.displayName}
                            onChange={e => setFormData({ ...formData, displayName: e.target.value })}
                            placeholder="e.g., GPT-4 Turbo"
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                        />
                    </div>

                    {/* Max Tokens */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">Max Tokens</label>
                        <input
                            type="number"
                            value={formData.maxTokens}
                            onChange={e => setFormData({ ...formData, maxTokens: parseInt(e.target.value) || 4096 })}
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                        />
                    </div>

                    {/* Capabilities */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-2">Capabilities</label>
                        <div className="flex flex-wrap gap-3">
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsStreaming}
                                    onChange={e => setFormData({ ...formData, supportsStreaming: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>⚡ Streaming</span>
                            </label>
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsTools}
                                    onChange={e => setFormData({ ...formData, supportsTools: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>🔧 Tools</span>
                            </label>
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsVision}
                                    onChange={e => setFormData({ ...formData, supportsVision: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>👁 Vision</span>
                            </label>
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsThinking}
                                    onChange={e => setFormData({ ...formData, supportsThinking: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>💭 Thinking</span>
                            </label>
                        </div>
                    </div>

                    {/* Context Window */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">Context Window (tokens)</label>
                        <input
                            type="number"
                            value={formData.maxContextTokens || ''}
                            onChange={e => setFormData({ ...formData, maxContextTokens: parseInt(e.target.value) || 0 })}
                            placeholder="e.g., 128000"
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                        />
                    </div>

                    {/* Enabled Toggle */}
                    <label className="flex items-center gap-2 text-sm cursor-pointer">
                        <input
                            type="checkbox"
                            checked={formData.isEnabled}
                            onChange={e => setFormData({ ...formData, isEnabled: e.target.checked })}
                            className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                        />
                        <span>Enabled</span>
                    </label>
                </div>

                {/* Actions */}
                <div className="flex justify-end gap-3 mt-6">
                    <button
                        onClick={onClose}
                        className="px-4 py-2 text-sm text-gray-400 hover:text-white transition-colors"
                    >
                        Cancel
                    </button>
                    <button
                        onClick={handleSave}
                        disabled={!formData.name.trim()}
                        className="px-4 py-2 bg-purple-600 hover:bg-purple-700 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg text-sm transition-colors flex items-center gap-2"
                    >
                        <Check size={14} />
                        {isNew ? 'Add Model' : 'Save Changes'}
                    </button>
                </div>
            </div>
        </div>
    );
};

export const AISettingsPanel: React.FC<AISettingsPanelProps> = ({ isOpen, onClose }) => {
    const [settings, setSettings] = useState<AISettings>(getDefaultAISettings());
    const settingsRef = useRef(settings);
    settingsRef.current = settings;
    const [activeTab, setActiveTab] = useState<'providers' | 'models' | 'advanced' | 'prompt' | 'plugins' | 'macros'>('providers');
    const [plugins, setPlugins] = useState<PluginManifest[]>([]);
    const t = useTranslation();
    const saveTimeoutRef = useRef<ReturnType<typeof setTimeout> | null>(null);

    // Clear debounced save on unmount
    useEffect(() => {
        return () => {
            if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
        };
    }, []);

    // Load plugins
    useEffect(() => {
        invoke<PluginManifest[]>('list_plugins')
            .then(setPlugins)
            .catch(() => setPlugins([]));
    }, []);
    const [editingProvider, setEditingProvider] = useState<AIProvider | null>(null);
    const [expandedProviders, setExpandedProviders] = useState<Set<string>>(new Set());
    const [testingProvider, setTestingProvider] = useState<string | null>(null);
    const [testResults, setTestResults] = useState<Record<string, { status: 'success' | 'error'; message?: string } | null>>({});
    const [detectingModels, setDetectingModels] = useState<string | null>(null);
    const [fetchingModels, setFetchingModels] = useState<string | null>(null);
    const [availableModels, setAvailableModels] = useState<{ providerId: string; models: string[] } | null>(null);
    const [addedModels, setAddedModels] = useState<Set<string>>(new Set());
    const [showApiKey, setShowApiKey] = useState<Record<string, boolean>>({});
    const [modelFilter, setModelFilter] = useState('');

    // Ollama pull model state
    const [pullModelName, setPullModelName] = useState('');
    const [isPulling, setIsPulling] = useState(false);
    const [pullProgress, setPullProgress] = useState<{ status: string; percent: number } | null>(null);

    // Model editing state
    const [editingModel, setEditingModel] = useState<{
        model: AIModel | null;
        providerId: string;
        isNew: boolean;
    } | null>(null);

    // Load settings from localStorage (sync) + vault (async) + API keys from OS Keyring
    useEffect(() => {
        // Helper: hydrate parsed settings with API keys from OS Keyring
        const hydrateApiKeys = async (parsed: AISettings): Promise<{ settings: AISettings; migrated: boolean }> => {
            let migrated = false;
            for (const provider of parsed.providers) {
                // Convert date strings back to Date objects
                provider.createdAt = new Date(provider.createdAt);
                provider.updatedAt = new Date(provider.updatedAt);

                // Migration: if apiKey exists in localStorage copy, move it to keyring
                if (provider.apiKey) {
                    try {
                        await invoke('store_credential', { account: `ai_apikey_${provider.id}`, password: provider.apiKey });
                        migrated = true;
                    } catch (e) {
                        console.warn(`Failed to migrate API key for ${provider.name} to keyring:`, e);
                    }
                } else {
                    // Load from keyring
                    try {
                        const key = await invoke<string>('get_credential', { account: `ai_apikey_${provider.id}` });
                        provider.apiKey = key;
                    } catch {
                        // Key not in keyring yet — leave empty
                    }
                }
            }
            return { settings: parsed, migrated };
        };

        const loadSettings = async () => {
            // 1. Synchronous localStorage read for immediate UI
            const saved = localStorage.getItem(AI_SETTINGS_KEY);
            if (saved) {
                try {
                    const parsed = JSON.parse(saved) as AISettings;
                    const { settings: hydrated, migrated } = await hydrateApiKeys(parsed);
                    setSettings(hydrated);

                    // After migration, strip API keys from localStorage
                    if (migrated) {
                        const stripped = {
                            ...hydrated,
                            providers: hydrated.providers.map((p: AIProvider) => ({ ...p, apiKey: undefined })),
                        };
                        localStorage.setItem(AI_SETTINGS_KEY, JSON.stringify(stripped));
                        logger.debug('[AI Settings] Migrated API keys from localStorage to OS Keyring');
                    }
                } catch (e) {
                    console.error('Failed to parse AI settings:', e);
                }
            }

            // 2. Async vault read — overrides localStorage if vault has data
            try {
                const vaultData = await secureGetWithFallback<AISettings>('ai_settings', AI_SETTINGS_KEY);
                if (vaultData && vaultData.providers && vaultData.providers.length > 0) {
                    const { settings: vaultHydrated } = await hydrateApiKeys(vaultData);
                    setSettings(vaultHydrated);
                }
            } catch {
                // Vault unavailable, localStorage data already loaded above
            }
        };
        loadSettings();
    }, []);

    // Save settings: API keys go to OS Keyring, rest to localStorage + vault (debounced)
    const saveSettings = useCallback((newSettings: AISettings) => {
        setSettings(newSettings);

        if (saveTimeoutRef.current) clearTimeout(saveTimeoutRef.current);
        saveTimeoutRef.current = setTimeout(() => {
            // Store API keys in OS Keyring
            for (const provider of newSettings.providers) {
                if (provider.apiKey) {
                    invoke('store_credential', { account: `ai_apikey_${provider.id}`, password: provider.apiKey })
                        .catch(e => console.warn(`Failed to store API key for ${provider.name}:`, e));
                }
            }

            // Strip API keys from localStorage copy
            const stripped = {
                ...newSettings,
                providers: newSettings.providers.map(p => ({ ...p, apiKey: undefined })),
            };
            localStorage.setItem(AI_SETTINGS_KEY, JSON.stringify(stripped));

            // Persist stripped settings to encrypted vault, then clear localStorage copy
            secureStoreAndClean('ai_settings', AI_SETTINGS_KEY, stripped)
                .then(() => localStorage.removeItem(AI_SETTINGS_KEY))
                .catch(() => {});
        }, 300);
    }, []);

    // Add provider from preset
    const addProviderFromPreset = (preset: typeof PROVIDER_PRESETS[0]) => {
        const now = new Date();
        const newProvider: AIProvider = {
            id: generateId(),
            ...preset,
            apiKey: '',
            createdAt: now,
            updatedAt: now,
        };

        // Add default models for this provider
        const defaultModels = DEFAULT_MODELS[preset.type] || [];
        const newModels: AIModel[] = defaultModels.map(m => ({
            id: generateId(),
            providerId: newProvider.id,
            ...m,
        }));

        saveSettings({
            ...settings,
            providers: [...settings.providers, newProvider],
            models: [...settings.models, ...newModels],
        });

        setExpandedProviders(new Set([...expandedProviders, newProvider.id]));
    };

    // Update provider
    const updateProvider = (provider: AIProvider) => {
        saveSettings({
            ...settings,
            providers: settings.providers.map(p =>
                p.id === provider.id ? { ...provider, updatedAt: new Date() } : p
            ),
        });
        setEditingProvider(null);
    };

    // Delete provider
    const deleteProvider = (providerId: string) => {
        // Remove API key from OS Keyring
        invoke('delete_credential', { account: `ai_apikey_${providerId}` }).catch(() => {});
        saveSettings({
            ...settings,
            providers: settings.providers.filter(p => p.id !== providerId),
            models: settings.models.filter(m => m.providerId !== providerId),
        });
    };

    // Toggle provider enabled
    const toggleProviderEnabled = (providerId: string) => {
        saveSettings({
            ...settings,
            providers: settings.providers.map(p =>
                p.id === providerId ? { ...p, isEnabled: !p.isEnabled, updatedAt: new Date() } : p
            ),
        });
    };

    // Test provider connection (calls real backend API validation)
    const testProvider = async (provider: AIProvider) => {
        setTestingProvider(provider.id);
        setTestResults(prev => ({ ...prev, [provider.id]: null }));

        try {
            if (!provider.apiKey && provider.type !== 'ollama') {
                throw new Error('API key required');
            }

            const result = await invoke<boolean>('ai_test_provider', {
                providerType: provider.type,
                baseUrl: provider.baseUrl,
                apiKey: provider.apiKey || null,
            });

            if (result) {
                setTestResults(prev => ({ ...prev, [provider.id]: { status: 'success' } }));
            } else {
                throw new Error('Connection test returned false');
            }
        } catch (error: any) {
            const msg = typeof error === 'string' ? error : error?.message || 'Connection failed';
            setTestResults(prev => ({ ...prev, [provider.id]: { status: 'error', message: msg } }));
        } finally {
            setTestingProvider(null);
        }
    };

    // Detect Ollama models via Tauri IPC (ai_list_models)
    const detectOllamaModels = async (provider: AIProvider) => {
        setDetectingModels(provider.id);
        try {
            const modelNames = await invoke<string[]>('ai_list_models', {
                providerType: 'ollama',
                baseUrl: provider.baseUrl,
                apiKey: null,
            });

            if (modelNames.length === 0) {
                setTestResults(prev => ({ ...prev, [provider.id]: { status: 'error', message: 'No models found' } }));
                return;
            }

            const existingNames = settings.models
                .filter(m => m.providerId === provider.id)
                .map(m => m.name);

            const VISION_PATTERNS = /llava|bakllava|moondream|minicpm-v/i;
            const newModels = modelNames
                .filter((name: string) => !existingNames.includes(name))
                .map((name: string, index: number) => applyRegistryDefaults({
                    id: generateId(),
                    providerId: provider.id,
                    name,
                    displayName: name.split(':')[0].replace(/-/g, ' ').replace(/\b\w/g, (c: string) => c.toUpperCase()),
                    maxTokens: 4096,
                    supportsStreaming: true,
                    supportsTools: false,
                    supportsVision: VISION_PATTERNS.test(name),
                    isEnabled: true,
                    isDefault: existingNames.length === 0 && index === 0,
                }) as AIModel);

            if (newModels.length > 0) {
                saveSettings({
                    ...settings,
                    models: [...settings.models, ...newModels],
                });
            }
            setTestResults(prev => ({ ...prev, [provider.id]: { status: 'success' } }));
        } catch {
            setTestResults(prev => ({ ...prev, [provider.id]: { status: 'error', message: 'Ollama not reachable' } }));
        } finally {
            setDetectingModels(null);
        }
    };

    // Pull (download) a model from the Ollama registry
    const handlePullModel = async (provider: AIProvider) => {
        if (!pullModelName.trim() || isPulling) return;
        setIsPulling(true);
        setPullProgress({ status: 'Starting...', percent: 0 });

        const streamId = `pull_${Date.now()}`;

        // Listen for progress events from Tauri backend
        const { listen } = await import('@tauri-apps/api/event');
        const unlisten = await listen<{ status: string; total?: number; completed?: number; done: boolean }>(
            `ollama-pull-${streamId}`,
            (event) => {
                const { status, total, completed, done } = event.payload;
                if (done) {
                    setPullProgress({ status: 'Complete!', percent: 100 });
                    setTimeout(() => {
                        setIsPulling(false);
                        setPullProgress(null);
                        setPullModelName('');
                        // Refresh model list
                        detectOllamaModels(provider);
                    }, 1500);
                    unlisten();
                    return;
                }
                const percent = total && completed ? Math.round((completed / total) * 100) : 0;
                setPullProgress({ status, percent });
            }
        );

        try {
            await invoke('ollama_pull_model', {
                baseUrl: provider.baseUrl || 'http://localhost:11434',
                modelName: pullModelName.trim(),
                streamId,
            });
        } catch (error) {
            setPullProgress({ status: `Error: ${error}`, percent: 0 });
            setTimeout(() => {
                setIsPulling(false);
                setPullProgress(null);
            }, 3000);
            unlisten();
        }
    };

    // Fetch available models from provider API
    const fetchProviderModels = async (provider: AIProvider) => {
        setFetchingModels(provider.id);
        setAddedModels(new Set());
        try {
            const models = await invoke<string[]>('ai_list_models', {
                providerType: provider.type,
                baseUrl: provider.baseUrl,
                apiKey: provider.apiKey || null,
            });
            setAvailableModels({ providerId: provider.id, models });
        } catch (error: any) {
            const msg = typeof error === 'string' ? error : error?.message || 'Failed to fetch models';
            setTestResults(prev => ({ ...prev, [provider.id]: { status: 'error', message: msg } }));
            setAvailableModels(null);
        } finally {
            setFetchingModels(null);
        }
    };

    // Add a model from the available models list
    const addModelFromList = (provider: AIProvider, modelName: string) => {
        const current = settingsRef.current;
        const existingNames = current.models.filter(m => m.providerId === provider.id).map(m => m.name);
        if (existingNames.includes(modelName)) {
            setAddedModels(prev => new Set(prev).add(modelName));
            return;
        }
        const noExisting = existingNames.length === 0;
        const newModel = applyRegistryDefaults({
            id: generateId(),
            providerId: provider.id,
            name: modelName,
            displayName: modelName.split('/').pop()?.split(':')[0]?.replace(/-/g, ' ').replace(/\b\w/g, c => c.toUpperCase()) || modelName,
            maxTokens: 4096,
            supportsStreaming: true,
            supportsVision: false,
            isEnabled: true,
            isDefault: noExisting,
        }) as AIModel;
        saveSettings({
            ...current,
            models: [...current.models, newModel],
        });
        setAddedModels(prev => new Set(prev).add(modelName));
    };

    // Save model (create or update)
    const saveModel = (model: AIModel) => {
        const existingIndex = settings.models.findIndex(m => m.id === model.id);
        if (existingIndex >= 0) {
            // Update
            saveSettings({
                ...settings,
                models: settings.models.map(m => m.id === model.id ? model : m)
            });
        } else {
            // Create
            saveSettings({
                ...settings,
                models: [...settings.models, model]
            });
        }
        setEditingModel(null);
    };

    // Get models for a provider
    const getProviderModels = (providerId: string) =>
        settings.models.filter(m => m.providerId === providerId);

    // Get unused presets
    const unusedPresets = PROVIDER_PRESETS.filter(
        preset => !settings.providers.some(p => p.type === preset.type)
    );

    if (!isOpen) return null;

    return (
        <div className="fixed inset-0 z-50 flex items-center justify-center">
            <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />

            <div className="relative bg-gray-900 text-gray-100 rounded-xl shadow-2xl w-full max-w-3xl max-h-[85vh] overflow-hidden flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-6 py-4 border-b border-gray-700">
                    <div className="flex items-center gap-3">
                        <Cpu className="text-purple-400" size={24} />
                        <h2 className="text-xl font-semibold">AI Settings</h2>
                    </div>
                    <button
                        onClick={onClose}
                        className="p-2 hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        <X size={20} />
                    </button>
                </div>

                {/* Tabs */}
                <div className="flex border-b border-gray-700">
                    {[
                        { id: 'providers', label: 'Providers', icon: <Server size={14} /> },
                        { id: 'models', label: 'Models', icon: <Cpu size={14} /> },
                        { id: 'advanced', label: 'Advanced', icon: <Sliders size={14} /> },
                        { id: 'prompt', label: 'System Prompt', icon: <MessageSquare size={14} /> },
                        { id: 'plugins', label: 'Plugins', icon: <Puzzle size={14} /> },
                        { id: 'macros', label: t('ai.settings.macros') || 'Macros', icon: <Layers size={14} /> },
                    ].map(tab => (
                        <button
                            key={tab.id}
                            onClick={() => setActiveTab(tab.id as typeof activeTab)}
                            className={`flex items-center gap-2 px-6 py-3 text-sm font-medium transition-colors ${activeTab === tab.id
                                ? 'text-purple-400 border-b-2 border-purple-400 bg-gray-800/50'
                                : 'text-gray-400 hover:text-white hover:bg-gray-800/30'
                                }`}
                        >
                            {tab.icon}
                            {tab.label}
                        </button>
                    ))}
                </div>

                {/* Content */}
                <div className="flex-1 overflow-y-auto p-6">
                    {activeTab === 'providers' && (
                        <div className="space-y-4">
                            {/* Add Provider Button */}
                            {unusedPresets.length > 0 && (
                                <div className="flex flex-wrap gap-2 mb-4">
                                    <span className="text-sm text-gray-400">Add:</span>
                                    {unusedPresets.map(preset => (
                                        <button
                                            key={preset.type}
                                            onClick={() => addProviderFromPreset(preset)}
                                            className="flex items-center gap-1.5 px-3 py-1.5 bg-gray-800 hover:bg-gray-700 rounded-lg text-sm transition-colors"
                                        >
                                            <Plus size={14} />
                                            {getProviderIcon(preset.type)}
                                            {preset.name}
                                        </button>
                                    ))}
                                </div>
                            )}

                            {/* Provider List */}
                            {settings.providers.length === 0 ? (
                                <div className="text-center py-12 text-gray-500">
                                    <Server size={48} className="mx-auto mb-4 opacity-50" />
                                    <p>No providers configured</p>
                                    <p className="text-sm mt-2">Add a provider above to get started</p>
                                </div>
                            ) : (
                                <div className="space-y-3">
                                    {settings.providers.map(provider => (
                                        <div
                                            key={provider.id}
                                            className="bg-gray-800 rounded-lg border border-gray-700 overflow-hidden"
                                        >
                                            {/* Provider Header */}
                                            <div className="flex items-center gap-3 px-4 py-3">
                                                <button
                                                    onClick={() => {
                                                        const newExpanded = new Set(expandedProviders);
                                                        if (newExpanded.has(provider.id)) {
                                                            newExpanded.delete(provider.id);
                                                        } else {
                                                            newExpanded.add(provider.id);
                                                        }
                                                        setExpandedProviders(newExpanded);
                                                    }}
                                                    className="p-1 hover:bg-gray-700 rounded"
                                                >
                                                    {expandedProviders.has(provider.id)
                                                        ? <ChevronDown size={16} />
                                                        : <ChevronRight size={16} />
                                                    }
                                                </button>

                                                <div className="text-lg">{getProviderIcon(provider.type)}</div>

                                                <div className="flex-1">
                                                    <div className="font-medium">{provider.name}</div>
                                                    <div className="text-xs text-gray-500">{provider.baseUrl}</div>
                                                </div>

                                                {/* Status indicator */}
                                                <div className={`w-2 h-2 rounded-full ${provider.isEnabled && provider.apiKey
                                                    ? 'bg-green-500'
                                                    : provider.isEnabled
                                                        ? 'bg-yellow-500'
                                                        : 'bg-gray-500'
                                                    }`} title={
                                                        provider.isEnabled && provider.apiKey
                                                            ? 'Active'
                                                            : provider.isEnabled
                                                                ? 'Missing API Key'
                                                                : 'Disabled'
                                                    } />

                                                {/* Toggle */}
                                                <button
                                                    onClick={() => toggleProviderEnabled(provider.id)}
                                                    className={`px-3 py-1 text-xs rounded-full transition-colors ${provider.isEnabled
                                                        ? 'bg-green-500/20 text-green-400'
                                                        : 'bg-gray-700 text-gray-400'
                                                        }`}
                                                >
                                                    {provider.isEnabled ? 'Enabled' : 'Disabled'}
                                                </button>

                                                {/* Delete */}
                                                <button
                                                    onClick={() => deleteProvider(provider.id)}
                                                    className="p-2 text-gray-400 hover:text-red-400 hover:bg-red-500/10 rounded-lg transition-colors"
                                                    title="Delete provider"
                                                >
                                                    <Trash2 size={16} />
                                                </button>
                                            </div>

                                            {/* Expanded content */}
                                            {expandedProviders.has(provider.id) && (
                                                <div className="px-4 pb-4 border-t border-gray-700 pt-4 space-y-3">
                                                    {/* API Key */}
                                                    <div>
                                                        <label className="block text-sm text-gray-400 mb-1">
                                                            <Key size={12} className="inline mr-1" />
                                                            API Key
                                                        </label>
                                                        <div className="flex gap-2">
                                                            <div className="flex-1 relative">
                                                                <input
                                                                    type={showApiKey[provider.id] ? 'text' : 'password'}
                                                                    value={provider.apiKey || ''}
                                                                    onChange={e => updateProvider({
                                                                        ...provider,
                                                                        apiKey: e.target.value
                                                                    })}
                                                                    placeholder={provider.type === 'ollama' ? 'Not required for Ollama' : 'Enter API key...'}
                                                                    className="w-full px-3 py-2 pr-9 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                                                                />
                                                                <button
                                                                    type="button"
                                                                    onClick={() => setShowApiKey(prev => ({ ...prev, [provider.id]: !prev[provider.id] }))}
                                                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 hover:text-gray-300 transition-colors"
                                                                    title={showApiKey[provider.id] ? 'Hide' : 'Show'}
                                                                >
                                                                    {showApiKey[provider.id] ? <EyeOff size={14} /> : <Eye size={14} />}
                                                                </button>
                                                            </div>
                                                            <button
                                                                onClick={() => testProvider(provider)}
                                                                disabled={testingProvider === provider.id}
                                                                className="px-4 py-2 bg-purple-600 hover:bg-purple-700 disabled:opacity-50 rounded-lg text-sm flex items-center gap-2 transition-colors"
                                                            >
                                                                {testingProvider === provider.id ? (
                                                                    <span className="animate-spin">⏳</span>
                                                                ) : (
                                                                    <Zap size={14} />
                                                                )}
                                                                Test
                                                            </button>
                                                            <button
                                                                onClick={() => fetchProviderModels(provider)}
                                                                disabled={fetchingModels === provider.id || (!provider.apiKey && provider.type !== 'ollama')}
                                                                className="px-4 py-2 bg-indigo-600 hover:bg-indigo-700 disabled:opacity-50 rounded-lg text-sm flex items-center gap-2 transition-colors whitespace-nowrap"
                                                                title="Browse available models"
                                                            >
                                                                {fetchingModels === provider.id ? (
                                                                    <span className="animate-spin">⏳</span>
                                                                ) : (
                                                                    <List size={14} />
                                                                )}
                                                                Models
                                                            </button>
                                                            {provider.type === 'ollama' && (
                                                                <button
                                                                    onClick={() => detectOllamaModels(provider)}
                                                                    disabled={detectingModels === provider.id}
                                                                    className="px-4 py-2 bg-cyan-600 hover:bg-cyan-700 disabled:opacity-50 rounded-lg text-sm flex items-center gap-2 transition-colors whitespace-nowrap"
                                                                    title="Auto-detect available Ollama models"
                                                                >
                                                                    {detectingModels === provider.id ? (
                                                                        <span className="animate-spin">⏳</span>
                                                                    ) : (
                                                                        <span>🔍</span>
                                                                    )}
                                                                    Detect
                                                                </button>
                                                            )}
                                                        </div>
                                                        {testResults[provider.id] && (
                                                            <div className={`mt-2 text-xs flex items-center gap-1 ${testResults[provider.id]?.status === 'success'
                                                                ? 'text-green-400'
                                                                : 'text-red-400'
                                                                }`}>
                                                                {testResults[provider.id]?.status === 'success'
                                                                    ? <><Check size={12} /> Connection successful</>
                                                                    : <><AlertCircle size={12} /> Connection failed{testResults[provider.id]?.message ? `: ${testResults[provider.id]!.message}` : ''}</>
                                                                }
                                                            </div>
                                                        )}
                                                        {provider.type === 'ollama' && testResults[provider.id]?.status === 'success' && (
                                                            <div className="mt-1 text-xs text-cyan-400">
                                                                {getProviderModels(provider.id).length} model(s) available
                                                            </div>
                                                        )}

                                                        {/* Pull model section (Ollama only) */}
                                                        {provider.type === 'ollama' && (
                                                            <div className="mt-3 border-t border-gray-700/50 pt-3">
                                                                <div className="text-[10px] text-gray-500 mb-1.5">Download Model</div>
                                                                <div className="flex items-center gap-2">
                                                                    <input
                                                                        type="text"
                                                                        value={pullModelName}
                                                                        onChange={(e) => setPullModelName(e.target.value)}
                                                                        placeholder="e.g., llama3, deepseek-coder:6.7b"
                                                                        className="flex-1 px-2 py-1 bg-gray-700 border border-gray-600 rounded text-xs text-white placeholder-gray-500 focus:outline-none focus:border-cyan-500"
                                                                        disabled={isPulling}
                                                                        onKeyDown={(e) => { if (e.key === 'Enter') handlePullModel(provider); }}
                                                                    />
                                                                    <button
                                                                        onClick={() => handlePullModel(provider)}
                                                                        disabled={isPulling || !pullModelName.trim()}
                                                                        className="px-3 py-1 bg-cyan-600 hover:bg-cyan-500 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded text-xs font-medium transition-colors flex items-center gap-1"
                                                                    >
                                                                        {isPulling ? '...' : 'Pull'}
                                                                    </button>
                                                                </div>
                                                                {pullProgress && (
                                                                    <div className="mt-2">
                                                                        <div className="flex items-center justify-between text-[10px] mb-1">
                                                                            <span className="text-gray-400 truncate max-w-[200px]">{pullProgress.status}</span>
                                                                            <span className="text-cyan-400">{pullProgress.percent}%</span>
                                                                        </div>
                                                                        <div className="h-1.5 bg-gray-700 rounded-full overflow-hidden">
                                                                            <div
                                                                                className="h-full bg-cyan-500 rounded-full transition-all duration-300"
                                                                                style={{ width: `${pullProgress.percent}%` }}
                                                                            />
                                                                        </div>
                                                                    </div>
                                                                )}
                                                            </div>
                                                        )}

                                                        {/* GPU Monitor */}
                                                        {provider.type === 'ollama' && (
                                                            <div className="mt-3">
                                                                <OllamaGpuMonitor
                                                                    baseUrl={provider.baseUrl || 'http://localhost:11434'}
                                                                    visible={true}
                                                                />
                                                            </div>
                                                        )}

                                                        {/* Context Caching Info */}
                                                        {provider.type === 'google' && (
                                                            <div className="mt-3 p-2 rounded bg-gray-800/30 border border-gray-700/50">
                                                                <div className="flex items-center gap-2 text-xs text-gray-400 mb-1">
                                                                    <span className="text-cyan-400">{'\u26A1'}</span>
                                                                    <span className="font-medium text-gray-300">Context Caching</span>
                                                                </div>
                                                                <p className="text-[11px] text-gray-500">
                                                                    Large system prompts and contexts (32K+ tokens) are automatically cached for 5 minutes, reducing latency and cost by up to 75% on subsequent requests.
                                                                </p>
                                                            </div>
                                                        )}
                                                    </div>

                                                    {/* Base URL */}
                                                    <div>
                                                        <label className="block text-sm text-gray-400 mb-1">
                                                            <Globe size={12} className="inline mr-1" />
                                                            Base URL
                                                        </label>
                                                        <input
                                                            type="text"
                                                            value={provider.baseUrl}
                                                            onChange={e => updateProvider({
                                                                ...provider,
                                                                baseUrl: e.target.value
                                                            })}
                                                            placeholder="https://api.example.com/v1"
                                                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm font-mono focus:outline-none focus:ring-2 focus:ring-purple-500"
                                                        />
                                                    </div>

                                                    {/* Models for this provider */}
                                                    <div>
                                                        <label className="block text-sm text-gray-400 mb-2">
                                                            <Cpu size={12} className="inline mr-1" />
                                                            Models ({getProviderModels(provider.id).length})
                                                        </label>
                                                        <div className="flex flex-wrap gap-2">
                                                            {getProviderModels(provider.id).map(model => (
                                                                <div
                                                                    key={model.id}
                                                                    className={`px-3 py-1.5 rounded-lg text-xs flex items-center gap-2 ${model.isEnabled
                                                                        ? 'bg-gray-700 text-white'
                                                                        : 'bg-gray-800 text-gray-500'
                                                                        }`}
                                                                >
                                                                    {model.displayName}
                                                                    {model.supportsTools && <span title="Supports tools">🔧</span>}
                                                                    {model.supportsVision && <span title="Supports vision">👁</span>}
                                                                    {model.supportsThinking && <span title="Thinking">💭</span>}
                                                                </div>
                                                            ))}
                                                        </div>
                                                    </div>
                                                </div>
                                            )}
                                        </div>
                                    ))}
                                </div>
                            )}
                        </div>
                    )}

                    {activeTab === 'models' && (
                        <div className="space-y-4">
                            {settings.providers.length === 0 ? (
                                <div className="text-center py-12 text-gray-500">
                                    <Cpu size={48} className="mx-auto mb-4 opacity-50" />
                                    <p>No providers configured</p>
                                    <p className="text-sm mt-2">Add a provider first to manage models</p>
                                </div>
                            ) : (
                                <>
                                    {settings.providers.map(provider => {
                                        const providerModels = getProviderModels(provider.id);
                                        return (
                                            <div key={provider.id} className="bg-gray-800 rounded-lg border border-gray-700 overflow-hidden">
                                                <div className="px-4 py-3 bg-gray-800/50 flex items-center gap-2 border-b border-gray-700">
                                                    {getProviderIcon(provider.type)}
                                                    <span className="font-medium">{provider.name}</span>
                                                    <span className="text-xs text-gray-500">({providerModels.length} models)</span>
                                                </div>

                                                <div className="p-3 space-y-2">
                                                    {providerModels.map(model => (
                                                        <div
                                                            key={model.id}
                                                            className="flex items-center gap-3 p-2 bg-gray-900/50 rounded-lg"
                                                        >
                                                            {/* Enable/Disable Toggle */}
                                                            <button
                                                                onClick={() => {
                                                                    saveSettings({
                                                                        ...settings,
                                                                        models: settings.models.map(m =>
                                                                            m.id === model.id ? { ...m, isEnabled: !m.isEnabled } : m
                                                                        )
                                                                    });
                                                                }}
                                                                className={`w-4 h-4 rounded border-2 flex items-center justify-center transition-colors ${model.isEnabled
                                                                    ? 'bg-purple-600 border-purple-600'
                                                                    : 'border-gray-500 hover:border-gray-400'
                                                                    }`}
                                                            >
                                                                {model.isEnabled && <Check size={10} />}
                                                            </button>

                                                            {/* Model Info */}
                                                            <div className="flex-1">
                                                                <div className="font-mono text-sm text-white">{model.name}</div>
                                                                <div className="text-xs text-gray-400">{model.displayName}</div>
                                                            </div>

                                                            {/* Capabilities */}
                                                            <div className="flex gap-1.5 text-xs flex-wrap">
                                                                {model.supportsTools && (
                                                                    <span className="px-1.5 py-0.5 bg-blue-500/20 text-blue-400 rounded" title="Function Calling">🔧</span>
                                                                )}
                                                                {model.supportsVision && (
                                                                    <span className="px-1.5 py-0.5 bg-green-500/20 text-green-400 rounded" title="Vision">👁</span>
                                                                )}
                                                                {model.supportsStreaming && (
                                                                    <span className="px-1.5 py-0.5 bg-purple-500/20 text-purple-400 rounded" title="Streaming">⚡</span>
                                                                )}
                                                                {model.supportsThinking && (
                                                                    <span className="px-1.5 py-0.5 bg-amber-500/20 text-amber-400 rounded" title="Thinking/Reasoning">💭</span>
                                                                )}
                                                                {model.toolCallQuality && (
                                                                    <span className="px-1.5 py-0.5 bg-yellow-500/10 text-yellow-400 rounded font-mono" title={`Tool quality: ${model.toolCallQuality}/5`}>
                                                                        {'★'.repeat(model.toolCallQuality)}{'☆'.repeat(5 - model.toolCallQuality)}
                                                                    </span>
                                                                )}
                                                                {model.bestFor && model.bestFor.length > 0 && model.bestFor.map(tag => (
                                                                    <span key={tag} className="px-1.5 py-0.5 bg-cyan-500/10 text-cyan-400 rounded">{tag}</span>
                                                                ))}
                                                                {provider.type === 'ollama' && (() => {
                                                                    const family = detectOllamaModelFamily(model.name);
                                                                    if (!family) return null;
                                                                    return (
                                                                        <span className="text-[9px] px-1.5 py-0.5 bg-cyan-900/30 text-cyan-400 rounded border border-cyan-800/50" title={family.promptStyle}>
                                                                            {family.family}
                                                                        </span>
                                                                    );
                                                                })()}
                                                            </div>

                                                            {/* Default Toggle */}
                                                            <button
                                                                onClick={() => {
                                                                    saveSettings({
                                                                        ...settings,
                                                                        models: settings.models.map(m => ({
                                                                            ...m,
                                                                            isDefault: m.providerId === model.providerId
                                                                                ? m.id === model.id
                                                                                : m.isDefault
                                                                        }))
                                                                    });
                                                                }}
                                                                className={`px-2 py-1 text-xs rounded transition-colors ${model.isDefault
                                                                    ? 'bg-yellow-500/20 text-yellow-400'
                                                                    : 'bg-gray-700 text-gray-500 hover:text-white'
                                                                    }`}
                                                                title="Set as default for this provider"
                                                            >
                                                                {model.isDefault ? '★ Default' : 'Set Default'}
                                                            </button>

                                                            {/* Edit */}
                                                            <button
                                                                onClick={() => setEditingModel({
                                                                    model: model,
                                                                    providerId: provider.id,
                                                                    isNew: false
                                                                })}
                                                                className="p-1.5 text-gray-500 hover:text-blue-400 hover:bg-blue-500/10 rounded transition-colors"
                                                                title="Edit model"
                                                            >
                                                                <Edit2 size={14} />
                                                            </button>

                                                            {/* Delete */}
                                                            <button
                                                                onClick={() => {
                                                                    saveSettings({
                                                                        ...settings,
                                                                        models: settings.models.filter(m => m.id !== model.id)
                                                                    });
                                                                }}
                                                                className="p-1.5 text-gray-500 hover:text-red-400 hover:bg-red-500/10 rounded transition-colors"
                                                                title="Remove model"
                                                            >
                                                                <Trash2 size={14} />
                                                            </button>
                                                        </div>
                                                    ))}

                                                    {/* Add Custom Model */}
                                                    <button
                                                        onClick={() => setEditingModel({
                                                            model: null,
                                                            providerId: provider.id,
                                                            isNew: true
                                                        })}
                                                        className="w-full py-2 text-sm text-gray-400 hover:text-white hover:bg-gray-700/50 rounded-lg flex items-center justify-center gap-2 transition-colors"
                                                    >
                                                        <Plus size={14} />
                                                        Add Custom Model
                                                    </button>
                                                </div>
                                            </div>
                                        );
                                    })}
                                </>
                            )}

                            {/* Auto-Routing section (inside Models tab) */}
                            <div className="mt-6 pt-4 border-t border-gray-700/50">
                                <div className="flex items-center justify-between p-4 bg-gray-800 rounded-lg">
                                    <div>
                                        <h3 className="font-medium flex items-center gap-2"><Zap size={14} className="text-purple-400" /> Auto-Routing</h3>
                                        <p className="text-sm text-gray-400">Automatically select the best model for each task type</p>
                                    </div>
                                    <button
                                        onClick={() => saveSettings({
                                            ...settings,
                                            autoRouting: { ...settings.autoRouting, enabled: !settings.autoRouting.enabled }
                                        })}
                                        className={`px-4 py-2 rounded-lg text-sm transition-colors ${settings.autoRouting.enabled
                                            ? 'bg-purple-600 text-white'
                                            : 'bg-gray-700 text-gray-400'
                                            }`}
                                    >
                                        {settings.autoRouting.enabled ? 'Enabled' : 'Disabled'}
                                    </button>
                                </div>

                                {settings.autoRouting.enabled && (
                                    <div className="space-y-3 mt-3">
                                        <p className="text-sm text-gray-400 mb-4">
                                            Configure which model to use for each task type.
                                        </p>

                                        {[
                                            { type: 'code_generation' as const, label: 'Code Generation', desc: 'Writing new code, functions, components' },
                                            { type: 'code_review' as const, label: 'Code Review', desc: 'Reviewing, refactoring, suggesting improvements' },
                                            { type: 'quick_answer' as const, label: 'Quick Answer', desc: 'Simple questions, short responses' },
                                            { type: 'file_analysis' as const, label: 'File Analysis', desc: 'Analyzing file contents, understanding code' },
                                            { type: 'terminal_command' as const, label: 'Terminal', desc: 'Shell commands, scripts, automation' },
                                            { type: 'general' as const, label: 'General', desc: 'Default for unclassified tasks' },
                                        ].map(task => {
                                            const rule = settings.autoRouting.rules.find(r => r.taskType === task.type);
                                            const allModels = settings.providers
                                                .filter(p => p.isEnabled && p.apiKey)
                                                .flatMap(p =>
                                                    settings.models
                                                        .filter(m => m.providerId === p.id && m.isEnabled)
                                                        .map(m => ({ ...m, providerName: p.name }))
                                                );

                                            return (
                                                <div
                                                    key={task.type}
                                                    className="flex items-center gap-4 p-3 bg-gray-800 rounded-lg"
                                                >
                                                    <div className="flex-1">
                                                        <div className="font-medium text-sm">{task.label}</div>
                                                        <div className="text-xs text-gray-500">{task.desc}</div>
                                                    </div>
                                                    <select
                                                        value={rule?.preferredModelId || ''}
                                                        onChange={(e) => {
                                                            const newRules = settings.autoRouting.rules.filter(r => r.taskType !== task.type);
                                                            if (e.target.value) {
                                                                newRules.push({ taskType: task.type, preferredModelId: e.target.value });
                                                            }
                                                            saveSettings({
                                                                ...settings,
                                                                autoRouting: { ...settings.autoRouting, rules: newRules }
                                                            });
                                                        }}
                                                        className="px-3 py-1.5 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500 min-w-[200px]"
                                                    >
                                                        <option value="">Auto (Default)</option>
                                                        {allModels.map(model => (
                                                            <option key={model.id} value={model.id}>
                                                                {model.displayName} ({model.providerName})
                                                            </option>
                                                        ))}
                                                    </select>
                                                </div>
                                            );
                                        })}

                                        <div className="mt-3 p-3 bg-blue-900/20 border border-blue-600/30 rounded-lg text-xs text-blue-300">
                                            Leave as "Auto (Default)" to use the model selected in the chat header.
                                        </div>
                                    </div>
                                )}
                            </div>
                        </div>
                    )}

                    {/* Advanced Settings Tab */}
                    {activeTab === 'advanced' && (
                        <div className="space-y-6">
                            <div className="text-sm text-gray-400 mb-4">
                                Configure advanced AI parameters for response generation.
                            </div>

                            {/* Conversation Style */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-3">Conversation Style</h4>
                                <div className="flex gap-2">
                                    {(['precise', 'balanced', 'creative'] as const).map(style => (
                                        <button
                                            key={style}
                                            onClick={() => {
                                                const newSettings = {
                                                    ...settings,
                                                    advancedSettings: {
                                                        ...settings.advancedSettings,
                                                        conversationStyle: style,
                                                        temperature: style === 'precise' ? 0.3 : style === 'creative' ? 1.2 : 0.7,
                                                    }
                                                };
                                                setSettings(newSettings);
                                                saveSettings(newSettings);
                                            }}
                                            className={`flex-1 px-4 py-2 rounded-lg text-sm font-medium transition-colors ${settings.advancedSettings?.conversationStyle === style
                                                    ? 'bg-purple-600 text-white'
                                                    : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
                                                }`}
                                        >
                                            {style === 'precise' && '🎯 Precise'}
                                            {style === 'balanced' && '⚖️ Balanced'}
                                            {style === 'creative' && '🎨 Creative'}
                                        </button>
                                    ))}
                                </div>
                                <p className="text-xs text-gray-500 mt-2">
                                    Precise = factual responses. Creative = more imaginative. Balanced = default.
                                </p>
                            </div>

                            {/* Temperature */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <div className="flex items-center justify-between mb-2">
                                    <h4 className="text-sm font-medium text-white">Temperature</h4>
                                    <span className="text-sm text-purple-400 font-mono">
                                        {(settings.advancedSettings?.temperature ?? 0.7).toFixed(1)}
                                    </span>
                                </div>
                                <input
                                    type="range"
                                    min="0"
                                    max="2"
                                    step="0.1"
                                    value={settings.advancedSettings?.temperature ?? 0.7}
                                    onChange={(e) => {
                                        const newSettings = {
                                            ...settings,
                                            advancedSettings: {
                                                ...settings.advancedSettings,
                                                temperature: parseFloat(e.target.value),
                                            }
                                        };
                                        setSettings(newSettings);
                                        saveSettings(newSettings);
                                    }}
                                    className="w-full h-2 bg-gray-700 rounded-lg appearance-none cursor-pointer accent-purple-500"
                                />
                                <div className="flex justify-between text-xs text-gray-500 mt-1">
                                    <span>0.0 (Very focused)</span>
                                    <span>2.0 (Very random)</span>
                                </div>
                            </div>

                            {/* Max Tokens */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-2">Max Response Length</h4>
                                <div className="flex items-center gap-4">
                                    <input
                                        type="number"
                                        min="256"
                                        max="32768"
                                        step="256"
                                        value={settings.advancedSettings?.maxTokens ?? 4096}
                                        onChange={(e) => {
                                            const newSettings = {
                                                ...settings,
                                                advancedSettings: {
                                                    ...settings.advancedSettings,
                                                    maxTokens: parseInt(e.target.value) || 4096,
                                                }
                                            };
                                            setSettings(newSettings);
                                            saveSettings(newSettings);
                                        }}
                                        className="flex-1 px-3 py-2 bg-gray-700 border border-gray-600 rounded-lg text-sm text-white focus:outline-none focus:border-purple-500"
                                    />
                                    <span className="text-xs text-gray-500">tokens</span>
                                </div>
                                <p className="text-xs text-gray-500 mt-2">
                                    Higher values allow longer responses but use more API credits.
                                </p>
                            </div>

                            {/* Thinking Budget */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-1">Thinking Budget</h4>
                                <p className="text-[10px] text-gray-500 mb-3">
                                    Token budget for extended thinking (Claude, o3, Gemini). Higher values allow deeper reasoning.
                                </p>
                                <div className="flex items-center gap-2 flex-wrap mb-3">
                                    {[
                                        { label: 'Off', value: 0 },
                                        { label: 'Light', value: 5000 },
                                        { label: 'Balanced', value: 10000 },
                                        { label: 'Deep', value: 25000 },
                                        { label: 'Maximum', value: 100000 },
                                    ].map(preset => (
                                        <button
                                            key={preset.value}
                                            onClick={() => {
                                                const newSettings = {
                                                    ...settings,
                                                    advancedSettings: {
                                                        ...settings.advancedSettings,
                                                        thinkingBudget: preset.value,
                                                    }
                                                };
                                                setSettings(newSettings);
                                                saveSettings(newSettings);
                                            }}
                                            className={`px-2 py-1 rounded text-[10px] font-medium transition-colors ${
                                                settings.advancedSettings?.thinkingBudget === preset.value
                                                    ? 'bg-purple-600/40 text-purple-300 border border-purple-500/50'
                                                    : 'bg-gray-700 text-gray-400 hover:text-gray-200 border border-gray-600'
                                            }`}
                                        >
                                            {preset.label} ({preset.value === 0 ? '0' : `${(preset.value / 1000)}K`})
                                        </button>
                                    ))}
                                </div>
                                <input
                                    type="range"
                                    min={0}
                                    max={100000}
                                    step={1000}
                                    value={settings.advancedSettings?.thinkingBudget || 0}
                                    onChange={(e) => {
                                        const newSettings = {
                                            ...settings,
                                            advancedSettings: {
                                                ...settings.advancedSettings,
                                                thinkingBudget: parseInt(e.target.value),
                                            }
                                        };
                                        setSettings(newSettings);
                                        saveSettings(newSettings);
                                    }}
                                    className="w-full accent-purple-500"
                                />
                                <div className="flex items-center justify-between text-[10px] text-gray-500 mt-1">
                                    <span>0 (Off)</span>
                                    <span className="text-purple-400 font-mono">
                                        {((settings.advancedSettings?.thinkingBudget || 0) / 1000).toFixed(0)}K tokens
                                    </span>
                                    <span>100K</span>
                                </div>
                            </div>

                            {/* Web Search (Kimi / Qwen only) */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-1">{t('ai.webSearch.title')}</h4>
                                <p className="text-[10px] text-gray-500 mb-3">
                                    {t('ai.webSearch.description')}
                                </p>
                                <label className="flex items-center gap-2 text-sm cursor-pointer">
                                    <input
                                        type="checkbox"
                                        checked={settings.advancedSettings?.webSearchEnabled || false}
                                        onChange={(e) => {
                                            const newSettings = {
                                                ...settings,
                                                advancedSettings: {
                                                    ...settings.advancedSettings,
                                                    webSearchEnabled: e.target.checked,
                                                },
                                            };
                                            setSettings(newSettings);
                                            saveSettings(newSettings);
                                        }}
                                        className="rounded border-gray-600 bg-gray-900 text-blue-500 focus:ring-blue-500"
                                    />
                                    <span className="text-gray-300">{t('ai.webSearch.enable')}</span>
                                </label>
                                <p className="text-[10px] text-gray-500 mt-2 italic">
                                    {t('ai.webSearch.providerNote')}
                                </p>
                            </div>
                        </div>
                    )}

                    {activeTab === 'plugins' && (
                        <div className="space-y-4">
                            <div className="text-sm text-gray-400 mb-4">
                                Extend AeroAgent with custom tools. Place plugin folders in the app plugins directory.
                            </div>

                            {plugins.length === 0 ? (
                                <div className="text-center py-12 text-gray-500">
                                    <Puzzle size={48} className="mx-auto mb-4 opacity-30" />
                                    <p className="text-lg font-medium">No plugins installed</p>
                                    <p className="text-sm mt-2">
                                        Create a folder with a <code className="bg-gray-800 px-1.5 py-0.5 rounded text-xs text-cyan-400">plugin.json</code> manifest in the plugins directory.
                                    </p>
                                </div>
                            ) : (
                                <div className="space-y-3">
                                    {plugins.map(plugin => (
                                        <div key={plugin.id} className="bg-gray-800 rounded-lg p-4">
                                            <div className="flex items-center justify-between">
                                                <div className="flex items-center gap-3">
                                                    <Puzzle size={18} className="text-cyan-400" />
                                                    <div>
                                                        <h3 className="font-medium text-white">{plugin.name}</h3>
                                                        <p className="text-xs text-gray-400">
                                                            v{plugin.version} by {plugin.author} — {plugin.tools.length} tool{plugin.tools.length !== 1 ? 's' : ''}
                                                        </p>
                                                    </div>
                                                </div>
                                                <div className="flex items-center gap-2">
                                                    <span className={`text-xs px-2 py-1 rounded ${plugin.enabled !== false ? 'bg-green-900/50 text-green-400' : 'bg-gray-700 text-gray-500'}`}>
                                                        {plugin.enabled !== false ? 'Enabled' : 'Disabled'}
                                                    </span>
                                                    <button
                                                        onClick={async () => {
                                                            try {
                                                                await invoke('remove_plugin', { pluginId: plugin.id });
                                                                setPlugins(prev => prev.filter(p => p.id !== plugin.id));
                                                            } catch (e) {
                                                                logger.error('Failed to remove plugin', e);
                                                            }
                                                        }}
                                                        className="p-1.5 text-gray-500 hover:text-red-400 hover:bg-red-900/20 rounded transition-colors"
                                                        title="Remove plugin"
                                                    >
                                                        <Trash2 size={14} />
                                                    </button>
                                                </div>
                                            </div>
                                            {plugin.tools.length > 0 && (
                                                <div className="mt-3 flex flex-wrap gap-1.5">
                                                    {plugin.tools.map(tool => (
                                                        <span key={tool.name} className="inline-flex items-center gap-1 bg-gray-700/60 text-gray-300 text-xs px-2 py-0.5 rounded">
                                                            <span className="text-cyan-400">&#129513;</span>
                                                            {tool.name}
                                                            <span className={`ml-1 w-1.5 h-1.5 rounded-full ${
                                                                tool.dangerLevel === 'safe' ? 'bg-green-400' :
                                                                tool.dangerLevel === 'high' ? 'bg-red-400' : 'bg-yellow-400'
                                                            }`} />
                                                        </span>
                                                    ))}
                                                </div>
                                            )}
                                        </div>
                                    ))}
                                </div>
                            )}
                        </div>
                    )}

                    {activeTab === 'prompt' && (
                        <div className="space-y-6">
                            <div className="text-sm text-gray-400 mb-4">
                                Customize the system prompt that defines AeroAgent's behavior and personality.
                            </div>

                            {/* Toggle */}
                            <div className="flex items-center justify-between p-4 bg-gray-800 rounded-lg">
                                <div>
                                    <h3 className="font-medium">Custom System Prompt</h3>
                                    <p className="text-sm text-gray-400">Override the default AeroAgent system prompt</p>
                                </div>
                                <button
                                    onClick={() => {
                                        const newSettings = {
                                            ...settings,
                                            advancedSettings: {
                                                ...settings.advancedSettings,
                                                useCustomPrompt: !settings.advancedSettings?.useCustomPrompt,
                                            }
                                        };
                                        saveSettings(newSettings);
                                    }}
                                    className={`px-4 py-2 rounded-lg text-sm transition-colors ${settings.advancedSettings?.useCustomPrompt
                                        ? 'bg-purple-600 text-white'
                                        : 'bg-gray-700 text-gray-400'
                                        }`}
                                >
                                    {settings.advancedSettings?.useCustomPrompt ? 'Enabled' : 'Disabled'}
                                </button>
                            </div>

                            {/* Editor */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <div className="flex items-center justify-between mb-3">
                                    <h4 className="text-sm font-medium text-white">Prompt Content</h4>
                                    <div className="flex items-center gap-3">
                                        <span className="text-xs text-gray-500">
                                            ~{Math.round((settings.advancedSettings?.customSystemPrompt || '').length / 4)} tokens
                                        </span>
                                        <button
                                            onClick={() => {
                                                const newSettings = {
                                                    ...settings,
                                                    advancedSettings: {
                                                        ...settings.advancedSettings,
                                                        customSystemPrompt: '',
                                                    }
                                                };
                                                saveSettings(newSettings);
                                            }}
                                            className="px-3 py-1 text-xs bg-gray-700 hover:bg-gray-600 rounded-lg transition-colors"
                                        >
                                            Reset to Default
                                        </button>
                                    </div>
                                </div>
                                <textarea
                                    value={settings.advancedSettings?.customSystemPrompt || ''}
                                    onChange={(e) => {
                                        const newSettings = {
                                            ...settings,
                                            advancedSettings: {
                                                ...settings.advancedSettings,
                                                customSystemPrompt: e.target.value,
                                            }
                                        };
                                        saveSettings(newSettings);
                                    }}
                                    placeholder={`Enter custom instructions for AeroAgent...\n\nThe default prompt includes: identity, tone, tool definitions, protocol expertise, and behavior rules. Your custom prompt will replace all of this (tool definitions and context are always appended automatically).`}
                                    className="w-full h-64 px-4 py-3 bg-gray-900 border border-gray-600 rounded-lg text-sm text-gray-200 font-mono focus:outline-none focus:ring-2 focus:ring-purple-500 resize-y"
                                    disabled={!settings.advancedSettings?.useCustomPrompt}
                                />
                                <p className="text-xs text-gray-500 mt-2">
                                    Variables like {'{remotePath}'}, {'{localPath}'}, {'{serverHost}'} in the context block are appended automatically. Tool definitions are always included regardless of custom prompt.
                                </p>
                            </div>
                        </div>
                    )}

                    {activeTab === 'macros' && (
                        <div className="space-y-4">
                            <div className="text-sm text-gray-400 mb-4">
                                {t('ai.settings.macrosDescription') || 'Macros are reusable multi-step tool sequences. They appear as tools in AeroAgent\'s available actions.'}
                            </div>

                            {DEFAULT_MACROS.map(macro => (
                                <div key={macro.id} className="bg-gray-800 rounded-lg p-4">
                                    <div className="flex items-center justify-between mb-2">
                                        <div className="flex items-center gap-2">
                                            <Layers size={16} className="text-purple-400" />
                                            <h3 className="font-medium text-white">{macro.displayName}</h3>
                                            <span className="text-xs bg-gray-700 px-2 py-0.5 rounded text-gray-400 font-mono">
                                                macro_{macro.name}
                                            </span>
                                        </div>
                                    </div>
                                    <p className="text-sm text-gray-400 mb-3">{macro.description}</p>

                                    <div className="space-y-1.5">
                                        <span className="text-xs text-gray-500 font-medium">{t('ai.settings.macroSteps') || 'Steps'}:</span>
                                        {macro.steps.map((step, idx) => (
                                            <div key={idx} className="flex items-center gap-2 text-xs">
                                                <span className="w-5 h-5 rounded-full bg-gray-700 flex items-center justify-center text-gray-400 shrink-0">
                                                    {idx + 1}
                                                </span>
                                                <span className="text-cyan-400 font-mono">{step.toolName}</span>
                                                <span className="text-gray-500">
                                                    {Object.entries(step.args).map(([k, v]) => `${k}=${v}`).join(', ')}
                                                </span>
                                            </div>
                                        ))}
                                    </div>

                                    {macro.parameters.length > 0 && (
                                        <div className="mt-3 pt-3 border-t border-gray-700">
                                            <span className="text-xs text-gray-500 font-medium">{t('ai.settings.macroParameters') || 'Parameters'}:</span>
                                            <div className="flex flex-wrap gap-1.5 mt-1">
                                                {macro.parameters.map(param => (
                                                    <span key={param.name} className="text-xs bg-gray-700/60 text-gray-300 px-2 py-0.5 rounded">
                                                        {param.name}{param.required ? '*' : ''}
                                                    </span>
                                                ))}
                                            </div>
                                        </div>
                                    )}
                                </div>
                            ))}

                            <div className="text-xs text-gray-600 mt-4 p-3 bg-gray-800/50 rounded-lg">
                                {t('ai.settings.macrosCustomNote') || 'Custom macro creation will be available in a future update. Currently showing built-in macros.'}
                            </div>
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="px-6 py-4 border-t border-gray-700 flex justify-between items-center">
                    <div className="text-sm text-gray-500">
                        {settings.providers.filter(p => p.isEnabled).length} provider(s) enabled
                    </div>
                    <button
                        onClick={onClose}
                        className="px-4 py-2 bg-purple-600 hover:bg-purple-700 rounded-lg text-sm transition-colors"
                    >
                        Done
                    </button>
                </div>
            </div>

            {/* Model Edit Modal */}
            {editingModel && (
                <ModelEditModal
                    model={editingModel.model}
                    providerId={editingModel.providerId}
                    isNew={editingModel.isNew}
                    onSave={saveModel}
                    onClose={() => setEditingModel(null)}
                />
            )}

            {/* Available Models Modal */}
            {availableModels && (() => {
                const provider = settings.providers.find(p => p.id === availableModels.providerId);
                const existingNames = new Set(settings.models.filter(m => m.providerId === availableModels.providerId).map(m => m.name));
                const filtered = availableModels.models.filter(m => !modelFilter || m.toLowerCase().includes(modelFilter.toLowerCase()));

                return (
                    <div className="fixed inset-0 bg-black/60 z-[60] flex items-center justify-center" onClick={() => { setAvailableModels(null); setModelFilter(''); }}>
                        <div className="bg-gray-800 rounded-xl border border-gray-600 w-full max-w-lg max-h-[70vh] flex flex-col" onClick={e => e.stopPropagation()}>
                            <div className="flex items-center justify-between px-4 py-3 border-b border-gray-700">
                                <div className="flex items-center gap-2">
                                    <List size={16} className="text-indigo-400" />
                                    <span className="font-medium text-sm">Available Models</span>
                                    {provider && <span className="text-xs text-gray-500">({provider.type})</span>}
                                </div>
                                <button onClick={() => { setAvailableModels(null); setModelFilter(''); }} className="p-1 hover:bg-gray-700 rounded transition-colors">
                                    <X size={16} />
                                </button>
                            </div>
                            <div className="px-4 py-2 border-b border-gray-700/50">
                                <input
                                    type="text"
                                    value={modelFilter}
                                    onChange={e => setModelFilter(e.target.value)}
                                    placeholder="Filter models..."
                                    className="w-full px-3 py-1.5 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500"
                                    autoFocus
                                />
                            </div>
                            <div className="flex-1 overflow-auto px-2 py-1">
                                {filtered.length === 0 ? (
                                    <div className="text-center py-8 text-gray-500 text-sm">No models found</div>
                                ) : (
                                    filtered.map(modelName => {
                                        const alreadyAdded = existingNames.has(modelName) || addedModels.has(modelName);
                                        return (
                                            <div key={modelName} className="flex items-center justify-between px-3 py-1.5 hover:bg-gray-700/40 rounded-lg group">
                                                <span className="text-sm font-mono text-gray-200 truncate mr-2">{modelName}</span>
                                                {alreadyAdded ? (
                                                    <span className="text-xs text-green-400 flex items-center gap-1 shrink-0">
                                                        <Check size={12} /> Added
                                                    </span>
                                                ) : (
                                                    <button
                                                        onClick={() => provider && addModelFromList(provider, modelName)}
                                                        className="text-xs px-2 py-0.5 bg-indigo-600 hover:bg-indigo-500 rounded text-white transition-colors opacity-0 group-hover:opacity-100 shrink-0"
                                                    >
                                                        Add
                                                    </button>
                                                )}
                                            </div>
                                        );
                                    })
                                )}
                            </div>
                            <div className="px-4 py-2 border-t border-gray-700/50 text-xs text-gray-500">
                                {availableModels.models.length} model(s) available
                            </div>
                        </div>
                    </div>
                );
            })()}
        </div>
    );
};

export default AISettingsPanel;
