import React from 'react';
import { Fingerprint, ShieldAlert } from 'lucide-react';
import { useTranslation } from '../i18n';

export interface HostKeyInfo {
    status: 'known' | 'unknown' | 'changed' | 'error';
    fingerprint: string;
    algorithm: string;
    changed_line?: number;
}

interface HostKeyDialogProps {
    visible: boolean;
    info: HostKeyInfo | null;
    host: string;
    port: number;
    onAccept: () => void;
    onReject: () => void;
}

export const HostKeyDialog: React.FC<HostKeyDialogProps> = ({
    visible,
    info,
    host,
    port,
    onAccept,
    onReject,
}) => {
    const t = useTranslation();

    if (!visible || !info || (info.status !== 'unknown' && info.status !== 'changed')) return null;

    const isChanged = info.status === 'changed';

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-center justify-center bg-black/50 backdrop-blur-sm"
            onClick={onReject}
            role="dialog"
            aria-modal="true"
            aria-label="Host Key Verification"
        >
            <div
                className="bg-white dark:bg-gray-800 rounded-2xl shadow-2xl border border-gray-200 dark:border-gray-700 max-w-md w-full mx-4 overflow-hidden"
                onClick={e => e.stopPropagation()}
            >
                <div className="p-6">
                    {/* Header */}
                    <div className="flex items-center gap-3 mb-4">
                        <div className={`w-10 h-10 rounded-full flex items-center justify-center flex-shrink-0 ${
                            isChanged
                                ? 'bg-red-100 dark:bg-red-900/30'
                                : 'bg-amber-100 dark:bg-amber-900/30'
                        }`}>
                            {isChanged
                                ? <ShieldAlert size={20} className="text-red-500" />
                                : <Fingerprint size={20} className="text-amber-500" />
                            }
                        </div>
                        <h3 className="text-lg font-semibold text-gray-900 dark:text-white">
                            {isChanged ? t('protocol.hostKeyChangedTitle') : t('protocol.hostKeyNewTitle')}
                        </h3>
                    </div>

                    {/* Body text */}
                    <p className="text-sm text-gray-600 dark:text-gray-400 leading-relaxed mb-4">
                        {isChanged ? t('protocol.hostKeyChangedBody') : t('protocol.hostKeyNewBody')}
                    </p>

                    {/* MITM warning (key changed only) */}
                    {isChanged && (
                        <div className="bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-800 rounded-lg p-3 mb-4">
                            <p className="text-xs text-red-700 dark:text-red-300 font-medium">
                                {t('protocol.hostKeyChangedWarning')}
                            </p>
                        </div>
                    )}

                    {/* Host info */}
                    <div className="bg-gray-50 dark:bg-gray-900/50 rounded-lg p-3 mb-3 space-y-2">
                        <div className="flex justify-between text-sm">
                            <span className="text-gray-500 dark:text-gray-400">{t('protocol.hostKeyHost')}</span>
                            <span className="font-medium text-gray-900 dark:text-white">{host}:{port}</span>
                        </div>
                        <div className="flex justify-between text-sm">
                            <span className="text-gray-500 dark:text-gray-400">{t('protocol.hostKeyAlgorithm')}</span>
                            <span className="font-mono text-gray-900 dark:text-white">{info.algorithm}</span>
                        </div>
                    </div>

                    {/* Fingerprint */}
                    <div className="space-y-1">
                        <span className="text-xs text-gray-500 dark:text-gray-400 font-medium">
                            {t('protocol.hostKeyFingerprint')}
                        </span>
                        <div className="font-mono text-xs bg-gray-100 dark:bg-gray-700 p-3 rounded-lg text-gray-900 dark:text-gray-100 break-all select-all leading-relaxed">
                            {info.fingerprint}
                        </div>
                    </div>
                </div>

                {/* Actions */}
                <div className="flex border-t border-gray-200 dark:border-gray-700">
                    <button
                        onClick={onReject}
                        className="flex-1 px-4 py-3 text-sm font-medium text-gray-700 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                    >
                        {t('common.cancel')}
                    </button>
                    <button
                        onClick={onAccept}
                        className={`flex-1 px-4 py-3 text-sm font-medium border-l border-gray-200 dark:border-gray-700 transition-colors ${
                            isChanged
                                ? 'text-red-600 dark:text-red-400 hover:bg-red-50 dark:hover:bg-red-900/20'
                                : 'text-amber-600 dark:text-amber-400 hover:bg-amber-50 dark:hover:bg-amber-900/20'
                        }`}
                    >
                        {isChanged ? t('protocol.hostKeyAcceptNewAction') : t('protocol.hostKeyTrustAction')}
                    </button>
                </div>
            </div>
        </div>
    );
};
