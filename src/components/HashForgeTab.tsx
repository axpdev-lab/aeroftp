// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useCallback, useEffect, useRef, type DragEvent as ReactDragEvent } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { getCurrentWebview } from '@tauri-apps/api/webview';
import { FileSearch, Type, Loader2, AlertTriangle, CheckCircle2, Shuffle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { pickFile } from '../utils/pickPath';
import { nativeDropOwnerAt } from '../utils/nativeDropOwner';
import { BLAKE3_MODES, blake3Args, type Blake3Mode } from '../utils/hashForgeArgs';
import { Argon2idPanel } from './Argon2idPanel';
import { CopyButton, PillButton, randomHex } from './CyberToolsShared';

const HASH_ALGOS = ['MD5', 'SHA-1', 'SHA-256', 'SHA-512', 'BLAKE3', 'Argon2id'] as const;

const HASH_ENCODINGS = ['utf-8', 'base64', 'hex', 'binary'] as const;

// Absolute path from HTML5 DataTransfer (Windows primary path; Linux fallback).
// On Linux the webview keeps Tauri's native drag handler — Nautilus→WebKitGTK
// often advertises text/uri-list while getData/files stay empty, so the real
// path comes from onDragDropEvent. Staging files[0] still covers synthetic
// drops and engines that expose a File blob without .path.
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
    const mimes = ['text/uri-list', 'text/plain', 'text/html', 'text/x-moz-url', 'URL'];
    for (const mime of mimes) {
        let raw = '';
        try { raw = dt.getData(mime); } catch { /* ignore */ }
        if (!raw) continue;
        const fromLines = pathFromUriPayload(raw);
        if (fromLines) return fromLines;
    }

    // 3) Last resort: any type whose payload looks like a file URI / abs path
    try {
        for (const t of Array.from(dt.types || [])) {
            let raw = '';
            try { raw = dt.getData(t); } catch { /* ignore */ }
            if (!raw) continue;
            const fromLines = pathFromUriPayload(raw);
            if (fromLines) return fromLines;
        }
    } catch { /* ignore */ }

    return null;
}

