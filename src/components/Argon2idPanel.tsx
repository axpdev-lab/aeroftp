// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useEffect, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Loader2, Shuffle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { CopyButton, PillButton, randomHex } from './CyberToolsShared';

/** Salt encodings offered in the panel: a salt is usually random bytes (hex)
 *  or a fixed label (UTF-8). */
const SALT_ENCODINGS = ['hex', 'utf-8'] as const;
type SaltEncoding = (typeof SALT_ENCODINGS)[number];

/** RFC 9106 section 4, second recommended option: t=3, p=4, m=64 MiB, and a
 *  128-bit salt with a 256-bit tag. */
const DEFAULT_MEMORY_KIB = 64 * 1024;
const DEFAULT_ITERATIONS = 3;
const DEFAULT_PARALLELISM = 4;
const DEFAULT_OUTPUT_LEN = 32;
/** Mirrors ARGON2_MEMORY_KIB_MAX in cyber_tools.rs; the backend enforces it. */
const MEMORY_KIB_MAX = 2 * 1024 * 1024;

interface Argon2idOutput {
    hex: string;
    phc: string;
}

interface Argon2idPanelProps {
    /** Password text from the Hash Forge input area. */
    password: string;
    /** Encoding selected for that text (utf-8, base64, hex, binary). */
    passwordEncoding: string;
}

/**
 * Argon2id derivation for the Hash Forge tab.
 *
 * Unlike the digests it does not recompute on every keystroke: each run
 * allocates the chosen memory on purpose, so it runs when the user asks.
 * A result is cleared as soon as any input changes, so what is shown always
 * belongs to the inputs on screen.
 */
export const Argon2idPanel: React.FC<Argon2idPanelProps> = ({ password, passwordEncoding }) => {
    const t = useTranslation();
    const [salt, setSalt] = useState(() => randomHex(16));
    const [saltEncoding, setSaltEncoding] = useState<SaltEncoding>('hex');
    const [memoryKib, setMemoryKib] = useState(DEFAULT_MEMORY_KIB);
    const [iterations, setIterations] = useState(DEFAULT_ITERATIONS);
    const [parallelism, setParallelism] = useState(DEFAULT_PARALLELISM);
    const [outputLen, setOutputLen] = useState(DEFAULT_OUTPUT_LEN);
    const [output, setOutput] = useState<Argon2idOutput | null>(null);
    const [error, setError] = useState('');
    const [running, setRunning] = useState(false);
    const runGenRef = useRef(0);

    useEffect(() => {
        runGenRef.current += 1;
        setOutput(null);
        setError('');
        setRunning(false);
    }, [password, passwordEncoding, salt, saltEncoding, memoryKib, iterations, parallelism, outputLen]);

    const derive = useCallback(async () => {
        const gen = ++runGenRef.current;
        setRunning(true);
        setError('');
        setOutput(null);
        try {
            const result = await invoke<Argon2idOutput>('argon2id_hash', {
                password,
                passwordEncoding,
                salt,
                saltEncoding,
                memoryKib,
                iterations,
                parallelism,
                outputLen,
            });
            if (gen === runGenRef.current) setOutput(result);
        } catch (e) {
            if (gen === runGenRef.current) setError(String(e));
        } finally {
            if (gen === runGenRef.current) setRunning(false);
        }
    }, [password, passwordEncoding, salt, saltEncoding, memoryKib, iterations, parallelism, outputLen]);

    const numberField = (
        label: string,
        value: number,
        set: (n: number) => void,
        min: number,
        max: number,
    ) => (
        <label className="block">
            <span className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{label}</span>
            <input
                type="number"
                min={min}
                max={max}
                value={value}
                onChange={e => {
                    const n = Math.floor(Number(e.target.value));
                    if (Number.isFinite(n)) set(n);
                }}
                className="w-full px-3 py-1.5 text-sm font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
            />
        </label>
    );

    return (
        <div className="space-y-3">
            <div>
                <label className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1 block">{t('cyberTools.argon2Salt')}</label>
                <div className="flex gap-2">
                    <input
                        value={salt}
                        onChange={e => setSalt(e.target.value)}
                        spellCheck={false}
                        className="flex-1 min-w-0 px-3 py-1.5 text-xs font-mono rounded border border-gray-300 dark:border-gray-600 bg-white dark:bg-gray-900 text-gray-900 dark:text-gray-100 focus:outline-none focus:ring-1 focus:ring-cyan-500"
                    />
                    <button
                        onClick={() => { setSalt(randomHex(16)); setSaltEncoding('hex'); }}
                        className="flex items-center gap-1 px-2 py-1 text-xs rounded bg-gray-100 dark:bg-gray-700 hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors cursor-pointer"
                    >
                        <Shuffle size={12} /> {t('cyberTools.hashRandom')}
                    </button>
                </div>
                <div className="flex items-center gap-1.5 mt-1.5">
                    {SALT_ENCODINGS.map(enc => (
                        <PillButton key={enc} active={saltEncoding === enc} onClick={() => setSaltEncoding(enc)}>
                            {enc === 'hex' ? t('cyberTools.hashEncodingHex') : t('cyberTools.hashEncodingUtf8')}
                        </PillButton>
                    ))}
                    <span className="text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.argon2SaltHint')}</span>
                </div>
            </div>

            <div className="grid grid-cols-4 gap-2">
                {numberField(t('cyberTools.argon2Memory'), memoryKib, setMemoryKib, 8, MEMORY_KIB_MAX)}
                {numberField(t('cyberTools.argon2Iterations'), iterations, setIterations, 1, 64)}
                {numberField(t('cyberTools.argon2Parallelism'), parallelism, setParallelism, 1, 255)}
                {numberField(t('cyberTools.argon2OutputLength'), outputLen, setOutputLen, 4, 1024)}
            </div>
            <p className="text-[10px] text-gray-400 dark:text-gray-500">{t('cyberTools.argon2DefaultsHint')}</p>

            <button
                onClick={() => { void derive(); }}
                disabled={running}
                className="w-full flex items-center justify-center gap-2 px-3 py-2 text-sm font-medium rounded bg-cyan-500 hover:bg-cyan-600 disabled:opacity-60 text-white transition-colors cursor-pointer disabled:cursor-not-allowed"
            >
                {running && <Loader2 size={14} className="animate-spin" />}
                {running ? t('cyberTools.hashCalculating') : t('cyberTools.argon2Derive')}
            </button>

            {error && (
                <code className="block px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 break-all border border-gray-200 dark:border-gray-700 text-red-600 dark:text-red-400">
                    {error}
                </code>
            )}

            {output && (
                <div className="space-y-2">
                    {[
                        { label: t('cyberTools.hashResult'), value: output.hex },
                        { label: t('cyberTools.argon2Phc'), value: output.phc },
                    ].map(row => (
                        <div key={row.label} className="space-y-1">
                            <label className="text-xs font-medium text-gray-500 dark:text-gray-400">{row.label}</label>
                            <div className="flex items-start gap-2">
                                <code className="flex-1 min-w-0 px-3 py-2 text-xs font-mono rounded bg-gray-50 dark:bg-gray-900 break-all border border-gray-200 dark:border-gray-700 select-all text-gray-800 dark:text-gray-200">
                                    {row.value}
                                </code>
                                <CopyButton text={row.value} label={t('cyberTools.hashCopy')} />
                            </div>
                        </div>
                    ))}
                </div>
            )}
        </div>
    );
};
