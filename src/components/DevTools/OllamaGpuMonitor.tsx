import React, { useState, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Cpu, RefreshCw } from 'lucide-react';

interface OllamaRunningModelJS {
    name: string;
    size: number;
    vramSize: number;
    expiresAt: string;
}

interface OllamaGpuMonitorProps {
    baseUrl: string;
    visible?: boolean;
    compact?: boolean;
}

function formatBytes(bytes: number): string {
    if (bytes === 0) return '0 B';
    const units = ['B', 'KB', 'MB', 'GB', 'TB'];
    const i = Math.floor(Math.log(bytes) / Math.log(1024));
    return `${(bytes / Math.pow(1024, i)).toFixed(1)} ${units[i]}`;
}

function getExpiryLabel(expiresAt: string): string {
    const now = Date.now();
    const expires = new Date(expiresAt).getTime();
    const diff = expires - now;
    if (diff <= 0) return 'expired';
    if (diff < 60000) return `${Math.round(diff / 1000)}s`;
    return `${Math.round(diff / 60000)}m`;
}

export const OllamaGpuMonitor: React.FC<OllamaGpuMonitorProps> = ({ baseUrl, visible = true, compact = false }) => {
    const [models, setModels] = useState<OllamaRunningModelJS[]>([]);
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState<string | null>(null);

    useEffect(() => {
        let cancelled = false;

        const refresh = async () => {
            if (!baseUrl) return;
            setLoading(true);
            setError(null);
            try {
                const result = await invoke<OllamaRunningModelJS[]>('ollama_list_running', { baseUrl });
                if (!cancelled) {
                    setModels(result);
                }
            } catch (err) {
                if (!cancelled) {
                    setError(err instanceof Error ? err.message : String(err));
                }
            } finally {
                if (!cancelled) {
                    setLoading(false);
                }
            }
        };

        if (visible && baseUrl) {
            refresh();
            const interval = setInterval(refresh, 15000);
            return () => {
                cancelled = true;
                clearInterval(interval);
            };
        }

        return () => { cancelled = true; };
    }, [visible, baseUrl]);

    // Manual refresh callback for the UI button
    const handleRefresh = useCallback(async () => {
        if (!baseUrl) return;
        setLoading(true);
        setError(null);
        try {
            const result = await invoke<OllamaRunningModelJS[]>('ollama_list_running', { baseUrl });
            setModels(result);
        } catch (err) {
            setError(err instanceof Error ? err.message : String(err));
        } finally {
            setLoading(false);
        }
    }, [baseUrl]);

    if (!visible) return null;

    if (compact) {
        return (
            <div className="flex items-center gap-2 text-xs text-gray-400">
                <Cpu size={12} />
                <span>{models.length} model{models.length !== 1 ? 's' : ''} running</span>
                {models.length > 0 && (
                    <span className="text-cyan-400">
                        {formatBytes(models.reduce((sum, m) => sum + m.vramSize, 0))} VRAM
                    </span>
                )}
            </div>
        );
    }

    return (
        <div className="rounded-lg border border-gray-700 bg-gray-800/30 p-3">
            <div className="flex items-center justify-between mb-2">
                <div className="flex items-center gap-2 text-xs font-medium text-gray-300">
                    <Cpu size={14} className="text-cyan-400" />
                    <span>GPU Monitor</span>
                </div>
                <button
                    onClick={handleRefresh}
                    disabled={loading}
                    className="text-gray-500 hover:text-gray-300 transition-colors disabled:opacity-50"
                >
                    <RefreshCw size={12} className={loading ? 'animate-spin' : ''} />
                </button>
            </div>

            {error && (
                <div className="text-xs text-red-400 mb-2">
                    {error}
                </div>
            )}

            {models.length === 0 && !error && (
                <div className="text-xs text-gray-500">
                    No models currently loaded
                </div>
            )}

            {models.map((model, idx) => {
                const vramPercent = model.size > 0 ? Math.round((model.vramSize / model.size) * 100) : 0;
                const barColor = vramPercent > 90 ? 'bg-red-500' : vramPercent > 70 ? 'bg-yellow-500' : 'bg-green-500';

                return (
                    <div key={`${model.name}-${idx}`} className="mb-2 last:mb-0">
                        <div className="flex items-center justify-between text-xs mb-1">
                            <span className="text-gray-300 truncate max-w-[60%]">{model.name}</span>
                            <span className="text-gray-500 flex items-center gap-2">
                                <span>{formatBytes(model.vramSize)} VRAM</span>
                                <span className="text-gray-600">{getExpiryLabel(model.expiresAt)}</span>
                            </span>
                        </div>
                        <div className="w-full h-1.5 bg-gray-700 rounded-full overflow-hidden">
                            <div
                                className={`h-full rounded-full transition-all duration-300 ${barColor}`}
                                style={{ width: `${Math.min(100, vramPercent)}%` }}
                            />
                        </div>
                    </div>
                );
            })}
        </div>
    );
};
