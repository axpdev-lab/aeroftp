import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { X, Wifi, Activity, Monitor, ScrollText, Layout, Copy, Trash2, Pause, Play } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { EffectiveTheme } from '../hooks/useTheme';

// ─── Shared timestamp helper ───────────────────────────────────────────────
function ts() {
    return new Date().toLocaleTimeString('en-US', { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

// ─── Global console capture (singleton, survives mount/unmount) ───────────
interface CapturedLog {
    id: number;
    timestamp: string;
    level: 'DEBUG' | 'INFO' | 'WARN' | 'ERROR' | 'TRACE';
    message: string;
}

const globalLogBuffer: CapturedLog[] = [];
let globalLogId = 0;
let globalCaptureActive = false;
const globalLogListeners = new Set<() => void>();

function activateGlobalCapture() {
    if (globalCaptureActive) return;
    globalCaptureActive = true;

    const origLog = console.log;
    const origWarn = console.warn;
    const origError = console.error;
    const origDebug = console.debug;

    const addEntry = (level: CapturedLog['level'], args: unknown[]) => {
        const msg = args.map(a => typeof a === 'string' ? a : JSON.stringify(a)).join(' ');
        const entry: CapturedLog = { id: globalLogId++, timestamp: ts(), level, message: msg };
        globalLogBuffer.push(entry);
        if (globalLogBuffer.length > 500) globalLogBuffer.splice(0, globalLogBuffer.length - 500);
        queueMicrotask(() => globalLogListeners.forEach(fn => fn()));
    };

    console.log = (...args) => { origLog(...args); addEntry('INFO', args); };
    console.warn = (...args) => { origWarn(...args); addEntry('WARN', args); };
    console.error = (...args) => { origError(...args); addEntry('ERROR', args); };
    console.debug = (...args) => { origDebug(...args); addEntry('DEBUG', args); };
}

function clearGlobalLogs() {
    globalLogBuffer.length = 0;
    globalLogListeners.forEach(fn => fn());
}

// ─── Global network capture (transfer_event + invoke interceptor) ────────
interface NetworkEntry {
    id: number;
    timestamp: string;
    type: 'TRANSFER' | 'INVOKE' | 'EVENT';
    status: 'start' | 'progress' | 'complete' | 'error' | 'ok';
    command: string;
    detail: string;
    duration?: number;
}

const globalNetworkBuffer: NetworkEntry[] = [];
let globalNetworkId = 0;
let globalNetworkActive = false;
const globalNetworkListeners = new Set<() => void>();

function notifyNetworkListeners() {
    queueMicrotask(() => globalNetworkListeners.forEach(fn => fn()));
}

function addNetworkEntry(entry: Omit<NetworkEntry, 'id' | 'timestamp'>) {
    const e: NetworkEntry = { ...entry, id: globalNetworkId++, timestamp: ts() };
    globalNetworkBuffer.push(e);
    if (globalNetworkBuffer.length > 300) globalNetworkBuffer.splice(0, globalNetworkBuffer.length - 300);
    notifyNetworkListeners();
}

function clearGlobalNetwork() {
    globalNetworkBuffer.length = 0;
    notifyNetworkListeners();
}

// Commands to skip in invoke interceptor (too noisy / internal)
const INVOKE_SKIP = new Set([
    'get_system_info', 'plugin:event|listen', 'plugin:event|unlisten',
    'plugin:webview|get_all_webviews', 'tauri_invoke_handler',
]);

function activateNetworkCapture() {
    if (globalNetworkActive) return;
    globalNetworkActive = true;

    // 1) Listen for transfer_event from Rust backend
    listen<{ event_type: string; transfer_id: string; filename: string; direction: string; message?: string }>('transfer_event', (event) => {
        const d = event.payload;
        const evType = d.event_type.toLowerCase();
        const status: NetworkEntry['status'] = evType.includes('error') ? 'error'
            : evType.includes('complete') || evType.includes('done') ? 'complete'
            : evType.includes('start') || evType.includes('begin') ? 'start'
            : 'progress';
        addNetworkEntry({
            type: 'TRANSFER',
            status,
            command: `${d.direction} ${d.event_type}`,
            detail: `${d.filename}${d.message ? ` — ${d.message}` : ''}`,
        });
    });

    // 2) Intercept __TAURI_INTERNALS__.invoke to log all IPC calls with timing
    const internals = (window as any).__TAURI_INTERNALS__;
    if (internals && !internals.__debugPatched) {
        const origFn = internals.invoke.bind(internals);
        internals.__debugPatched = true;
        internals.invoke = async (cmd: string, args?: any, options?: any) => {
            if (INVOKE_SKIP.has(cmd)) return origFn(cmd, args, options);
            const t0 = performance.now();
            const argSummary = args ? Object.keys(args).filter((k: string) => k !== 'appWindow' && k !== '__invokeKey').join(',') : '';
            addNetworkEntry({
                type: 'INVOKE',
                status: 'start',
                command: cmd,
                detail: argSummary ? `args: ${argSummary}` : '',
            });
            try {
                const result = await origFn(cmd, args, options);
                const dur = Math.round(performance.now() - t0);
                addNetworkEntry({
                    type: 'INVOKE',
                    status: 'ok',
                    command: cmd,
                    detail: `${dur}ms`,
                    duration: dur,
                });
                return result;
            } catch (err: any) {
                const dur = Math.round(performance.now() - t0);
                addNetworkEntry({
                    type: 'INVOKE',
                    status: 'error',
                    command: cmd,
                    detail: `${dur}ms — ${String(err).slice(0, 120)}`,
                    duration: dur,
                });
                throw err;
            }
        };
    }
}

interface SystemInfo {
    app_version: string;
    os: string;
    os_version: string;
    arch: string;
    tauri_version: string;
    rust_version: string;
    keyring_backend: string;
    config_dir: string;
    vault_exists: boolean;
    known_hosts_exists: boolean;
}

type LogEntry = CapturedLog;

// TransferEvent type removed — handled by global network capture

type TabId = 'connection' | 'network' | 'system' | 'logs' | 'frontend';

const TAB_IDS: { id: TabId; icon: React.ReactNode }[] = [
    { id: 'connection', icon: <Wifi size={13} /> },
    { id: 'network', icon: <Activity size={13} /> },
    { id: 'system', icon: <Monitor size={13} /> },
    { id: 'logs', icon: <ScrollText size={13} /> },
    { id: 'frontend', icon: <Layout size={13} /> },
];

const InfoRow: React.FC<{ label: string; value: string | React.ReactNode; mono?: boolean }> = ({ label, value, mono }) => (
    <div className="flex items-start py-1 px-3 border-b border-gray-100 dark:border-gray-700/50 last:border-0">
        <span className="text-xs text-gray-500 dark:text-gray-400 w-40 shrink-0">{label}</span>
        <span className={`text-xs text-gray-800 dark:text-gray-200 ${mono ? 'font-mono' : ''}`}>{value}</span>
    </div>
);

const StatusDot: React.FC<{ active: boolean }> = ({ active }) => (
    <span className={`inline-block w-2 h-2 rounded-full ${active ? 'bg-green-500' : 'bg-gray-400'}`} />
);

interface DebugPanelProps {
    isVisible: boolean;
    onClose: () => void;
    isConnected: boolean;
    connectionParams: { server: string; username: string; protocol?: string };
    currentRemotePath: string;
    appTheme?: EffectiveTheme;
}

const DebugPanel: React.FC<DebugPanelProps> = ({
    isVisible,
    onClose,
    isConnected,
    connectionParams,
    currentRemotePath,
    appTheme = 'dark',
}) => {
    const t = useTranslation();
    const resizeTheme = useMemo(() => {
        switch (appTheme) {
            case 'light': return { base: 'bg-gray-300 hover:bg-blue-500', bar: 'bg-gray-400 group-hover:bg-white' };
            case 'tokyo': return { base: 'bg-[#292e42] hover:bg-[#7aa2f7]', bar: 'bg-[#414868] group-hover:bg-[#7aa2f7]' };
            case 'cyber': return { base: 'bg-[#0d1117] hover:bg-emerald-500', bar: 'bg-emerald-800/60 group-hover:bg-emerald-400' };
            default: return { base: 'bg-gray-700 hover:bg-blue-500', bar: 'bg-gray-500 group-hover:bg-blue-400' };
        }
    }, [appTheme]);
    const [activeTab, setActiveTab] = useState<TabId>('connection');
    const [height, setHeight] = useState(320);
    const [systemInfo, setSystemInfo] = useState<SystemInfo | null>(null);
    const [logs, setLogs] = useState<LogEntry[]>([]);
    const [logFilter, setLogFilter] = useState<string>('ALL');
    const [logPaused, setLogPaused] = useState(false);
    const [networkEvents, setNetworkEvents] = useState<NetworkEntry[]>([]);
    const [connectTime] = useState(() => isConnected ? new Date() : null);
    const logEndRef = useRef<HTMLDivElement>(null);
    const networkEndRef = useRef<HTMLDivElement>(null);
    const resizeRef = useRef<HTMLDivElement>(null);

    // Load system info
    useEffect(() => {
        if (!isVisible) return;
        (async () => {
            try {
                const info: SystemInfo = await invoke('get_system_info');
                setSystemInfo(info);
            } catch (e) {
                console.error('Failed to load system info:', e);
            }
        })();
    }, [isVisible]);

    // Activate global captures on first mount and subscribe to updates
    useEffect(() => {
        activateGlobalCapture();
        activateNetworkCapture();

        setLogs([...globalLogBuffer]);
        setNetworkEvents([...globalNetworkBuffer]);

        const logListener = () => {
            if (!pausedRef.current) setLogs([...globalLogBuffer]);
        };
        const netListener = () => {
            if (!pausedRef.current) setNetworkEvents([...globalNetworkBuffer]);
        };
        globalLogListeners.add(logListener);
        globalNetworkListeners.add(netListener);
        return () => {
            globalLogListeners.delete(logListener);
            globalNetworkListeners.delete(netListener);
        };
    }, []);

    // Track logPaused via ref so listener closure stays current
    const pausedRef = useRef(logPaused);
    useEffect(() => { pausedRef.current = logPaused; }, [logPaused]);

    // Auto-scroll logs + network
    useEffect(() => {
        if (!logPaused && activeTab === 'logs') {
            logEndRef.current?.scrollIntoView({ behavior: 'smooth' });
        }
        if (!logPaused && activeTab === 'network') {
            networkEndRef.current?.scrollIntoView({ behavior: 'smooth' });
        }
    }, [logs, networkEvents, logPaused, activeTab]);

    // Resize handle
    const handleResize = useCallback((e: React.MouseEvent) => {
        e.preventDefault();
        const startY = e.clientY;
        const startH = height;
        const onMove = (ev: MouseEvent) => {
            const newH = Math.max(150, Math.min(600, startH - (ev.clientY - startY)));
            setHeight(newH);
        };
        const onUp = () => { document.removeEventListener('mousemove', onMove); document.removeEventListener('mouseup', onUp); };
        document.addEventListener('mousemove', onMove);
        document.addEventListener('mouseup', onUp);
    }, [height]);

    const copyLogs = useCallback(() => {
        const text = logs.map(l => `[${l.timestamp}] [${l.level}] ${l.message}`).join('\n');
        navigator.clipboard.writeText(text);
    }, [logs]);

    if (!isVisible) return null;

    const levelColor: Record<string, string> = {
        DEBUG: 'text-gray-500',
        INFO: 'text-blue-500',
        WARN: 'text-yellow-500',
        ERROR: 'text-red-500',
        TRACE: 'text-gray-400',
    };

    const uptime = connectTime ? Math.floor((Date.now() - connectTime.getTime()) / 1000) : 0;
    const uptimeStr = connectTime
        ? `${Math.floor(uptime / 3600)}h ${Math.floor((uptime % 3600) / 60)}m ${uptime % 60}s`
        : '—';

    // Frontend tab stats
    const localStorageSize = (() => {
        let size = 0;
        for (let i = 0; i < localStorage.length; i++) {
            const key = localStorage.key(i);
            if (key) size += (localStorage.getItem(key)?.length || 0) * 2; // UTF-16
        }
        return (size / 1024).toFixed(1);
    })();

    return (
        <div className="border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-900 flex flex-col shrink-0" style={{ height }}>
            {/* Resize handle */}
            <div
                ref={resizeRef}
                onMouseDown={handleResize}
                className={`h-2 cursor-ns-resize ${resizeTheme.base} transition-colors flex-shrink-0 flex items-center justify-center group`}
            >
                <div className={`w-10 h-0.5 rounded-full ${resizeTheme.bar} transition-colors`} />
            </div>

            {/* Header with tabs */}
            <div className="flex items-center justify-between px-3 py-1 border-b border-gray-200 dark:border-gray-700 bg-gray-100 dark:bg-gray-800">
                <div className="flex items-center gap-1">
                    {TAB_IDS.map(tab => (
                        <button
                            key={tab.id}
                            onClick={() => setActiveTab(tab.id)}
                            className={`flex items-center gap-1 px-2.5 py-1 rounded text-xs transition-colors ${
                                activeTab === tab.id
                                    ? 'bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-400 font-medium'
                                    : 'text-gray-600 dark:text-gray-400 hover:bg-gray-200 dark:hover:bg-gray-700'
                            }`}
                        >
                            {tab.icon}
                            {t(`debug.tabs.${tab.id}`)}
                        </button>
                    ))}
                </div>
                <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500">
                    <X size={14} />
                </button>
            </div>

            {/* Tab content */}
            <div className="flex-1 overflow-y-auto text-xs">
                {/* Connection Tab */}
                {activeTab === 'connection' && (
                    <div className="p-2">
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700">
                            <InfoRow label={t('debug.connection.status')} value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={isConnected} />
                                    {isConnected ? t('debug.connection.connected') : t('debug.connection.disconnected')}
                                </span>
                            } />
                            <InfoRow label={t('debug.connection.protocol')} value={connectionParams.protocol?.toUpperCase() || '—'} mono />
                            <InfoRow label={t('debug.connection.server')} value={connectionParams.server || '—'} mono />
                            <InfoRow label={t('debug.connection.username')} value={connectionParams.username || '—'} mono />
                            <InfoRow label={t('debug.connection.remotePath')} value={currentRemotePath || '/'} mono />
                            <InfoRow label={t('debug.connection.uptime')} value={uptimeStr} mono />
                            <InfoRow label={t('debug.connection.credentialStorage')} value={systemInfo?.keyring_backend || t('common.loading')} />
                            <InfoRow label={t('debug.connection.vaultFile')} value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.vault_exists || false} />
                                    {systemInfo?.vault_exists ? t('debug.connection.present') : t('debug.connection.notCreated')}
                                </span>
                            } />
                            <InfoRow label={t('debug.connection.knownHosts')} value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.known_hosts_exists || false} />
                                    {systemInfo?.known_hosts_exists ? t('debug.connection.present') : t('debug.connection.notCreated')}
                                </span>
                            } />
                        </div>
                    </div>
                )}

                {/* Network Tab */}
                {activeTab === 'network' && (
                    <div className="p-2">
                        <div className="flex items-center justify-between mb-2">
                            <span className="text-xs text-gray-500">
                                IPC + Transfers ({networkEvents.length})
                            </span>
                            <button
                                onClick={() => { clearGlobalNetwork(); setNetworkEvents([]); }}
                                className="flex items-center gap-1 text-xs px-2 py-0.5 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500"
                            >
                                <Trash2 size={11} /> {t('debug.network.clear')}
                            </button>
                        </div>
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700 overflow-y-auto" style={{ maxHeight: height - 80 }}>
                            {networkEvents.length === 0 ? (
                                <div className="p-4 text-center text-gray-400">{t('debug.network.noActivity')}</div>
                            ) : (
                                <table className="w-full">
                                    <thead className="sticky top-0 bg-gray-50 dark:bg-gray-800">
                                        <tr className="text-[10px] text-gray-500 border-b border-gray-200 dark:border-gray-700">
                                            <th className="text-left py-1 px-2 w-16">{t('debug.network.time')}</th>
                                            <th className="text-left py-1 px-2 w-16">Type</th>
                                            <th className="text-left py-1 px-2 w-16">Status</th>
                                            <th className="text-left py-1 px-2 w-48">Command</th>
                                            <th className="text-left py-1 px-2">{t('debug.network.detail')}</th>
                                        </tr>
                                    </thead>
                                    <tbody className="font-mono text-[11px]">
                                        {networkEvents.map(evt => (
                                            <tr key={evt.id} className="border-b border-gray-50 dark:border-gray-700/30">
                                                <td className="py-0.5 px-2 text-gray-400 whitespace-nowrap">{evt.timestamp}</td>
                                                <td className="py-0.5 px-2">
                                                    <span className={`px-1 py-0.5 rounded text-[9px] font-semibold ${
                                                        evt.type === 'TRANSFER' ? 'bg-purple-100 text-purple-700 dark:bg-purple-900/30 dark:text-purple-400' :
                                                        evt.type === 'INVOKE' ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400' :
                                                        'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                                    }`}>
                                                        {evt.type}
                                                    </span>
                                                </td>
                                                <td className="py-0.5 px-2">
                                                    <span className={`px-1 py-0.5 rounded text-[9px] font-semibold ${
                                                        evt.status === 'error' ? 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400' :
                                                        evt.status === 'complete' || evt.status === 'ok' ? 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400' :
                                                        evt.status === 'start' ? 'bg-amber-100 text-amber-700 dark:bg-amber-900/30 dark:text-amber-400' :
                                                        'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                                    }`}>
                                                        {evt.status}
                                                    </span>
                                                </td>
                                                <td className="py-0.5 px-2 text-gray-700 dark:text-gray-200 truncate max-w-[200px]">{evt.command}</td>
                                                <td className="py-0.5 px-2 text-gray-500 dark:text-gray-400 truncate max-w-md">{evt.detail}</td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            )}
                            <div ref={networkEndRef} />
                        </div>
                    </div>
                )}

                {/* System Tab */}
                {activeTab === 'system' && (
                    <div className="p-2">
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700">
                            <InfoRow label={t('debug.system.appVersion')} value={systemInfo?.app_version || '...'} mono />
                            <InfoRow label={t('debug.system.os')} value={`${systemInfo?.os || '...'} (${systemInfo?.arch || '...'})`} mono />
                            <InfoRow label={t('debug.system.tauriVersion')} value={systemInfo?.tauri_version || '...'} mono />
                            <InfoRow label={t('debug.system.rustToolchain')} value={systemInfo?.rust_version || '...'} mono />
                            <InfoRow label={t('debug.system.keyringBackend')} value={systemInfo?.keyring_backend || '...'} />
                            <InfoRow label={t('debug.system.configDir')} value={systemInfo?.config_dir || '...'} mono />
                            <InfoRow label={t('debug.system.vault')} value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.vault_exists || false} />
                                    {systemInfo?.vault_exists ? t('debug.system.vaultExists') : t('debug.system.notCreated')}
                                </span>
                            } />
                            <InfoRow label={t('debug.system.knownHosts')} value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.known_hosts_exists || false} />
                                    {systemInfo?.known_hosts_exists ? t('debug.system.knownHostsExists') : t('debug.system.notCreated')}
                                </span>
                            } />
                            <InfoRow label={t('debug.system.snapPackage')} value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={!!(window as any).__TAURI_INTERNALS__} />
                                    {typeof window !== 'undefined' ? t('debug.system.tauriRuntime') : t('debug.system.browser')}
                                </span>
                            } />
                        </div>
                    </div>
                )}

                {/* Logs Tab */}
                {activeTab === 'logs' && (
                    <div className="p-2 flex flex-col" style={{ height: height - 60 }}>
                        <div className="flex items-center justify-between mb-2">
                            <div className="flex items-center gap-1">
                                <select
                                    value={logFilter}
                                    onChange={e => setLogFilter(e.target.value)}
                                    className="text-xs px-2 py-0.5 rounded border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800"
                                >
                                    <option value="ALL">{t('debug.logs.allLevels')}</option>
                                    <option value="ERROR">{t('debug.logs.error')}</option>
                                    <option value="WARN">{t('debug.logs.warning')}</option>
                                    <option value="INFO">{t('debug.logs.info')}</option>
                                    <option value="DEBUG">{t('debug.logs.debug')}</option>
                                </select>
                                <span className="text-gray-400 text-[10px]">{logs.length} {t('debug.logs.entries')}</span>
                            </div>
                            <div className="flex items-center gap-1">
                                <button onClick={() => setLogPaused(!logPaused)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500" title={logPaused ? t('debug.logs.resume') : t('debug.logs.pause')}>
                                    {logPaused ? <Play size={12} /> : <Pause size={12} />}
                                </button>
                                <button onClick={copyLogs} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500" title={t('debug.logs.copyAll')}>
                                    <Copy size={12} />
                                </button>
                                <button onClick={() => { clearGlobalLogs(); setLogs([]); }} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500" title={t('debug.logs.clear')}>
                                    <Trash2 size={12} />
                                </button>
                            </div>
                        </div>
                        <div className="flex-1 overflow-y-auto bg-gray-100 dark:bg-gray-900 rounded-lg p-2 font-mono text-[11px] leading-relaxed">
                            {logs
                                .filter(l => logFilter === 'ALL' || l.level === logFilter)
                                .map(l => (
                                    <div key={l.id} className="flex gap-2 hover:bg-gray-200/50 dark:hover:bg-gray-800/50">
                                        <span className="text-gray-500 dark:text-gray-600 shrink-0">{l.timestamp}</span>
                                        <span className={`shrink-0 w-12 text-right ${levelColor[l.level]}`}>{l.level}</span>
                                        <span className="text-gray-700 dark:text-gray-300 break-all">{l.message}</span>
                                    </div>
                                ))}
                            <div ref={logEndRef} />
                            {logs.length === 0 && (
                                <div className="text-gray-600 text-center py-4">{t('debug.logs.emptyMessage')}</div>
                            )}
                        </div>
                    </div>
                )}

                {/* Frontend Tab */}
                {activeTab === 'frontend' && (
                    <div className="p-2">
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700">
                            <InfoRow label={t('debug.frontend.reactMode')} value={React.version ? `React ${React.version}` : 'Unknown'} mono />
                            <InfoRow label={t('debug.frontend.language')} value={document.documentElement.lang || navigator.language} mono />
                            <InfoRow label={t('debug.frontend.localStorageKeys')} value={`${localStorage.length} ${t('debug.frontend.keys')}`} mono />
                            <InfoRow label={t('debug.frontend.localStorageSize')} value={`${localStorageSize} KB`} mono />
                            <InfoRow label={t('debug.frontend.windowSize')} value={`${window.innerWidth} x ${window.innerHeight}`} mono />
                            <InfoRow label={t('debug.frontend.devicePixelRatio')} value={`${window.devicePixelRatio}x`} mono />
                            <InfoRow label={t('debug.frontend.colorScheme')} value={window.matchMedia('(prefers-color-scheme: dark)').matches ? t('debug.frontend.dark') : t('debug.frontend.light')} />
                            <InfoRow label={t('debug.frontend.userAgent')} value={
                                <span className="break-all text-[10px]">{navigator.userAgent}</span>
                            } />
                        </div>

                        {/* localStorage keys list */}
                        <h4 className="text-xs text-gray-500 mt-3 mb-1 font-semibold">{t('debug.frontend.localStorageKeysTitle')}</h4>
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700 overflow-hidden">
                            {Array.from({ length: localStorage.length }).map((_, i) => {
                                const key = localStorage.key(i);
                                if (!key) return null;
                                const val = localStorage.getItem(key) || '';
                                return (
                                    <div key={key} className="flex items-center py-1 px-3 border-b border-gray-50 dark:border-gray-700/50 last:border-0">
                                        <span className="text-xs font-mono text-gray-700 dark:text-gray-300 w-48 shrink-0 truncate">{key}</span>
                                        <span className="text-[10px] text-gray-400 truncate">{val.length > 80 ? val.slice(0, 80) + '...' : val}</span>
                                    </div>
                                );
                            })}
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
};

export { activateGlobalCapture, activateNetworkCapture };
export default DebugPanel;