/** Pull a filesystem path out of uri-list / plain / html drag payloads. */
function pathFromUriPayload(raw: string): string | null {
    // Direct lines (uri-list / plain)
    for (const line of raw.split(/\r?\n/)) {
        const p = uriOrPathToFsPath(line);
        if (p) return p;
    }
    // HTML from Nautilus sometimes embeds file:// in href/src
    const hrefMatch = raw.match(/(?:href|src)=["'](file:[^"']+)["']/i)
        || raw.match(/(file:\/\/[^\s"'<>]+)/i);
    if (hrefMatch?.[1]) {
        const p = uriOrPathToFsPath(hrefMatch[1]);
        if (p) return p;
    }
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

interface HashForgeTabProps {
    /** Which OS file drops are meant for Hash Forge: every drop in the window
     *  when it is shown as a modal over the app, or only drops on its own
     *  panel when it sits next to other surfaces (AeroTools). */
    nativeDropScope: 'window' | 'panel';
}

export const HashForgeTab: React.FC<HashForgeTabProps> = ({ nativeDropScope }) => {
    const t = useTranslation();
    const [mode, setMode] = useState<'text' | 'file'>('text');
    const [input, setInput] = useState('');
    const [filePath, setFilePath] = useState('');
    const [algorithm, setAlgorithm] = useState('sha256');
    const [encoding, setEncoding] = useState<HashEncoding>('utf-8');
    const [outputLen, setOutputLen] = useState(32);
    const [blake3Mode, setBlake3Mode] = useState<Blake3Mode>('hash');
    const [blake3Key, setBlake3Key] = useState('');
    const [blake3Context, setBlake3Context] = useState('');
    const [result, setResult] = useState('');
    const [expected, setExpected] = useState('');
    const [match, setMatch] = useState<boolean | null>(null);
    const [loading, setLoading] = useState(false);
    const [dragActive, setDragActive] = useState(false);
    const dragDepthRef = useRef(0);
    const calcGenRef = useRef(0);

    const algoMap: Record<string, string> = { 'MD5': 'md5', 'SHA-1': 'sha1', 'SHA-256': 'sha256', 'SHA-512': 'sha512', 'BLAKE3': 'blake3', 'Argon2id': 'argon2id' };
    const isBlake3 = algorithm === 'blake3';
    // Argon2id is a password hash: text input only, derived on request by
    // Argon2idPanel instead of the debounced auto-calculation below.
    const isArgon2 = algorithm === 'argon2id';
    const effectiveMode = isArgon2 ? 'text' : mode;
    // A BLAKE3 key is a secret: it is not kept once keyed mode is left, so
    // it does not linger in the open window while another algorithm is used.
    useEffect(() => {
        if (algorithm !== 'blake3' || blake3Mode !== 'keyed') setBlake3Key('');
    }, [algorithm, blake3Mode]);

    const b3Args = blake3Args(algorithm, blake3Mode, blake3Key, blake3Context);
    const blake3KeyArg = b3Args?.blake3Key ?? null;
    const blake3ContextArg = b3Args?.blake3Context ?? null;
    const blake3Incomplete = b3Args === null;
    const blake3Label = (m: Blake3Mode): string => {
        switch (m) {
            case 'hash': return t('cyberTools.hashBlake3ModeHash');
            case 'keyed': return t('cyberTools.hashBlake3ModeKeyed');
            case 'derive': return t('cyberTools.hashBlake3ModeDerive');
        }
    };

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
        if (isArgon2 || blake3Incomplete || (mode === 'file' && !filePath)) {
            ++calcGenRef.current;
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
                        blake3Key: blake3KeyArg,
                        blake3Context: blake3ContextArg,
                    });
                } else {
                    const clampedLen = Math.min(1024, Math.max(1, Math.floor(outputLen) || 32));
                    hash = await invoke<string>('hash_file', {
                        path: filePath,
                        algorithm,
                        outputLen: isBlake3 ? clampedLen : null,
                        blake3Key: blake3KeyArg,
                        blake3Context: blake3ContextArg,
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
    }, [mode, input, filePath, algorithm, encoding, outputLen, isBlake3, isArgon2, blake3Incomplete, blake3KeyArg, blake3ContextArg]);

    // A staged drop is a plaintext copy of the dropped file in the temp dir.
    // Track it so it is removed as soon as it is replaced or the panel goes
    // away, instead of lingering until the OS clears /tmp.
    const stagedPathRef = useRef<string | null>(null);
    const discardStaged = useCallback(() => {
        const staged = stagedPathRef.current;
        if (!staged) return;
        stagedPathRef.current = null;
        void invoke('discard_hash_drop', { path: staged }).catch(() => {});
    }, []);

    useEffect(() => discardStaged, [discardStaged]);

    const applyDroppedPath = useCallback((path: string) => {
        // Replacing the current selection: the previous staged copy is dead.
        if (stagedPathRef.current !== path) discardStaged();
        setMode('file');
        setFilePath(path);
    }, [discardStaged]);

    const selectFile = useCallback(async () => {
        const selected = await pickFile({ multiple: false, directory: false });
        if (selected) {
            applyDroppedPath(selected as string);
        }
    }, [applyDroppedPath]);

    // Primary drop path on Linux/macOS: Tauri native onDragDropEvent (GTK/WebKit
    // file URIs). Windows keeps disable_drag_drop_handler so HTML5 handleHtmlDrop
    // owns drops there. The event is webview-wide. As a modal, Hash Forge covers
    // the app and takes every drop (App.tsx gates the local-panel import
    // listener while the modal is open). As an AeroTools panel it takes only the
    // drops that land on its own panel, which App.tsx in turn leaves alone.
    const zoneRef = useRef<HTMLDivElement>(null);
    useEffect(() => {
        const isForHashForge = (position: { x: number; y: number }) =>
            nativeDropScope === 'window' || !!nativeDropOwnerAt(position)?.contains(zoneRef.current);
        let unlisten: (() => void) | undefined;
        let cancelled = false;
        (async () => {
            try {
                const webview = getCurrentWebview();
                const un = await webview.onDragDropEvent((event) => {
                    if (event.payload.type === 'over' || event.payload.type === 'enter') {
                        setDragActive(isForHashForge(event.payload.position));
                    } else if (event.payload.type === 'leave') {
                        setDragActive(false);
                        dragDepthRef.current = 0;
                    } else if (event.payload.type === 'drop' && event.payload.paths.length > 0) {
                        setDragActive(false);
                        dragDepthRef.current = 0;
                        if (isForHashForge(event.payload.position)) applyDroppedPath(event.payload.paths[0]);
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
    }, [nativeDropScope, applyDroppedPath]);

    // Auto-compare when expected changes against a stable result (re-run is
    // also covered by the calculate effect when expected is in its deps).
    useEffect(() => {
        if (result && !result.startsWith('Error:') && expected.trim()) {
            invoke<boolean>('compare_hashes', { hashA: result, hashB: expected.trim() }).then(setMatch);
        } else if (!expected.trim()) {
            setMatch(null);
        }
    }, [expected, result]);

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

        // Some engines advertise uri-list/html but only yield the payload via getAsString.
        const asyncUri = await new Promise<string | null>(resolve => {
            let settled = false;
            const done = (v: string | null) => {
                if (settled) return;
                settled = true;
                resolve(v);
            };
            const timer = window.setTimeout(() => done(null), 400);
            try {
                const items = Array.from(dt.items || []).filter(i => i.kind === 'string');
                if (items.length === 0) {
                    window.clearTimeout(timer);
                    done(null);
                    return;
                }
                let remaining = items.length;
                let found: string | null = null;
                for (const item of items) {
                    item.getAsString(s => {
                        if (!found) {
                            const p = pathFromUriPayload(s || '');
                            if (p) found = p;
                        }
                        remaining -= 1;
                        if (remaining === 0) {
                            window.clearTimeout(timer);
                            done(found);
                        }
                    });
                }
            } catch {
                window.clearTimeout(timer);
                done(null);
            }
        });
        if (asyncUri) {
            applyDroppedPath(asyncUri);
            return;
        }

        if (!file) return;

        // File blob present, no absolute path: stage contents for hash_file.
        try {
            setLoading(true);
            const dataBase64 = await fileToBase64(file);
            const staged = await invoke<string>('stage_hash_drop', {
                name: file.name,
                dataBase64,
            });
            applyDroppedPath(staged);
            stagedPathRef.current = staged;
        } catch (err) {
            setResult(`Error: ${err}`);
            setLoading(false);
        }
    }, [applyDroppedPath]);

    return (
        <div
            ref={zoneRef}
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
            {!isArgon2 && <div className="flex gap-2">
                <PillButton active={mode === 'text'} onClick={() => setMode('text')}>
                    <span className="flex items-center gap-1"><Type size={12} /> {t('cyberTools.hashModeText')}</span>
                </PillButton>
                <PillButton active={mode === 'file'} onClick={() => setMode('file')}>
                    <span className="flex items-center gap-1"><FileSearch size={12} /> {t('cyberTools.hashModeFile')}</span>
                </PillButton>
            </div>}

            {/* Input */}
            {effectiveMode === 'text' ? (
                <div className="space-y-2">
                    <textarea
                        value={input}
                        onChange={e => setInput(e.target.value)}
                        placeholder={isArgon2 ? t('cyberTools.argon2PasswordPlaceholder') : t('cyberTools.hashInputPlaceholder')}
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
                        onClick={() => { void selectFile(); }}
                        onKeyDown={e => {
                            if (e.key === 'Enter' || e.key === ' ') {
                                e.preventDefault();
                                void selectFile();
                            }
                        }}
                        placeholder={t('cyberTools.hashSelectFile')}
                        title={t('cyberTools.hashSelectFile')}
                        className="flex-1 px-3 py-2 text-sm rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 truncate cursor-pointer hover:border-cyan-500/60 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                    />
                    <button
                        onClick={() => { void selectFile(); }}
                        className="px-3 py-2 text-sm rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
                        title={t('cyberTools.hashSelectFile')}
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

            {/* BLAKE3 mode: plain, keyed (b3sum --keyed), derive-key (b3sum --derive-key) */}
            {isBlake3 && (
                <div className="space-y-2">
                    <div>
                        <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.hashBlake3Mode')}</label>
                        <div className="flex flex-wrap gap-1.5">
                            {BLAKE3_MODES.map(m => (
                                <PillButton key={m} active={blake3Mode === m} onClick={() => setBlake3Mode(m)}>
                                    {blake3Label(m)}
                                </PillButton>
                            ))}
                        </div>
                    </div>
                    {blake3Mode === 'keyed' && (
                        <div>
                            <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.hashBlake3Key')}</label>
                            <div className="flex gap-2">
                                <input
                                    value={blake3Key}
                                    onChange={e => setBlake3Key(e.target.value)}
                                    spellCheck={false}
                                    placeholder="0123...cdef"
                                    className="flex-1 min-w-0 px-3 py-1.5 text-xs font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                                />
                                <button
                                    onClick={() => setBlake3Key(randomHex(32))}
                                    className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
                                >
                                    <Shuffle size={12} /> {t('cyberTools.hashRandom')}
                                </button>
                            </div>
                        </div>
                    )}
                    {blake3Mode === 'derive' && (
                        <div>
                            <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.hashBlake3Context')}</label>
                            <input
                                value={blake3Context}
                                onChange={e => setBlake3Context(e.target.value)}
                                spellCheck={false}
                                className="w-full px-3 py-1.5 text-xs font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                            />
                            <p className="mt-1 text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.hashBlake3ContextHint')}</p>
                        </div>
                    )}
                </div>
            )}

            {isArgon2 && <Argon2idPanel password={input} passwordEncoding={encoding} />}

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
                        <code className={`flex-1 min-w-0 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 break-all border border-gray-200 dark:border-gray-700 select-all ${
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
