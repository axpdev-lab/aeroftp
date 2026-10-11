// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Policy C (owner, 2026-10-11): on a server whose replace sets the old file
// aside, a slot change replaces the vault marker only with the user's
// explicit consent, given here with the risks in front of them. On a server
// that replaces in one step nothing is asked; on one that cannot replace at
// all the change is not offered as possible.

import * as React from 'react';
import { useTranslation } from '../i18n';

/** How the marker can be replaced, as `aerocrypt_list_slots` reports it. */
export type MarkerReplaceMode = 'atomic' | 'setsAside' | 'unsupported';

interface Props {
    mode: MarkerReplaceMode | null | undefined;
    /** A revocation (remove, rotate): the old marker still opens with the old factor. */
    revocation: boolean;
    checked: boolean;
    onChange: (checked: boolean) => void;
    disabled?: boolean;
}

/** Whether a slot change may be sent, and with which `allowNonAtomic`. */
export function setAsideConsentState(
    mode: MarkerReplaceMode | null | undefined,
    checked: boolean,
): { blocked: boolean; allowNonAtomic: boolean } {
    if (mode === 'setsAside') {
        return { blocked: !checked, allowNonAtomic: checked };
    }
    return { blocked: false, allowNonAtomic: false };
}

export const AeroCryptSetAsideConsent: React.FC<Props> = ({
    mode,
    revocation,
    checked,
    onChange,
    disabled,
}) => {
    const t = useTranslation();
    if (mode === 'unsupported') {
        return (
            <p className="text-xs text-amber-800 dark:text-amber-200">
                {t('aerocryptNative.keyslotsMarkerReplaceUnsupported')}
            </p>
        );
    }
    if (mode !== 'setsAside') {
        return null;
    }
    return (
        <div className="space-y-1 rounded border border-amber-300 dark:border-amber-700 bg-amber-50 dark:bg-amber-900/30 p-2 text-xs text-amber-900 dark:text-amber-100">
            <p>{t('aerocryptNative.keyslotsSetAsideRisks')}</p>
            {revocation && <p>{t('aerocryptNative.keyslotsSetAsideRevocation')}</p>}
            <label className="flex items-start gap-2 font-medium">
                <input
                    type="checkbox"
                    checked={checked}
                    disabled={disabled}
                    onChange={(e) => onChange(e.target.checked)}
                    className="mt-0.5"
                />
                <span>{t('aerocryptNative.keyslotsSetAsideConsent')}</span>
            </label>
        </div>
    );
};
