import React, { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { X, Wifi, Activity, Monitor, ScrollText, Layout, ChevronDown, ChevronUp, Copy, Trash2, Pause, Play } from 'lucide-react';

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

interface LogEntry {
    id: number;
    timestamp: string;
    level: 'DEBUG' | 'INFO' | 'WARN' | 'ERROR' | 'TRACE';
    message: string;
    source?: string;
}

interface TransferEvent {
    event_type: string;
    transfer_id: string;
    filename: string;
    direction: string;
    message?: string;
}

type TabId = 'connection' | 'network' | 'system' | 'logs' | 'frontend';

const TABS: { id: TabId; label: string; icon: React.ReactNode }[] = [
    { id: 'connection', label: 'Connection', icon: <Wifi size={13} /> },
    { id: 'network', label: 'Network', icon: <Activity size={13} /> },
    { id: 'system', label: 'System', icon: <Monitor size={13} /> },
    { id: 'logs', label: 'Logs', icon: <ScrollText size={13} /> },
    { id: 'frontend', label: 'Frontend', icon: <Layout size={13} /> },
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
}

const DebugPanel: React.FC<DebugPanelProps> = ({
    isVisible,
    onClose,
    isConnected,
    connectionParams,
    currentRemotePath,
}) => {
    const [activeTab, setActiveTab] = useState<TabId>('connection');
    const [height, setHeight] = useState(320);
    const [systemInfo, setSystemInfo] = useState<SystemInfo | null>(null);
    const [logs, setLogs] = useState<LogEntry[]>([]);
    const [logFilter, setLogFilter] = useState<string>('ALL');
    const [logPaused, setLogPaused] = useState(false);
    const [networkEvents, setNetworkEvents] = useState<{ time: string; type: string; detail: string }[]>([]);
    const [connectTime] = useState(() => isConnected ? new Date() : null);
    const logEndRef = useRef<HTMLDivElement>(null);
    const logIdRef = useRef(0);
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

    // Listen for transfer events as network activity
    useEffect(() => {
        if (!isVisible) return;
        const unlisten = listen<TransferEvent>('transfer_event', (event) => {
            const d = event.payload;
            const time = new Date().toLocaleTimeString('en-US', { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
            setNetworkEvents(prev => {
                const entry = { time, type: d.event_type.toUpperCase(), detail: `${d.direction} ${d.filename}${d.message ? ` - ${d.message}` : ''}` };
                const next = [...prev, entry];
                return next.length > 200 ? next.slice(-200) : next;
            });
        });
        return () => { unlisten.then(fn => fn()); };
    }, [isVisible]);

    // Capture console.log/warn/error for the Logs tab
    useEffect(() => {
        if (!isVisible) return;

        const origLog = console.log;
        const origWarn = console.warn;
        const origError = console.error;
        const origDebug = console.debug;

        const addLog = (level: LogEntry['level'], args: unknown[]) => {
            if (logPaused) return;
            const msg = args.map(a => typeof a === 'string' ? a : JSON.stringify(a)).join(' ');
            const time = new Date().toLocaleTimeString('en-US', { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
            setLogs(prev => {
                const entry: LogEntry = { id: logIdRef.current++, timestamp: time, level, message: msg };
                const next = [...prev, entry];
                return next.length > 500 ? next.slice(-500) : next;
            });
        };

        console.log = (...args) => { origLog(...args); addLog('INFO', args); };
        console.warn = (...args) => { origWarn(...args); addLog('WARN', args); };
        console.error = (...args) => { origError(...args); addLog('ERROR', args); };
        console.debug = (...args) => { origDebug(...args); addLog('DEBUG', args); };

        return () => {
            console.log = origLog;
            console.warn = origWarn;
            console.error = origError;
            console.debug = origDebug;
        };
    }, [isVisible, logPaused]);

    // Auto-scroll logs
    useEffect(() => {
        if (!logPaused && activeTab === 'logs') {
            logEndRef.current?.scrollIntoView({ behavior: 'smooth' });
        }
    }, [logs, logPaused, activeTab]);

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
                className="h-1 cursor-row-resize bg-gray-200 dark:bg-gray-700 hover:bg-blue-400 dark:hover:bg-blue-600 transition-colors"
            />

            {/* Header with tabs */}
            <div className="flex items-center justify-between px-3 py-1 border-b border-gray-200 dark:border-gray-700 bg-gray-100 dark:bg-gray-800">
                <div className="flex items-center gap-1">
                    {TABS.map(tab => (
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
                            {tab.label}
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
                            <InfoRow label="Status" value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={isConnected} />
                                    {isConnected ? 'Connected' : 'Disconnected'}
                                </span>
                            } />
                            <InfoRow label="Protocol" value={connectionParams.protocol?.toUpperCase() || '—'} mono />
                            <InfoRow label="Server" value={connectionParams.server || '—'} mono />
                            <InfoRow label="Username" value={connectionParams.username || '—'} mono />
                            <InfoRow label="Remote Path" value={currentRemotePath || '/'} mono />
                            <InfoRow label="Uptime" value={uptimeStr} mono />
                            <InfoRow label="Credential Storage" value={systemInfo?.keyring_backend || 'Loading...'} />
                            <InfoRow label="Vault File" value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.vault_exists || false} />
                                    {systemInfo?.vault_exists ? 'Present' : 'Not created'}
                                </span>
                            } />
                            <InfoRow label="Known Hosts" value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.known_hosts_exists || false} />
                                    {systemInfo?.known_hosts_exists ? 'Present' : 'Not created'}
                                </span>
                            } />
                        </div>
                    </div>
                )}

                {/* Network Tab */}
                {activeTab === 'network' && (
                    <div className="p-2">
                        <div className="flex items-center justify-between mb-2">
                            <span className="text-xs text-gray-500">Transfer events ({networkEvents.length})</span>
                            <button
                                onClick={() => setNetworkEvents([])}
                                className="flex items-center gap-1 text-xs px-2 py-0.5 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500"
                            >
                                <Trash2 size={11} /> Clear
                            </button>
                        </div>
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700 overflow-y-auto" style={{ maxHeight: height - 80 }}>
                            {networkEvents.length === 0 ? (
                                <div className="p-4 text-center text-gray-400">No network activity yet. Transfer files to see events.</div>
                            ) : (
                                <table className="w-full">
                                    <thead className="sticky top-0 bg-gray-50 dark:bg-gray-800">
                                        <tr className="text-[10px] text-gray-500 border-b border-gray-200 dark:border-gray-700">
                                            <th className="text-left py-1 px-2 w-20">Time</th>
                                            <th className="text-left py-1 px-2 w-28">Event</th>
                                            <th className="text-left py-1 px-2">Detail</th>
                                        </tr>
                                    </thead>
                                    <tbody className="font-mono text-[11px]">
                                        {networkEvents.map((evt, i) => (
                                            <tr key={i} className="border-b border-gray-50 dark:border-gray-700/30">
                                                <td className="py-0.5 px-2 text-gray-400">{evt.time}</td>
                                                <td className="py-0.5 px-2">
                                                    <span className={`px-1.5 py-0.5 rounded text-[9px] font-semibold ${
                                                        evt.type.includes('ERROR') ? 'bg-red-100 text-red-700 dark:bg-red-900/30 dark:text-red-400' :
                                                        evt.type.includes('COMPLETE') ? 'bg-green-100 text-green-700 dark:bg-green-900/30 dark:text-green-400' :
                                                        evt.type.includes('START') ? 'bg-blue-100 text-blue-700 dark:bg-blue-900/30 dark:text-blue-400' :
                                                        'bg-gray-100 text-gray-600 dark:bg-gray-700 dark:text-gray-400'
                                                    }`}>
                                                        {evt.type}
                                                    </span>
                                                </td>
                                                <td className="py-0.5 px-2 text-gray-600 dark:text-gray-300 truncate max-w-md">{evt.detail}</td>
                                            </tr>
                                        ))}
                                    </tbody>
                                </table>
                            )}
                        </div>
                    </div>
                )}

                {/* System Tab */}
                {activeTab === 'system' && (
                    <div className="p-2">
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700">
                            <InfoRow label="AeroFTP Version" value={systemInfo?.app_version || '...'} mono />
                            <InfoRow label="Operating System" value={`${systemInfo?.os || '...'} (${systemInfo?.arch || '...'})`} mono />
                            <InfoRow label="Tauri Version" value={systemInfo?.tauri_version || '...'} mono />
                            <InfoRow label="Rust Toolchain" value={systemInfo?.rust_version || '...'} mono />
                            <InfoRow label="Keyring Backend" value={systemInfo?.keyring_backend || '...'} />
                            <InfoRow label="Config Directory" value={systemInfo?.config_dir || '...'} mono />
                            <InfoRow label="Vault (vault.db)" value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.vault_exists || false} />
                                    {systemInfo?.vault_exists ? 'Exists (AES-256-GCM + Argon2id)' : 'Not created'}
                                </span>
                            } />
                            <InfoRow label="Known Hosts" value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={systemInfo?.known_hosts_exists || false} />
                                    {systemInfo?.known_hosts_exists ? 'Exists (~/.ssh/known_hosts)' : 'Not created'}
                                </span>
                            } />
                            <InfoRow label="Snap Package" value={
                                <span className="flex items-center gap-1.5">
                                    <StatusDot active={!!(window as any).__TAURI_INTERNALS__} />
                                    {typeof window !== 'undefined' ? 'Tauri Runtime' : 'Browser'}
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
                                    <option value="ALL">All Levels</option>
                                    <option value="ERROR">Error</option>
                                    <option value="WARN">Warning</option>
                                    <option value="INFO">Info</option>
                                    <option value="DEBUG">Debug</option>
                                </select>
                                <span className="text-gray-400 text-[10px]">{logs.length} entries</span>
                            </div>
                            <div className="flex items-center gap-1">
                                <button onClick={() => setLogPaused(!logPaused)} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500" title={logPaused ? 'Resume' : 'Pause'}>
                                    {logPaused ? <Play size={12} /> : <Pause size={12} />}
                                </button>
                                <button onClick={copyLogs} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500" title="Copy all">
                                    <Copy size={12} />
                                </button>
                                <button onClick={() => setLogs([])} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500" title="Clear">
                                    <Trash2 size={12} />
                                </button>
                            </div>
                        </div>
                        <div className="flex-1 overflow-y-auto bg-gray-900 rounded-lg p-2 font-mono text-[11px] leading-relaxed">
                            {logs
                                .filter(l => logFilter === 'ALL' || l.level === logFilter)
                                .map(l => (
                                    <div key={l.id} className="flex gap-2 hover:bg-gray-800/50">
                                        <span className="text-gray-600 shrink-0">{l.timestamp}</span>
                                        <span className={`shrink-0 w-12 text-right ${levelColor[l.level]}`}>{l.level}</span>
                                        <span className="text-gray-300 break-all">{l.message}</span>
                                    </div>
                                ))}
                            <div ref={logEndRef} />
                            {logs.length === 0 && (
                                <div className="text-gray-600 text-center py-4">Console output will appear here when Debug Mode is active.</div>
                            )}
                        </div>
                    </div>
                )}

                {/* Frontend Tab */}
                {activeTab === 'frontend' && (
                    <div className="p-2">
                        <div className="bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700">
                            <InfoRow label="React Mode" value={React.version ? `React ${React.version}` : 'Unknown'} mono />
                            <InfoRow label="Language" value={document.documentElement.lang || navigator.language} mono />
                            <InfoRow label="LocalStorage Keys" value={`${localStorage.length} keys`} mono />
                            <InfoRow label="LocalStorage Size" value={`${localStorageSize} KB`} mono />
                            <InfoRow label="Window Size" value={`${window.innerWidth} x ${window.innerHeight}`} mono />
                            <InfoRow label="Device Pixel Ratio" value={`${window.devicePixelRatio}x`} mono />
                            <InfoRow label="Color Scheme" value={window.matchMedia('(prefers-color-scheme: dark)').matches ? 'Dark' : 'Light'} />
                            <InfoRow label="User Agent" value={
                                <span className="break-all text-[10px]">{navigator.userAgent}</span>
                            } />
                        </div>

                        {/* localStorage keys list */}
                        <h4 className="text-xs text-gray-500 mt-3 mb-1 font-semibold">LocalStorage Keys</h4>
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

export default DebugPanel;
