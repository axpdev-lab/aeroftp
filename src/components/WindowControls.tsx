// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { getCurrentWindow } from '@tauri-apps/api/window';
import { Maximize2, Minus, Square, X } from 'lucide-react';
import { useTranslation } from '../i18n';

/**
 * Minimize / maximize / close buttons for the custom (undecorated) window.
 * Extracted so screens shown before the main titlebar (e.g. the multi-user
 * "Choose an account" lock screen) can offer the same controls without
 * pulling in the whole CustomTitlebar. Mirrors its window-controls cluster.
 * Requested in discussion #270.
 */
export const WindowControls: React.FC<{ className?: string }> = ({ className }) => {
    const t = useTranslation();
    const [isMaximized, setIsMaximized] = React.useState(false);

    React.useEffect(() => {
        let cancelled = false;
        let unlisten: (() => void) | undefined;
        const sync = async () => {
            try {
                const max = await getCurrentWindow().isMaximized();
                if (!cancelled) setIsMaximized(max);
            } catch {
                /* non-Tauri/test env: keep default */
            }
        };
        void sync();
        getCurrentWindow()
            .onResized(sync)
            .then(u => { if (cancelled) u(); else unlisten = u; })
            .catch(() => { /* listener unavailable in test env */ });
        return () => { cancelled = true; unlisten?.(); };
    }, []);

    const handleMinimize = async (e: React.MouseEvent) => {
        e.stopPropagation(); e.preventDefault();
        try { await getCurrentWindow().minimize(); } catch { /* no-op */ }
    };
    const handleMaximize = async (e: React.MouseEvent) => {
        e.stopPropagation(); e.preventDefault();
        try { await getCurrentWindow().toggleMaximize(); } catch { /* no-op */ }
    };
    const handleClose = async (e: React.MouseEvent) => {
        e.stopPropagation(); e.preventDefault();
        try { await getCurrentWindow().close(); } catch { /* no-op */ }
    };

    return (
        <div className={`flex items-center gap-0.5 ${className ?? ''}`}>
            <button
                onClick={handleMinimize}
                className="flex h-7 w-7 items-center justify-center rounded-lg transition-colors hover:bg-[var(--color-bg-tertiary)] cursor-pointer"
                title={t('ui.minimize')}
                aria-label={t('ui.minimize')}
            >
                <Minus size={14} className="text-[var(--color-text-secondary)]" />
            </button>
            <button
                onClick={handleMaximize}
                className="flex h-7 w-7 items-center justify-center rounded-lg transition-colors hover:bg-[var(--color-bg-tertiary)] cursor-pointer"
                title={isMaximized ? t('ui.restore') : t('ui.maximize')}
                aria-label={isMaximized ? t('ui.restore') : t('ui.maximize')}
            >
                {isMaximized
                    ? <Square size={11} className="text-[var(--color-text-secondary)]" />
                    : <Maximize2 size={14} className="text-[var(--color-text-secondary)]" />}
            </button>
            <button
                onClick={handleClose}
                className="group flex h-7 w-7 items-center justify-center rounded-lg transition-colors hover:bg-red-500/90 cursor-pointer"
                title={t('ui.close')}
                aria-label={t('ui.close')}
            >
                <X size={15} className="text-[var(--color-text-secondary)] group-hover:text-white" />
            </button>
        </div>
    );
};

export default WindowControls;
