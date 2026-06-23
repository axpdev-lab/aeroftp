// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Check, X } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface PasswordMatchHintProps {
    password: string;
    confirm: string;
    className?: string;
}

/**
 * Live confirm-match indicator: updates on every keystroke instead of only on
 * submit. Renders nothing until the confirm field has input, then a subtle
 * success or warning line. The owning dialog keeps its own submit gating
 * (`password === confirm`); this just makes the state visible while typing.
 */
export const PasswordMatchHint: React.FC<PasswordMatchHintProps> = ({ password, confirm, className }) => {
    const t = useTranslation();
    if (!confirm) return null;
    const matches = password === confirm;
    return (
        <p
            className={`mt-1 flex items-center gap-1 text-xs ${
                matches ? 'text-emerald-600 dark:text-emerald-400' : 'text-red-500'
            } ${className ?? ''}`}
        >
            {matches ? <Check size={12} /> : <X size={12} />}
            {matches ? t('password.match') : t('password.mismatch')}
        </p>
    );
};

export default PasswordMatchHint;
