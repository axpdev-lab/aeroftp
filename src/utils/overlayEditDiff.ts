// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { AeroCryptOverlayBinding } from '../types';
import { normalizeRemotePath, resolveOverlayScope } from './overlayScope';

/** The overlay fields of the Quick Connect form, as the user has left them. */
export interface OverlayEditForm {
    enabled: boolean;
    /** False on a backend where the overlay does not apply (media or repo APIs). */
    eligible: boolean;
    kind: 'aerocrypt' | 'rclone-crypt' | null;
    withHeader: boolean;
    useDefaultSalt: boolean;
    filenameEncryption: 'standard' | 'obfuscate' | 'off';
    directoryNameEncryption: boolean;
    /** The Overlays Remote Path field, before it is resolved against the Remote Path. */
    overlaysRemotePath: string;
    /** The Remote Path field. */
    remoteDir: string;
    /** Typed secrets. Never prefilled on edit, so any text is a change. */
    password: string;
    salt: string;
    keyfilePath: string;
}

/** What the stored profile holds, and what the edit form was hydrated with. */
export interface OverlayEditStored {
    binding: AeroCryptOverlayBinding | undefined;
    /** The profile's stored Remote Path, which a blank remoteScope stands for. */
    remotePath: string;
    /** The keyfile path read back from the vault for display ('' when none). */
    hydratedKeyfilePath: string;
}

/**
 * Whether the overlay part of an edit differs from the stored profile.
 *
 * #369: the OAuth edit form has its own Save, so a user can change settings
 * without signing in again, but it only enabled on a name, path or icon change.
 * Every overlay setting was left out, so changing one left Save greyed out and
 * the only way to persist it was a new OAuth sign-in. This compares the overlay
 * the way `aeroCryptOverlayFields` would persist it, so "changed" here means
 * "saving would write something different".
 */
export function overlayEditDiffers(form: OverlayEditForm, stored: OverlayEditStored): boolean {
    // Enabled with no kind chosen builds no binding (fail to plaintext), and an
    // ineligible backend never gets one: both persist as "no overlay".
    const formBinds = form.enabled && form.eligible && form.kind !== null;
    const storedBinds = !!stored.binding?.enabled;
    if (formBinds !== storedBinds) return true;
    if (!formBinds || !stored.binding) return false;

    const b = stored.binding;
    const isRclone = form.kind === 'rclone-crypt';
    if (form.kind !== b.kind) return true;
    if (form.withHeader !== !!b.withHeader) return true;
    if (form.useDefaultSalt !== !!b.useDefaultSalt) return true;
    const formNames = isRclone ? form.filenameEncryption : 'standard';
    if (formNames !== (b.filenameEncryption || 'standard')) return true;
    if (isRclone && form.directoryNameEncryption !== (b.directoryNameEncryption ?? true)) return true;

    // A blank stored scope means "the Remote Path", and the form materializes it
    // on edit, so compare the anchor each side actually resolves to.
    const storedAnchor = normalizeRemotePath(b.remoteScope || '') || normalizeRemotePath(stored.remotePath);
    const formAnchor = resolveOverlayScope(form.overlaysRemotePath, form.remoteDir);
    if (formAnchor !== storedAnchor) return true;

    if (form.password.trim() !== '') return true;
    if (isRclone && form.salt.trim() !== '') return true;
    // A blank keyfile field keeps the stored path, the same convention as the
    // password, so only a different non-empty path is a change.
    const keyfile = form.keyfilePath.trim();
    if (!isRclone && keyfile !== '' && keyfile !== stored.hydratedKeyfilePath.trim()) return true;
    return false;
}
