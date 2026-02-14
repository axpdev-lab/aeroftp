import React, { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { TransferProgressBar } from '../TransferProgressBar';
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
    const t = useTranslation();
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
                    {isNew ? t('ai.settings.addModel') : t('ai.settings.editModel')}
                </h3>

                <div className="space-y-4">
                    {/* Model Name */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">{t('ai.settings.modelName')}</label>
                        <input
                            type="text"
                            value={formData.name}
                            onChange={e => handleNameChange(e.target.value)}
                            placeholder={t('ai.settings.modelNamePlaceholder')}
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                            autoFocus
                        />
                        {isNew && lookupModelSpec(formData.name) && (
                            <span className="text-xs text-green-400 mt-1 block">{t('ai.settings.knownModelHint')}</span>
                        )}
                    </div>

                    {/* Display Name */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">{t('ai.settings.displayName')}</label>
                        <input
                            type="text"
                            value={formData.displayName}
                            onChange={e => setFormData({ ...formData, displayName: e.target.value })}
                            placeholder={t('ai.settings.displayNamePlaceholder')}
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                        />
                    </div>

                    {/* Max Tokens */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">{t('ai.settings.maxTokens')}</label>
                        <input
                            type="number"
                            value={formData.maxTokens}
                            onChange={e => setFormData({ ...formData, maxTokens: parseInt(e.target.value) || 4096 })}
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                        />
                    </div>

                    {/* Capabilities */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-2">{t('ai.settings.capabilities')}</label>
                        <div className="flex flex-wrap gap-3">
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsStreaming}
                                    onChange={e => setFormData({ ...formData, supportsStreaming: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>⚡ {t('ai.settings.streaming')}</span>
                            </label>
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsTools}
                                    onChange={e => setFormData({ ...formData, supportsTools: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>🔧 {t('ai.settings.tools')}</span>
                            </label>
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsVision}
                                    onChange={e => setFormData({ ...formData, supportsVision: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>👁 {t('ai.settings.vision')}</span>
                            </label>
                            <label className="flex items-center gap-2 text-sm cursor-pointer">
                                <input
                                    type="checkbox"
                                    checked={formData.supportsThinking}
                                    onChange={e => setFormData({ ...formData, supportsThinking: e.target.checked })}
                                    className="rounded border-gray-600 bg-gray-900 text-purple-500 focus:ring-purple-500"
                                />
                                <span>💭 {t('ai.settings.thinking')}</span>
                            </label>
                        </div>
                    </div>

                    {/* Context Window */}
                    <div>
                        <label className="block text-sm text-gray-400 mb-1">{t('ai.settings.contextWindow')}</label>
                        <input
                            type="number"
                            value={formData.maxContextTokens || ''}
                            onChange={e => setFormData({ ...formData, maxContextTokens: parseInt(e.target.value) || 0 })}
                            placeholder={t('ai.settings.contextWindowPlaceholder')}
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
                        <span>{t('ai.settings.enabled')}</span>
                    </label>
                </div>

                {/* Actions */}
                <div className="flex justify-end gap-3 mt-6">
                    <button
                        onClick={onClose}
                        className="px-4 py-2 text-sm text-gray-400 hover:text-white transition-colors"
                    >
                        {t('ai.settings.cancel')}
                    </button>
                    <button
                        onClick={handleSave}
                        disabled={!formData.name.trim()}
                        className="px-4 py-2 bg-purple-600 hover:bg-purple-700 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg text-sm transition-colors flex items-center gap-2"
                    >
                        <Check size={14} />
                        {isNew ? t('ai.settings.addModel') : t('ai.settings.saveChanges')}
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
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh]">
            <div className="absolute inset-0 bg-black/50 backdrop-blur-sm" onClick={onClose} />

            <div className="relative bg-gray-900 text-gray-100 rounded-xl shadow-2xl w-full max-w-3xl max-h-[85vh] overflow-hidden flex flex-col">
                {/* Header */}
                <div className="flex items-center justify-between px-6 py-4 border-b border-gray-700">
                    <div className="flex items-center gap-3">
                        <Cpu className="text-purple-400" size={24} />
                        <h2 className="text-xl font-semibold">{t('ai.settings.title')}</h2>
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
                        { id: 'providers', label: t('ai.settings.providers'), icon: <Server size={14} /> },
                        { id: 'models', label: t('ai.settings.models'), icon: <Cpu size={14} /> },
                        { id: 'advanced', label: t('ai.settings.advanced'), icon: <Sliders size={14} /> },
                        { id: 'prompt', label: t('ai.settings.prompt'), icon: <MessageSquare size={14} /> },
                        { id: 'plugins', label: t('ai.settings.plugins'), icon: <Puzzle size={14} /> },
                        { id: 'macros', label: t('ai.settings.macros'), icon: <Layers size={14} /> },
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
                                    <p>{t('ai.settings.noProvidersConfigured')}</p>
                                    <p className="text-sm mt-2">{t('ai.settings.addProviderHint')}</p>
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
                                                            ? t('ai.settings.active')
                                                            : provider.isEnabled
                                                                ? t('ai.settings.missingApiKey')
                                                                : t('ai.settings.disabled')
                                                    } />

                                                {/* Toggle */}
                                                <button
                                                    onClick={() => toggleProviderEnabled(provider.id)}
                                                    className={`px-3 py-1 text-xs rounded-full transition-colors ${provider.isEnabled
                                                        ? 'bg-green-500/20 text-green-400'
                                                        : 'bg-gray-700 text-gray-400'
                                                        }`}
                                                >
                                                    {provider.isEnabled ? t('ai.settings.enabled') : t('ai.settings.disabled')}
                                                </button>

                                                {/* Delete */}
                                                <button
                                                    onClick={() => deleteProvider(provider.id)}
                                                    className="p-2 text-gray-400 hover:text-red-400 hover:bg-red-500/10 rounded-lg transition-colors"
                                                    title={t('ai.settings.deleteProvider')}
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
                                                            {t('ai.settings.apiKey')}
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
                                                                    placeholder={provider.type === 'ollama' ? t('ai.settings.notRequiredOllama') : t('ai.settings.enterApiKey')}
                                                                    className="w-full px-3 py-2 pr-9 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                                                                />
                                                                <button
                                                                    type="button"
                                                                    onClick={() => setShowApiKey(prev => ({ ...prev, [provider.id]: !prev[provider.id] }))}
                                                                    className="absolute right-2 top-1/2 -translate-y-1/2 text-gray-500 hover:text-gray-300 transition-colors"
                                                                    title={showApiKey[provider.id] ? t('ai.settings.hideKey') : t('ai.settings.showKey')}
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
                                                                {t('ai.settings.test')}
                                                            </button>
                                                            <button
                                                                onClick={() => fetchProviderModels(provider)}
                                                                disabled={fetchingModels === provider.id || (!provider.apiKey && provider.type !== 'ollama')}
                                                                className="px-4 py-2 bg-indigo-600 hover:bg-indigo-700 disabled:opacity-50 rounded-lg text-sm flex items-center gap-2 transition-colors whitespace-nowrap"
                                                                title={t('ai.settings.browseModels')}
                                                            >
                                                                {fetchingModels === provider.id ? (
                                                                    <span className="animate-spin">⏳</span>
                                                                ) : (
                                                                    <List size={14} />
                                                                )}
                                                                {t('ai.settings.fetchModels')}
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
                                                                    {t('ai.settings.detectModels')}
                                                                </button>
                                                            )}
                                                        </div>
                                                        {testResults[provider.id] && (
                                                            <div className={`mt-2 text-xs flex items-center gap-1 ${testResults[provider.id]?.status === 'success'
                                                                ? 'text-green-400'
                                                                : 'text-red-400'
                                                                }`}>
                                                                {testResults[provider.id]?.status === 'success'
                                                                    ? <><Check size={12} /> {t('ai.settings.testSuccess')}</>
                                                                    : <><AlertCircle size={12} /> {t('ai.settings.testFailed')}{testResults[provider.id]?.message ? `: ${testResults[provider.id]!.message}` : ''}</>
                                                                }
                                                            </div>
                                                        )}
                                                        {provider.type === 'ollama' && testResults[provider.id]?.status === 'success' && (
                                                            <div className="mt-1 text-xs text-cyan-400">
                                                                {t('ai.settings.modelsAvailable', { count: getProviderModels(provider.id).length })}
                                                            </div>
                                                        )}

                                                        {/* Pull model section (Ollama only) */}
                                                        {provider.type === 'ollama' && (
                                                            <div className="mt-3 border-t border-gray-700/50 pt-3">
                                                                <div className="text-[10px] text-gray-500 mb-1.5">{t('ai.settings.downloadModel')}</div>
                                                                <div className="flex items-center gap-2">
                                                                    <input
                                                                        type="text"
                                                                        value={pullModelName}
                                                                        onChange={(e) => setPullModelName(e.target.value)}
                                                                        placeholder={t('ai.settings.pullModelPlaceholder')}
                                                                        className="flex-1 px-2 py-1 bg-gray-700 border border-gray-600 rounded text-xs text-white placeholder-gray-500 focus:outline-none focus:border-cyan-500"
                                                                        disabled={isPulling}
                                                                        onKeyDown={(e) => { if (e.key === 'Enter') handlePullModel(provider); }}
                                                                    />
                                                                    <button
                                                                        onClick={() => handlePullModel(provider)}
                                                                        disabled={isPulling || !pullModelName.trim()}
                                                                        className="px-3 py-1 bg-cyan-600 hover:bg-cyan-500 disabled:bg-gray-700 disabled:text-gray-500 text-white rounded text-xs font-medium transition-colors flex items-center gap-1"
                                                                    >
                                                                        {isPulling ? '...' : t('ai.settings.pullModel')}
                                                                    </button>
                                                                </div>
                                                                {pullProgress && (
                                                                    <div className="mt-2">
                                                                        <TransferProgressBar
                                                                            percentage={pullProgress.percent}
                                                                            filename={pullProgress.status}
                                                                            size="sm"
                                                                            variant="gradient"
                                                                        />
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
                                                                    <span className="font-medium text-gray-300">{t('ai.settings.contextCaching')}</span>
                                                                </div>
                                                                <p className="text-[11px] text-gray-500">
                                                                    {t('ai.settings.contextCachingDesc')}
                                                                </p>
                                                            </div>
                                                        )}
                                                    </div>

                                                    {/* Base URL */}
                                                    <div>
                                                        <label className="block text-sm text-gray-400 mb-1">
                                                            <Globe size={12} className="inline mr-1" />
                                                            {t('ai.settings.baseUrl')}
                                                        </label>
                                                        <input
                                                            type="text"
                                                            value={provider.baseUrl}
                                                            onChange={e => updateProvider({
                                                                ...provider,
                                                                baseUrl: e.target.value
                                                            })}
                                                            placeholder={t('ai.settings.baseUrlPlaceholder')}
                                                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm font-mono focus:outline-none focus:ring-2 focus:ring-purple-500"
                                                        />
                                                    </div>

                                                    {/* Models for this provider */}
                                                    <div>
                                                        <label className="block text-sm text-gray-400 mb-2">
                                                            <Cpu size={12} className="inline mr-1" />
                                                            {t('ai.settings.modelsCount', { count: getProviderModels(provider.id).length })}
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
                                    <p>{t('ai.settings.noProvidersForModels')}</p>
                                    <p className="text-sm mt-2">{t('ai.settings.addProviderFirst')}</p>
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
                                                    <span className="text-xs text-gray-500">({t('ai.settings.modelsCountLabel', { count: providerModels.length })})</span>
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
                                                                title={t('ai.settings.setDefault')}
                                                            >
                                                                {model.isDefault ? '★ ' + t('ai.settings.defaultBadge') : t('ai.settings.setDefault')}
                                                            </button>

                                                            {/* Edit */}
                                                            <button
                                                                onClick={() => setEditingModel({
                                                                    model: model,
                                                                    providerId: provider.id,
                                                                    isNew: false
                                                                })}
                                                                className="p-1.5 text-gray-500 hover:text-blue-400 hover:bg-blue-500/10 rounded transition-colors"
                                                                title={t('ai.settings.editModelTooltip')}
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
                                                                title={t('ai.settings.removeModel')}
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
                                                        {t('ai.settings.addCustomModel')}
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
                                        <h3 className="font-medium flex items-center gap-2"><Zap size={14} className="text-purple-400" /> {t('ai.settings.autoRouting')}</h3>
                                        <p className="text-sm text-gray-400">{t('ai.settings.autoRoutingDescription')}</p>
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
                                        {settings.autoRouting.enabled ? t('ai.settings.enabled') : t('ai.settings.disabled')}
                                    </button>
                                </div>

                                {settings.autoRouting.enabled && (
                                    <div className="space-y-3 mt-3">
                                        <p className="text-sm text-gray-400 mb-4">
                                            {t('ai.settings.autoRoutingInstruction')}
                                        </p>

                                        {[
                                            { type: 'code_generation' as const, label: t('ai.settings.codeGeneration'), desc: t('ai.settings.codeGenerationDesc') },
                                            { type: 'code_review' as const, label: t('ai.settings.codeReview'), desc: t('ai.settings.codeReviewDesc') },
                                            { type: 'quick_answer' as const, label: t('ai.settings.quickAnswer'), desc: t('ai.settings.quickAnswerDesc') },
                                            { type: 'file_analysis' as const, label: t('ai.settings.fileAnalysis'), desc: t('ai.settings.fileAnalysisDesc') },
                                            { type: 'terminal_command' as const, label: t('ai.settings.terminalCommands'), desc: t('ai.settings.terminalCommandsDesc') },
                                            { type: 'general' as const, label: t('ai.settings.general'), desc: t('ai.settings.generalDesc') },
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
                                                        <option value="">{t('ai.settings.autoDefault')}</option>
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
                                            {t('ai.settings.autoRoutingTip')}
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
                                {t('ai.settings.advancedDescription')}
                            </div>

                            {/* Conversation Style */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-3">{t('ai.settings.conversationStyle')}</h4>
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
                                            {style === 'precise' && ('🎯 ' + t('ai.settings.precise'))}
                                            {style === 'balanced' && ('⚖️ ' + t('ai.settings.balanced'))}
                                            {style === 'creative' && ('🎨 ' + t('ai.settings.creative'))}
                                        </button>
                                    ))}
                                </div>
                                <p className="text-xs text-gray-500 mt-2">
                                    {t('ai.settings.styleHelp')}
                                </p>
                            </div>

                            {/* Temperature */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <div className="flex items-center justify-between mb-2">
                                    <h4 className="text-sm font-medium text-white">{t('ai.settings.temperature')}</h4>
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
                                    <span>{t('ai.settings.tempLow')}</span>
                                    <span>{t('ai.settings.tempHigh')}</span>
                                </div>
                            </div>

                            {/* Max Tokens */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-2">{t('ai.settings.maxResponseLength')}</h4>
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
                                    <span className="text-xs text-gray-500">{t('ai.settings.tokens')}</span>
                                </div>
                                <p className="text-xs text-gray-500 mt-2">
                                    {t('ai.settings.maxResponseHelp')}
                                </p>
                            </div>

                            {/* Thinking Budget */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-1">{t('ai.settings.thinkingBudget')}</h4>
                                <p className="text-[10px] text-gray-500 mb-3">
                                    {t('ai.settings.thinkingBudgetDesc')}
                                </p>
                                <div className="flex items-center gap-2 flex-wrap mb-3">
                                    {[
                                        { label: t('ai.settings.thinkingOff'), value: 0 },
                                        { label: t('ai.settings.thinkingLight'), value: 5000 },
                                        { label: t('ai.settings.balanced'), value: 10000 },
                                        { label: t('ai.settings.thinkingDeep'), value: 25000 },
                                        { label: t('ai.settings.thinkingMaximum'), value: 100000 },
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
                                    <span>0 ({t('ai.settings.thinkingOff')})</span>
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

                            {/* Streaming Timeout */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <h4 className="text-sm font-medium text-white mb-1">{t('ai.streamingTimeout.title')}</h4>
                                <p className="text-[10px] text-gray-500 mb-3">
                                    {t('ai.streamingTimeout.description')}
                                </p>
                                <div className="flex items-center gap-3">
                                    <input
                                        type="number"
                                        min={30}
                                        max={600}
                                        step={10}
                                        value={settings.advancedSettings?.streamingTimeoutSecs ?? 120}
                                        onChange={(e) => {
                                            const val = Math.max(30, Math.min(600, parseInt(e.target.value) || 120));
                                            const newSettings = {
                                                ...settings,
                                                advancedSettings: {
                                                    ...settings.advancedSettings,
                                                    streamingTimeoutSecs: val,
                                                },
                                            };
                                            setSettings(newSettings);
                                            saveSettings(newSettings);
                                        }}
                                        className="w-24 px-3 py-1.5 bg-gray-900 border border-gray-600 rounded text-sm text-white focus:border-purple-500 focus:outline-none"
                                    />
                                    <span className="text-sm text-gray-400">{t('ai.streamingTimeout.seconds')}</span>
                                </div>
                                <p className="text-[10px] text-gray-500 mt-2 italic">
                                    {t('ai.streamingTimeout.hint')}
                                </p>
                            </div>
                        </div>
                    )}

                    {activeTab === 'plugins' && (
                        <div className="space-y-4">
                            <div className="text-sm text-gray-400 mb-4">
                                {t('ai.settings.pluginsDesc')}
                            </div>

                            {plugins.length === 0 ? (
                                <div className="text-center py-12 text-gray-500">
                                    <Puzzle size={48} className="mx-auto mb-4 opacity-30" />
                                    <p className="text-lg font-medium">{t('ai.settings.noPlugins')}</p>
                                    <p className="text-sm mt-2">
                                        {t('ai.settings.pluginCreateHint')}
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
                                                            {'v' + plugin.version} {t('ai.settings.pluginBy', { author: plugin.author })} — {t('ai.settings.pluginTools', { count: plugin.tools.length })}
                                                        </p>
                                                    </div>
                                                </div>
                                                <div className="flex items-center gap-2">
                                                    <span className={`text-xs px-2 py-1 rounded ${plugin.enabled !== false ? 'bg-green-900/50 text-green-400' : 'bg-gray-700 text-gray-500'}`}>
                                                        {plugin.enabled !== false ? t('ai.settings.pluginEnabled') : t('ai.settings.pluginDisabled')}
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
                                                        title={t('ai.settings.removePlugin')}
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
                                {t('ai.settings.promptDesc')}
                            </div>

                            {/* Toggle */}
                            <div className="flex items-center justify-between p-4 bg-gray-800 rounded-lg">
                                <div>
                                    <h3 className="font-medium">{t('ai.settings.customSystemPrompt')}</h3>
                                    <p className="text-sm text-gray-400">{t('ai.settings.overrideSystemPrompt')}</p>
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
                                    {settings.advancedSettings?.useCustomPrompt ? t('ai.settings.enabled') : t('ai.settings.disabled')}
                                </button>
                            </div>

                            {/* Editor */}
                            <div className="bg-gray-800/50 rounded-lg p-4">
                                <div className="flex items-center justify-between mb-3">
                                    <h4 className="text-sm font-medium text-white">{t('ai.settings.promptContent')}</h4>
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
                                            {t('ai.settings.resetToDefault')}
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
                                    placeholder={t('ai.settings.promptPlaceholder')}
                                    className="w-full h-64 px-4 py-3 bg-gray-900 border border-gray-600 rounded-lg text-sm text-gray-200 font-mono focus:outline-none focus:ring-2 focus:ring-purple-500 resize-y"
                                    disabled={!settings.advancedSettings?.useCustomPrompt}
                                />
                                <p className="text-xs text-gray-500 mt-2">
                                    {t('ai.settings.promptHint')}
                                </p>
                            </div>
                        </div>
                    )}

                    {activeTab === 'macros' && (
                        <div className="space-y-4">
                            <div className="text-sm text-gray-400 mb-4">
                                {t('ai.settings.macrosDescription')}
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
                                        <span className="text-xs text-gray-500 font-medium">{t('ai.settings.macroSteps')}:</span>
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
                                            <span className="text-xs text-gray-500 font-medium">{t('ai.settings.macroParameters')}:</span>
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
                                {t('ai.settings.macrosCustomNote')}
                            </div>
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className="px-6 py-4 border-t border-gray-700 flex justify-between items-center">
                    <div className="text-sm text-gray-500">
                        {t('ai.settings.providersEnabled', { count: settings.providers.filter(p => p.isEnabled).length })}
                    </div>
                    <button
                        onClick={onClose}
                        className="px-4 py-2 bg-purple-600 hover:bg-purple-700 rounded-lg text-sm transition-colors"
                    >
                        {t('ai.settings.done')}
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
                                    <span className="font-medium text-sm">{t('ai.settings.fetchModelsTitle')}</span>
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
                                    placeholder={t('ai.settings.fetchModelsFilter')}
                                    className="w-full px-3 py-1.5 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500"
                                    autoFocus
                                />
                            </div>
                            <div className="flex-1 overflow-auto px-2 py-1">
                                {filtered.length === 0 ? (
                                    <div className="text-center py-8 text-gray-500 text-sm">{t('ai.settings.fetchModelsEmpty')}</div>
                                ) : (
                                    filtered.map(modelName => {
                                        const alreadyAdded = existingNames.has(modelName) || addedModels.has(modelName);
                                        return (
                                            <div key={modelName} className="flex items-center justify-between px-3 py-1.5 hover:bg-gray-700/40 rounded-lg group">
                                                <span className="text-sm font-mono text-gray-200 truncate mr-2">{modelName}</span>
                                                {alreadyAdded ? (
                                                    <span className="text-xs text-green-400 flex items-center gap-1 shrink-0">
                                                        <Check size={12} /> {t('ai.settings.fetchModelsAdded')}
                                                    </span>
                                                ) : (
                                                    <button
                                                        onClick={() => provider && addModelFromList(provider, modelName)}
                                                        className="text-xs px-2 py-0.5 bg-indigo-600 hover:bg-indigo-500 rounded text-white transition-colors opacity-0 group-hover:opacity-100 shrink-0"
                                                    >
                                                        {t('ai.settings.fetchModelsAdd')}
                                                    </button>
                                                )}
                                            </div>
                                        );
                                    })
                                )}
                            </div>
                            <div className="px-4 py-2 border-t border-gray-700/50 text-xs text-gray-500">
                                {t('ai.settings.modelsAvailable', { count: availableModels.models.length })}
                            </div>
                        </div>
                    </div>
                );
            })()}
        </div>
    );
};

export default AISettingsPanel;
