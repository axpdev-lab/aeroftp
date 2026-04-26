import * as React from 'react';
import { useMemo, useState, useEffect, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    AlertTriangle,
    Check,
    Copy,
    DownloadCloud,
    Gauge,
    RefreshCw,
    ShieldCheck,
    Trash2,
    UploadCloud,
    X,
    Zap,
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { useTauriListener } from '../hooks/useTauriListener';
import {
    SpeedTestDialogProps,
    SpeedTestPhase,
    SpeedTestProgress,
    SpeedTestResult,
} from './SpeedTestDialog.types';
import {
    buildSpeedTestConnection,
    formatBytes,
    formatDuration,
    formatMBps,
    formatMbps,
    SPEEDTEST_SIZES,
    supportsSpeedTest,
} from '../utils/speedTest';

const phaseOrder: SpeedTestPhase[] = ['connecting', 'uploading', 'downloading', 'cleaning_up', 'done'];

function phaseIndex(phase: SpeedTestPhase): number {
    const idx = phaseOrder.indexOf(phase);
    return idx < 0 ? 0 : idx;
}

const ThroughputBar: React.FC<{ label: string; icon: React.ReactNode; mbps: number; bytesPerSec: number; maxMbps: number; tone: 'upload' | 'download' }> = ({
    label,
    icon,
    mbps,
    bytesPerSec,
    maxMbps,
    tone,
}) => {
    const pct = Math.max(4, Math.min(100, maxMbps > 0 ? (mbps / maxMbps) * 100 : 0));
    const color = tone === 'upload' ? 'bg-blue-500' : 'bg-emerald-500';

    return (
        <div className="py-3">
            <div className="flex items-center justify-between gap-4 mb-2">
                <div className="flex items-center gap-2 text-sm font-medium">
                    <span className={tone === 'upload' ? 'text-blue-500' : 'text-emerald-500'}>{icon}</span>
                    <span>{label}</span>
                </div>
                <div className="text-right">
                    <div className="text-xl font-semibold tabular-nums">{formatMbps(mbps)} Mbps</div>
                    <div className="text-[11px] text-gray-500 tabular-nums">{formatMBps(bytesPerSec)}</div>
                </div>
            </div>
            <div className="h-2 bg-gray-100 dark:bg-gray-700 rounded-full overflow-hidden">
                <div className={`h-full ${color} rounded-full transition-all duration-700`} style={{ width: `${pct}%` }} />
            </div>
        </div>
    );
};

export const SpeedTestDialog: React.FC<SpeedTestDialogProps> = ({ servers, initialServerId, onClose }) => {
    const t = useTranslation();
    const supportedServers = useMemo(() => servers.filter(supportsSpeedTest), [servers]);
    const initialSupportedId = useMemo(() => {
        if (initialServerId && supportedServers.some(s => s.id === initialServerId)) return initialServerId;
        return supportedServers[0]?.id || '';
    }, [initialServerId, supportedServers]);

    const [selectedServerId, setSelectedServerId] = useState(initialSupportedId);
    const [sizeBytes, setSizeBytes] = useState<number>(10 * 1024 * 1024);
    const [remoteDir, setRemoteDir] = useState('/');
    const [phase, setPhase] = useState<SpeedTestPhase>('idle');
    const [progress, setProgress] = useState<SpeedTestProgress | null>(null);
    const [result, setResult] = useState<SpeedTestResult | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [copied, setCopied] = useState(false);
    const running = phase !== 'idle' && phase !== 'done';

    const selectedServer = useMemo(
        () => supportedServers.find(s => s.id === selectedServerId) || null,
        [supportedServers, selectedServerId],
    );

    useEffect(() => {
        if (!selectedServer) return;
        setRemoteDir(selectedServer.initialPath || '/');
    }, [selectedServer?.id]);

    useTauriListener<SpeedTestProgress>(
        'speedtest-progress',
        (event) => {
            setProgress(event.payload);
            setPhase(event.payload.phase);
        },
        [],
        { enabled: running },
    );

    const handleRun = useCallback(async () => {
        if (!selectedServer || running) return;
        setResult(null);
        setError(null);
        setCopied(false);
        setProgress(null);
        setPhase('connecting');

        try {
            const connection = await buildSpeedTestConnection(selectedServer);
            const response = await invoke<SpeedTestResult>('speedtest_run', {
                request: {
                    connection,
                    size_bytes: sizeBytes,
                    remote_dir: remoteDir.trim() || '/',
                    server_name: selectedServer.name || selectedServer.host,
                },
            });
            setResult(response);
            setPhase('done');
        } catch (err) {
            setError(String(err));
            setPhase('idle');
        }
    }, [remoteDir, running, selectedServer, sizeBytes]);

    const handleCancel = useCallback(async () => {
        try {
            await invoke('speedtest_cancel');
        } catch {
            // The backend may have completed between the click and the cancel invoke.
        }
    }, []);

    const copyResult = useCallback(() => {
        if (!result) return;
        const lines = [
            `AeroFTP Speed Test - ${new Date().toLocaleString()}`,
            `Server: ${result.server_name || selectedServer?.name || selectedServer?.host || '-'}`,
            `Protocol: ${result.protocol}`,
            `Size: ${formatBytes(result.size_bytes)}`,
            `Upload: ${formatMbps(result.upload_mbps)} Mbps (${formatMBps(result.upload_bytes_per_sec)}) in ${formatDuration(result.upload_duration_ms)}`,
            `Download: ${formatMbps(result.download_mbps)} Mbps (${formatMBps(result.download_bytes_per_sec)}) in ${formatDuration(result.download_duration_ms)}`,
            `Integrity: ${result.integrity_verified ? 'verified' : 'corrupted'}`,
            `Cleanup: ${result.temp_file_cleaned ? 'removed' : `failed - ${result.remote_path}`}`,
        ];
        navigator.clipboard.writeText(lines.join('\n')).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1800);
        });
    }, [result, selectedServer]);

    const selectedSizeLabel = formatBytes(sizeBytes);
    const totalPhasePct = phase === 'idle'
        ? 0
        : phase === 'done'
            ? 100
            : ((phaseIndex(phase) + 1) / phaseOrder.length) * 100;
    const transferPct = progress && progress.total_bytes > 0
        ? Math.min(100, (progress.transferred_bytes / progress.total_bytes) * 100)
        : totalPhasePct;
    const maxMbps = result ? Math.max(result.upload_mbps, result.download_mbps, 1) : 1;

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm animate-fade-in"
            onClick={(e) => { if (e.target === e.currentTarget && !running) onClose(); }}
        >
            <div
                className="w-full max-w-2xl bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                <div className="px-5 py-4 border-b border-gray-200 dark:border-gray-700 flex items-center justify-between">
                    <div className="flex items-center gap-3 min-w-0">
                        <div className="p-2 bg-indigo-100 dark:bg-indigo-900/40 rounded-lg">
                            <Gauge size={20} className="text-indigo-600 dark:text-indigo-400" />
                        </div>
                        <div className="min-w-0">
                            <h2 className="text-base font-semibold">{t('speedTest.title')}</h2>
                            <p className="text-xs text-gray-500 truncate">{t('speedTest.subtitle')}</p>
                        </div>
                    </div>
                    <button
                        onClick={running ? handleCancel : onClose}
                        className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        title={running ? t('speedTest.cancel') : t('common.close')}
                    >
                        <X size={18} />
                    </button>
                </div>

                {supportedServers.length === 0 ? (
                    <div className="px-5 py-12 text-center">
                        <AlertTriangle size={32} className="mx-auto text-amber-500 mb-3" />
                        <p className="text-sm font-medium">{t('speedTest.noSupportedServers')}</p>
                        <p className="text-xs text-gray-500 mt-1">{t('speedTest.unsupportedProvider')}</p>
                    </div>
                ) : (
                    <>
                        <div className="px-5 py-4 space-y-4">
                            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                                <label className="block">
                                    <span className="block text-xs font-medium text-gray-500 mb-1.5">{t('speedTest.selectServer')}</span>
                                    <select
                                        value={selectedServerId}
                                        onChange={(e) => setSelectedServerId(e.target.value)}
                                        disabled={running}
                                        className="w-full h-9 px-3 rounded-lg bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500/30"
                                    >
                                        {supportedServers.map(server => (
                                            <option key={server.id} value={server.id}>
                                                {server.name || server.host} ({(server.protocol || 'ftp').toUpperCase()})
                                            </option>
                                        ))}
                                    </select>
                                </label>

                                <label className="block">
                                    <span className="block text-xs font-medium text-gray-500 mb-1.5">{t('speedTest.remoteDir')}</span>
                                    <input
                                        value={remoteDir}
                                        onChange={(e) => setRemoteDir(e.target.value)}
                                        disabled={running}
                                        placeholder="/"
                                        title={t('speedTest.remoteDirHint')}
                                        className="w-full h-9 px-3 rounded-lg bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-indigo-500/30"
                                    />
                                </label>
                            </div>

                            <div>
                                <div className="text-xs font-medium text-gray-500 mb-2">{t('speedTest.selectSize')}</div>
                                <div className="grid grid-cols-3 gap-2">
                                    {SPEEDTEST_SIZES.map(size => (
                                        <button
                                            key={size.bytes}
                                            onClick={() => setSizeBytes(size.bytes)}
                                            disabled={running}
                                            className={`h-10 px-2 rounded-lg text-xs font-medium border transition-colors ${
                                                sizeBytes === size.bytes
                                                    ? 'bg-indigo-600 border-indigo-600 text-white'
                                                    : 'bg-gray-50 dark:bg-gray-900 border-gray-200 dark:border-gray-700 hover:border-indigo-300'
                                            }`}
                                        >
                                            {t(size.labelKey)}
                                        </button>
                                    ))}
                                </div>
                            </div>

                            {!running && !result && (
                                <div className="flex gap-3 rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 px-3 py-2.5">
                                    <AlertTriangle size={16} className="text-amber-600 dark:text-amber-400 mt-0.5 flex-shrink-0" />
                                    <p className="text-xs text-amber-800 dark:text-amber-200 leading-5">
                                        {t('speedTest.disclaimer', { size: selectedSizeLabel })}
                                    </p>
                                </div>
                            )}

                            {running && (
                                <div className="py-4">
                                    <div className="flex items-center justify-between mb-2">
                                        <div className="flex items-center gap-2 text-sm font-medium">
                                            <RefreshCw size={15} className="animate-spin text-indigo-500" />
                                            <span>
                                                {phase === 'connecting' && t('speedTest.connecting')}
                                                {phase === 'uploading' && t('speedTest.uploading', { size: selectedSizeLabel })}
                                                {phase === 'downloading' && t('speedTest.downloading', { size: selectedSizeLabel })}
                                                {phase === 'cleaning_up' && t('speedTest.cleaningUp')}
                                            </span>
                                        </div>
                                        {progress?.bytes_per_sec ? (
                                            <span className="text-xs text-gray-500 tabular-nums">{formatMBps(progress.bytes_per_sec)}</span>
                                        ) : null}
                                    </div>
                                    <div className="h-2 bg-gray-100 dark:bg-gray-700 rounded-full overflow-hidden">
                                        <div className="h-full bg-indigo-500 rounded-full transition-all duration-300" style={{ width: `${transferPct}%` }} />
                                    </div>
                                    <p className="mt-2 text-[11px] text-gray-500">{t('speedTest.cancelHint')}</p>
                                </div>
                            )}

                            {error && (
                                <div className="flex gap-2 rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/20 px-3 py-2.5 text-sm text-red-700 dark:text-red-300">
                                    <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" />
                                    <span>{error}</span>
                                </div>
                            )}

                            {result && (
                                <div className="divide-y divide-gray-100 dark:divide-gray-700">
                                    <ThroughputBar
                                        label={t('speedTest.uploadSpeed')}
                                        icon={<UploadCloud size={16} />}
                                        mbps={result.upload_mbps}
                                        bytesPerSec={result.upload_bytes_per_sec}
                                        maxMbps={maxMbps}
                                        tone="upload"
                                    />
                                    <ThroughputBar
                                        label={t('speedTest.downloadSpeed')}
                                        icon={<DownloadCloud size={16} />}
                                        mbps={result.download_mbps}
                                        bytesPerSec={result.download_bytes_per_sec}
                                        maxMbps={maxMbps}
                                        tone="download"
                                    />

                                    <div className="grid grid-cols-2 sm:grid-cols-4 gap-3 py-4 text-sm">
                                        <div>
                                            <div className="text-[11px] text-gray-500">{t('speedTest.totalTime')}</div>
                                            <div className="font-medium tabular-nums">{formatDuration(result.upload_duration_ms + result.download_duration_ms)}</div>
                                        </div>
                                        <div>
                                            <div className="text-[11px] text-gray-500">{t('speedTest.integrity')}</div>
                                            <div className={`font-medium flex items-center gap-1 ${result.integrity_verified ? 'text-emerald-600' : 'text-red-600'}`}>
                                                <ShieldCheck size={14} />
                                                {result.integrity_verified ? t('speedTest.integrityVerified') : t('speedTest.integrityCorrupted')}
                                            </div>
                                        </div>
                                        <div>
                                            <div className="text-[11px] text-gray-500">{t('speedTest.cleanup')}</div>
                                            <div className={`font-medium flex items-center gap-1 ${result.temp_file_cleaned ? 'text-emerald-600' : 'text-amber-600'}`}>
                                                <Trash2 size={14} />
                                                {result.temp_file_cleaned ? t('speedTest.cleanupRemoved') : t('speedTest.cleanupFailed')}
                                            </div>
                                        </div>
                                        <div>
                                            <div className="text-[11px] text-gray-500">{t('speedTest.selectSize')}</div>
                                            <div className="font-medium">{formatBytes(result.size_bytes)}</div>
                                        </div>
                                    </div>

                                    {!result.temp_file_cleaned && (
                                        <div className="py-3">
                                            <div className="text-xs text-amber-600 dark:text-amber-400 mb-1">{t('speedTest.manualCleanupHint')}</div>
                                            <code className="block text-xs bg-gray-100 dark:bg-gray-900 rounded-lg px-3 py-2 break-all">{result.remote_path}</code>
                                        </div>
                                    )}
                                </div>
                            )}
                        </div>

                        <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700 flex items-center justify-between gap-3">
                            <div className="flex items-center gap-2 text-[11px] text-gray-500">
                                <Zap size={13} />
                                <span>{selectedServer ? `${(selectedServer.protocol || 'ftp').toUpperCase()} - ${selectedServer.host}` : '-'}</span>
                            </div>
                            <div className="flex items-center gap-2">
                                {result && (
                                    <button
                                        onClick={copyResult}
                                        className="px-3 py-1.5 text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors flex items-center gap-1.5"
                                    >
                                        {copied ? <Check size={13} className="text-emerald-500" /> : <Copy size={13} />}
                                        {copied ? t('speedTest.copied') : t('speedTest.copyResult')}
                                    </button>
                                )}
                                <button
                                    onClick={result ? handleRun : running ? handleCancel : onClose}
                                    className="px-3 py-1.5 text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                                >
                                    {result ? t('speedTest.runAgain') : running ? t('speedTest.cancel') : t('common.close')}
                                </button>
                                {!result && !running && (
                                    <button
                                        onClick={handleRun}
                                        disabled={!selectedServer}
                                        className="px-3 py-1.5 text-xs font-medium bg-indigo-600 hover:bg-indigo-700 disabled:bg-indigo-400 text-white rounded-lg transition-colors flex items-center gap-1.5"
                                    >
                                        <Gauge size={13} />
                                        {t('speedTest.start')}
                                    </button>
                                )}
                            </div>
                        </div>
                    </>
                )}
            </div>
        </div>
    );
};

export default SpeedTestDialog;
