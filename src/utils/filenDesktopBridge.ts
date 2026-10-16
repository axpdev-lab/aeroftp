// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ConnectionParams } from '../types';

/** Whether a preset is one of Filen Desktop's local bridges (WebDAV or S3). */
export function isFilenDesktopBridge(providerId: string | undefined): boolean {
    return providerId === 'filen-desktop-webdav' || providerId === 'filen-desktop-s3';
}

/**
 * Issue #215: the Filen Desktop local bridges keep "admin" only as a
 * placeholder hint, not a hard default, because the real bridge credentials
 * are whatever the user set inside Filen Desktop > Network Drive. Many users
 * connect first just to check the bridge is up, so empty credentials (S3 maps
 * access/secret key to username/password) fall back to "admin". Explicit
 * values win. The connect and the bucket Fetch (#958) both go through here,
 * so a profile that connects with the default keys can also list its buckets.
 */
export function withFilenBridgeCredentialDefaults<T extends Pick<ConnectionParams, 'providerId' | 'username' | 'password'>>(params: T): T {
    if (!isFilenDesktopBridge(params.providerId)) return params;
    return {
        ...params,
        username: params.username || 'admin',
        password: params.password || 'admin',
    };
}
