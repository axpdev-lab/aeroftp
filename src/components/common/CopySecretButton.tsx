// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Check, Copy } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useClipboardCopy } from '../../hooks/useClipboardCopy';

interface CopySecretButtonProps {
    /** The secret to put on the clipboard. Never rendered, not even in the tooltip. */
    value: string;
    size?: number;
    className?: string;
}

/**
 * Copy a revealed secret (a Crypt password or salt, Ehud #215).
 *
 * The sibling of `CopyLinkButton`, which puts its URL in the tooltip so a user
 * can see where a link goes. A secret must not end up in a tooltip, so this one
 * says only "Copy". The field it sits in is read-only, and a read-only field in
 * WebKitGTK is selectable but easy to miss, so the button is the reliable path.
 */
export const CopySecretButton: React.FC<CopySecretButtonProps> = ({ value, size = 16, className = '' }) => {
    const t = useTranslation();
    const { copied, copy } = useClipboardCopy();

    return (
        <button
            type="button"
            tabIndex={-1}
            onClick={(e) => { e.preventDefault(); e.stopPropagation(); void copy(value); }}
            title={copied ? t('common.copied') : t('common.copy')}
            aria-label={t('common.copy')}
            className={`text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 ${className}`}
        >
            {copied ? <Check size={size} className="text-green-500" /> : <Copy size={size} />}
        </button>
    );
};
