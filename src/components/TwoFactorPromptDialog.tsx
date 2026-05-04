import * as React from 'react';
import { useState, useEffect, useRef } from 'react';
import { X, ShieldCheck, Loader2, AlertTriangle } from 'lucide-react';
import { PROVIDER_LOGOS } from './ProviderLogos';
import { useTranslation } from '../i18n';

/**
 * 2FA TOTP prompt that appears when a connect attempt to a 2FA-aware
 * provider (MEGA / Filen / Internxt) fails because the server requires
 * a fresh code. The dialog accepts a 6-digit code and calls back with
 * it so the caller can retry the connect transparently.
 *
 * Behavior:
 * - Auto-focuses the input on mount and selects all digits.
 * - Esc closes (treated as Cancel).
 * - Enter submits when 6 digits are typed.
 * - Provider logo + display name are surfaced so the user knows which
 *   account they are authenticating against (multi-profile setups).
 * - The input is numeric-only via inputMode and a strict /\d{0,6}/
 *   filter, with autoComplete="one-time-code" so password managers and
 *   Android SMS-bridge style autofill can offer the code.
 */
export interface TwoFactorPromptDialogProps {
    /** Open/closed state. Caller controls. */
    isOpen: boolean;
    /** Provider key to look up the logo (mega / filen / internxt). */
    providerKey: string;
    /** Display name shown next to the logo, e.g. masked email. */
    accountLabel: string;
    /** Optional last error to display above the input (e.g. "Wrong code"). */
    lastError?: string | null;
    /** Submit handler. Receives the 6-digit code. Caller drives the retry
     *  and is expected to flip `loading` while the connect runs. */
    onSubmit: (code: string) => void | Promise<void>;
    /** Cancel handler: Esc, click outside, or the explicit Cancel button. */
    onCancel: () => void;
    /** Whether a connect attempt is currently in flight. Disables the form. */
    loading?: boolean;
}

export function TwoFactorPromptDialog({
    isOpen,
    providerKey,
    accountLabel,
    lastError,
    onSubmit,
    onCancel,
    loading = false,
}: TwoFactorPromptDialogProps) {
    const t = useTranslation();
    const [code, setCode] = useState('');
    const inputRef = useRef<HTMLInputElement>(null);

    useEffect(() => {
        if (isOpen) {
            setCode('');
            // Defer to next tick so the modal mount transition completes.
            setTimeout(() => {
                inputRef.current?.focus();
                inputRef.current?.select();
            }, 50);
        }
    }, [isOpen]);

    useEffect(() => {
        if (!isOpen) return;
        const onKey = (e: KeyboardEvent) => {
            if (e.key === 'Escape' && !loading) onCancel();
        };
        window.addEventListener('keydown', onKey);
        return () => window.removeEventListener('keydown', onKey);
    }, [isOpen, loading, onCancel]);

    if (!isOpen) return null;

    const Logo = PROVIDER_LOGOS[providerKey] || PROVIDER_LOGOS[providerKey.toLowerCase()];
    const isValid = /^\d{6}$/.test(code);

    const submit = () => {
        if (!isValid || loading) return;
        onSubmit(code);
    };

    return (
        <div
            className="fixed inset-0 z-50 flex items-start justify-center pt-[15vh] bg-black/50 backdrop-blur-sm animate-fade-in"
            onClick={(e) => { if (e.target === e.currentTarget && !loading) onCancel(); }}
            role="dialog"
            aria-modal="true"
            aria-labelledby="twofa-title"
        >
            <div
                className="w-full max-w-md bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 overflow-hidden animate-scale-in"
                onClick={(e) => e.stopPropagation()}
            >
                <div className="flex items-center justify-between px-5 py-4 border-b border-gray-200 dark:border-gray-700">
                    <div className="flex items-center gap-3 min-w-0">
                        <div className="p-2 bg-emerald-100 dark:bg-emerald-900/40 rounded-lg shrink-0">
                            <ShieldCheck size={18} className="text-emerald-600 dark:text-emerald-400" />
                        </div>
                        <div className="min-w-0">
                            <h2 id="twofa-title" className="text-base font-semibold truncate">
                                {t('twoFactor.title')}
                            </h2>
                            <div className="flex items-center gap-2 text-xs text-gray-500 dark:text-gray-400 mt-0.5">
                                {Logo && (
                                    <span className="inline-flex items-center justify-center w-4 h-4">
                                        <Logo size={14} />
                                    </span>
                                )}
                                <span className="truncate">{accountLabel}</span>
                            </div>
                        </div>
                    </div>
                    <button
                        onClick={onCancel}
                        disabled={loading}
                        className="p-1.5 rounded-lg text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors disabled:opacity-50"
                        aria-label={t('common.close')}
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="px-5 py-5">
                    <p className="text-sm text-gray-600 dark:text-gray-300 mb-4">
                        {t('twoFactor.description')}
                    </p>

                    {lastError && (
                        <div className="mb-3 flex items-start gap-2 px-3 py-2 rounded-lg bg-amber-50 dark:bg-amber-900/20 border border-amber-200 dark:border-amber-800/40 text-xs text-amber-800 dark:text-amber-200">
                            <AlertTriangle size={14} className="shrink-0 mt-0.5" />
                            <span>{lastError}</span>
                        </div>
                    )}

                    <input
                        ref={inputRef}
                        type="text"
                        value={code}
                        onChange={(e) => {
                            const cleaned = e.target.value.replace(/\D/g, '').slice(0, 6);
                            setCode(cleaned);
                        }}
                        onKeyDown={(e) => { if (e.key === 'Enter') submit(); }}
                        disabled={loading}
                        placeholder="000000"
                        maxLength={6}
                        inputMode="numeric"
                        autoComplete="one-time-code"
                        className="w-full px-4 py-3 text-center text-2xl tracking-[0.5em] font-mono bg-gray-50 dark:bg-gray-900/50 border border-gray-300 dark:border-gray-600 rounded-lg focus:outline-none focus:ring-2 focus:ring-emerald-500/40 focus:border-emerald-500 disabled:opacity-50"
                    />

                    <p className="mt-2 text-[11px] text-gray-400 dark:text-gray-500 text-center">
                        {t('twoFactor.hint')}
                    </p>
                </div>

                <div className="flex justify-end gap-2 px-5 py-3 border-t border-gray-200 dark:border-gray-700 bg-gray-50 dark:bg-gray-800/50">
                    <button
                        onClick={onCancel}
                        disabled={loading}
                        className="px-4 py-2 text-sm text-gray-600 dark:text-gray-400 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors disabled:opacity-50"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        onClick={submit}
                        disabled={!isValid || loading}
                        className="px-4 py-2 text-sm font-medium text-white bg-emerald-600 hover:bg-emerald-700 disabled:bg-gray-300 dark:disabled:bg-gray-700 disabled:text-gray-500 rounded-lg transition-colors flex items-center gap-2"
                    >
                        {loading ? (
                            <>
                                <Loader2 size={14} className="animate-spin" />
                                {t('twoFactor.connecting')}
                            </>
                        ) : (
                            t('twoFactor.connect')
                        )}
                    </button>
                </div>
            </div>
        </div>
    );
}
