// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState, useRef, useEffect } from 'react';
import { Download, Folder, FileArchive, Package, ChevronDown, AlertTriangle } from 'lucide-react';
import { useTranslation } from '../../i18n';

export type SaveAllTarget = 'folder' | 'zip' | 'aerozip';

/**
 * "Save all..." dropdown for the vault browse views (AeroMount Save-All, #322).
 *
 * Offers the whole decrypted tree as a Folder, a .zip, or a .aerozip. Choosing a
 * target first shows a confirm dialog that spells out the plaintext-export risk
 * (the .zip target adds a "not encrypted" note), then calls back so the parent
 * runs the actual file dialog + backend command. Shared by CryptomatorBrowser and
 * the .aerovault VaultBrowse.
 */
interface SaveAllMenuProps {
    disabled?: boolean;
    onExport: (target: SaveAllTarget) => void;
    className?: string;
}

export const SaveAllMenu: React.FC<SaveAllMenuProps> = ({ disabled, onExport, className }) => {
    const t = useTranslation();
    const [open, setOpen] = useState(false);
    const [confirmTarget, setConfirmTarget] = useState<SaveAllTarget | null>(null);
    const ref = useRef<HTMLDivElement>(null);

    useEffect(() => {
        if (!open) return;
        const onDoc = (e: MouseEvent) => {
            if (ref.current && !ref.current.contains(e.target as Node)) setOpen(false);
        };
        document.addEventListener('mousedown', onDoc);
        return () => document.removeEventListener('mousedown', onDoc);
    }, [open]);

    const items: { target: SaveAllTarget; icon: React.ReactNode; label: string }[] = [
        { target: 'folder', icon: <Folder size={14} className="text-yellow-500 dark:text-yellow-400" />, label: t('saveAll.folder') },
        { target: 'zip', icon: <FileArchive size={14} className="text-blue-500 dark:text-blue-400" />, label: t('saveAll.zip') },
        { target: 'aerozip', icon: <Package size={14} className="text-emerald-500 dark:text-emerald-400" />, label: t('saveAll.aerozip') },
    ];

    return (
        <div className={`relative ${className || ''}`} ref={ref}>
            <button
                onClick={() => setOpen(o => !o)}
                disabled={disabled}
                title={t('saveAll.hint')}
                className="flex items-center gap-1 px-2 py-1 text-xs bg-blue-700 hover:bg-blue-600 text-white rounded disabled:opacity-50"
            >
                <Download size={14} /> {t('saveAll.button')} <ChevronDown size={12} />
            </button>
            {open && (
                <div className="absolute z-50 mt-1 left-0 min-w-[190px] bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded shadow-lg py-1">
                    {items.map(it => (
                        <button
                            key={it.target}
                            onClick={() => { setOpen(false); setConfirmTarget(it.target); }}
                            className="w-full flex items-center gap-2 px-3 py-1.5 text-xs text-left text-gray-700 dark:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700"
                        >
                            {it.icon} {it.label}
                        </button>
                    ))}
                </div>
            )}
            {confirmTarget && (
                <div className="fixed inset-0 z-[120] flex items-center justify-center bg-black/50" onClick={() => setConfirmTarget(null)}>
                    <div
                        className="w-[min(92vw,440px)] rounded-lg border border-gray-200 dark:border-gray-700 bg-white dark:bg-gray-900 shadow-2xl p-5"
                        onClick={e => e.stopPropagation()}
                    >
                        <div className="flex items-center gap-2 mb-2 text-amber-600 dark:text-amber-400">
                            <AlertTriangle size={18} />
                            <span className="font-medium text-sm text-gray-900 dark:text-gray-100">{t('saveAll.confirmTitle')}</span>
                        </div>
                        <p className="text-sm text-gray-600 dark:text-gray-300">{t('saveAll.confirmBody')}</p>
                        {confirmTarget === 'zip' && (
                            <p className="text-xs text-amber-600 dark:text-amber-400 mt-2">{t('saveAll.confirmZipNote')}</p>
                        )}
                        <div className="flex justify-end gap-2 mt-4">
                            <button onClick={() => setConfirmTarget(null)} className="px-3 py-1 text-xs rounded bg-gray-200 dark:bg-gray-700 hover:bg-gray-300 dark:hover:bg-gray-600 text-gray-700 dark:text-gray-200">
                                {t('saveAll.cancel')}
                            </button>
                            <button
                                onClick={() => { const tg = confirmTarget; setConfirmTarget(null); onExport(tg); }}
                                className="px-3 py-1 text-xs rounded bg-blue-600 hover:bg-blue-500 text-white"
                            >
                                {t('saveAll.continue')}
                            </button>
                        </div>
                    </div>
                </div>
            )}
        </div>
    );
};
