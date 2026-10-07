// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useState, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Eye, EyeOff, Loader2, AlertTriangle } from 'lucide-react';
import { useTranslation } from '../i18n';
import { CopyButton, PillButton } from './CyberToolsShared';

export const CryptoLabTab: React.FC = () => {
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
                    <button data-agent="deny" tabIndex={-1}
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
