import * as React from 'react';
import { useMemo, useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import {
    AlertTriangle,
    Check,
    Copy,
    DownloadCloud,
    FileJson,
    Gauge,
    History,
    Layers,
    RefreshCw,
    Server as ServerIcon,
    ShieldCheck,
    Trash2,
    UploadCloud,
    X,
    Zap,
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { useTauriListener } from '../hooks/useTauriListener';
import {
    SpeedTestCompareResult,
    SpeedTestDialogProps,
    SpeedTestHistorySummary,
    SpeedTestMode,
    SpeedTestPhase,
    SpeedTestProgress,
    SpeedTestResult,
    SpeedTestRunOutcome,
} from './SpeedTestDialog.types';
import {
    buildJsonReport,
    buildMarkdownReport,
    buildMethodologyReport,
    buildSpeedTestConnection,
    downloadCsvReport,
    downloadJsonReport,
    downloadMarkdownReport,
    EXPERT_MAX,
    EXPERT_THRESHOLD,
    formatBytes,
    formatDuration,
    formatMBps,
    formatMbps,
    hashHost,
    rankOutcomes,
    singleResultAsOutcome,
    SPEEDTEST_SIZES,
    supportsSpeedTest,
} from '../utils/speedTest';
import { ServerProfile } from '../types';

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

function genTestId(): string {
    if (typeof crypto !== 'undefined' && 'randomUUID' in crypto) {
        return (crypto as { randomUUID: () => string }).randomUUID();
    }
    return `test-${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
}

const phaseOrder: SpeedTestPhase[] = ['connecting', 'uploading', 'downloading', 'cleaning_up', 'done'];

function phaseIndex(phase: SpeedTestPhase): number {
    const idx = phaseOrder.indexOf(phase);
    return idx < 0 ? 0 : idx;
}

function recordResultToHistory(
    server: ServerProfile | undefined,
    result: SpeedTestResult,
): Promise<void> {
    return (async () => {
        try {
            const hostHash = server?.host ? await hashHost(server.host) : '';
            // Privacy posture (audit P1-11): persist only stable identifiers
            // (server_id, host_hash) — never the user-facing server_name, which
            // can contain customer/environment/ticket details. The UI looks up
            // the display name from the live profile list at render time.
            await invoke('speedtest_history_record', {
                record: {
                    server_id: server?.id || null,
                    server_name: null,
                    host_hash: hostHash || null,
                    protocol: result.protocol,
                    size_bytes: result.size_bytes,
                    upload_bytes_per_sec: result.upload_bytes_per_sec,
                    download_bytes_per_sec: result.download_bytes_per_sec,
                    upload_duration_ms: result.upload_duration_ms,
                    download_duration_ms: result.download_duration_ms,
                    integrity_verified: result.integrity_verified,
                    cleanup_ok: result.temp_file_cleaned,
                },
            });
        } catch {
            // History is best-effort.
        }
    })();
}

// ---------------------------------------------------------------------------
// Subcomponents
// ---------------------------------------------------------------------------

const ThroughputBar: React.FC<{
    label: string;
    icon: React.ReactNode;
    mbps: number;
    bytesPerSec: number;
    maxMbps: number;
    tone: 'upload' | 'download';
}> = ({ label, icon, mbps, bytesPerSec, maxMbps, tone }) => {
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

// Per-server live progress strip used in compare mode.
const PerServerProgress: React.FC<{
    name: string;
    protocol: string;
    progress: SpeedTestProgress | null;
    result: SpeedTestResult | null;
    error: string | null;
    sizeBytes: number;
}> = ({ name, protocol, progress, result, error, sizeBytes }) => {
    const t = useTranslation();
    const isDone = result != null || progress?.phase === 'done';
    const phase = progress?.phase || (result ? 'done' : error ? 'connecting' : 'connecting');
    const pct = isDone
        ? 100
        : error
            ? 100
            : progress && progress.total_bytes > 0
                ? Math.min(100, (progress.transferred_bytes / progress.total_bytes) * 100)
                : ((phaseIndex(phase) + 1) / phaseOrder.length) * 100;
    const phaseLabelKey = phase === 'uploading'
        ? 'speedTest.uploading'
        : phase === 'downloading'
            ? 'speedTest.downloading'
            : phase === 'cleaning_up'
                ? 'speedTest.cleaningUp'
                : phase === 'done'
                    ? 'speedTest.completed'
                    : 'speedTest.connecting';
    const liveBps = progress?.bytes_per_sec ?? null;
    const labelRight = error
        ? t('speedTest.errorLabel')
        : result
            ? `${formatMbps(result.download_mbps)} Mbps`
            : isDone
                ? t('speedTest.completed')
                : liveBps != null
                    ? formatMBps(liveBps)
                    : t(phaseLabelKey, { size: formatBytes(sizeBytes) });
    return (
        <div className="py-2">
            <div className="flex items-center justify-between gap-3 text-xs mb-1">
                <div className="flex items-center gap-1.5 min-w-0">
                    <ServerIcon size={12} className="text-gray-400 flex-shrink-0" />
                    <span className="truncate font-medium">{name}</span>
                    <span className="text-gray-400">({protocol.toUpperCase()})</span>
                </div>
                <span className={`tabular-nums ${error ? 'text-red-500' : isDone ? 'text-emerald-600' : 'text-gray-500'}`}>
                    {labelRight}
                </span>
            </div>
            <div className="h-1.5 bg-gray-100 dark:bg-gray-700 rounded-full overflow-hidden">
                <div
                    className={`h-full rounded-full transition-all duration-300 ${error ? 'bg-red-500' : isDone ? 'bg-emerald-500' : 'bg-indigo-500'}`}
                    style={{ width: `${pct}%` }}
                />
            </div>
        </div>
    );
};

// ---------------------------------------------------------------------------
// Main dialog
// ---------------------------------------------------------------------------

export const SpeedTestDialog: React.FC<SpeedTestDialogProps> = ({
    servers,
    initialServerId,
    initialMode = 'single',
    onClose,
}) => {
    const t = useTranslation();
    const supportedServers = useMemo(() => servers.filter(supportsSpeedTest), [servers]);
    const initialSupportedId = useMemo(() => {
        if (initialServerId && supportedServers.some(s => s.id === initialServerId)) return initialServerId;
        return supportedServers[0]?.id || '';
    }, [initialServerId, supportedServers]);

    // Mode + shared state
    const [mode, setMode] = useState<SpeedTestMode>(initialMode);

    // Single-mode state
    const [selectedServerId, setSelectedServerId] = useState(initialSupportedId);
    const [sizeBytes, setSizeBytes] = useState<number>(10 * 1024 * 1024);
    const [expertSizeMb, setExpertSizeMb] = useState<number>(250);
    const [useExpertSize, setUseExpertSize] = useState(false);
    const [expertConfirmed, setExpertConfirmed] = useState(false);
    const [verifyIntegrity, setVerifyIntegrity] = useState(true);
    const [remoteDir, setRemoteDir] = useState('/');
    const [phase, setPhase] = useState<SpeedTestPhase>('idle');
    const [progress, setProgress] = useState<SpeedTestProgress | null>(null);
    const [result, setResult] = useState<SpeedTestResult | null>(null);
    const [error, setError] = useState<string | null>(null);
    const [copied, setCopied] = useState(false);
    const [historySummary, setHistorySummary] = useState<SpeedTestHistorySummary | null>(null);

    // Compare-mode state
    const [compareSelected, setCompareSelected] = useState<Set<string>>(new Set());
    const [compareParallel, setCompareParallel] = useState<number>(2);
    const [compareRunning, setCompareRunning] = useState(false);
    const [compareResult, setCompareResult] = useState<SpeedTestCompareResult | null>(null);
    const [compareError, setCompareError] = useState<string | null>(null);
    const compareTestIdsRef = useRef<Map<string, { serverId: string; serverName: string; protocol: string }>>(new Map());
    const [compareProgress, setCompareProgress] = useState<Map<string, SpeedTestProgress>>(new Map());

    const running = phase !== 'idle' && phase !== 'done';
    const anyRunning = running || compareRunning;

    const selectedServer = useMemo(
        () => supportedServers.find(s => s.id === selectedServerId) || null,
        [supportedServers, selectedServerId],
    );

    const effectiveSize = useExpertSize
        ? Math.max(1, Math.min(EXPERT_MAX / (1024 * 1024), Math.floor(expertSizeMb))) * 1024 * 1024
        : sizeBytes;
    const sizeRequiresConfirm = effectiveSize > EXPERT_THRESHOLD;

    useEffect(() => {
        if (!selectedServer) return;
        setRemoteDir(selectedServer.initialPath || '/');
    }, [selectedServer?.id]);

    // Load history summary when single-mode server changes
    useEffect(() => {
        if (mode !== 'single' || !selectedServer) {
            setHistorySummary(null);
            return;
        }
        let cancelled = false;
        (async () => {
            try {
                const summary = await invoke<SpeedTestHistorySummary>('speedtest_history_summary', {
                    serverId: selectedServer.id,
                });
                if (!cancelled) setHistorySummary(summary.samples > 0 ? summary : null);
            } catch {
                if (!cancelled) setHistorySummary(null);
            }
        })();
        return () => { cancelled = true; };
    }, [mode, selectedServer?.id]);

    // Progress listener — routes by test_id
    useTauriListener<SpeedTestProgress>(
        'speedtest-progress',
        (event) => {
            const p = event.payload;
            if (mode === 'compare') {
                if (compareTestIdsRef.current.has(p.test_id)) {
                    setCompareProgress(prev => {
                        const next = new Map(prev);
                        next.set(p.test_id, p);
                        return next;
                    });
                }
            } else {
                setProgress(p);
                setPhase(p.phase);
            }
        },
        [mode],
        { enabled: anyRunning },
    );

    // ---------- single run ----------
    const handleRun = useCallback(async () => {
        if (!selectedServer || running) return;
        if (sizeRequiresConfirm && !expertConfirmed) {
            setError(t('speedTest.errorExpertConfirm'));
            return;
        }
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
                    size_bytes: effectiveSize,
                    remote_dir: remoteDir.trim() || '/',
                    server_name: selectedServer.name || selectedServer.host,
                    test_id: genTestId(),
                    expert_confirmed: sizeRequiresConfirm && expertConfirmed,
                    verify_integrity: verifyIntegrity,
                },
            });
            setResult(response);
            setPhase('done');
            void recordResultToHistory(selectedServer, response);
        } catch (err) {
            setError(String(err));
            setPhase('idle');
        }
    }, [effectiveSize, remoteDir, running, selectedServer, sizeRequiresConfirm, expertConfirmed, verifyIntegrity, t]);

    // ---------- compare run ----------
    const handleRunCompare = useCallback(async () => {
        if (compareRunning || compareSelected.size === 0) return;
        // Audit P1-7: enforce hard cap on compare-mode test count.
        if (compareSelected.size > 8) {
            setCompareError(t('speedTest.errorCompareTooMany', { max: 8 }));
            return;
        }
        if (sizeRequiresConfirm && !expertConfirmed) {
            setCompareError(t('speedTest.errorExpertConfirm'));
            return;
        }
        setCompareError(null);
        setCompareResult(null);
        setCompareProgress(new Map());
        compareTestIdsRef.current = new Map();
        // Audit P1-9: keep ALL pre-invoke setup inside try/finally so an
        // exception thrown while building connection payloads can never leave
        // the dialog stuck in compareRunning=true.
        setCompareRunning(true);
        try {
            const targetServers = supportedServers.filter(s => compareSelected.has(s.id));
            const tests = await Promise.all(
                targetServers.map(async (server) => {
                    const connection = await buildSpeedTestConnection(server);
                    const testId = genTestId();
                    compareTestIdsRef.current.set(testId, {
                        serverId: server.id,
                        serverName: server.name || server.host,
                        protocol: server.protocol || 'ftp',
                    });
                    return {
                        connection,
                        size_bytes: effectiveSize,
                        remote_dir: server.initialPath || '/',
                        server_name: server.name || server.host,
                        test_id: testId,
                        expert_confirmed: sizeRequiresConfirm && expertConfirmed,
                        verify_integrity: verifyIntegrity,
                    };
                }),
            );

            const response = await invoke<SpeedTestCompareResult>('speedtest_compare', {
                request: { tests, max_parallel: compareParallel },
            });
            setCompareResult(response);
            // Persist successful results to history
            response.results.forEach(o => {
                if (o.result) {
                    const meta = compareTestIdsRef.current.get(o.test_id);
                    const server = meta ? targetServers.find(s => s.id === meta.serverId) : undefined;
                    void recordResultToHistory(server, o.result);
                }
            });
        } catch (err) {
            setCompareError(String(err));
        } finally {
            setCompareRunning(false);
        }
    }, [compareParallel, compareRunning, compareSelected, effectiveSize, expertConfirmed, sizeRequiresConfirm, supportedServers, verifyIntegrity, t]);

    const handleCancel = useCallback(async () => {
        try {
            await invoke('speedtest_cancel');
        } catch {
            // The backend may have completed between the click and the cancel invoke.
        }
    }, []);

    const toggleCompareServer = useCallback((id: string) => {
        setCompareSelected(prev => {
            const next = new Set(prev);
            if (next.has(id)) next.delete(id);
            else next.add(id);
            return next;
        });
    }, []);

    const buildOutcomes = useCallback((): SpeedTestRunOutcome[] => (
        mode === 'compare'
            ? (compareResult?.results || [])
            : (result ? [singleResultAsOutcome(result)] : [])
    ), [mode, result, compareResult]);

    const buildCtx = useCallback(() => ({
        sizeBytes: effectiveSize,
        remoteDir: mode === 'single' ? remoteDir : undefined,
        startedAt: compareResult?.started_at_ms,
        finishedAt: compareResult?.finished_at_ms,
    }), [compareResult, effectiveSize, mode, remoteDir]);

    const copyResult = useCallback(() => {
        const outcomes = buildOutcomes();
        if (outcomes.length === 0) return;
        navigator.clipboard.writeText(buildMethodologyReport(outcomes, buildCtx())).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1800);
        });
    }, [buildCtx, buildOutcomes]);

    const copyMarkdown = useCallback(() => {
        const outcomes = buildOutcomes();
        if (outcomes.length === 0) return;
        navigator.clipboard.writeText(buildMarkdownReport(outcomes, buildCtx())).then(() => {
            setCopied(true);
            window.setTimeout(() => setCopied(false), 1800);
        });
    }, [buildCtx, buildOutcomes]);

    const exportJson = useCallback(() => {
        const outcomes = buildOutcomes();
        if (outcomes.length === 0) return;
        downloadJsonReport(buildJsonReport(outcomes, buildCtx()), `aeroftp-speedtest-${Date.now()}.json`);
    }, [buildCtx, buildOutcomes]);

    const exportCsv = useCallback(() => {
        const outcomes = buildOutcomes();
        if (outcomes.length === 0) return;
        downloadCsvReport(outcomes, `aeroftp-speedtest-${Date.now()}.csv`);
    }, [buildOutcomes]);

    const exportMd = useCallback(() => {
        const outcomes = buildOutcomes();
        if (outcomes.length === 0) return;
        downloadMarkdownReport(outcomes, buildCtx(), `aeroftp-speedtest-${Date.now()}.md`);
    }, [buildCtx, buildOutcomes]);

    const [exportMenuOpen, setExportMenuOpen] = useState(false);
    const exportMenuRef = useRef<HTMLDivElement>(null);
    useEffect(() => {
        if (!exportMenuOpen) return;
        const onDocClick = (e: MouseEvent) => {
            if (exportMenuRef.current && !exportMenuRef.current.contains(e.target as Node)) {
                setExportMenuOpen(false);
            }
        };
        document.addEventListener('mousedown', onDocClick);
        return () => document.removeEventListener('mousedown', onDocClick);
    }, [exportMenuOpen]);

    // Esc closes the dialog when idle, cancels when running
    useEffect(() => {
        const onKey = (e: KeyboardEvent) => {
            if (e.key !== 'Escape') return;
            if (anyRunning) {
                e.preventDefault();
                void handleCancel();
            } else {
                e.preventDefault();
                onClose();
            }
        };
        document.addEventListener('keydown', onKey);
        return () => document.removeEventListener('keydown', onKey);
    }, [anyRunning, handleCancel, onClose]);

    const selectedSizeLabel = formatBytes(effectiveSize);
    const totalPhasePct = phase === 'idle'
        ? 0
        : phase === 'done'
            ? 100
            : ((phaseIndex(phase) + 1) / phaseOrder.length) * 100;
    const transferPct = progress && progress.total_bytes > 0
        ? Math.min(100, (progress.transferred_bytes / progress.total_bytes) * 100)
        : totalPhasePct;
    const maxMbps = result ? Math.max(result.upload_mbps, result.download_mbps, 1) : 1;

    const rankedCompare = useMemo(
        () => (compareResult ? rankOutcomes(compareResult.results) : []),
        [compareResult],
    );

    const canSubmit = mode === 'single'
        ? Boolean(selectedServer)
        : compareSelected.size >= 2 && compareSelected.size <= 8;

    const renderTabs = () => (
        <div className="px-5 pt-4">
            <div className="inline-flex items-center bg-gray-100 dark:bg-gray-900 rounded-lg p-1">
                <button
                    onClick={() => setMode('single')}
                    disabled={anyRunning}
                    className={`px-3 py-1.5 text-xs font-medium rounded-md flex items-center gap-1.5 transition-colors ${mode === 'single' ? 'bg-white dark:bg-gray-700 shadow-sm' : 'text-gray-500 hover:text-gray-700 dark:hover:text-gray-200'}`}
                >
                    <Gauge size={13} />
                    {t('speedTest.tabSingle')}
                </button>
                <button
                    onClick={() => setMode('compare')}
                    disabled={anyRunning}
                    className={`px-3 py-1.5 text-xs font-medium rounded-md flex items-center gap-1.5 transition-colors ${mode === 'compare' ? 'bg-white dark:bg-gray-700 shadow-sm' : 'text-gray-500 hover:text-gray-700 dark:hover:text-gray-200'}`}
                >
                    <Layers size={13} />
                    {t('speedTest.tabCompare')}
                </button>
            </div>
        </div>
    );

    const renderSizeSelector = () => (
        <div>
            <div className="text-xs font-medium text-gray-500 mb-2">{t('speedTest.selectSize')}</div>
            <div className="grid grid-cols-3 gap-2">
                {SPEEDTEST_SIZES.map(size => (
                    <button
                        key={size.bytes}
                        onClick={() => { setUseExpertSize(false); setSizeBytes(size.bytes); setExpertConfirmed(false); }}
                        disabled={anyRunning}
                        className={`h-10 px-2 rounded-lg text-xs font-medium border transition-colors ${
                            !useExpertSize && sizeBytes === size.bytes
                                ? 'bg-indigo-600 border-indigo-600 text-white'
                                : 'bg-gray-50 dark:bg-gray-900 border-gray-200 dark:border-gray-700 hover:border-indigo-300'
                        }`}
                    >
                        {t(size.labelKey)}
                    </button>
                ))}
            </div>
            <div className="mt-2">
                <label className={`flex items-center gap-2 text-xs ${anyRunning ? 'opacity-60' : ''}`}>
                    <input
                        type="checkbox"
                        checked={useExpertSize}
                        onChange={e => { setUseExpertSize(e.target.checked); if (!e.target.checked) setExpertConfirmed(false); }}
                        disabled={anyRunning}
                    />
                    <span className="font-medium">{t('speedTest.expertSize')}</span>
                </label>
                {useExpertSize && (
                    <div className="mt-2 flex items-center gap-2">
                        <input
                            type="number"
                            min={1}
                            max={EXPERT_MAX / (1024 * 1024)}
                            value={expertSizeMb}
                            onChange={e => setExpertSizeMb(Number(e.target.value) || 1)}
                            disabled={anyRunning}
                            className="w-24 h-8 px-2 rounded-lg bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 text-xs tabular-nums"
                        />
                        <span className="text-xs text-gray-500">MB ({t('speedTest.expertRangeHint')})</span>
                    </div>
                )}
            </div>
            {sizeRequiresConfirm && (
                <label className="mt-2 flex items-start gap-2 rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 px-3 py-2 text-xs cursor-pointer">
                    <input
                        type="checkbox"
                        checked={expertConfirmed}
                        onChange={e => setExpertConfirmed(e.target.checked)}
                        disabled={anyRunning}
                        className="mt-0.5"
                    />
                    <span className="text-amber-800 dark:text-amber-200 leading-5">
                        {t('speedTest.expertConfirm', { size: selectedSizeLabel, total: formatBytes(effectiveSize * 2) })}
                    </span>
                </label>
            )}
            <label className={`mt-2 flex items-center gap-2 text-xs ${anyRunning ? 'opacity-60' : ''}`}>
                <input
                    type="checkbox"
                    checked={verifyIntegrity}
                    onChange={e => setVerifyIntegrity(e.target.checked)}
                    disabled={anyRunning}
                />
                <span>{t('speedTest.integrityToggle')}</span>
            </label>
        </div>
    );

    const renderSingleControls = () => (
        <div className="px-5 py-4 space-y-4">
            <div className="grid grid-cols-1 sm:grid-cols-2 gap-3">
                <label className="block">
                    <span className="block text-xs font-medium text-gray-500 mb-1.5">{t('speedTest.selectServer')}</span>
                    <select
                        value={selectedServerId}
                        onChange={(e) => setSelectedServerId(e.target.value)}
                        disabled={anyRunning}
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
                        disabled={anyRunning}
                        placeholder="/"
                        title={t('speedTest.remoteDirHint')}
                        className="w-full h-9 px-3 rounded-lg bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 text-sm font-mono focus:outline-none focus:ring-2 focus:ring-indigo-500/30"
                    />
                </label>
            </div>

            {renderSizeSelector()}

            {historySummary && historySummary.last && (
                <div className="rounded-lg border border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900/50 px-3 py-2 text-xs flex items-center justify-between">
                    <div className="flex items-center gap-2">
                        <History size={13} className="text-gray-400" />
                        <span className="text-gray-500">{t('speedTest.historyTitle')}</span>
                        <span className="tabular-nums">
                            {t('speedTest.historyLast', { mbps: formatMbps((historySummary.last.download_bytes_per_sec * 8) / 1_000_000) })}
                        </span>
                        {historySummary.best_download && (
                            <span className="tabular-nums text-gray-500">
                                · {t('speedTest.historyBest', { mbps: formatMbps((historySummary.best_download.download_bytes_per_sec * 8) / 1_000_000) })}
                            </span>
                        )}
                    </div>
                    {historySummary.regression_warning && (
                        <span className="text-amber-600 dark:text-amber-400 flex items-center gap-1">
                            <AlertTriangle size={12} />
                            {t('speedTest.regressionWarning')}
                        </span>
                    )}
                </div>
            )}

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

                    <div className="grid grid-cols-2 sm:grid-cols-5 gap-3 py-4 text-sm">
                        <div>
                            <div className="text-[11px] text-gray-500">{t('speedTest.totalTime')}</div>
                            <div className="font-medium tabular-nums">{formatDuration(result.upload_duration_ms + result.download_duration_ms)}</div>
                        </div>
                        <div title={t('speedTest.ttfbHint')}>
                            <div className="text-[11px] text-gray-500">{t('speedTest.ttfb')}</div>
                            <div className="font-medium tabular-nums">{result.download_ttfb_ms != null ? `${result.download_ttfb_ms} ms` : '—'}</div>
                        </div>
                        <div>
                            <div className="text-[11px] text-gray-500">{t('speedTest.integrity')}</div>
                            <div className={`font-medium flex items-center gap-1 ${
                                !result.integrity_checked ? 'text-gray-500' : result.integrity_verified ? 'text-emerald-600' : 'text-red-600'
                            }`}>
                                <ShieldCheck size={14} />
                                {!result.integrity_checked
                                    ? t('speedTest.integritySkipped')
                                    : result.integrity_verified
                                        ? t('speedTest.integrityVerified')
                                        : t('speedTest.integrityCorrupted')}
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
    );

    const renderCompareControls = () => (
        <div className="px-5 py-4 space-y-4">
            <div>
                <div className="flex items-center justify-between mb-2">
                    <span className="text-xs font-medium text-gray-500">
                        {t('speedTest.compareSelectServers', { count: compareSelected.size })}
                    </span>
                    <div className="flex items-center gap-2 text-xs">
                        <button
                            type="button"
                            onClick={() => setCompareSelected(new Set(supportedServers.slice(0, 8).map(s => s.id)))}
                            disabled={anyRunning}
                            className="text-indigo-600 hover:underline disabled:opacity-50"
                        >
                            {t('speedTest.selectAll')}
                        </button>
                        <span className="text-gray-300">·</span>
                        <button
                            type="button"
                            onClick={() => setCompareSelected(new Set())}
                            disabled={anyRunning}
                            className="text-indigo-600 hover:underline disabled:opacity-50"
                        >
                            {t('speedTest.clear')}
                        </button>
                    </div>
                </div>
                <div className="rounded-lg border border-gray-200 dark:border-gray-700 max-h-56 overflow-auto divide-y divide-gray-100 dark:divide-gray-700">
                    {supportedServers.map(server => {
                        const checked = compareSelected.has(server.id);
                        return (
                            <label
                                key={server.id}
                                className={`flex items-center gap-2 px-3 py-2 text-sm cursor-pointer hover:bg-gray-50 dark:hover:bg-gray-900/40 ${checked ? 'bg-indigo-50/40 dark:bg-indigo-900/10' : ''}`}
                            >
                                <input
                                    type="checkbox"
                                    checked={checked}
                                    onChange={() => toggleCompareServer(server.id)}
                                    disabled={anyRunning}
                                />
                                <span className="flex-1 truncate">{server.name || server.host}</span>
                                <span className="text-[11px] text-gray-500">{(server.protocol || 'ftp').toUpperCase()}</span>
                            </label>
                        );
                    })}
                </div>
                <p className="mt-1 text-[11px] text-gray-500">{t('speedTest.compareHint')}</p>
            </div>

            <div className="grid grid-cols-2 gap-3">
                <label className="block">
                    <span className="block text-xs font-medium text-gray-500 mb-1.5">{t('speedTest.parallelLabel')}</span>
                    <select
                        value={compareParallel}
                        onChange={(e) => setCompareParallel(Number(e.target.value))}
                        disabled={anyRunning}
                        className="w-full h-9 px-3 rounded-lg bg-gray-50 dark:bg-gray-900 border border-gray-200 dark:border-gray-700 text-sm focus:outline-none focus:ring-2 focus:ring-indigo-500/30"
                    >
                        {[1, 2, 3, 4].map(n => <option key={n} value={n}>{n}</option>)}
                    </select>
                </label>
                <div className="flex items-end text-xs text-gray-500">
                    <span>{t('speedTest.parallelHint')}</span>
                </div>
            </div>

            {renderSizeSelector()}

            {!compareRunning && !compareResult && (
                <div className="flex gap-3 rounded-lg border border-amber-200 dark:border-amber-800 bg-amber-50 dark:bg-amber-900/20 px-3 py-2.5">
                    <AlertTriangle size={16} className="text-amber-600 dark:text-amber-400 mt-0.5 flex-shrink-0" />
                    <p className="text-xs text-amber-800 dark:text-amber-200 leading-5">
                        {t('speedTest.compareDisclaimer', {
                            count: compareSelected.size || 0,
                            size: selectedSizeLabel,
                            total: formatBytes(effectiveSize * 2 * Math.max(compareSelected.size, 1)),
                        })}
                    </p>
                </div>
            )}

            {compareRunning && (
                <div className="py-2">
                    <div className="flex items-center gap-2 text-sm font-medium mb-2">
                        <RefreshCw size={15} className="animate-spin text-indigo-500" />
                        <span>{t('speedTest.compareRunning', { count: compareSelected.size })}</span>
                    </div>
                    <div className="space-y-1">
                        {Array.from(compareTestIdsRef.current.entries()).map(([testId, meta]) => (
                            <PerServerProgress
                                key={testId}
                                name={meta.serverName}
                                protocol={meta.protocol}
                                progress={compareProgress.get(testId) || null}
                                result={null}
                                error={null}
                                sizeBytes={effectiveSize}
                            />
                        ))}
                    </div>
                    <p className="mt-2 text-[11px] text-gray-500">{t('speedTest.cancelHint')}</p>
                </div>
            )}

            {compareError && (
                <div className="flex gap-2 rounded-lg border border-red-200 dark:border-red-800 bg-red-50 dark:bg-red-900/20 px-3 py-2.5 text-sm text-red-700 dark:text-red-300">
                    <AlertTriangle size={16} className="mt-0.5 flex-shrink-0" />
                    <span>{compareError}</span>
                </div>
            )}

            {compareResult && rankedCompare.length > 0 && (
                <div className="rounded-lg border border-gray-200 dark:border-gray-700 overflow-hidden">
                    <table className="w-full text-xs">
                        <thead className="bg-gray-50 dark:bg-gray-900/60">
                            <tr className="text-left text-gray-500">
                                <th className="px-3 py-2 font-medium">#</th>
                                <th className="px-3 py-2 font-medium">{t('speedTest.colServer')}</th>
                                <th className="px-3 py-2 font-medium">{t('speedTest.colProtocol')}</th>
                                <th className="px-3 py-2 font-medium text-right">{t('speedTest.colDownload')}</th>
                                <th className="px-3 py-2 font-medium text-right">{t('speedTest.colUpload')}</th>
                                <th className="px-3 py-2 font-medium text-right">{t('speedTest.colTotal')}</th>
                                <th className="px-3 py-2 font-medium text-center">{t('speedTest.colIntegrity')}</th>
                                <th className="px-3 py-2 font-medium text-center">{t('speedTest.colCleanup')}</th>
                                <th className="px-3 py-2 font-medium text-right">{t('speedTest.colScore')}</th>
                            </tr>
                        </thead>
                        <tbody className="divide-y divide-gray-100 dark:divide-gray-700">
                            {rankedCompare.map(o => {
                                if (!o.result) {
                                    return (
                                        <tr key={o.test_id} className="bg-red-50/40 dark:bg-red-900/10">
                                            <td className="px-3 py-2 text-gray-400">—</td>
                                            <td className="px-3 py-2 truncate">{o.server_name || '?'}</td>
                                            <td className="px-3 py-2 text-gray-400" colSpan={7}>
                                                {o.error || t('speedTest.errorLabel')}
                                            </td>
                                        </tr>
                                    );
                                }
                                const r = o.result;
                                return (
                                    <tr key={o.test_id} className={o.rank === 1 ? 'bg-emerald-50/30 dark:bg-emerald-900/10' : ''}>
                                        <td className="px-3 py-2 font-medium tabular-nums">{o.rank}</td>
                                        <td className="px-3 py-2 truncate" title={o.server_name || ''}>{o.server_name || '?'}</td>
                                        <td className="px-3 py-2 uppercase text-gray-500">{r.protocol}</td>
                                        <td className="px-3 py-2 text-right tabular-nums">{formatMbps(r.download_mbps)}<span className="text-gray-400 ml-1">Mbps</span></td>
                                        <td className="px-3 py-2 text-right tabular-nums">{formatMbps(r.upload_mbps)}<span className="text-gray-400 ml-1">Mbps</span></td>
                                        <td className="px-3 py-2 text-right tabular-nums">{formatDuration(r.upload_duration_ms + r.download_duration_ms)}</td>
                                        <td className="px-3 py-2 text-center">
                                            {!r.integrity_checked
                                                ? <span className="text-gray-400" title={t('speedTest.integritySkipped')}>—</span>
                                                : r.integrity_verified
                                                    ? <ShieldCheck size={13} className="inline text-emerald-500" />
                                                    : <AlertTriangle size={13} className="inline text-red-500" />}
                                        </td>
                                        <td className="px-3 py-2 text-center">
                                            {r.temp_file_cleaned
                                                ? <Check size={13} className="inline text-emerald-500" />
                                                : <Trash2 size={13} className="inline text-amber-500" />}
                                        </td>
                                        <td className="px-3 py-2 text-right tabular-nums font-medium">{(o.score * 100).toFixed(0)}</td>
                                    </tr>
                                );
                            })}
                        </tbody>
                    </table>
                </div>
            )}
        </div>
    );

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/50 backdrop-blur-sm animate-fade-in"
            onClick={(e) => { if (e.target === e.currentTarget && !anyRunning) onClose(); }}
        >
            <div
                className="w-full max-w-3xl bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in max-h-[90vh] flex flex-col"
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
                        onClick={anyRunning ? handleCancel : onClose}
                        className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                        title={anyRunning ? t('speedTest.cancel') : t('common.close')}
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
                        {renderTabs()}
                        <div className="flex-1 overflow-auto">
                            {mode === 'single' ? renderSingleControls() : renderCompareControls()}
                        </div>

                        <div className="px-5 py-3 border-t border-gray-200 dark:border-gray-700 flex items-center justify-between gap-3">
                            <div className="flex items-center gap-2 text-[11px] text-gray-500">
                                <Zap size={13} />
                                <span>
                                    {mode === 'single'
                                        ? (selectedServer ? `${(selectedServer.protocol || 'ftp').toUpperCase()} - ${selectedServer.host}` : '-')
                                        : t('speedTest.compareFooter', { count: compareSelected.size })}
                                </span>
                            </div>
                            <div className="flex items-center gap-2">
                                {(result || compareResult) && (
                                    <div className="relative" ref={exportMenuRef}>
                                        <button
                                            onClick={() => setExportMenuOpen(o => !o)}
                                            className="px-3 py-1.5 text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors flex items-center gap-1.5"
                                        >
                                            {copied ? <Check size={13} className="text-emerald-500" /> : <FileJson size={13} />}
                                            {copied ? t('speedTest.copied') : t('speedTest.exportReport')}
                                        </button>
                                        {exportMenuOpen && (
                                            <div className="absolute bottom-full right-0 mb-1 w-56 rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800 shadow-lg overflow-hidden text-xs z-10">
                                                <button
                                                    className="w-full text-left px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/60 flex items-center gap-2"
                                                    onClick={() => { copyResult(); setExportMenuOpen(false); }}
                                                >
                                                    <Copy size={12} /> {t('speedTest.copyReport')}
                                                </button>
                                                <button
                                                    className="w-full text-left px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/60 flex items-center gap-2"
                                                    onClick={() => { copyMarkdown(); setExportMenuOpen(false); }}
                                                >
                                                    <Copy size={12} /> {t('speedTest.copyMarkdown')}
                                                </button>
                                                <div className="border-t border-gray-100 dark:border-gray-700" />
                                                <button
                                                    className="w-full text-left px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/60 flex items-center gap-2"
                                                    onClick={() => { exportJson(); setExportMenuOpen(false); }}
                                                >
                                                    <FileJson size={12} /> {t('speedTest.downloadJson')}
                                                </button>
                                                <button
                                                    className="w-full text-left px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/60 flex items-center gap-2"
                                                    onClick={() => { exportCsv(); setExportMenuOpen(false); }}
                                                >
                                                    <FileJson size={12} /> {t('speedTest.downloadCsv')}
                                                </button>
                                                <button
                                                    className="w-full text-left px-3 py-2 hover:bg-gray-50 dark:hover:bg-gray-700/60 flex items-center gap-2"
                                                    onClick={() => { exportMd(); setExportMenuOpen(false); }}
                                                >
                                                    <FileJson size={12} /> {t('speedTest.downloadMarkdown')}
                                                </button>
                                            </div>
                                        )}
                                    </div>
                                )}
                                <button
                                    onClick={
                                        anyRunning
                                            ? handleCancel
                                            : (result || compareResult)
                                                ? (mode === 'single' ? handleRun : handleRunCompare)
                                                : onClose
                                    }
                                    className="px-3 py-1.5 text-xs font-medium bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 rounded-lg transition-colors"
                                >
                                    {anyRunning
                                        ? t('speedTest.cancel')
                                        : (result || compareResult)
                                            ? t('speedTest.runAgain')
                                            : t('common.close')}
                                </button>
                                {!result && !compareResult && !anyRunning && (
                                    <button
                                        onClick={mode === 'single' ? handleRun : handleRunCompare}
                                        disabled={!canSubmit}
                                        className="px-3 py-1.5 text-xs font-medium bg-indigo-600 hover:bg-indigo-700 disabled:bg-indigo-400 disabled:cursor-not-allowed text-white rounded-lg transition-colors flex items-center gap-1.5"
                                    >
                                        <Gauge size={13} />
                                        {mode === 'single' ? t('speedTest.start') : t('speedTest.startCompare')}
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
