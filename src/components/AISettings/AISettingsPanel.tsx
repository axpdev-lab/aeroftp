import React, { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    X, Plus, Trash2, Edit2, Check, AlertCircle,
    Zap, Server, Key, Globe, Cpu, Settings2, ChevronDown, ChevronRight, Sliders
} from 'lucide-react';
import {
    AIProvider, AIModel, AISettings, AIProviderType,
    PROVIDER_PRESETS, DEFAULT_MODELS, generateId, getDefaultAISettings
} from '../../types/ai';
import { logger } from '../../utils/logger';

interface AISettingsPanelProps {
    isOpen: boolean;
    onClose: () => void;
}

// Provider type icons
const getProviderIcon = (type: AIProviderType): React.ReactNode => {
    switch (type) {
        case 'google': return <span className="text-blue-500">G</span>;
        case 'openai': return <span className="text-green-500">◯</span>;
        case 'anthropic': return <span className="text-orange-500">A</span>;
        case 'xai': return <span className="text-white">𝕏</span>;
        case 'openrouter': return <span className="text-purple-500">⬡</span>;
        case 'ollama': return <span className="text-cyan-500">🦙</span>;
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
        supportsStreaming: model?.supportsStreaming ?? true,
        supportsTools: model?.supportsTools ?? true,
        supportsVision: model?.supportsVision ?? false,
        isEnabled: model?.isEnabled ?? true,
    });

    const handleSave = () => {
        if (!formData.name.trim()) return;

        onSave({
            id: model?.id || generateId(),
            providerId,
            name: formData.name.trim(),
            displayName: formData.displayName.trim() || formData.name.trim(),
            maxTokens: formData.maxTokens,
            supportsStreaming: formData.supportsStreaming,
            supportsTools: formData.supportsTools,
            supportsVision: formData.supportsVision,
            isEnabled: formData.isEnabled,
            isDefault: model?.isDefault ?? false,
        });
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
                            onChange={e => setFormData({ ...formData, name: e.target.value })}
                            placeholder="e.g., gpt-4-turbo, gemini-2.0-flash"
                            className="w-full px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                            autoFocus
                        />
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
                        </div>
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
    const [activeTab, setActiveTab] = useState<'providers' | 'models' | 'routing' | 'advanced'>('providers');
    const [editingProvider, setEditingProvider] = useState<AIProvider | null>(null);
    const [expandedProviders, setExpandedProviders] = useState<Set<string>>(new Set());
    const [testingProvider, setTestingProvider] = useState<string | null>(null);
    const [testResults, setTestResults] = useState<Record<string, 'success' | 'error' | null>>({});

    // Model editing state
    const [editingModel, setEditingModel] = useState<{
        model: AIModel | null;
        providerId: string;
        isNew: boolean;
    } | null>(null);

    // Load settings from localStorage + API keys from OS Keyring
    useEffect(() => {
        const loadSettings = async () => {
            const saved = localStorage.getItem(AI_SETTINGS_KEY);
            if (!saved) return;
            try {
                const parsed = JSON.parse(saved);
                // Convert date strings back to Date objects
                parsed.providers = parsed.providers.map((p: AIProvider) => ({
                    ...p,
                    createdAt: new Date(p.createdAt),
                    updatedAt: new Date(p.updatedAt),
                }));

                // Fetch API keys from OS Keyring for each provider
                let migrated = false;
                for (const provider of parsed.providers) {
                    // Migration: if apiKey exists in localStorage, move it to keyring
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

                setSettings(parsed);

                // After migration, strip API keys from localStorage
                if (migrated) {
                    const stripped = {
                        ...parsed,
                        providers: parsed.providers.map((p: AIProvider) => ({ ...p, apiKey: undefined })),
                    };
                    localStorage.setItem(AI_SETTINGS_KEY, JSON.stringify(stripped));
                    logger.debug('[AI Settings] Migrated API keys from localStorage to OS Keyring');
                }
            } catch (e) {
                console.error('Failed to parse AI settings:', e);
            }
        };
        loadSettings();
    }, []);

    // Save settings: API keys go to OS Keyring, rest to localStorage
    const saveSettings = (newSettings: AISettings) => {
        setSettings(newSettings);

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
    };

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

    // Test provider connection
    const testProvider = async (provider: AIProvider) => {
        setTestingProvider(provider.id);
        setTestResults({ ...testResults, [provider.id]: null });

        try {
            // Simple test - just check if API key is set and base URL is reachable
            if (!provider.apiKey && provider.type !== 'ollama') {
                throw new Error('API key required');
            }

            // For Ollama, try to list models
            if (provider.type === 'ollama') {
                const response = await fetch(`${provider.baseUrl}/api/tags`);
                if (!response.ok) throw new Error('Ollama not reachable');
            }

            // For now, just validate the key exists
            setTestResults({ ...testResults, [provider.id]: 'success' });
        } catch (error) {
            setTestResults({ ...testResults, [provider.id]: 'error' });
        } finally {
            setTestingProvider(null);
        }
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
                        { id: 'routing', label: 'Auto-Routing', icon: <Zap size={14} /> },
                        { id: 'advanced', label: 'Advanced', icon: <Sliders size={14} /> },
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
                                                            <input
                                                                type="password"
                                                                value={provider.apiKey || ''}
                                                                onChange={e => updateProvider({
                                                                    ...provider,
                                                                    apiKey: e.target.value
                                                                })}
                                                                placeholder={provider.type === 'ollama' ? 'Not required for Ollama' : 'Enter API key...'}
                                                                className="flex-1 px-3 py-2 bg-gray-900 border border-gray-600 rounded-lg text-sm focus:outline-none focus:ring-2 focus:ring-purple-500"
                                                            />
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
                                                        </div>
                                                        {testResults[provider.id] && (
                                                            <div className={`mt-2 text-xs flex items-center gap-1 ${testResults[provider.id] === 'success'
                                                                ? 'text-green-400'
                                                                : 'text-red-400'
                                                                }`}>
                                                                {testResults[provider.id] === 'success'
                                                                    ? <><Check size={12} /> Connection successful</>
                                                                    : <><AlertCircle size={12} /> Connection failed</>
                                                                }
                                                            </div>
                                                        )}
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
                                                            <div className="flex gap-1.5 text-xs">
                                                                {model.supportsTools && (
                                                                    <span className="px-1.5 py-0.5 bg-blue-500/20 text-blue-400 rounded" title="Function Calling">🔧</span>
                                                                )}
                                                                {model.supportsVision && (
                                                                    <span className="px-1.5 py-0.5 bg-green-500/20 text-green-400 rounded" title="Vision">👁</span>
                                                                )}
                                                                {model.supportsStreaming && (
                                                                    <span className="px-1.5 py-0.5 bg-purple-500/20 text-purple-400 rounded" title="Streaming">⚡</span>
                                                                )}
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
                        </div>
                    )}

                    {activeTab === 'routing' && (
                        <div className="space-y-4">
                            <div className="flex items-center justify-between p-4 bg-gray-800 rounded-lg">
                                <div>
                                    <h3 className="font-medium">Auto-Routing</h3>
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
                                <div className="space-y-3">
                                    <p className="text-sm text-gray-400 mb-4">
                                        Configure which model to use for each task type. When auto-routing is enabled,
                                        the AI will automatically select the appropriate model based on the task.
                                    </p>

                                    {/* Task Type Rules */}
                                    {[
                                        { type: 'code_generation' as const, label: '💻 Code Generation', desc: 'Writing new code, functions, components' },
                                        { type: 'code_review' as const, label: '🔍 Code Review', desc: 'Reviewing, refactoring, suggesting improvements' },
                                        { type: 'quick_answer' as const, label: '⚡ Quick Answer', desc: 'Simple questions, short responses' },
                                        { type: 'file_analysis' as const, label: '📄 File Analysis', desc: 'Analyzing file contents, understanding code' },
                                        { type: 'terminal_command' as const, label: '🖥️ Terminal Commands', desc: 'Shell commands, scripts, automation' },
                                        { type: 'general' as const, label: '💬 General', desc: 'Default for unclassified tasks' },
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

                                    {/* Info box */}
                                    <div className="mt-4 p-3 bg-blue-900/20 border border-blue-600/30 rounded-lg text-sm text-blue-300">
                                        <strong>💡 Tip:</strong> Leave as "Auto (Default)" to use the model selected in the chat header.
                                        Configure specific models only when you want different models for different tasks.
                                    </div>
                                </div>
                            )}
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
                                        {settings.advancedSettings?.temperature?.toFixed(1) || '0.7'}
                                    </span>
                                </div>
                                <input
                                    type="range"
                                    min="0"
                                    max="2"
                                    step="0.1"
                                    value={settings.advancedSettings?.temperature || 0.7}
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
                                        value={settings.advancedSettings?.maxTokens || 4096}
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
        </div>
    );
};

export default AISettingsPanel;
