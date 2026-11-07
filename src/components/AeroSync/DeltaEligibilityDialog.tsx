// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// CO-5: surface the result of `sftp_probe_delta_eligibility` to the user
// when the active SFTP session cannot drive delta sync. The dialog asks
// before the queued plan runs: Continue runs it on the classic copy path
// (with an opt-out to suppress the prompt for the current saved server),
// Cancel, the X and ESC drop it.

import * as React from 'react';
import { ShieldAlert, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { useGuardedClose } from '../../hooks/useGuardedClose';

export interface DeltaEligibilityDialogProps {
    isOpen: boolean;
    reason: string;
    serverLabel: string;
    /** Continue: the queued plan runs on the classic copy path. */
    onContinue: (skipFuturePrompts: boolean) => void;
    /** Cancel: the queued plan does not run. */
    onCancel: () => void;
}

export const DeltaEligibilityDialog: React.FC<DeltaEligibilityDialogProps> = ({
    isOpen,
    reason,
    serverLabel,
    onContinue,
    onCancel,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [skipFuture, setSkipFuture] = React.useState(false);

    React.useEffect(() => {
        if (isOpen) setSkipFuture(false);
    }, [isOpen]);

    // A question, not a notice: the queued plan runs only on Continue. The
    // backdrop is inert, and ESC and the X cancel the plan (Discussion #1527:
    // a stray click outside used to start it).
    const guarded = useGuardedClose({ guard: null, onClose: onCancel, backdrop: 'never', escape: isOpen });

    if (!isOpen) return null;

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-start justify-center pt-[10vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerosync.deltaEligibility.title') || 'Delta sync not available'}
            onClick={guarded.onBackdropClick}
        >
            <div
                {...modalDrag.panelProps}
                className="relative bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-md overflow-hidden flex flex-col"
                onClick={(e) => e.stopPropagation()}
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <ShieldAlert size={18} className="text-amber-500" />
                        <h2 className="text-base font-semibold">
                            {t('aerosync.deltaEligibility.title') || 'Delta sync not available'}
                        </h2>
                    </div>
                    <button
                        type="button"
                        onClick={guarded.requestClose}
                        aria-label={t('common.close') || 'Close'}
                        className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="px-4 py-3 space-y-3 text-sm text-gray-700 dark:text-gray-200">
                    <p className="font-medium">
                        {t('aerosync.deltaEligibility.heading')
                            || 'Delta sync unavailable for this server.'}
                    </p>
                    <div className="rounded-md border border-amber-200 bg-amber-50 px-3 py-2 text-[12px] text-amber-900 dark:border-amber-800 dark:bg-amber-900/30 dark:text-amber-100">
                        {reason}
                    </div>
                    <p className="text-[12px] text-gray-500 dark:text-gray-400">
                        <span className="font-semibold text-gray-700 dark:text-gray-300">
                            {t('aerosync.deltaEligibility.serverLabel') || 'Server:'}
                        </span>{' '}
                        <span className="font-mono">{serverLabel}</span>
                    </p>
                    <p className="text-[12px] text-gray-500 dark:text-gray-400">
                        {t('aerosync.deltaEligibility.fallbackHint')
                            || 'AeroSync will fall back to the classic copy path. The chosen preset still runs.'}
                    </p>
                    <label className="mt-2 flex items-center gap-2 cursor-pointer">
                        <input
                            type="checkbox"
                            checked={skipFuture}
                            onChange={(e) => setSkipFuture(e.target.checked)}
                            className="rounded border-gray-300 dark:border-gray-600"
                        />
                        <span className="text-[12px] text-gray-700 dark:text-gray-300">
                            {t('aerosync.deltaEligibility.skipFuture')
                                || "Don't ask again for this server"}
                        </span>
                    </label>
                </div>

                <div className="flex justify-end gap-2 px-4 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        type="button"
                        onClick={onCancel}
                        className="px-3 py-1.5 rounded-md bg-gray-100 dark:bg-gray-700 text-gray-700 dark:text-gray-200 text-sm font-medium hover:bg-gray-200 dark:hover:bg-gray-600 transition-colors"
                    >
                        {t('common.cancel') || 'Cancel'}
                    </button>
                    <button
                        type="button"
                        onClick={() => onContinue(skipFuture)}
                        className="px-3 py-1.5 rounded-md bg-blue-600 text-white text-sm font-medium hover:bg-blue-700 transition-colors"
                    >
                        {t('aerosync.deltaEligibility.continue') || 'Continue'}
                    </button>
                </div>
            </div>
        </div>
    );
};

export default DeltaEligibilityDialog;
