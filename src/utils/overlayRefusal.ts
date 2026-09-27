// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/** The last overlay refusal the auto-unlock saw, for the profile it was about. */
export interface OverlayRefusal {
    savedServerId: string;
    kind: string;
    reason: string;
}

/**
 * The reason to show in the locked-overlay banner of `savedServerId`: an
 * rclone-crypt refusal of that same profile, whose text says how to answer it.
 * rclone-crypt opens with any key, so its refusals are the secrets it will not
 * guess at; an AeroCrypt failure (a wrong password) keeps the banner's own text.
 */
export function refusalFor(refusal: OverlayRefusal | null, savedServerId: string): string | undefined {
    return refusal && refusal.savedServerId === savedServerId && refusal.kind === 'rclone-crypt'
        ? refusal.reason
        : undefined;
}
