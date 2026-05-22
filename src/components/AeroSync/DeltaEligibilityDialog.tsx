// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// CO-5: surface the result of `sftp_probe_delta_eligibility` to the user
// when the active SFTP session cannot drive delta sync. The dialog is
// purely informational: the chosen preset still runs against the
// classic copy path, but the user gets the probe verdict and an opt-out
// to suppress the prompt for the current saved server.

import * as React from 'react';
import { ShieldAlert, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';

export interface DeltaEligibilityDialogProps {
    isOpen: boolean;
    reason: string;
    serverLabel: string;
    onClose: (skipFuturePrompts: boolean) => void;
}

export const DeltaEligibilityDialog: React.FC<DeltaEligibilityDialogProps> = ({
    isOpen,
    reason,
    serverLabel,
    onClose,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();
    const [skipFuture, setSkipFuture] = React.useState(false);

    React.useEffect(() => {
        if (isOpen) setSkipFuture(false);
    }, [isOpen]);

    // Escape dismisses the dialog, matching the other AeroSync modals.
    // The latest `skipFuture` value is read through a ref so the handler
    // stays stable without re-binding the listener on every keystroke.
    const skipFutureRef = React.useRef(skipFuture);
    skipFutureRef.current = skipFuture;
    React.useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => {
            if (e.key === 'Escape') onClose(skipFutureRef.current);
        };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    if (!isOpen) return null;

    const dismiss = () => onClose(skipFuture);

    return (
        <div
            className="fixed inset-0 z-[9999] flex items-start justify-center pt-[10vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerosync.deltaEligibility.title') || 'Delta sync not available'}
            onClick={dismiss}
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
                        onClick={dismiss}
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

                <div className="flex justify-end px-4 py-3 border-t border-gray-200 dark:border-gray-700">
                    <button
                        type="button"
                        onClick={dismiss}
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
