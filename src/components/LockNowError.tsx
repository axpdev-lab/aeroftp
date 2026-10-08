// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { useEffect, useRef } from 'react';
import { useTranslation } from '../i18n';
import { MODAL_Z } from '../utils/modalLayers';

/** A lock failure must remain visible even over a partially locked app or with toasts disabled. */
export function LockNowError({ error, onClose }: { error: 'failed' | 'unavailable' | 'stale'; onClose: () => void }) {
    const t = useTranslation();
    const close = useRef<HTMLButtonElement>(null);
    useEffect(() => {
        const previous = document.activeElement instanceof HTMLElement ? document.activeElement : null;
        close.current?.focus();
        return () => { if (previous?.isConnected) previous.focus(); };
    }, []);
    return (
        <div className={`fixed inset-0 ${MODAL_Z.elevatedConfirm} flex items-center justify-center bg-black/50`} role="alertdialog" aria-modal="true" data-keyboard-island aria-labelledby="lock-now-error-title" aria-describedby="lock-now-error-message"
            onKeyDown={event => { event.stopPropagation(); if (event.key === 'Escape') onClose(); if (event.key === 'Tab') { event.preventDefault(); close.current?.focus(); } }}>
            <div className="max-w-sm mx-4 p-5 rounded-xl shadow-xl bg-[var(--color-bg-primary)] text-[var(--color-text-primary)] space-y-4">
                <h2 id="lock-now-error-title" className="font-semibold">{t('shortcuts.lockNow')}</h2>
                <p id="lock-now-error-message" className="text-sm">{t(error === 'unavailable' ? 'lockScreen.lockNotConfigured' : error === 'stale' ? 'lockScreen.lockContextChanged' : 'lockScreen.lockFailed')}</p>
                <button ref={close} type="button" className="px-3 py-1 rounded bg-blue-600 text-white" onClick={onClose}>{t('common.close')}</button>
            </div>
        </div>
    );
}
