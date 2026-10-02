// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { useEffect } from 'react';
import { X } from 'lucide-react';
import { useTranslation } from '../i18n';
import { useDraggableModal } from '../hooks/useDraggableModal';
import { CyberShieldIcon } from './icons/CyberShieldIcon';
import { CyberToolsPanel } from './CyberToolsPanel';

interface CyberToolsModalProps {
    onClose: () => void;
}

/**
 * Security Tools as a floating window, opened by the Cyber-theme titlebar
 * easter egg (#369). AeroTools shows the same CyberToolsPanel as a column
 * instead (#347), so the app stays usable while the tools are open.
 */
export const CyberToolsModal: React.FC<CyberToolsModalProps> = ({ onClose }) => {
    const t = useTranslation();
    const modalDrag = useDraggableModal();

    // Close on Escape
    useEffect(() => {
        const handler = (e: KeyboardEvent) => { if (e.key === 'Escape') onClose(); };
        window.addEventListener('keydown', handler);
        return () => window.removeEventListener('keydown', handler);
    }, [onClose]);

    // No outside-click-to-close: users copy hashes / drag files here, an accidental
    // backdrop click must not dismiss (close via the X button or Escape).
    return (
        <div className="fixed inset-0 z-50 flex items-start justify-center pt-[5vh] bg-black/60">
            <div
                {...modalDrag.panelProps}
                className="bg-white dark:bg-gray-800 rounded-lg shadow-2xl border border-gray-200 dark:border-gray-700 w-[640px] max-h-[85vh] flex flex-col animate-scale-in"
            >
                {/* Header */}
                <div
                    {...modalDrag.dragHandleProps}
                    className="flex items-center justify-between px-4 py-3 border-b border-gray-200 dark:border-gray-700 cursor-grab active:cursor-grabbing"
                >
                    <div className="flex items-center gap-2 pointer-events-none">
                        <CyberShieldIcon size={18} className="text-cyan-500 dark:text-cyan-400" />
                        <span className="font-medium text-gray-900 dark:text-gray-100">{t('cyberTools.title')}</span>
                    </div>
                    <button onClick={onClose} className="p-1 hover:bg-gray-100 dark:hover:bg-gray-700 rounded transition-colors cursor-pointer">
                        <X size={18} className="text-gray-500" />
                    </button>
                </div>

                <CyberToolsPanel nativeDropScope="window" />
            </div>
        </div>
    );
};

export default CyberToolsModal;
