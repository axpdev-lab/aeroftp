// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useState, useEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import { pickSave } from '../utils/pickPath';
import { writeTextFile } from '@tauri-apps/plugin-fs';
import { X, Wifi, Activity, Monitor, ScrollText, Layout, Copy, Trash2, Pause, Play, Download, FlaskConical, CheckCircle2, XCircle, AlertTriangle, Circle, Loader2, Package } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { EffectiveTheme } from '../hooks/useTheme';
import { TRANSFER_EVENT_BRIDGE } from '../hooks/useTransferEvents';
import { usePointerDrag } from '../hooks/usePointerDrag';

// ─── Shared timestamp helper ───────────────────────────────────────────────
function ts() {
    return new Date().toLocaleTimeString('en-US', { hour12: false, hour: '2-digit', minute: '2-digit', second: '2-digit' });
}

// ─── Privacy redaction pipeline ────────────────────────────────────────────
// Applied to every log entry (JS console, backend Rust, IPC argument summaries)
// before it reaches the buffer. Default: always on, no toggle. The Rust log file
// on disk keeps the raw form for offline forensic use; everything that appears
// in the panel UI, copy-to-clipboard, and future exports is sanitized first.
//
// Patterns are ordered most-specific-first so a Bearer token does not get
// double-replaced by the JWT heuristic. Replacement strings are stable so
// duplicate detection across log lines still works after redaction.
const REDACTION_PATTERNS: ReadonlyArray<{ pattern: RegExp; replacement: string }> = [
    // API keys (Anthropic, OpenAI projects, generic sk-*)
    { pattern: /sk-(ant|proj|live|test)-[A-Za-z0-9_\-]{16,}/g, replacement: 'sk-***REDACTED***' },
    { pattern: /sk_(live|test)_[A-Za-z0-9]{16,}/g, replacement: 'sk_***REDACTED***' },
    // OAuth Bearer / x-api-key headers
    { pattern: /\bBearer\s+[A-Za-z0-9_\-.~+/]{8,}=*/g, replacement: 'Bearer ***REDACTED***' },
    { pattern: /\bx-api-key\s*[:=]\s*[^\s,;'"<>]+/gi, replacement: 'x-api-key: ***REDACTED***' },
    { pattern: /\bauthorization\s*[:=]\s*[^\s,;'"<>]+/gi, replacement: 'authorization: ***REDACTED***' },
    // JWT-shaped tokens (3 base64url segments separated by '.')
    { pattern: /\beyJ[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}\.[A-Za-z0-9_-]{6,}/g, replacement: '***JWT-REDACTED***' },
    // Inline credentials in URLs: ftp://user:pass@host -> ftp://user:***@host
    { pattern: /\b((?:ftps?|sftp|https?|webdav):\/\/[^:\s@/]+:)[^@\s]+(@)/gi, replacement: '$1***REDACTED***$2' },
    // Serialized secret fields in JSON: "password":"..." -> "password":"***REDACTED***"
    {
        pattern:
            /("(?:password|passwd|pwd|secret|secret_key|secretkey|api_key|apikey|access_key|token|refresh_token|passphrase|private_key|privatekey|client_secret|totp_secret|consumer_secret)"\s*:\s*)"[^"]*"/gi,
        replacement: '$1"***REDACTED***"',
    },
    // Secret fields in key=value / key: value form
    {
        pattern:
            /(\b(?:password|passwd|pwd|secret|secret_key|api_key|passphrase|private_key|client_secret|token)\s*[:=]\s*)[^\s,;'"<>]+/gi,
        replacement: '$1***REDACTED***',
    },
    // Email addresses
    { pattern: /[A-Za-z0-9._%+-]+@[A-Za-z0-9.-]+\.[A-Za-z]{2,}/g, replacement: '***@***' },
    // IPv4 addresses, but keep loopback / unspecified for local-debug clarity
    {
        pattern: /\b(?!127\.0\.0\.1\b|0\.0\.0\.0\b)((?:\d{1,3}\.){3}\d{1,3})\b/g,
        replacement: '***.***.***.***',
    },
    // Home directories
    { pattern: /\/home\/[A-Za-z0-9._-]+/g, replacement: '/home/***' },
    { pattern: /\/Users\/[A-Za-z0-9._-]+/g, replacement: '/Users/***' },
    { pattern: /C:\\Users\\[^\\]+/gi, replacement: 'C:\\Users\\***' },
    // Generic high-entropy hex blobs (likely keys / hashes / nonces), 32+ chars
    { pattern: /\b[A-Fa-f0-9]{32,}\b/g, replacement: '***HEX-REDACTED***' },
];

function redactSensitive(msg: string): string {
    if (!msg) return msg;
    let result = msg;
    for (const { pattern, replacement } of REDACTION_PATTERNS) {
        result = result.replace(pattern, replacement);
    }
    return result;
}

function safeStringifyLogArg(value: unknown): string {
    if (typeof value === 'string') return value;
    if (typeof value === 'bigint') return `${value.toString()}n`;
    if (value instanceof Error) return `${value.name}: ${value.message}`;
    try {
        const serialized = JSON.stringify(value);
        if (serialized !== undefined) return serialized;
    } catch { /* fall through */ }
    try {
        return String(value);
    } catch {
        return Object.prototype.toString.call(value);
    }
}

function localStoragePreview(value: string): string {
    const preview = redactSensitive(value.slice(0, 200));
    return preview.length > 80 ? `${preview.slice(0, 80)}...` : preview;
}

// ─── Global console + backend log capture (singleton, survives mount/unmount) ─
type LogLevelName = 'DEBUG' | 'INFO' | 'WARN' | 'ERROR' | 'TRACE';
type LogSource = 'js' | 'rust';

interface CapturedLog {
    id: number;
    timestamp: string;
    level: LogLevelName;
    message: string;
    source: LogSource;
}

const globalLogBuffer: CapturedLog[] = [];
let globalLogId = 0;
let globalCaptureRefCount = 0;
let restoreConsole: (() => void) | null = null;
const globalLogListeners = new Set<() => void>();

// tauri-plugin-log 2.x emits LogLevel as u16: 1=Trace, 2=Debug, 3=Info, 4=Warn, 5=Error.
const RUST_LEVEL_MAP: Record<number, LogLevelName> = {
    1: 'TRACE',
    2: 'DEBUG',
    3: 'INFO',
    4: 'WARN',
    5: 'ERROR',
};

let backendBridgeUnlisten: UnlistenFn | null = null;
let backendBridgeRefCount = 0;

/// Ref-counted patch of `console.{log,warn,error,debug}` that survives as long
/// as at least one DebugPanel is mounted. When the last panel unmounts, the
/// originals are restored: preventing a permanent interceptor for users who
/// opened debug once and moved on.
function activateGlobalCapture() {
    globalCaptureRefCount += 1;
    if (globalCaptureRefCount > 1) return;

    const origLog = console.log;
    const origWarn = console.warn;
    const origError = console.error;
    const origDebug = console.debug;

    const addEntry = (level: CapturedLog['level'], args: unknown[]) => {
        const raw = args.map(safeStringifyLogArg).join(' ');
        const entry: CapturedLog = { id: globalLogId++, timestamp: ts(), level, message: redactSensitive(raw), source: 'js' };
        globalLogBuffer.push(entry);
        if (globalLogBuffer.length > 500) globalLogBuffer.splice(0, globalLogBuffer.length - 500);
        queueMicrotask(() => globalLogListeners.forEach(fn => fn()));
    };

    console.log = (...args) => { origLog(...args); addEntry('INFO', args); };
    console.warn = (...args) => { origWarn(...args); addEntry('WARN', args); };
    console.error = (...args) => { origError(...args); addEntry('ERROR', args); };
    console.debug = (...args) => { origDebug(...args); addEntry('DEBUG', args); };

    restoreConsole = () => {
        console.log = origLog;
        console.warn = origWarn;
        console.error = origError;
        console.debug = origDebug;
    };
}

function deactivateGlobalCapture() {
    if (globalCaptureRefCount === 0) return;
    globalCaptureRefCount -= 1;
    if (globalCaptureRefCount === 0 && restoreConsole) {
        restoreConsole();
        restoreConsole = null;
    }
}

function pushBackendEntry(message: string, level: LogLevelName) {
    const entry: CapturedLog = { id: globalLogId++, timestamp: ts(), level, message: redactSensitive(message), source: 'rust' };
    globalLogBuffer.push(entry);
    if (globalLogBuffer.length > 500) globalLogBuffer.splice(0, globalLogBuffer.length - 500);
    queueMicrotask(() => globalLogListeners.forEach(fn => fn()));
}

// Subscribe to backend `log::*` events forwarded by tauri-plugin-log via
// `TargetKind::Webview`. Ref-counted to mirror activateGlobalCapture: the
// listener is torn down when the last DebugPanel unmounts.
function activateBackendLogBridge() {
    backendBridgeRefCount += 1;
    if (backendBridgeRefCount > 1) return;
    // Async setup; intentionally not awaited, fire-and-forget on mount.
    listen<{ message: string; level: number }>('log://log', (event) => {
        const lvl = RUST_LEVEL_MAP[event.payload.level] || 'INFO';
        // Plugin formats with `{level} {target}: {message}` in stdout, but the
        // emitted payload only carries the message string. Keep as-is so the
        // user sees the same line the file logger writes.
        pushBackendEntry(event.payload.message, lvl);
    }).then((unlisten) => {
        // If we already detached before the promise resolved, the ref count
        // is 0 and we should call unlisten immediately.
        if (backendBridgeRefCount === 0) {
            unlisten();
        } else {
            backendBridgeUnlisten = unlisten;
        }
    }).catch(() => { /* tauri host not available (browser preview) */ });
}

function deactivateBackendLogBridge() {
    if (backendBridgeRefCount === 0) return;
    backendBridgeRefCount -= 1;
    if (backendBridgeRefCount === 0 && backendBridgeUnlisten) {
        backendBridgeUnlisten();
        backendBridgeUnlisten = null;
    }
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
let globalNetworkRefCount = 0;
let restoreNetworkCapture: (() => void) | null = null;
const globalNetworkListeners = new Set<() => void>();

function notifyNetworkListeners() {
    queueMicrotask(() => globalNetworkListeners.forEach(fn => fn()));
}

function addNetworkEntry(entry: Omit<NetworkEntry, 'id' | 'timestamp'>) {
    const e: NetworkEntry = {
        ...entry,
        id: globalNetworkId++,
        timestamp: ts(),
        detail: redactSensitive(entry.detail || ''),
    };
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
    globalNetworkRefCount += 1;
    if (globalNetworkRefCount > 1) return;

    // 1) Listen for bridged transfer events from the primary transfer listener
    const transferListener = (event: Event) => {
        const d = (event as CustomEvent<{ event_type: string; transfer_id: string; filename: string; direction: string; message?: string }>).detail;
        const evType = d.event_type.toLowerCase();
        const status: NetworkEntry['status'] = evType.includes('error') ? 'error'
            : evType.includes('complete') || evType.includes('done') ? 'complete'
            : evType.includes('start') || evType.includes('begin') ? 'start'
            : 'progress';
        addNetworkEntry({
            type: 'TRANSFER',
            status,
            command: `${d.direction} ${d.event_type}`,
            detail: `${d.filename}${d.message ? `: ${d.message}` : ''}`,
        });
    };
    window.addEventListener(TRANSFER_EVENT_BRIDGE, transferListener);

    // 2) Intercept __TAURI_INTERNALS__.invoke to log all IPC calls with timing
    const internals = (window as any).__TAURI_INTERNALS__;
    let revertInvokePatch: (() => void) | null = null;
    if (internals && !internals.__debugPatched) {
        try {
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
                        detail: `${dur}ms: ${String(err).slice(0, 120)}`,
                        duration: dur,
                    });
                    throw err;
                }
            };
            revertInvokePatch = () => {
                try {
                    internals.invoke = origFn;
                    delete internals.__debugPatched;
                } catch { /* frozen */ }
            };
        } catch { /* __TAURI_INTERNALS__ may be frozen/sealed */ }
    }

    restoreNetworkCapture = () => {
        window.removeEventListener(TRANSFER_EVENT_BRIDGE, transferListener);
        revertInvokePatch?.();
    };
}

function deactivateNetworkCapture() {
    if (globalNetworkRefCount === 0) return;
    globalNetworkRefCount -= 1;
    if (globalNetworkRefCount === 0 && restoreNetworkCapture) {
        restoreNetworkCapture();
        restoreNetworkCapture = null;
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

// ─── Export helpers ───────────────────────────────────────────────────────
//
// Buffers fed to these helpers are ALREADY redacted by `redactSensitive`
// at push time, so no extra sanitization happens here. The file format
// determines layout only.

type ExportFormat = 'text' | 'json' | 'ndjson' | 'csv';
const EXPORT_EXT: Record<ExportFormat, string> = { text: 'log', json: 'json', ndjson: 'ndjson', csv: 'csv' };

function csvEscape(s: string): string {
    if (/[",\n\r]/.test(s)) {
        return `"${s.replace(/"/g, '""')}"`;
    }
    return s;
}

function formatLogsText(logs: CapturedLog[]): string {
    return logs.map(l => `[${l.timestamp}] [${l.source.toUpperCase()}] [${l.level}] ${l.message}`).join('\n');
}

function formatLogsJson(logs: CapturedLog[]): string {
    return JSON.stringify(logs, null, 2);
}

function formatLogsNdjson(logs: CapturedLog[]): string {
    return logs.map(l => JSON.stringify(l)).join('\n');
}

function formatLogsCsv(logs: CapturedLog[]): string {
    const header = 'timestamp,source,level,message';
    const rows = logs.map(l => `${l.timestamp},${l.source},${l.level},${csvEscape(l.message)}`);
    return [header, ...rows].join('\n');
}

function formatNetworkText(events: NetworkEntry[]): string {
    return events.map(e => {
        const dur = e.duration ? ` ${e.duration}ms` : '';
        return `[${e.timestamp}] [${e.type}] [${e.status}]${dur} ${e.command}: ${e.detail}`;
    }).join('\n');
}

function formatNetworkJson(events: NetworkEntry[]): string {
    return JSON.stringify(events, null, 2);
}

function formatNetworkNdjson(events: NetworkEntry[]): string {
    return events.map(e => JSON.stringify(e)).join('\n');
}

function formatNetworkCsv(events: NetworkEntry[]): string {
    const header = 'timestamp,type,status,command,duration_ms,detail';
    const rows = events.map(e => [
        e.timestamp,
        e.type,
        e.status,
        csvEscape(e.command),
        e.duration != null ? String(e.duration) : '',
        csvEscape(e.detail),
    ].join(','));
    return [header, ...rows].join('\n');
}

function exportFilename(kind: 'logs' | 'network', fmt: ExportFormat): string {
    const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
    return `aeroftp-${kind}-${stamp}.${EXPORT_EXT[fmt]}`;
}

async function exportToFile(content: string, kind: 'logs' | 'network', fmt: ExportFormat) {
    const path = await pickSave({
        defaultPath: exportFilename(kind, fmt),
        filters: [{ name: fmt.toUpperCase(), extensions: [EXPORT_EXT[fmt]] }],
    });
    if (!path) return;
    await writeTextFile(path, content);
}

type LogEntry = CapturedLog;

// TransferEvent type removed: handled by global network capture

type TabId = 'connection' | 'network' | 'system' | 'logs' | 'frontend' | 'tests';

const TAB_IDS: { id: TabId; icon: React.ReactNode }[] = [
    { id: 'connection', icon: <Wifi size={13} /> },
    { id: 'network', icon: <Activity size={13} /> },
    { id: 'system', icon: <Monitor size={13} /> },
    { id: 'logs', icon: <ScrollText size={13} /> },
    { id: 'tests', icon: <FlaskConical size={13} /> },
    { id: 'frontend', icon: <Layout size={13} /> },
];

// ─── Tests tab ─────────────────────────────────────────────────────────────
//
// 6 tests are backend-driven (Tauri commands in `debug_tests.rs`) and 2 are
// frontend-only (IPC latency benchmark, i18n key sweep). Each test produces
// the same shape so the UI renders them uniformly.

type TestStatus = 'idle' | 'running' | 'pass' | 'fail' | 'warn' | 'skipped';

interface TestRunResult {
    status: 'pass' | 'fail' | 'warn' | 'skipped';
    duration_ms: number;
    message: string;
    details?: string;
}

interface TestRecord {
    status: TestStatus;
    duration_ms?: number;
    message?: string;
    details?: string;
}

type TestId =
    | 'connectivity'
    | 'vault_roundtrip'
    | 'known_hosts'
    | 'aerovault_roundtrip'
    | 'plugin_integrity'
    | 'provider_selftest'
    | 'ipc_speed'
    | 'i18n_sweep';

const TEST_CATALOG: { id: TestId; labelKey: string; runner: 'backend' | 'frontend'; cmd?: string }[] = [
    { id: 'connectivity', labelKey: 'debug.tests.connectivity', runner: 'backend', cmd: 'debug_test_connectivity' },
    { id: 'vault_roundtrip', labelKey: 'debug.tests.vaultRoundtrip', runner: 'backend', cmd: 'debug_test_vault_roundtrip' },
    { id: 'known_hosts', labelKey: 'debug.tests.knownHosts', runner: 'backend', cmd: 'debug_test_known_hosts' },
    { id: 'aerovault_roundtrip', labelKey: 'debug.tests.aerovaultRoundtrip', runner: 'backend', cmd: 'debug_test_aerovault_roundtrip' },
    { id: 'plugin_integrity', labelKey: 'debug.tests.pluginIntegrity', runner: 'backend', cmd: 'debug_test_plugin_integrity' },
    { id: 'provider_selftest', labelKey: 'debug.tests.providerSelftest', runner: 'backend', cmd: 'debug_test_provider_selftest' },
    { id: 'ipc_speed', labelKey: 'debug.tests.ipcSpeed', runner: 'frontend' },
    { id: 'i18n_sweep', labelKey: 'debug.tests.i18nSweep', runner: 'frontend' },
];

async function runIpcSpeedTest(): Promise<TestRunResult> {
    const t0 = performance.now();
    const samples: number[] = [];
    const N = 50;
    for (let i = 0; i < N; i++) {
        const s = performance.now();
        try {
            await invoke('get_system_info');
        } catch {
            return { status: 'fail', duration_ms: Math.round(performance.now() - t0), message: 'invoke get_system_info failed mid-bench' };
        }
        samples.push(performance.now() - s);
    }
    samples.sort((a, b) => a - b);
    const p50 = samples[Math.floor(N * 0.5)];
    const p95 = samples[Math.floor(N * 0.95)];
    const max = samples[N - 1];
    const status: 'pass' | 'warn' = p95 > 50 ? 'warn' : 'pass';
    return {
        status,
        duration_ms: Math.round(performance.now() - t0),
        message: `${N} invokes — P50 ${p50.toFixed(1)}ms · P95 ${p95.toFixed(1)}ms · max ${max.toFixed(1)}ms`,
    };
}

async function runI18nSweepTest(): Promise<TestRunResult> {
    const t0 = performance.now();
    try {
        const lang = (document.documentElement.lang || navigator.language || 'en').split('-')[0];
        if (lang === 'en') {
            return { status: 'skipped', duration_ms: Math.round(performance.now() - t0), message: 'Active locale is English (no comparison needed)' };
        }
        const [enRes, locRes] = await Promise.all([
            fetch(`/locales/${'en'}.json`).catch(() => null),
            fetch(`/locales/${lang}.json`).catch(() => null),
        ]);
        if (!enRes || !enRes.ok || !locRes || !locRes.ok) {
            return { status: 'skipped', duration_ms: Math.round(performance.now() - t0), message: 'Locale files not reachable via /locales (build mode only)' };
        }
        const en = await enRes.json();
        const loc = await locRes.json();
        const enKeys = new Set<string>();
        const locValues: Record<string, string> = {};
        const walk = (obj: any, prefix: string, sink: Set<string> | null, values: Record<string, string> | null) => {
            for (const k of Object.keys(obj)) {
                const key = prefix ? `${prefix}.${k}` : k;
                if (obj[k] && typeof obj[k] === 'object' && !Array.isArray(obj[k])) {
                    walk(obj[k], key, sink, values);
                } else if (typeof obj[k] === 'string') {
                    if (sink) sink.add(key);
                    if (values) values[key] = obj[k];
                }
            }
        };
        walk(en, '', enKeys, null);
        walk(loc.translations || loc, '', null, locValues);
        let missing = 0;
        let placeholder = 0;
        for (const k of enKeys) {
            if (!(k in locValues)) missing++;
            else if (locValues[k].includes('[NEEDS TRANSLATION]')) placeholder++;
        }
        const status: 'pass' | 'warn' = (missing + placeholder) === 0 ? 'pass' : 'warn';
        return {
            status,
            duration_ms: Math.round(performance.now() - t0),
            message: `${lang}: ${enKeys.size} reference keys · ${missing} missing · ${placeholder} placeholder`,
        };
    } catch (e) {
        return { status: 'fail', duration_ms: Math.round(performance.now() - t0), message: `Sweep failed: ${String(e).slice(0, 120)}` };
    }
}

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
            case 'truedark': return { base: 'bg-[#161b22] hover:bg-[#58a6ff]', bar: 'bg-[#30363d] group-hover:bg-[#79c0ff]' };
            case 'green': return { base: 'bg-[#18302a] hover:bg-green-500', bar: 'bg-green-800/60 group-hover:bg-green-400' };
            default: return { base: 'bg-gray-700 hover:bg-blue-500', bar: 'bg-gray-500 group-hover:bg-blue-400' };
        }
    }, [appTheme]);
    const [activeTab, setActiveTab] = useState<TabId>('connection');
    const [height, setHeight] = useState(320);
    const [systemInfo, setSystemInfo] = useState<SystemInfo | null>(null);
    const [logs, setLogs] = useState<LogEntry[]>([]);
    const [logFilter, setLogFilter] = useState<string>('ALL');
    const [sourceFilter, setSourceFilter] = useState<'all' | LogSource>('all');
    const [logPaused, setLogPaused] = useState(false);
    const [exportMenuOpen, setExportMenuOpen] = useState(false);
    const exportMenuRef = useRef<HTMLDivElement>(null);
    const [testResults, setTestResults] = useState<Record<TestId, TestRecord>>(() => {
        const init: Partial<Record<TestId, TestRecord>> = {};
        for (const t of TEST_CATALOG) init[t.id] = { status: 'idle' };
        return init as Record<TestId, TestRecord>;
    });
    const [testsRunningAll, setTestsRunningAll] = useState(false);
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

    // Activate global captures on first mount and subscribe to updates.
    // Ref-counted: patches are reverted when the last DebugPanel unmounts.
    useEffect(() => {
        activateGlobalCapture();
        activateNetworkCapture();
        activateBackendLogBridge();

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
            deactivateGlobalCapture();
            deactivateNetworkCapture();
            deactivateBackendLogBridge();
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

    // Resize handle: usePointerDrag holds the capture on the handle itself
    // so unmount mid-drag can release it without touching document globals.
    const resizeStartRef = useRef<{ y: number; startHeight: number } | null>(null);
    const { onPointerDown: onResizePointerDown } = usePointerDrag({
        onPointerMove: (ev) => {
            const s = resizeStartRef.current;
            if (!s) return;
            setHeight(Math.max(150, Math.min(600, s.startHeight - (ev.clientY - s.y))));
        },
        onPointerUp: () => { resizeStartRef.current = null; },
        onPointerCancel: () => { resizeStartRef.current = null; },
    });
    const handleResize = useCallback((e: React.PointerEvent<HTMLDivElement>) => {
        e.preventDefault();
        resizeStartRef.current = { y: e.clientY, startHeight: height };
        onResizePointerDown(e);
    }, [height, onResizePointerDown]);

    const copyLogs = useCallback(() => {
        const text = logs.map(l => `[${l.timestamp}] [${l.source.toUpperCase()}] [${l.level}] ${l.message}`).join('\n');
        navigator.clipboard.writeText(text);
    }, [logs]);

    // Close export dropdown on outside click
    useEffect(() => {
        if (!exportMenuOpen) return;
        const onDown = (e: MouseEvent) => {
            if (exportMenuRef.current && !exportMenuRef.current.contains(e.target as Node)) {
                setExportMenuOpen(false);
            }
        };
        document.addEventListener('mousedown', onDown);
        return () => document.removeEventListener('mousedown', onDown);
    }, [exportMenuOpen]);

    const runSingleTest = useCallback(async (id: TestId) => {
        const spec = TEST_CATALOG.find(t => t.id === id);
        if (!spec) return;
        setTestResults(prev => ({ ...prev, [id]: { status: 'running' } }));
        try {
            let res: TestRunResult;
            if (spec.runner === 'backend' && spec.cmd) {
                res = await invoke<TestRunResult>(spec.cmd);
            } else if (id === 'ipc_speed') {
                res = await runIpcSpeedTest();
            } else if (id === 'i18n_sweep') {
                res = await runI18nSweepTest();
            } else {
                res = { status: 'fail', duration_ms: 0, message: 'Unknown runner' };
            }
            setTestResults(prev => ({
                ...prev,
                [id]: {
                    status: res.status,
                    duration_ms: res.duration_ms,
                    message: res.message,
                    details: res.details,
                },
            }));
        } catch (err) {
            setTestResults(prev => ({
                ...prev,
                [id]: { status: 'fail', message: String(err).slice(0, 200) },
            }));
        }
    }, []);

    const runAllTests = useCallback(async () => {
        if (testsRunningAll) return;
        setTestsRunningAll(true);
        for (const t of TEST_CATALOG) {
            await runSingleTest(t.id);
        }
        setTestsRunningAll(false);
    }, [testsRunningAll, runSingleTest]);

    const copyTestResults = useCallback(() => {
        const lines: string[] = ['# AeroFTP DebugPanel diagnostic suite'];
        for (const t of TEST_CATALOG) {
            const r = testResults[t.id];
            const label = t.id;
            if (r.status === 'idle') {
                lines.push(`- [ ] ${label}: not run`);
            } else if (r.status === 'running') {
                lines.push(`- [~] ${label}: running...`);
            } else {
                const dur = r.duration_ms != null ? ` (${r.duration_ms}ms)` : '';
                const tag = r.status === 'pass' ? 'PASS' : r.status === 'warn' ? 'WARN' : r.status === 'skipped' ? 'SKIP' : 'FAIL';
                lines.push(`- [${tag}] ${label}${dur}: ${r.message ?? ''}`);
            }
        }
        navigator.clipboard.writeText(lines.join('\n'));
    }, [testResults]);

    const runExportBundle = useCallback(async () => {
        try {
            const stamp = new Date().toISOString().replace(/[:.]/g, '-').slice(0, 19);
            const path = await pickSave({
                defaultPath: `aeroftp-diagnostic-${stamp}.zip`,
                filters: [{ name: 'ZIP', extensions: ['zip'] }],
            });
            if (!path) return;

            // Buffers are already redacted at push time. We re-stream them
            // here as NDJSON because that is the format that survives every
            // common downstream tool (jq, Datadog, grep).
            const logsNdjson = logs.map(l => JSON.stringify(l)).join('\n');
            const networkNdjson = networkEvents.map(e => JSON.stringify(e)).join('\n');

            // localStorage: only emit key + length + truncated value preview,
            // never raw values. Anything that looks sensitive gets the same
            // redactSensitive sweep client-side before serialization.
            const localStorageDump: Array<{ key: string; length: number; preview: string }> = [];
            for (let i = 0; i < localStorage.length; i++) {
                const k = localStorage.key(i);
                if (!k) continue;
                const v = localStorage.getItem(k) || '';
                localStorageDump.push({
                    key: k,
                    length: v.length,
                    preview: redactSensitive(v.slice(0, 200)),
                });
            }

            await invoke<string>('debug_export_bundle', {
                outputPath: path,
                bundle: {
                    logs_ndjson: logsNdjson,
                    network_ndjson: networkNdjson,
                    system_info: systemInfo ?? {},
                    tests_state: testResults,
                    local_storage_keys: localStorageDump,
                    app_version: systemInfo?.app_version ?? 'unknown',
                },
            });
        } catch (err) {
            console.error('DebugPanel bundle export failed:', err);
        }
    }, [logs, networkEvents, systemInfo, testResults]);

    const runExport = useCallback(async (fmt: ExportFormat) => {
        setExportMenuOpen(false);
        try {
            if (activeTab === 'network') {
                const content =
                    fmt === 'text' ? formatNetworkText(networkEvents)
                    : fmt === 'json' ? formatNetworkJson(networkEvents)
                    : fmt === 'ndjson' ? formatNetworkNdjson(networkEvents)
                    : formatNetworkCsv(networkEvents);
                await exportToFile(content, 'network', fmt);
            } else {
                // Default: export the unified Logs buffer (covers logs/connection/system/frontend tabs).
                const content =
                    fmt === 'text' ? formatLogsText(logs)
                    : fmt === 'json' ? formatLogsJson(logs)
                    : fmt === 'ndjson' ? formatLogsNdjson(logs)
                    : formatLogsCsv(logs);
                await exportToFile(content, 'logs', fmt);
            }
        } catch (err) {
            console.error('DebugPanel export failed:', err);
        }
    }, [activeTab, logs, networkEvents]);

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
        : '-';

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
                onPointerDown={handleResize}
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
                <div className="flex items-center gap-1">
                    {/* Diagnostic bundle export: one-click ZIP with system_info,
                        logs, network, tests state, localStorage preview, and
                        the redacted aeroftp.log tail. */}
                    <button
                        onClick={runExportBundle}
                        className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 flex items-center gap-1"
                        title={t('debug.bundle.title')}
                    >
                        <Package size={14} />
                    </button>
                    {/* Export dropdown: appears for any tab, content scope depends on active tab. */}
                    <div ref={exportMenuRef} className="relative">
                        <button
                            onClick={() => setExportMenuOpen(v => !v)}
                            className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 flex items-center gap-1"
                            title={t('debug.export.title')}
                        >
                            <Download size={14} />
                        </button>
                        {exportMenuOpen && (
                            <div className="absolute right-0 top-full mt-1 min-w-[180px] py-1 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg shadow-xl z-50">
                                <div className="px-3 py-1 text-[10px] uppercase tracking-wider text-gray-400">
                                    {activeTab === 'network' ? t('debug.export.scopeNetwork') : t('debug.export.scopeLogs')}
                                </div>
                                {(['text', 'json', 'ndjson', 'csv'] as ExportFormat[]).map(fmt => (
                                    <button
                                        key={fmt}
                                        onClick={() => runExport(fmt)}
                                        className="w-full px-3 py-1.5 text-xs text-left hover:bg-gray-100 dark:hover:bg-gray-700 flex items-center justify-between"
                                    >
                                        <span>{t(`debug.export.${fmt}`)}</span>
                                        <span className="text-[10px] text-gray-400 font-mono">.{EXPORT_EXT[fmt]}</span>
                                    </button>
                                ))}
                            </div>
                        )}
                    </div>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500">
                        <X size={14} />
                    </button>
                </div>
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
                            <InfoRow label={t('debug.connection.protocol')} value={connectionParams.protocol?.toUpperCase() || '-'} mono />
                            <InfoRow label={t('debug.connection.server')} value={connectionParams.server || '-'} mono />
                            <InfoRow label={t('debug.connection.username')} value={connectionParams.username || '-'} mono />
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
                                <select
                                    value={sourceFilter}
                                    onChange={e => setSourceFilter(e.target.value as 'all' | LogSource)}
                                    className="text-xs px-2 py-0.5 rounded border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-800"
                                    title={t('debug.logs.sourceFilterTitle')}
                                >
                                    <option value="all">{t('debug.logs.allSources')}</option>
                                    <option value="rust">{t('debug.logs.sourceRust')}</option>
                                    <option value="js">{t('debug.logs.sourceJs')}</option>
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
                                .filter(l => sourceFilter === 'all' || l.source === sourceFilter)
                                .map(l => (
                                    <div key={l.id} className="flex gap-2 hover:bg-gray-200/50 dark:hover:bg-gray-800/50">
                                        <span className="text-gray-500 dark:text-gray-600 shrink-0">{l.timestamp}</span>
                                        <span
                                            className={`shrink-0 w-9 text-center text-[9px] font-semibold rounded px-1 ${
                                                l.source === 'rust'
                                                    ? 'bg-orange-100 text-orange-700 dark:bg-orange-900/30 dark:text-orange-400'
                                                    : 'bg-sky-100 text-sky-700 dark:bg-sky-900/30 dark:text-sky-400'
                                            }`}
                                            title={l.source === 'rust' ? t('debug.logs.sourceRust') : t('debug.logs.sourceJs')}
                                        >
                                            {l.source === 'rust' ? 'RUST' : 'JS'}
                                        </span>
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
                                        <span className="text-[10px] text-gray-400 truncate">{val.length} bytes · {localStoragePreview(val)}</span>
                                    </div>
                                );
                            })}
                        </div>
                    </div>
                )}

                {/* Tests Tab */}
                {activeTab === 'tests' && (
                    <div className="p-2 flex flex-col" style={{ height: height - 60 }}>
                        <div className="flex items-center justify-between mb-2">
                            <div className="flex items-center gap-2">
                                <button
                                    onClick={runAllTests}
                                    disabled={testsRunningAll}
                                    className="flex items-center gap-1 text-xs px-2 py-1 rounded bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300 hover:bg-blue-200 dark:hover:bg-blue-900/60 disabled:opacity-50"
                                >
                                    {testsRunningAll ? <Loader2 size={12} className="animate-spin" /> : <Play size={12} />}
                                    {t('debug.tests.runAll')}
                                </button>
                                <button
                                    onClick={copyTestResults}
                                    className="flex items-center gap-1 text-xs px-2 py-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500"
                                    title={t('debug.tests.copyResultsTitle')}
                                >
                                    <Copy size={12} /> {t('debug.tests.copyResults')}
                                </button>
                            </div>
                            <span className="text-[10px] text-gray-400">
                                {Object.values(testResults).filter(r => r.status === 'pass').length}/{TEST_CATALOG.length} {t('debug.tests.passed')}
                            </span>
                        </div>
                        <div className="flex-1 overflow-y-auto bg-white dark:bg-gray-800 rounded-lg border border-gray-200 dark:border-gray-700">
                            {TEST_CATALOG.map(test => {
                                const r = testResults[test.id];
                                const statusIcon =
                                    r.status === 'pass' ? <CheckCircle2 size={14} className="text-green-500" /> :
                                    r.status === 'fail' ? <XCircle size={14} className="text-red-500" /> :
                                    r.status === 'warn' ? <AlertTriangle size={14} className="text-amber-500" /> :
                                    r.status === 'skipped' ? <Circle size={14} className="text-gray-400" /> :
                                    r.status === 'running' ? <Loader2 size={14} className="text-blue-500 animate-spin" /> :
                                    <Circle size={14} className="text-gray-300 dark:text-gray-600" />;
                                return (
                                    <div key={test.id} className="flex items-start gap-2 py-2 px-3 border-b border-gray-100 dark:border-gray-700/50 last:border-0 hover:bg-gray-50 dark:hover:bg-gray-700/30">
                                        <div className="pt-0.5">{statusIcon}</div>
                                        <div className="flex-1 min-w-0">
                                            <div className="flex items-baseline gap-2">
                                                <span className="text-xs font-medium text-gray-800 dark:text-gray-200">{t(test.labelKey)}</span>
                                                <span className="text-[10px] text-gray-400 font-mono">{test.runner === 'backend' ? 'rust' : 'js'}</span>
                                                {r.duration_ms != null && (
                                                    <span className="text-[10px] text-gray-400 font-mono">{r.duration_ms}ms</span>
                                                )}
                                            </div>
                                            {r.message && (
                                                <div className="text-[11px] text-gray-500 dark:text-gray-400 mt-0.5 break-all">{r.message}</div>
                                            )}
                                        </div>
                                        <button
                                            onClick={() => runSingleTest(test.id)}
                                            disabled={r.status === 'running' || testsRunningAll}
                                            className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-700 text-gray-500 disabled:opacity-30"
                                            title={t('debug.tests.runOne')}
                                        >
                                            <Play size={11} />
                                        </button>
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

export {
    activateGlobalCapture,
    activateNetworkCapture,
    activateBackendLogBridge,
    deactivateGlobalCapture,
    deactivateNetworkCapture,
    deactivateBackendLogBridge,
};
export default DebugPanel;
