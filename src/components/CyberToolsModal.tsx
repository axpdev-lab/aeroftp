// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useCallback, useEffect, useRef, type DragEvent as ReactDragEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import {
    X, Hash, Lock, KeyRound, Copy, Check, FileSearch, Type,
    RefreshCw, Eye, EyeOff, Loader2, AlertTriangle, CheckCircle2
} from 'lucide-react';
import { useTranslation } from '../i18n';
import { Checkbox } from './ui/Checkbox';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { PasswordForgeTab } from './PasswordForgeTab';

interface CyberToolsModalProps {
    onClose: () => void;
}

type TabId = 'hash' | 'crypto' | 'password';

export const CyberToolsModal: React.FC<CyberToolsModalProps> = ({ onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [activeTab, setActiveTab] = useState<TabId>('hash');

    // Close on Escape
    useEffect(() => {
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [onClose]);

    const tabs: { id: TabId; label: string; icon: React.ReactNode }[] = [
        { id: 'hash', label: t('cyberTools.hashForge'), icon: <Hash size={15} /> },
        { id: 'crypto', label: t('cyberTools.cryptoLab'), icon: <Lock size={15} /> },
        { id: 'password', label: t('cyberTools.passwordForge'), icon: <KeyRound size={15} /> },
    ];

    // No outside-click-to-close: users copy hashes / drag files here, an accidental
    // backdrop click must not dismiss (close via the X button or Escape).
    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60">
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-[560px] max-h-[85vh] flex flex-col animate-scale-in"
            >
                {/* Header */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <svg viewBox="0 0 120 120" width={18} height={18} fill="currentColor" className="text-cyan-500 dark:text-cyan-400">
                            <path d="M126.3,13.2C97.8,18 78.1,45.1 82.4,73.6c1.1,7.3 4.4,16.1 8.1,21.5l1,1.4-39.9,39.9c-21.9,22-40.3,40.7-40.7,41.5-0.5,1-0.8,2.6-0.8,4.4 0,7.9 8.3,12.5 15,8.1l2-1.3 9,8.9c8,7.9 9.2,8.9 11.2,9.4 1.2,0.3 2.5,0.6 3,0.6 3.2,0 7.2-2.7 8.7-5.8 0.9-2 1-6 0.1-8.1-0.4-0.9-2.4-3.4-4.5-5.7l-3.9-4 5.6-5.6 5.6-5.6 3.8,3.8c5.5,5.4 7.9,6.4 12.5,5 6.2-1.8 8.9-9.2 5.4-14.8-0.5-0.7-4.5-5-9-9.5l-8.1-8.1 18.6-18.7c17.6-17.6 18.7-18.9 19.8-21.6 1.8-4.4 3.9-7.8 7.3-11.3l3-3.2-4.2-4.2c-4.8-4.8-7.2-8.5-8.9-13.9-3-9.6-1.9-20 3.1-28.5 2.8-4.8 8.4-10.3 13.1-12.6 6.3-3.2 9.7-4.1 16.6-4.1 5,0 6.6,0.2 9.5,1.1 4.7,1.5 9.9,4.2 12.8,6.8l2.4,2.1 2.7-0.7c4.2-1.1 11.4-1.7 15.5-1.4 3.1,0.3 3.6,0.2 3.3-0.3-3-5.1-9-11.9-13.6-15.4-6.4-4.9-16.2-9.2-24-10.4-3.4-0.7-13.9-0.7-17.2-0.1z" transform="matrix(0.509,0,0,0.509,-5.137,-5.118)"/>
                            <path d="M167.1,54.2c-15.1,3.4-25.7,12.2-29.9,24.7-1.2,3.6-1.2,3.9-1.4,19.5l-0.2,15.8h-4.9c-7.6,0-12.4,1.6-16.9,5.7-2.9,2.6-5.3,6.6-6.3,10.4-0.7,2.7-0.8,5.8-0.8,24.7 0,22.9 0.2,27.7 2.1,35.4 5.4,23 23,42.1 45.6,49.7 8.3,2.8 11.4,3.2 22,3.2 10.5,0 13.7-0.5 22-3.2 11-3.6 20.3-9.6 28.6-18.4 9-9.3 14.9-20.9 17.8-34.4 0.9-4.1 1-6.4 1.2-28.5 0.1-16.2 0-24.9-0.3-26.8-0.8-4.2-2.9-8.2-6-11.2-4.7-4.7-9.6-6.5-17.7-6.5h-5l-0.1-16.1c-0.1-16-0.1-16-1.4-19.8C211.8,67.2 201.2,58.1 187.9,54.6 182.3,53.2 172.6,53 167.1,54.2zM184.8,74.1c6,1.8 11.1,6.4 12.4,11 0.3,1 0.5,7.5 0.5,15.4v13.8h-21-21V99.7c0-16.6 0-16.3 3.7-20.3 2.7-2.8 6.8-5.1 11.2-6.1 3.7-0.8 10.2-0.5 14.2,0.8zM181.7,152.9c2.4,1.2 5.4,4.6 6.2,6.8 0.8,2.5 0.7,6.1-0.2,8.6-0.8,2-4.8,6.4-5.9,6.4-0.8,0-0.5,1.2 2.8,9 1.8,4.3 3.2,8.1 3.2,8.6 0,3.1-1.6,3.6-11.3,3.6h-8.2l-1.3-1.3c-0.7-0.7-1.3-1.7-1.3-2.1 0-0.4 1.4-4.4 3.1-9l3.2-8.3-1.6-0.9c-3.6-1.9-6.1-6.2-6.1-10.6 0-5.5 3.4-10.1 8.8-11.8 1.9-0.5 6.6,0 8.6,1z" transform="matrix(0.509,0,0,0.509,-5.137,-5.118)"/>
                        </svg>
                        <span className="font-medium text-gray-900 dark:text-gray-100">{t('cyberTools.title')}</span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded transition-colors cursor-pointer">
                        <X size={18} className="text-gray-500" />
                    </button>
                </div>

                {/* Tabs */}
                <div className="flex border-b border-gray-200 dark:border-gray-700 px-2">
                    {tabs.map(tab => (
                        <button
                            key={tab.id}
                            onClick={() => setActiveTab(tab.id)}
                            className={`flex items-center gap-1.5 px-3 py-2 text-sm font-medium transition-colors border-b-2 cursor-pointer ${
                                activeTab === tab.id
                                    ? 'border-cyan-500 text-cyan-600 dark:text-cyan-400'
                                    : 'border-transparent text-gray-500 hover:text-gray-700 dark:hover:text-gray-300'
                            }`}
                        >
                            {tab.icon}
                            {tab.label}
                        </button>
                    ))}
                </div>

                {/* Content */}
                <div className="p-4 overflow-y-auto flex-1">
                    {activeTab === 'hash' && <HashForgeTab />}
                    {activeTab === 'crypto' && <CryptoLabTab />}
                    {activeTab === 'password' && <PasswordForgeTab />}
                </div>
            </div>
        </div>
    );
};

// ─── Shared Components ──────────────────────────────────────────────────────

const CopyButton: React.FC<{ text: string; label?: string }> = ({ text, label }) => {
    const [copied, setCopied] = useState(false);
    const handleCopy = useCallback(async () => {
        try {
            await invoke('copy_to_clipboard', { text });
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        } catch { /* clipboard may fail in some environments */ }
    }, [text]);

    return (
        <button
            onClick={handleCopy}
            className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
            title={label}
        >
            {copied ? <Check size={12} className="text-green-500" /> : <Copy size={12} />}
            {copied ? 'Copied!' : (label || 'Copy')}
        </button>
    );
};

const PillButton: React.FC<{ active: boolean; onClick: () => void; children: React.ReactNode }> = ({ active, onClick, children }) => (
    <button
        onClick={onClick}
        className={`px-3 py-1 text-xs font-medium rounded-md transition-colors cursor-pointer ${
            active
                ? 'bg-cyan-500 text-white'
                : 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 hover:bg-gray-200 dark:hover:bg-gray-600'
        }`}
    >
        {children}
    </button>
);

// ─── Hash Forge Tab ─────────────────────────────────────────────────────────

const HASH_ALGOS = ['MD5', 'SHA-1', 'SHA-256', 'SHA-512', 'BLAKE3'] as const;
const HASH_ENCODINGS = ['utf-8', 'base64', 'hex', 'binary'] as const;

// Absolute path of an OS file dropped via HTML drag-and-drop.
// The main webview always uses `disable_drag_drop_handler` (Windows HTML5
// requirement), so Tauri's `onDragDropEvent` never fires — only DataTransfer
// is available. On WebKitGTK, `File.path` is usually absent and `text/uri-list`
// is often empty; callers must fall back to staging `files[0]` contents.
function uriOrPathToFsPath(raw: string): string | null {
    const line = raw.trim();
    if (!line || line.startsWith('#')) return null;
    if (line.startsWith('file:') || line.startsWith('FILE:')) {
        try {
            const u = new URL(line);
            let p = decodeURIComponent(u.pathname);
            // Windows file URLs: pathname is "/C:/Users/..." → "C:/Users/..."
            if (/^\/[A-Za-z]:[\\/]/.test(p)) p = p.slice(1);
            return p || null;
        } catch {
            try {
                return decodeURIComponent(line.replace(/^file:\/\/(localhost)?/i, '')) || null;
            } catch {
                return null;
            }
        }
    }
    // Plain absolute path (some file managers put this in text/plain)
    if (line.startsWith('/') || /^[A-Za-z]:[\\/]/.test(line)) return line;
    return null;
}

function extractDroppedPath(dt: DataTransfer): string | null {
    // 1) Non-standard File.path (Electron / some WebKit builds)
    for (const f of Array.from(dt.files || [])) {
        const p = (f as File & { path?: string }).path;
        if (p && p.trim()) return p.trim();
    }
    for (let i = 0; i < (dt.items?.length ?? 0); i++) {
        const item = dt.items[i];
        if (item.kind !== 'file') continue;
        const f = item.getAsFile() as (File & { path?: string }) | null;
        if (f?.path?.trim()) return f.path.trim();
    }

    // 2) MIME types file managers commonly set (must read synchronously in drop)
    const mimes = ['text/uri-list', 'text/plain', 'text/x-moz-url', 'URL'];
    for (const mime of mimes) {
        let raw = '';
        try { raw = dt.getData(mime); } catch { /* ignore */ }
        if (!raw) continue;
        for (const line of raw.split(/\r?\n/)) {
            const p = uriOrPathToFsPath(line);
            if (p) return p;
        }
    }

    // 3) Last resort: any type whose payload looks like a file URI / abs path
    try {
        for (const t of Array.from(dt.types || [])) {
            let raw = '';
            try { raw = dt.getData(t); } catch { /* ignore */ }
            if (!raw) continue;
            for (const line of raw.split(/\r?\n/)) {
                const p = uriOrPathToFsPath(line);
                if (p) return p;
            }
        }
    } catch { /* ignore */ }

    return null;
}

/** Read a dropped File as standard base64 (no data-URL prefix) for stage_hash_drop. */
function fileToBase64(file: File): Promise<string> {
    return new Promise((resolve, reject) => {
        const reader = new FileReader();
        reader.onload = () => {
            const result = reader.result;
            if (typeof result !== 'string') {
                reject(new Error('Failed to read dropped file'));
                return;
            }
            const comma = result.indexOf(',');
            resolve(comma >= 0 ? result.slice(comma + 1) : result);
        };
        reader.onerror = () => reject(reader.error ?? new Error('FileReader failed'));
        reader.readAsDataURL(file);
    });
}
type HashEncoding = (typeof HASH_ENCODINGS)[number];

const HashForgeTab: React.FC = () => {
    const t = useTranslation();
    const [mode, setMode] = useState<'text' | 'file'>('text');
    const [input, setInput] = useState('');
    const [filePath, setFilePath] = useState('');
    const [algorithm, setAlgorithm] = useState('sha256');
    const [encoding, setEncoding] = useState<HashEncoding>('utf-8');
    const [outputLen, setOutputLen] = useState(32);
    const [result, setResult] = useState('');
    const [expected, setExpected] = useState('');
    const [match, setMatch] = useState<boolean | null>(null);
    const [loading, setLoading] = useState(false);
    const [dragActive, setDragActive] = useState(false);
    const dragDepthRef = useRef(0);
    const calcGenRef = useRef(0);

    const algoMap: Record<string, string> = { 'MD5': 'md5', 'SHA-1': 'sha1', 'SHA-256': 'sha256', 'SHA-512': 'sha512', 'BLAKE3': 'blake3' };
    const isBlake3 = algorithm === 'blake3';

    const encodingLabel = (enc: HashEncoding): string => {
        switch (enc) {
            case 'utf-8': return t('cyberTools.hashEncodingUtf8');
            case 'base64': return t('cyberTools.hashEncodingBase64');
            case 'hex': return t('cyberTools.hashEncodingHex');
            case 'binary': return t('cyberTools.hashEncodingBinary');
        }
    };

    // Debounced auto-calculate (~300ms) on input/algorithm/encoding/output-len/file.
    // Empty text input is hashed (BLAKE3 empty vector is intentional). File mode
    // waits for a path. Calculate button removed (BLAKE3-demo parity).
    useEffect(() => {
        if (mode === 'file' && !filePath) {
            setResult('');
            setMatch(null);
            setLoading(false);
            return;
        }

        const gen = ++calcGenRef.current;
        setLoading(true);
        const timer = window.setTimeout(async () => {
            try {
                let hash: string;
                if (mode === 'text') {
                    const clampedLen = Math.min(1024, Math.max(1, Math.floor(outputLen) || 32));
                    hash = await invoke<string>('hash_text', {
                        text: input,
                        algorithm,
                        encoding,
                        outputLen: isBlake3 ? clampedLen : null,
                    });
                } else {
                    const clampedLen = Math.min(1024, Math.max(1, Math.floor(outputLen) || 32));
                    hash = await invoke<string>('hash_file', {
                        path: filePath,
                        algorithm,
                        outputLen: isBlake3 ? clampedLen : null,
                    });
                }
                if (gen !== calcGenRef.current) return;
                setResult(hash);
                // Match against expected is handled by the separate compare effect.
                setMatch(null);
            } catch (e) {
                if (gen !== calcGenRef.current) return;
                setResult(`Error: ${e}`);
                setMatch(null);
            } finally {
                if (gen === calcGenRef.current) setLoading(false);
            }
        }, 300);

        return () => {
            window.clearTimeout(timer);
        };
    }, [mode, input, filePath, algorithm, encoding, outputLen, isBlake3]);

    const selectFile = useCallback(async () => {
        const selected = await open({ multiple: false, directory: false });
        if (selected) {
            setMode('file');
            setFilePath(selected as string);
        }
    }, []);

    // Belt-and-suspenders: if the webview ever re-enables the native drag-drop
    // handler, prefer its absolute paths (no blob staging). With the current
    // `disable_drag_drop_handler` this listener never fires on any platform.
    useEffect(() => {
        let unlisten: (() => void) | undefined;
        let cancelled = false;
        (async () => {
            try {
                const webview = getCurrentWebview();
                const un = await webview.onDragDropEvent((event) => {
                    if (event.payload.type === 'over' || event.payload.type === 'enter') {
                        setDragActive(true);
                    } else if (event.payload.type === 'leave') {
                        setDragActive(false);
                    } else if (event.payload.type === 'drop' && event.payload.paths.length > 0) {
                        setDragActive(false);
                        dragDepthRef.current = 0;
                        setMode('file');
                        setFilePath(event.payload.paths[0]);
                    }
                });
                if (cancelled) un();
                else unlisten = un;
            } catch {
                /* webview API unavailable outside Tauri */
            }
        })();
        return () => {
            cancelled = true;
            if (unlisten) unlisten();
        };
    }, []);

    // Auto-compare when expected changes against a stable result (re-run is
    // also covered by the calculate effect when expected is in its deps).
    useEffect(() => {
        if (result && !result.startsWith('Error:') && expected.trim()) {
            invoke<boolean>('compare_hashes', { hashA: result, hashB: expected.trim() }).then(setMatch);
        } else if (!expected.trim()) {
            setMatch(null);
        }
    }, [expected, result]);

    const applyDroppedPath = useCallback((path: string) => {
        setMode('file');
        setFilePath(path);
    }, []);

    const handleHtmlDrop = useCallback(async (e: ReactDragEvent) => {
        e.preventDefault();
        e.stopPropagation();
        dragDepthRef.current = 0;
        setDragActive(false);

        const dt = e.dataTransfer;
        // Read DataTransfer synchronously — some engines clear getData after
        // the drop handler returns (including across await boundaries).
        const path = extractDroppedPath(dt);
        const file = dt.files?.[0] ?? null;

        if (path) {
            applyDroppedPath(path);
            return;
        }
        if (!file) return;

        // WebKitGTK common case: File blob present, no absolute path. Stage
        // contents so hash_file can stream the real bytes (not the path string).
        try {
            setLoading(true);
            const dataBase64 = await fileToBase64(file);
            const staged = await invoke<string>('stage_hash_drop', {
                name: file.name,
                dataBase64,
            });
            applyDroppedPath(staged);
        } catch (err) {
            setResult(`Error: ${err}`);
            setLoading(false);
        }
    }, [applyDroppedPath]);

    return (
        <div
            className={`relative space-y-4 rounded-md transition-colors ${
                dragActive ? 'ring-2 ring-cyan-500 ring-offset-2 dark:ring-offset-gray-800 bg-cyan-500/5' : ''
            }`}
            onDragEnter={e => {
                e.preventDefault();
                e.stopPropagation();
                dragDepthRef.current += 1;
                setDragActive(true);
            }}
            onDragOver={e => {
                e.preventDefault();
                e.stopPropagation();
                e.dataTransfer.dropEffect = 'copy';
            }}
            onDragLeave={e => {
                e.preventDefault();
                e.stopPropagation();
                // Depth counter: entering children fires leave on parent; only
                // clear the indicator when the pointer actually leaves the zone.
                dragDepthRef.current = Math.max(0, dragDepthRef.current - 1);
                if (dragDepthRef.current === 0) setDragActive(false);
            }}
            onDrop={e => { void handleHtmlDrop(e); }}
        >
            {dragActive && (
                <div className="absolute inset-0 z-10 flex flex-col items-center justify-center gap-2 rounded-md border-2 border-dashed border-cyan-500 bg-cyan-500/10 backdrop-blur-[1px] pointer-events-none">
                    <FileSearch size={28} className="text-cyan-500" />
                    <span className="text-sm font-medium text-cyan-700 dark:text-cyan-300">{t('cyberTools.hashDropHint')}</span>
                </div>
            )}
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.hashDescription')}</p>
            <p className="text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.hashDropHint')}</p>

            {/* Mode toggle */}
            <div className="flex gap-2">
                <PillButton active={mode === 'text'} onClick={() => setMode('text')}>
                    <span className="flex items-center gap-1"><Type size={12} /> {t('cyberTools.hashModeText')}</span>
                </PillButton>
                <PillButton active={mode === 'file'} onClick={() => setMode('file')}>
                    <span className="flex items-center gap-1"><FileSearch size={12} /> {t('cyberTools.hashModeFile')}</span>
                </PillButton>
            </div>

            {/* Input */}
            {mode === 'text' ? (
                <div className="space-y-2">
                    <textarea
                        value={input}
                        onChange={e => setInput(e.target.value)}
                        placeholder={t('cyberTools.hashInputPlaceholder')}
                        className="w-full h-24 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 resize-none focus:outline-none focus:ring-1 focus:ring-cyan-500 font-mono"
                    />
                    <div>
                        <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.hashEncoding')}</label>
                        <div className="flex flex-wrap gap-1.5">
                            {HASH_ENCODINGS.map(enc => (
                                <PillButton key={enc} active={encoding === enc} onClick={() => setEncoding(enc)}>
                                    {encodingLabel(enc)}
                                </PillButton>
                            ))}
                        </div>
                    </div>
                </div>
            ) : (
                <div className="flex gap-2">
                    <input
                        value={filePath}
                        readOnly
                        placeholder={t('cyberTools.hashSelectFile')}
                        className="flex-1 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 truncate"
                    />
                    <button
                        onClick={selectFile}
                        className="px-3 py-2 text-sm rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
                    >
                        <FileSearch size={16} />
                    </button>
                </div>
            )}

            {/* Algorithm */}
            <div>
                <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.hashAlgorithm')}</label>
                <div className="flex flex-wrap gap-1.5">
                    {HASH_ALGOS.map(a => (
                        <PillButton key={a} active={algorithm === algoMap[a]} onClick={() => setAlgorithm(algoMap[a])}>
                            {a}{(a === 'MD5' || a === 'SHA-1') ? ` · ${t('cyberTools.hashLegacy')}` : ''}
                        </PillButton>
                    ))}
                </div>
            </div>

            {/* BLAKE3 XOF output length (bytes) */}
            {isBlake3 && (
                <div>
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">
                        {t('cyberTools.hashOutputLength')}
                    </label>
                    <div className="flex items-center gap-2">
                        <input
                            type="number"
                            min={1}
                            max={1024}
                            value={outputLen}
                            onChange={e => {
                                const n = Number(e.target.value);
                                if (Number.isFinite(n)) setOutputLen(n);
                            }}
                            className="w-24 px-3 py-1.5 text-sm font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                        />
                        <span className="text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.hashOutputLengthHint')}</span>
                    </div>
                </div>
            )}

            {/* Loading indicator (auto-calc — no Calculate button) */}
            {loading && (
                <div className="flex items-center justify-center gap-2 py-1 text-xs text-gray-500 dark:text-gray-400">
                    <Loader2 size={14} className="animate-spin" /> {t('cyberTools.hashCalculating')}
                </div>
            )}

            {/* Result */}
            {result && (
                <div className="space-y-2">
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.hashResult')}</label>
                    <div className="flex items-start gap-2">
                        <code className={`flex-1 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 break-all border border-gray-200 dark:border-gray-700 select-all ${
                            result.startsWith('Error:') ? 'text-red-600 dark:text-red-400' : 'text-gray-800 dark:text-gray-200'
                        }`}>
                            {result}
                        </code>
                        {!result.startsWith('Error:') && (
                            <CopyButton text={result} label={t('cyberTools.hashCopy')} />
                        )}
                    </div>
                </div>
            )}

            {/* Compare */}
            {result && !result.startsWith('Error:') && (
                <div className="space-y-1">
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.hashExpected')}</label>
                    <input
                        value={expected}
                        onChange={e => setExpected(e.target.value)}
                        placeholder={t('cyberTools.hashExpectedPlaceholder')}
                        className={`w-full px-3 py-2 text-xs font-mono rounded border bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 ${
                            match === true ? 'border-green-500 focus:ring-green-500' :
                            match === false ? 'border-red-500 focus:ring-red-500' :
                            'border-gray-300 dark:border-gray-600 focus:ring-cyan-500'
                        }`}
                    />
                    {match === true && (
                        <div className="flex items-center gap-1 text-xs text-green-500">
                            <CheckCircle2 size={12} /> {t('cyberTools.hashMatch')}
                        </div>
                    )}
                    {match === false && (
                        <div className="flex items-center gap-1 text-xs text-red-500">
                            <AlertTriangle size={12} /> {t('cyberTools.hashMismatch')}
                        </div>
                    )}
                </div>
            )}
        </div>
    );
};

// ─── CryptoLab Tab ──────────────────────────────────────────────────────────

const CryptoLabTab: React.FC = () => {
    const t = useTranslation();
    const [mode, setMode] = useState<'encrypt' | 'decrypt'>('encrypt');
    const [algorithm, setAlgorithm] = useState('aes-256-gcm');
    const [input, setInput] = useState('');
    const [password, setPassword] = useState('');
    const [showPassword, setShowPassword] = useState(false);
    const [output, setOutput] = useState('');
    const [loading, setLoading] = useState(false);
    const [error, setError] = useState('');

    const execute = useCallback(async () => {
        setError('');
        setOutput('');
        if (!input.trim()) { setError(t('cyberTools.cryptoNoInput')); return; }
        if (!password) { setError(t('cyberTools.cryptoNoPassword')); return; }

        setLoading(true);
        try {
            if (mode === 'encrypt') {
                const result: string = await invoke('crypto_encrypt_text', {
                    plaintext: input, password, algorithm
                });
                setOutput(result);
            } else {
                const result: string = await invoke('crypto_decrypt_text', {
                    encoded: input.trim(), password
                });
                setOutput(result);
            }
        } catch (e) {
            setError(String(e));
        }
        setLoading(false);
    }, [mode, algorithm, input, password, t]);

    return (
        <div className="space-y-4">
            <p className="text-xs text-gray-500 dark:text-gray-400">{t('cyberTools.cryptoDescription')}</p>

            {/* Mode */}
            <div className="flex gap-2">
                <PillButton active={mode === 'encrypt'} onClick={() => { setMode('encrypt'); setInput(''); setOutput(''); setError(''); }}>
                    {t('cyberTools.cryptoEncrypt')}
                </PillButton>
                <PillButton active={mode === 'decrypt'} onClick={() => { setMode('decrypt'); setInput(''); setOutput(''); setError(''); }}>
                    {t('cyberTools.cryptoDecrypt')}
                </PillButton>
            </div>

            {/* Algorithm (only for encrypt) */}
            {mode === 'encrypt' && (
                <div>
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.cryptoAlgorithm')}</label>
                    <div className="flex gap-1.5">
                        <PillButton active={algorithm === 'aes-256-gcm'} onClick={() => setAlgorithm('aes-256-gcm')}>AES-256-GCM</PillButton>
                        <PillButton active={algorithm === 'chacha20-poly1305'} onClick={() => setAlgorithm('chacha20-poly1305')}>ChaCha20-Poly1305</PillButton>
                    </div>
                </div>
            )}

            {/* Input */}
            <textarea
                value={input}
                onChange={e => setInput(e.target.value)}
                placeholder={mode === 'encrypt' ? t('cyberTools.cryptoInputPlaceholder') : t('cyberTools.cryptoCiphertextPlaceholder')}
                className="w-full h-24 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 resize-none focus:outline-none focus:ring-1 focus:ring-cyan-500 font-mono"
            />

            {/* Password */}
            <div>
                <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.cryptoPassword')}</label>
                <div className="relative">
                    <input
                        type={showPassword ? 'text' : 'password'}
                        value={password}
                        onChange={e => setPassword(e.target.value)}
                        placeholder={t('cyberTools.cryptoPasswordPlaceholder')}
                        className="w-full px-3 py-2 pr-10 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                    />
                    <button tabIndex={-1}
                        onClick={() => setShowPassword(!showPassword)}
                        className="absolute right-2 top-1/2 -translate-y-1/2 p-1 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 cursor-pointer"
                    >
                        {showPassword ? <EyeOff size={14} /> : <Eye size={14} />}
                    </button>
                </div>
            </div>

            {/* KDF info */}
            <p className="text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.cryptoKdfInfo')}</p>

            {/* Execute */}
            <button
                onClick={execute}
                disabled={loading}
                className="w-full py-2 text-sm font-medium rounded bg-cyan-500 hover:bg-cyan-600 disabled:bg-gray-300 dark:disabled:bg-gray-700 text-white transition-colors cursor-pointer disabled:cursor-not-allowed flex items-center justify-center gap-2"
            >
                {loading ? (
                    <><Loader2 size={14} className="animate-spin" /> {mode === 'encrypt' ? t('cyberTools.cryptoEncrypting') : t('cyberTools.cryptoDecrypting')}</>
                ) : (
                    mode === 'encrypt' ? t('cyberTools.cryptoEncrypt') : t('cyberTools.cryptoDecrypt')
                )}
            </button>

            {/* Error */}
            {error && (
                <div className="flex items-center gap-1.5 text-xs text-red-500">
                    <AlertTriangle size={12} /> {error}
                </div>
            )}

            {/* Output */}
            {output && (
                <div className="space-y-2">
                    <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{t('cyberTools.cryptoResult')}</label>
                    <div className="flex items-start gap-2">
                        <code className="flex-1 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 text-gray-800 dark:text-gray-200 break-all border border-gray-200 dark:border-gray-700 select-all max-h-40 overflow-y-auto">
                            {output}
                        </code>
                        <CopyButton text={output} label={t('cyberTools.cryptoCopy')} />
                    </div>
                </div>
            )}
        </div>
    );
};

export default CyberToolsModal;
