// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// CO-7: thin modal wrapper around the existing `SyncScheduler` control,
// surfaced from the AeroSync header as the 5th launcher (alongside
// Templates, Multi-Path, Rollback, History). The scheduler itself is
// global (no localPath / remotePath knobs); the wrapper exists only to
// provide modal chrome + Escape / overlay-click dismissal.

import React, { useEffect } from 'react';
import { CalendarClock, X } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useDraggableModal } from '../../hooks/useDraggableModal';
import { SyncScheduler } from '../SyncScheduler';

interface SyncSchedulerDialogProps {
    isOpen: boolean;
    onClose: () => void;
}

export const SyncSchedulerDialog: React.FC<SyncSchedulerDialogProps> = ({
    isOpen,
    onClose,
}) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();

    useEffect(() => {
        if (!isOpen) return;
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [isOpen, onClose]);

    if (!isOpen) return null;

    return (
        <div
            className="fixed inset-0 z-[9998] flex items-start justify-center pt-[10vh] bg-black/50 animate-scale-in"
            role="dialog"
            aria-modal="true"
            aria-label={t('aerosync.launcherScheduler') || 'Scheduler'}
            onClick={onClose}
        >
            <div
                {...modalDrag.panelProps}
                className="relative bg-white dark:bg-gray-800 rounded-lg shadow-2xl w-full max-w-xl max-h-[80vh] overflow-hidden flex flex-col"
                onClick={(e) => e.stopPropagation()}
            >
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between p-4 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <CalendarClock size={18} className="text-blue-500" />
                        <h2 className="text-base font-semibold">
                            {t('aerosync.launcherScheduler') || 'Scheduler'}
                        </h2>
                    </div>
                    <button
                        type="button"
                        onClick={onClose}
                        aria-label={t('common.close') || 'Close'}
                        className="p-1.5 hover:bg-gray-100 dark:hover:bg-gray-700 rounded-lg transition-colors"
                    >
                        <X size={16} />
                    </button>
                </div>

                <div className="flex-1 overflow-y-auto p-3">
                    <SyncScheduler />
                </div>
            </div>
        </div>
    );
};

export default SyncSchedulerDialog;
