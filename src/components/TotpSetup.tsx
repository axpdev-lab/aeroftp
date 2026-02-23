import React, { useState, useEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useTranslation } from '../i18n';
import { Shield, Copy, Check, X, AlertCircle } from 'lucide-react';

interface TotpSetupProps {
    isOpen: boolean;
    onClose: () => void;
    onEnabled: (secret: string) => void;
}

export const TotpSetup: React.FC<TotpSetupProps> = ({ isOpen, onClose, onEnabled }) => {
    const t = useTranslation();
    const [step, setStep] = useState<'generate' | 'verify'>('generate');
    const [secret, setSecret] = useState('');
    const [uri, setUri] = useState('');
    const [code, setCode] = useState('');
    const [error, setError] = useState('');
    const [copied, setCopied] = useState(false);
    const copyTimerRef = useRef<ReturnType<typeof setTimeout>>();
    const dialogRef = useRef<HTMLDivElement>(null);

    // Fetch secret on open, cleanup on close (FE-003, FE-006)
    useEffect(() => {
        if (isOpen) {
            let cancelled = false;
            setStep('generate');
            setSecret('');
            setUri('');
            setCode('');
            setError('');
            invoke<{ secret: string; uri: string }>('totp_setup_start').then(result => {
                if (!cancelled) {
                    setSecret(result.secret);
                    setUri(result.uri);
                }
            }).catch(e => {
                if (!cancelled) setError(String(e));
            });
            return () => { cancelled = true; };
        } else {
            // Zeroize sensitive state on close (FE-003, SEC-002)
            setSecret('');
            setUri('');
            setCode('');
            setError('');
            setCopied(false);
            clearTimeout(copyTimerRef.current);
        }
    }, [isOpen]);

    // Escape key handler (FE-002)
    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose();
        };
        document.addEventListener('keydown', handler);
        return () => document.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    // Focus management on open (FE-013)
    useEffect(() => {
        if (isOpen && dialogRef.current) {
            dialogRef.current.focus();
        }
    }, [isOpen]);

    // Clipboard with error handling (FE-004)
    const handleCopy = useCallback(async () => {
        try {
            await navigator.clipboard.writeText(secret);
            setCopied(true);
            clearTimeout(copyTimerRef.current);
            copyTimerRef.current = setTimeout(() => setCopied(false), 2000);
        } catch {
            // Fallback: text is already select-all via <code> element
        }
    }, [secret]);

    // Clear timer on unmount (FE-012)
    useEffect(() => {
        return () => clearTimeout(copyTimerRef.current);
    }, []);

    const handleVerify = async () => {
        setError('');
        try {
            const valid = await invoke<boolean>('totp_setup_verify', { code });
            if (valid) {
                const storedSecret = await invoke<string>('totp_enable');
                onEnabled(storedSecret);
                onClose();
            } else {
                setError(t('security.totp.invalidCode'));
            }
        } catch (e) {
            setError(String(e));
        }
    };

    if (!isOpen) return null;

    return (
        <div
            className="fixed inset-0 z-[70] flex items-center justify-center p-4"
            role="dialog"
            aria-modal="true"
            aria-labelledby="totp-setup-title"
        >
            <div className="absolute inset-0 bg-black/60 backdrop-blur-sm" onClick={onClose} />
            <div
                ref={dialogRef}
                tabIndex={-1}
                className="relative w-full max-w-md rounded-xl overflow-hidden bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 shadow-2xl p-6 outline-none animate-scale-in"
                onClick={e => e.stopPropagation()}
            >
                <div className="flex items-center justify-between mb-4">
                    <div className="flex items-center gap-2">
                        <Shield size={20} className="text-green-400" />
                        <h3 id="totp-setup-title" className="font-bold text-gray-900 dark:text-gray-100">
                            {t('security.totp.setup')}
                        </h3>
                    </div>
                    <button onClick={onClose} className="p-1 rounded hover:bg-gray-100 dark:hover:bg-gray-700" title={t('common.close')}>
                        <X size={16} className="text-gray-500 dark:text-gray-400" />
                    </button>
                </div>

                {error && (
                    <div className="flex items-center gap-2 p-3 mb-4 rounded-lg bg-red-500/10 text-red-400 text-sm">
                        <AlertCircle size={14} />
                        {error}
                    </div>
                )}

                {step === 'generate' && secret && (
                    <div className="space-y-4">
                        <p className="text-sm text-gray-500 dark:text-gray-400">
                            {t('security.totp.scanQr')}
                        </p>

                        <div className="p-3 rounded-lg bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700">
                            <div className="flex items-center justify-between gap-2">
                                <code className="text-xs font-mono text-gray-900 dark:text-gray-100 break-all select-all">
                                    {secret}
                                </code>
                                <button onClick={handleCopy} className="flex-shrink-0 p-1.5 rounded hover:bg-gray-200 dark:hover:bg-gray-700" title={t('common.copy')}>
                                    {copied ? <Check size={14} className="text-green-400" /> : <Copy size={14} className="text-gray-500 dark:text-gray-400" />}
                                </button>
                            </div>
                        </div>

                        {uri && (
                            <div className="p-3 rounded-lg bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700">
                                <p className="text-xs text-gray-400 dark:text-gray-500 mb-1">otpauth URI:</p>
                                <code className="text-[10px] font-mono text-gray-600 dark:text-gray-400 break-all select-all">
                                    {uri}
                                </code>
                            </div>
                        )}

                        <button
                            onClick={() => setStep('verify')}
                            className="w-full py-2 rounded-lg bg-purple-600 text-white font-medium text-sm hover:bg-purple-700 transition-colors"
                        >
                            {t('security.totp.next')}
                        </button>
                    </div>
                )}

                {step === 'verify' && (
                    <div className="space-y-4">
                        <p className="text-sm text-gray-500 dark:text-gray-400">
                            {t('security.totp.enterCode')}
                        </p>

                        <input
                            type="text"
                            value={code}
                            onChange={e => setCode(e.target.value.replace(/\D/g, '').slice(0, 6))}
                            placeholder="000000"
                            maxLength={6}
                            autoFocus
                            className="w-full text-center text-3xl font-mono tracking-[0.5em] py-3 rounded-lg
                                bg-gray-50 dark:bg-gray-800 text-gray-900 dark:text-gray-100
                                border border-gray-200 dark:border-gray-700
                                focus:outline-none focus:border-purple-500"
                            onKeyDown={e => { if (e.key === 'Enter' && code.length === 6) handleVerify(); }}
                        />

                        <div className="flex gap-2">
                            <button
                                onClick={() => setStep('generate')}
                                className="flex-1 py-2 rounded-lg bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-300 text-sm hover:opacity-90"
                            >
                                {t('security.totp.back')}
                            </button>
                            <button
                                onClick={handleVerify}
                                disabled={code.length !== 6}
                                className="flex-1 py-2 rounded-lg bg-green-600 text-white font-medium text-sm
                                    hover:bg-green-700 transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
                            >
                                {t('security.totp.verify')}
                            </button>
                        </div>
                    </div>
                )}
            </div>
        </div>
    );
};
