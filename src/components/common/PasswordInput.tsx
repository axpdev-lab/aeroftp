// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { useState } from 'react';
import { Eye, EyeOff } from 'lucide-react';
import { useTranslation } from '../../i18n';

interface PasswordInputProps {
    value: string;
    onChange: (value: string) => void;
    placeholder?: string;
    autoFocus?: boolean;
    disabled?: boolean;
    id?: string;
    name?: string;
    onKeyDown?: (e: React.KeyboardEvent<HTMLInputElement>) => void;
    /** Tailwind classes for the <input>. The caller owns the look; the eye
     *  toggle is positioned over the right padding the caller leaves (pr-10). */
    className?: string;
    /** Accessible label for the field when no visible <label> is associated. */
    ariaLabel?: string;
    /** Start with the value revealed (default masked). */
    defaultVisible?: boolean;
}

const DEFAULT_INPUT_CLASS =
    'w-full px-3 py-2 pr-10 border border-gray-300 dark:border-gray-600 rounded bg-white dark:bg-gray-700 text-gray-900 dark:text-white';

/**
 * Shared masked password field with a built-in show/hide eye toggle.
 *
 * Replaces the ~dozen hand-rolled copies of the same markup across the create
 * dialogs (pure dedup, no behaviour change). The toggle state is local to the
 * component; callers only deal with the plaintext value.
 */
export const PasswordInput: React.FC<PasswordInputProps> = ({
    value,
    onChange,
    placeholder,
    autoFocus,
    disabled,
    id,
    name,
    onKeyDown,
    className,
    ariaLabel,
    defaultVisible = false,
}) => {
    const t = useTranslation();
    const [visible, setVisible] = useState(defaultVisible);

    return (
        <div className="relative">
            <input
                type={visible ? 'text' : 'password'}
                value={value}
                onChange={(e) => onChange(e.target.value)}
                onKeyDown={onKeyDown}
                className={className ?? DEFAULT_INPUT_CLASS}
                placeholder={placeholder}
                autoFocus={autoFocus}
                disabled={disabled}
                id={id}
                name={name}
                aria-label={ariaLabel}
            />
            <button
                type="button"
                onClick={() => setVisible((v) => !v)}
                title={visible ? t('accountLock.hidePassword') : t('accountLock.showPassword')}
                aria-label={visible ? t('accountLock.hidePassword') : t('accountLock.showPassword')}
                className="absolute right-2 top-1/2 -translate-y-1/2 p-1 rounded text-gray-400 hover:text-gray-600 dark:hover:text-gray-200 hover:bg-gray-100 dark:hover:bg-gray-700"
            >
                {visible ? <EyeOff size={16} /> : <Eye size={16} />}
            </button>
        </div>
    );
};

export default PasswordInput;
