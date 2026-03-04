import { useState, useCallback, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Sparkles, Key, CheckCircle, Copy, AlertTriangle, Loader2, X } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useLicense } from '../hooks/useLicense';

export default function LicenseTab() {
    const t = useTranslation();
    const { isPro, licenseId, humanReadableKey, graceDaysRemaining, activate, deactivate } = useLicense();
    const [token, setToken] = useState('');
    const [error, setError] = useState('');
    const [activating, setActivating] = useState(false);
    const [deactivating, setDeactivating] = useState(false);
    const [deviceFingerprint, setDeviceFingerprint] = useState('');
    const [copied, setCopied] = useState(false);

    useEffect(() => {
        invoke<string>('license_get_device_fingerprint').then(setDeviceFingerprint).catch(() => {});
    }, []);

    const handleActivate = useCallback(async () => {
        if (!token.trim()) return;
        setError('');
        setActivating(true);
        try {
            await activate(token.trim());
            setToken('');
        } catch (e) {
            setError(typeof e === 'string' ? e : (e as Error).message || t('license.activationFailed'));
        } finally {
            setActivating(false);
        }
    }, [token, activate, t]);

    const handleDeactivate = useCallback(async () => {
        setDeactivating(true);
        try {
            await deactivate();
        } catch {
            // silent
        } finally {
            setDeactivating(false);
        }
    }, [deactivate]);

    const copyFingerprint = useCallback(() => {
        navigator.clipboard.writeText(deviceFingerprint).then(() => {
            setCopied(true);
            setTimeout(() => setCopied(false), 2000);
        }).catch(() => {});
    }, [deviceFingerprint]);

    return (
        <div className="space-y-6">
            <h3 className="text-sm font-semibold text-gray-500 uppercase tracking-wide">{t('license.title')}</h3>

            {/* Status Card */}
            <div className={`p-4 rounded-lg border ${
                isPro
                    ? 'bg-emerald-50 dark:bg-emerald-900/20 border-emerald-200 dark:border-emerald-800'
                    : 'bg-gray-50 dark:bg-gray-700/50 border-gray-200 dark:border-gray-700'
            }`}>
                <div className="flex items-center gap-3">
                    <div className={`p-2 rounded-lg ${
                        isPro
                            ? 'bg-emerald-100 dark:bg-emerald-900/30 text-emerald-600 dark:text-emerald-400'
                            : 'bg-gray-100 dark:bg-gray-600 text-gray-500 dark:text-gray-400'
                    }`}>
                        {isPro ? <CheckCircle size={24} /> : <Sparkles size={24} />}
                    </div>
                    <div className="flex-1">
                        <div className="flex items-center gap-2">
                            <h4 className="font-medium text-base">
                                {isPro ? t('license.statusPro') : t('license.statusFree')}
                            </h4>
                            {isPro && (
                                <span className="px-2 py-0.5 text-xs font-bold rounded bg-gradient-to-r from-blue-500 to-purple-500 text-white">
                                    PRO
                                </span>
                            )}
                        </div>
                        {licenseId && (
                            <p className="text-xs text-gray-500 dark:text-gray-400 mt-0.5">{licenseId}</p>
                        )}
                        {graceDaysRemaining != null && (
                            <p className="text-xs text-amber-600 dark:text-amber-400 flex items-center gap-1 mt-1">
                                <AlertTriangle size={12} />
                                {t('license.graceRemaining', { days: graceDaysRemaining.toString() })}
                            </p>
                        )}
                    </div>
                </div>

                {/* License key display */}
                {isPro && humanReadableKey && (
                    <div className="mt-3 pt-3 border-t border-emerald-200 dark:border-emerald-800">
                        <div className="text-xs text-gray-500 dark:text-gray-400 mb-1">{t('license.yourKey')}</div>
                        <div className="font-mono text-sm text-gray-900 dark:text-gray-100 select-all">
                            {humanReadableKey}
                        </div>
                    </div>
                )}
            </div>

            {/* Activation Form */}
            {!isPro && (
                <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg space-y-3">
                    <h4 className="font-medium text-sm flex items-center gap-2">
                        <Key size={14} className="text-gray-500" />
                        {t('license.enterToken')}
                    </h4>
                    <textarea
                        value={token}
                        onChange={(e) => { setToken(e.target.value); setError(''); }}
                        placeholder={t('license.tokenPlaceholder')}
                        className="w-full px-3 py-2 text-sm font-mono bg-white dark:bg-gray-800 border border-gray-300 dark:border-gray-600 rounded-lg focus:ring-2 focus:ring-blue-500 focus:border-blue-500 dark:text-gray-100 resize-none"
                        rows={3}
                        spellCheck={false}
                    />

                    {error && (
                        <div className="flex items-start gap-2 text-sm text-red-600 dark:text-red-400">
                            <AlertTriangle size={14} className="shrink-0 mt-0.5" />
                            <span>{error}</span>
                        </div>
                    )}

                    <button
                        onClick={handleActivate}
                        disabled={!token.trim() || activating}
                        className="w-full flex items-center justify-center gap-2 px-4 py-2 text-sm font-medium text-white bg-blue-600 hover:bg-blue-700 disabled:opacity-50 disabled:cursor-not-allowed rounded-lg transition-colors"
                    >
                        {activating ? (
                            <Loader2 size={14} className="animate-spin" />
                        ) : (
                            <Key size={14} />
                        )}
                        {t('license.activate')}
                    </button>
                </div>
            )}

            {/* Deactivate */}
            {isPro && (
                <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                    <button
                        onClick={handleDeactivate}
                        disabled={deactivating}
                        className="w-full flex items-center justify-center gap-2 px-4 py-2 text-sm font-medium text-red-600 dark:text-red-400 bg-red-50 dark:bg-red-900/20 hover:bg-red-100 dark:hover:bg-red-900/30 border border-red-200 dark:border-red-800 rounded-lg transition-colors"
                    >
                        {deactivating ? (
                            <Loader2 size={14} className="animate-spin" />
                        ) : (
                            <X size={14} />
                        )}
                        {t('license.deactivate')}
                    </button>
                </div>
            )}

            {/* Device Fingerprint */}
            {deviceFingerprint && (
                <div className="p-4 bg-gray-50 dark:bg-gray-700/50 rounded-lg">
                    <div className="text-xs font-medium text-gray-500 dark:text-gray-400 mb-1">
                        {t('license.deviceFingerprint')}
                    </div>
                    <div className="flex items-center gap-2">
                        <code className="flex-1 text-xs text-gray-600 dark:text-gray-400 truncate select-all font-mono">
                            {deviceFingerprint.slice(0, 16)}...{deviceFingerprint.slice(-8)}
                        </code>
                        <button
                            onClick={copyFingerprint}
                            className="p-1 rounded hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors shrink-0"
                            title={t('common.copy')}
                        >
                            {copied ? (
                                <CheckCircle size={14} className="text-emerald-500" />
                            ) : (
                                <Copy size={14} className="text-gray-400" />
                            )}
                        </button>
                    </div>
                </div>
            )}

            {/* Pro Features List */}
            {!isPro && (
                <div className="border border-gray-200 dark:border-gray-700 rounded-lg overflow-hidden">
                    <div className="bg-gray-50 dark:bg-gray-700/50 px-4 py-2 border-b border-gray-200 dark:border-gray-700">
                        <h4 className="font-medium flex items-center gap-2 text-sm">
                            <Sparkles size={14} className="text-purple-500" />
                            {t('license.proFeatures')}
                        </h4>
                    </div>
                    <div className="p-4 space-y-2">
                        {[
                            t('license.featureVault'),
                            t('license.featureSync'),
                            t('license.featureAgent'),
                            t('license.featureBatchRename'),
                            t('license.featureThemes'),
                            t('license.featurePreview'),
                        ].map((feature, i) => (
                            <div key={i} className="flex items-center gap-2 text-sm text-gray-600 dark:text-gray-300">
                                <Sparkles size={12} className="text-purple-400 shrink-0" />
                                {feature}
                            </div>
                        ))}
                    </div>
                </div>
            )}

            {/* Info */}
            <p className="text-xs text-gray-400 dark:text-gray-500 leading-relaxed">
                {t('license.infoText')}
            </p>
        </div>
    );
}
