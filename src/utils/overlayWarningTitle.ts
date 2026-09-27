// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * The i18n key of the title for a warning `provider_apply_crypt_overlay`
 * returns. One field carries the rclone-crypt wrong-key warning and the
 * AeroCrypt marker notices, so the title follows the overlay kind, and says
 * "restored" only when a marker was restored.
 */
export function overlayWarningTitleKey(kind: string, markerRestored: boolean | undefined): string {
    if (kind === 'rclone-crypt') return 'aerocrypt.title';
    return markerRestored ? 'aerocryptNative.markerMissingRestoredTitle' : 'aerocryptNative.title';
}
