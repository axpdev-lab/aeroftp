import * as React from 'react';
import { useState, useEffect, useCallback } from 'react';
import { Shield, Server, Bot, Cloud, X, CheckCircle2, AlertTriangle, ArrowRight, Loader2 } from 'lucide-react';
import { secureStore } from '../utils/secureStorage';
import { useTranslation } from '../i18n';
import { logger } from '../utils/logger';

interface KeystoreMigrationWizardProps {
    isOpen: boolean;
    onComplete: () => void;
    onSkip: () => void;
    isLightTheme?: boolean;
}

interface DetectedData {
    serverProfiles: { count: number; data: unknown[] };
    aiSettings: { found: boolean; data: unknown | null };
    oauthCredentials: { count: number; data: Record<string, unknown> };
}

const LEGACY_OAUTH_PREFIXES = ['oauth_googledrive', 'oauth_dropbox', 'oauth_onedrive', 'oauth_box', 'oauth_pcloud'];

function detectLocalStorageData(): DetectedData {
    const result: DetectedData = {
        serverProfiles: { count: 0, data: [] },
        aiSettings: { found: false, data: null },
        oauthCredentials: { count: 0, data: {} },
    };

    // Server profiles
    try {
        const raw = localStorage.getItem('aeroftp-saved-servers');
        if (raw) {
            const parsed = JSON.parse(raw);
            if (Array.isArray(parsed)) {
                result.serverProfiles = { count: parsed.length, data: parsed };
            }
        }
    } catch { /* parse error */ }

    // AI settings
    try {
        const raw = localStorage.getItem('aeroftp_ai_settings');
        if (raw) {
            result.aiSettings = { found: true, data: JSON.parse(raw) };
        }
    } catch { /* parse error */ }

    // OAuth credentials
    try {
        const raw = localStorage.getItem('aeroftp_oauth_settings');
        if (raw) {
            const parsed = JSON.parse(raw);
            const count = Object.values(parsed).filter((v: unknown) => {
                const entry = v as { clientId?: string };
                return entry && entry.clientId;
            }).length;
            if (count > 0) {
                result.oauthCredentials = { count, data: parsed };
            }
        }
    } catch { /* parse error */ }

    // Legacy individual OAuth keys
    for (const prefix of LEGACY_OAUTH_PREFIXES) {
        for (const suffix of ['_client_id', '_client_secret']) {
            const key = prefix + suffix;
            const val = localStorage.getItem(key);
            if (val) {
                result.oauthCredentials.count = Math.max(result.oauthCredentials.count, 1);
                (result.oauthCredentials.data as Record<string, unknown>)[key] = val;
            }
        }
    }

    return result;
}

