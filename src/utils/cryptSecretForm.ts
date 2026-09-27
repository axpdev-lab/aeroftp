// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { CryptSecretForm, ServerProfile } from '../types';

/**
 * The forms a profile's rclone-crypt password and salt are recorded in, for
 * display in the profile form: what the binding records, `undefined` when it
 * records nothing. Which key a secret opens with is decided by the backend
 * (`rclone_crypt::crypt_secret_forms`), never here.
 */
export function cryptSecretForms(
    profile: Pick<ServerProfile, 'aeroCryptOverlay'> | null | undefined,
): { password?: CryptSecretForm; salt?: CryptSecretForm } {
    const binding = profile?.aeroCryptOverlay;
    return { password: binding?.passwordForm, salt: binding?.saltForm };
}

/**
 * The forms the profile form starts from: a bound overlay shows what it
 * records (possibly nothing yet); a profile getting its first overlay types
 * new values, which are as typed.
 */
export function hydratedSecretForms(
    profile: Pick<ServerProfile, 'aeroCryptOverlay'> | null | undefined,
): { password?: CryptSecretForm; salt?: CryptSecretForm } {
    return profile?.aeroCryptOverlay?.enabled
        ? cryptSecretForms(profile)
        : { password: 'clear', salt: 'clear' };
}

/**
 * Whether one secret's form is recorded and the other's is not, as after
 * answering a refusal for the password only. Saved that way, the other secret
 * stays on the reading the refusal was about, so the form asks for both.
 */
export function secretFormsHalfRecorded(forms: { password?: CryptSecretForm; salt?: CryptSecretForm }): boolean {
    return (forms.password === undefined) !== (forms.salt === undefined);
}
