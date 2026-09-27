// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { CryptSecretForm } from '../types';

/** What the legacy rclone-crypt import banner read from a profile's options. */
export interface RcloneCryptBannerValues {
    password: string;
    salt?: string;
    filenameEncryption?: string;
    directoryNameEncryption?: boolean;
    initialPath?: string;
}

/**
 * The overlay the banner opens. The importer revealed the password and salt
 * before it put them in the options, so they are clear: read again, a salt
 * rclone generated (22 URL-safe base64 characters) came back empty and the
 * overlay opened with rclone's default salt.
 */
export function bannerOverlayParams(banner: RcloneCryptBannerValues) {
    return {
        kind: 'rclone-crypt' as const,
        remoteScope: banner.initialPath ?? '',
        filenameEncryption: banner.filenameEncryption || 'standard',
        directoryNameEncryption: banner.directoryNameEncryption !== false,
        password: banner.password,
        salt: banner.salt || null,
        passwordForm: 'clear' as CryptSecretForm,
        saltForm: 'clear' as CryptSecretForm,
    };
}