export const KeystoreMigrationWizard: React.FC<KeystoreMigrationWizardProps> = ({
    isOpen,
    onComplete,
    onSkip,
    isLightTheme,
}) => {
    const t = useTranslation();
    const [step, setStep] = useState(1);
    const [detected, setDetected] = useState<DetectedData | null>(null);
    const [migrationProgress, setMigrationProgress] = useState(0);
    const [migrationTotal, setMigrationTotal] = useState(0);
    const [migratedCount, setMigratedCount] = useState(0);
    const [migrationError, setMigrationError] = useState<string | null>(null);
    const [removeLocalStorage, setRemoveLocalStorage] = useState(true);

    // Step 1: Detect
    useEffect(() => {
        if (isOpen && step === 1) {
            const data = detectLocalStorageData();
            setDetected(data);
            // If nothing found, auto-complete
            const total = data.serverProfiles.count + (data.aiSettings.found ? 1 : 0) + data.oauthCredentials.count;
            if (total === 0) {
                localStorage.setItem('keystore_migration_v2_done', 'true');
                onComplete();
            }
        }
    }, [isOpen, step, onComplete]);

    // Step 3: Migrate
    const runMigration = useCallback(async () => {
        if (!detected) return;
        setMigrationError(null);

        const tasks: Array<{ key: string; data: unknown }> = [];

        if (detected.serverProfiles.count > 0) {
            tasks.push({ key: 'server_profiles', data: detected.serverProfiles.data });
        }
        if (detected.aiSettings.found && detected.aiSettings.data) {
            tasks.push({ key: 'ai_settings', data: detected.aiSettings.data });
        }
        if (detected.oauthCredentials.count > 0) {
            tasks.push({ key: 'oauth_clients', data: detected.oauthCredentials.data });
        }

        setMigrationTotal(tasks.length);
        setMigrationProgress(0);

        let completed = 0;
        for (const task of tasks) {
            try {
                await secureStore(task.key, task.data);
                completed++;
                setMigrationProgress(completed);
            } catch (err) {
                logger.error(`[Migration] Failed to store ${task.key}:`, err);
                setMigrationError(String(err));
                return;
            }
        }

        setMigratedCount(completed);
        setStep(4);
    }, [detected]);

    if (!isOpen) return null;

    const totalItems = detected
        ? detected.serverProfiles.count + (detected.aiSettings.found ? 1 : 0) + detected.oauthCredentials.count
        : 0;

    const bgOverlay = isLightTheme ? 'bg-black/40' : 'bg-black/60';
    const bgPanel = isLightTheme ? 'bg-white' : 'bg-gray-800';
    const textPrimary = isLightTheme ? 'text-gray-900' : 'text-gray-100';
    const textSecondary = isLightTheme ? 'text-gray-600' : 'text-gray-400';
    const borderColor = isLightTheme ? 'border-gray-200' : 'border-gray-700';

    return (
        <div
            className={`fixed inset-0 ${bgOverlay} flex items-center justify-center z-50`}
            onClick={e => e.target === e.currentTarget && onSkip()}
        >
            <div className={`${bgPanel} rounded-xl shadow-2xl w-[480px] max-h-[90vh] overflow-hidden flex flex-col animate-scale-in`}>
                {/* Header */}
                <div className={`flex items-center justify-between px-5 py-4 border-b ${borderColor}`}>
                    <div className="flex items-center gap-3">
                        <div className="p-2 bg-amber-100 dark:bg-amber-900/30 text-amber-600 dark:text-amber-400 rounded-lg">
                            <Shield size={20} />
                        </div>
                        <div>
                            <h3 className={`text-lg font-semibold ${textPrimary}`}>{t('settings.migrationTitle')}</h3>
                            <p className={`text-xs ${textSecondary}`}>
                                {t('settings.migrationStep', { current: step, total: 4 })}
                            </p>
                        </div>
                    </div>
                    <button
                        onClick={onSkip}
                        className="p-1.5 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-700 transition-colors"
                    >
                        <X size={18} className={textSecondary} />
                    </button>
                </div>

                {/* Progress bar */}
                <div className={`h-1 ${isLightTheme ? 'bg-gray-100' : 'bg-gray-700'}`}>
                    <div
                        className="h-full bg-blue-500 transition-all duration-500"
                        style={{ width: `${(step / 4) * 100}%` }}
                    />
                </div>

                {/* Body */}
                <div className="p-5 flex-1 overflow-y-auto">
                    {/* Step 1 & 2: Detect + Preview */}
                    {(step === 1 || step === 2) && detected && (
                        <div className="space-y-4">
                            <p className={`text-sm ${textSecondary}`}>
                                {t('settings.migrationDesc')}
                            </p>

                            <div className="space-y-3">
                                {/* Server profiles */}
                                {detected.serverProfiles.count > 0 && (
                                    <div className={`flex items-center gap-3 p-3 rounded-lg border ${borderColor} ${isLightTheme ? 'bg-gray-50' : 'bg-gray-700/50'}`}>
                                        <div className="p-2 bg-blue-100 dark:bg-blue-900/30 rounded-lg">
                                            <Server size={18} className="text-blue-600 dark:text-blue-400" />
                                        </div>
                                        <div className="flex-1">
                                            <div className={`text-sm font-medium ${textPrimary}`}>
                                                {t('settings.migrationProfiles', { count: detected.serverProfiles.count })}
                                            </div>
                                            <div className={`text-xs ${textSecondary}`}>
                                                {step === 2 && `${detected.serverProfiles.count} server profiles found in unencrypted storage`}
                                            </div>
                                        </div>
                                        <AlertTriangle size={16} className="text-amber-500 shrink-0" />
                                    </div>
                                )}

                                {/* AI settings */}
                                {detected.aiSettings.found && (
                                    <div className={`flex items-center gap-3 p-3 rounded-lg border ${borderColor} ${isLightTheme ? 'bg-gray-50' : 'bg-gray-700/50'}`}>
                                        <div className="p-2 bg-purple-100 dark:bg-purple-900/30 rounded-lg">
                                            <Bot size={18} className="text-purple-600 dark:text-purple-400" />
                                        </div>
                                        <div className="flex-1">
                                            <div className={`text-sm font-medium ${textPrimary}`}>
                                                {t('settings.migrationAiSettings')}
                                            </div>
                                            <div className={`text-xs ${textSecondary}`}>
                                                {step === 2 && 'AI configuration found in unencrypted storage'}
                                            </div>
                                        </div>
                                        <AlertTriangle size={16} className="text-amber-500 shrink-0" />
                                    </div>
                                )}

                                {/* OAuth credentials */}
                                {detected.oauthCredentials.count > 0 && (
                                    <div className={`flex items-center gap-3 p-3 rounded-lg border ${borderColor} ${isLightTheme ? 'bg-gray-50' : 'bg-gray-700/50'}`}>
                                        <div className="p-2 bg-green-100 dark:bg-green-900/30 rounded-lg">
                                            <Cloud size={18} className="text-green-600 dark:text-green-400" />
                                        </div>
                                        <div className="flex-1">
                                            <div className={`text-sm font-medium ${textPrimary}`}>
                                                {t('settings.migrationOauth', { count: detected.oauthCredentials.count })}
                                            </div>
                                            <div className={`text-xs ${textSecondary}`}>
                                                {step === 2 && `${detected.oauthCredentials.count} OAuth credentials found in unencrypted storage`}
                                            </div>
                                        </div>
                                        <AlertTriangle size={16} className="text-amber-500 shrink-0" />
                                    </div>
                                )}
                            </div>
                        </div>
                    )}

                    {/* Step 3: Migrate */}
                    {step === 3 && (
                        <div className="space-y-4">
                            <div className="flex flex-col items-center gap-4 py-6">
                                <Loader2 size={40} className="text-blue-500 animate-spin" />
                                <p className={`text-sm font-medium ${textPrimary}`}>
                                    {t('settings.migrationInProgress')}
                                </p>
                                <div className={`w-full rounded-full h-2 ${isLightTheme ? 'bg-gray-200' : 'bg-gray-700'}`}>
                                    <div
                                        className="h-full bg-blue-500 rounded-full transition-all duration-300"
                                        style={{ width: migrationTotal > 0 ? `${(migrationProgress / migrationTotal) * 100}%` : '0%' }}
                                    />
                                </div>
                                <p className={`text-xs ${textSecondary}`}>
                                    {migrationProgress} / {migrationTotal}
                                </p>
                            </div>
                            {migrationError && (
                                <div className="p-3 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg text-red-600 dark:text-red-400 text-sm">
                                    {migrationError}
                                </div>
                            )}
                        </div>
                    )}

                    {/* Step 4: Cleanup & Confirm */}
                    {step === 4 && (
                        <div className="space-y-4">
                            <div className="flex flex-col items-center gap-3 py-4">
                                <div className="p-3 bg-emerald-100 dark:bg-emerald-900/30 rounded-full">
                                    <CheckCircle2 size={32} className="text-emerald-600 dark:text-emerald-400" />
                                </div>
                                <p className={`text-sm font-medium ${textPrimary} text-center`}>
                                    {t('settings.migrationComplete', { count: migratedCount })}
                                </p>
                            </div>

                            {/* Remove localStorage toggle */}
                            <label className={`flex items-center gap-3 p-3 rounded-lg border ${borderColor} cursor-pointer ${isLightTheme ? 'bg-gray-50' : 'bg-gray-700/50'}`}>
                                <input
                                    type="checkbox"
                                    checked={removeLocalStorage}
                                    onChange={e => setRemoveLocalStorage(e.target.checked)}
                                    className="w-4 h-4 rounded accent-emerald-500"
                                />
                                <div>
                                    <div className={`text-sm font-medium ${textPrimary}`}>
                                        {t('settings.migrationCleanup')}
                                    </div>
                                    <div className={`text-xs ${textSecondary}`}>
                                        Recommended for maximum security
                                    </div>
                                </div>
                            </label>
                        </div>
                    )}
                </div>

                {/* Footer */}
                <div className={`flex items-center justify-between px-5 py-4 border-t ${borderColor}`}>
                    <button
                        onClick={onSkip}
                        className={`text-sm ${textSecondary} hover:underline`}
                    >
                        {t('settings.migrationSkip')}
                    </button>

                    {(step === 1 || step === 2) && (
                        <button
                            onClick={() => {
                                if (step === 1) {
                                    setStep(2);
                                } else {
                                    setStep(3);
                                    // Trigger migration after render
                                    setTimeout(() => runMigration(), 100);
                                }
                            }}
                            className="px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white rounded-lg text-sm font-medium transition-colors flex items-center gap-2"
                        >
                            {step === 1 ? t('common.next') || 'Next' : t('settings.migrationFinish')}
                            <ArrowRight size={16} />
                        </button>
                    )}

                    {step === 4 && (
                        <button
                            onClick={() => {
                                if (removeLocalStorage && detected) {
                                    // Remove localStorage copies
                                    if (detected.serverProfiles.count > 0) {
                                        localStorage.removeItem('aeroftp-saved-servers');
                                    }
                                    if (detected.aiSettings.found) {
                                        localStorage.removeItem('aeroftp_ai_settings');
                                    }
                                    if (detected.oauthCredentials.count > 0) {
                                        localStorage.removeItem('aeroftp_oauth_settings');
                                        // Remove legacy keys
                                        for (const prefix of LEGACY_OAUTH_PREFIXES) {
                                            for (const suffix of ['_client_id', '_client_secret']) {
                                                localStorage.removeItem(prefix + suffix);
                                            }
                                        }
                                    }
                                }
                                localStorage.setItem('keystore_migration_v2_done', 'true');
                                onComplete();
                            }}
                            className="px-4 py-2 bg-emerald-600 hover:bg-emerald-700 text-white rounded-lg text-sm font-medium transition-colors flex items-center gap-2"
                        >
                            <CheckCircle2 size={16} />
                            {t('settings.migrationFinish')}
                        </button>
                    )}
                </div>
            </div>
        </div>
    );
};

export default KeystoreMigrationWizard;
