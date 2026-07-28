// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Check, Copy } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { useClipboardCopy } from '../../hooks/useClipboardCopy';
import {
    AEROCRYPT_DEFAULT_SALT_V1_HEX,
    AEROCRYPT_DEFAULT_SALT_V1_PREIMAGE,
    formatDefaultSaltForDisplay,
} from '../../utils/aerocryptDefaultSalt';

interface DefaultSaltDisclosureProps {
    /** Indentation class so the block lines up under whichever radios it follows. */
    className?: string;
}

/**
 * The public default salt, shown under the 128-bit / 256-bit radios (Ehud #369).
 *
 * Two create surfaces offer default-salt mode (the Quick Connect form and the
 * native AeroCrypt dialog) and both were showing the radios with no salt and no
 * statement of what the radios set, which is what made them read as "the salt is
 * 128/256-bit". One component so the two cannot drift apart, and so the copy
 * state exists once.
 *
 * The whole salt is a copy button: it is 64 hex characters, and selecting that
 * by hand off a screen is exactly the chore the button removes. Copy goes
 * through the Rust `copy_to_clipboard` command, the same path the password forge
 * uses, because WebKitGTK's `navigator.clipboard` is not reliable here. Nothing
 * secret is on the clipboard (the salt is public and versioned), so unlike a
 * password there is no auto-clear timer.
 */
export const DefaultSaltDisclosure: React.FC<DefaultSaltDisclosureProps> = ({ className = '' }) => {
    const t = useTranslation();
    const { copied, copy } = useClipboardCopy();

    return (
        <div className={`text-gray-500 dark:text-gray-400 leading-relaxed ${className}`}>
            <div>{t('aerocryptProfile.defaultSaltTierMeaning')}</div>
            <div className="mt-1 flex flex-wrap items-baseline gap-x-1.5">
                <span>
                    {t('aerocryptProfile.defaultSaltValueLabel')}{' '}
                    <code className="font-mono">&quot;{AEROCRYPT_DEFAULT_SALT_V1_PREIMAGE}&quot;</code>:
                </span>
                <button
                    type="button"
                    onClick={() => { void copy(AEROCRYPT_DEFAULT_SALT_V1_HEX); }}
                    title={copied ? t('common.copied') : t('common.copy')}
                    aria-label={`${t('common.copy')}: ${AEROCRYPT_DEFAULT_SALT_V1_HEX}`}
                    className="inline-flex items-baseline gap-1 rounded px-1 -mx-1 font-mono text-left break-all text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700/60 transition-colors cursor-pointer"
                >
                    <span>{formatDefaultSaltForDisplay()}</span>
                    <span className="shrink-0 self-center">
                        {copied
                            ? <Check size={12} className="text-green-500" />
                            : <Copy size={12} className="opacity-60" />}
                    </span>
                </button>
            </div>
        </div>
    );
};
