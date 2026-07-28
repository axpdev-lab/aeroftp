// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Check, Copy } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useClipboardCopy } from '../../hooks/useClipboardCopy';

interface CopyLinkButtonProps {
    /** The URL to put on the clipboard. Exactly what the sibling link opens. */
    url: string;
    /** Icon size in px. 10 next to a 10px link glyph, 11-14 for standalone rows. */
    size?: number;
    className?: string;
}

/**
 * The small copy affordance that sits beside an external link in a form
 * (Ehud #274).
 *
 * Clicking the link still opens it, which is what most people want. This is for
 * the other case: wanting to see where a "Create account" or "Generate an app
 * password" link actually goes before trusting it, or needing to open it on a
 * different device (the OAuth device-flow URL above all). Copying is the
 * cheapest way to do either, and it was previously impossible without a right
 * click and a context menu that the webview does not always offer.
 *
 * Deliberately not a link itself: it never navigates, so it cannot be mistaken
 * for a second destination. `stopPropagation` keeps it inert inside rows that
 * are themselves clickable.
 */
export const CopyLinkButton: React.FC<CopyLinkButtonProps> = ({ url, size = 10, className = '' }) => {
    const t = useTranslation();
    const { copied, copy } = useClipboardCopy();

    return (
        <button
            type="button"
            onClick={(e) => { e.preventDefault(); e.stopPropagation(); void copy(url); }}
            title={copied ? t('common.copied') : `${t('common.copy')}: ${url}`}
            aria-label={`${t('common.copy')}: ${url}`}
            className={`inline-flex items-center justify-center shrink-0 rounded p-0.5 align-middle text-current opacity-50 hover:opacity-100 hover:bg-black/5 dark:hover:bg-white/10 transition-opacity cursor-pointer ${className}`}
        >
            {copied
                ? <Check size={size} className="text-green-500 opacity-100" />
                : <Copy size={size} />}
        </button>
    );
};
