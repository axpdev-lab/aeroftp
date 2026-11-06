// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { CryptSecretForm } from '../types';

/** What the legacy rclone-crypt import banner read from a profile's options. */
export interface RcloneCryptBannerValues {
    password: string;
    salt?: string;
    filenameEncryption?: string;
    directoryNameEncryption?: boolean;
}

/**
 * The overlay the banner opens. The importer revealed the password and salt
 * before it put them in the options, so they are clear: read again, a salt
 * rclone generated (22 URL-safe base64 characters) came back empty and the
 * overlay opened with rclone's default salt.
 */
export function bannerOverlayParams(banner: RcloneCryptBannerValues, folder: string) {
    return {
        kind: 'rclone-crypt' as const,
        // The folder the panel shows, which the connect opened at the crypt
        // remote's folder, as the unlock dialog opens its own. The option
        // `rcloneCryptRemote` is rclone's `nas:docs`, remote name included:
        // read as a path it anchored `/nas:docs`, and the real folder fell
        // outside the overlay, where writes pass through in plaintext.
        remoteScope: folder,
        filenameEncryption: banner.filenameEncryption || 'standard',
        directoryNameEncryption: banner.directoryNameEncryption !== false,
        password: banner.password,
        salt: banner.salt || null,
        passwordForm: 'clear' as CryptSecretForm,
        saltForm: 'clear' as CryptSecretForm,
    };
}

/**
 * The folder the banner anchors at: the profile's start folder, where the
 * import put the crypt remote's folder, when it is absolute (the legacy
 * importer wrote it from the root). The panel's folder only when there is no
 * such folder: the banner can be clicked long after the connect, and anchored
 * at a subfolder the user moved to, the names rclone derived from the real
 * root read as noise and uploads were named where rclone never looks.
 */
export function bannerAnchorFolder(startFolder: string | null | undefined, shownFolder: string): string {
    return startFolder?.startsWith('/') ? startFolder : shownFolder;
}
