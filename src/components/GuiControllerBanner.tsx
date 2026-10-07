// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { Square, Bot } from 'lucide-react';
import { useTranslation } from '../i18n';
import type { GuiLease } from '../gui/controller';

export function GuiControllerBanner({ lease, onStop }: { lease: GuiLease | null; onStop: () => void }) {
    const t = useTranslation();
    if (!lease) return null;
    return <div role="status" aria-live="polite" className="fixed bottom-12 left-1/2 -translate-x-1/2 z-[9998] flex items-center gap-3 rounded-xl border border-amber-400 bg-amber-50 px-4 py-3 text-amber-950 shadow-xl dark:bg-gray-900 dark:text-amber-200">
        <Bot size={20} aria-hidden="true" />
        <div>
            <p className="text-sm font-medium">{t('guiController.banner', { agent: lease.owner })}</p>
            {lease.intent && <p className="text-xs">{t(lease.intent === 'connect' ? 'common.connect' : `guiController.actions.${lease.intent}`)}</p>}
        </div>
        <button data-gui-controller-stop type="button" onClick={onStop} className="flex items-center gap-1 rounded-lg bg-red-600 px-3 py-2 text-sm text-white hover:bg-red-700">
            <Square size={14} aria-hidden="true" />{t('guiController.actions.stop')}
        </button>
    </div>;
}
