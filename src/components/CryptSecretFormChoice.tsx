// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import React, { useId, useState } from 'react';
import { useTranslation } from '../i18n';
import type { CryptSecretForm } from '../types';
import { ConfirmDialog } from './Dialogs';

interface Props {
    /** Which secret this is about, e.g. the password field's label. */
    legend: string;
    /** `undefined`: not recorded (a value saved before AeroFTP asked). */
    value: CryptSecretForm | undefined;
    onChange: (form: CryptSecretForm) => void;
    /** A bound profile: the choice decides the key the files open with. */
    confirmChange?: boolean;
    disabled?: boolean;
    /** Save waits on this choice: the note shows as an error. */
    missing?: boolean;
}

/**
 * What choosing `next` does: nothing when it is the current choice, ask first
 * on a bound profile (where it changes the key the files open with), apply
 * otherwise.
 */
export function formChoiceAction(
    current: CryptSecretForm | undefined,
    next: CryptSecretForm,
    confirmChange: boolean,
): 'none' | 'confirm' | 'apply' {
    if (next === current) return 'none';
    return confirmChange ? 'confirm' : 'apply';
}

/**
 * How an rclone-crypt password or salt was entered: typed as it is, or pasted
 * from rclone.conf, where rclone keeps it obscured. Asked, never guessed: a
 * salt rclone generates reads exactly like an obscured empty one. A value
 * recorded in neither form shows neither option chosen.
 */
export const CryptSecretFormChoice: React.FC<Props> = ({ legend, value, onChange, confirmChange, disabled, missing }) => {
    const t = useTranslation();
    // One radio group per secret: the password and the salt choices share
    // their labels, so each group needs its own name.
    const name = useId();
    const [pending, setPending] = useState<CryptSecretForm | null>(null);
    const choose = (next: CryptSecretForm) => {
        const action = formChoiceAction(value, next, !!confirmChange);
        if (action === 'confirm') setPending(next);
        else if (action === 'apply') onChange(next);
    };
    const option = (form: CryptSecretForm, label: string) => (
        <label className="flex items-center gap-1.5">
            <input
                type="radio"
                name={name}
                checked={value === form}
                disabled={disabled}
                onChange={() => choose(form)}
            />
            <span>{label}</span>
        </label>
    );
    return (
        <fieldset className="mt-1.5 text-xs text-gray-600 dark:text-gray-400">
            <legend className="sr-only">{legend}</legend>
            <div className="flex flex-wrap items-center gap-x-4 gap-y-1">
                {option('clear', t('aerocryptProfile.secretFormTyped'))}
                {option('obscured', t('aerocryptProfile.pastedObscured'))}
            </div>
            <span className={`block mt-0.5 text-[11px] ${missing && value === undefined ? 'text-red-600 dark:text-red-400' : 'text-gray-500 dark:text-gray-500'}`}>
                {value === undefined ? t('aerocryptProfile.formNotRecorded') : t('aerocryptProfile.pastedObscuredHint')}
            </span>
            {pending && (
                <ConfirmDialog
                    message={t('aerocryptProfile.secretFormChangeConfirm')}
                    confirmLabel={t('common.confirm')}
                    confirmColor="blue"
                    onConfirm={() => {
                        onChange(pending);
                        setPending(null);
                    }}
                    onCancel={() => setPending(null)}
                />
            )}
        </fieldset>
    );
};
