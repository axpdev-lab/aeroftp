// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Which saved-profile protocols AeroCloud offers, and how mature its sync is
// on each protocol the backend dispatches.

import * as React from 'react';
import { useTranslation } from '../i18n';

export type AeroCloudMaturity = 'stable' | 'beta' | 'alpha';

/** Sync maturity per protocol (post-hardening audit 2026-03-26), shown for any configured AeroCloud. */
const AEROCLOUD_MATURITY: Record<string, AeroCloudMaturity> = {
    sftp: 'stable', s3: 'stable', azure: 'stable', webdav: 'stable',
    googledrive: 'stable', dropbox: 'stable', onedrive: 'stable',
    jottacloud: 'stable', kdrive: 'stable', koofr: 'stable',
    opendrive: 'stable', backblaze: 'stable',
    ftp: 'beta', ftps: 'beta', box: 'beta', pcloud: 'beta',
    zohoworkdrive: 'beta', yandexdisk: 'beta', mega: 'beta',
    filen: 'beta', internxt: 'beta',
    filelu: 'beta',
    imagekit: 'beta', uploadcare: 'beta', cloudinary: 'beta',
    fourshared: 'alpha',
};

/**
 * The protocols the setup wizard offers: those whose profile connect path was
 * verified live with `live_aerocloud_connects_saved_profiles` in
 * src-tauri/src/cloud_provider_factory.rs (2026-10-15, production vault).
 * Google Drive, Jottacloud and 4shared are dispatched by the backend but stay
 * out until a profile with a valid token passes that survey; Proton Drive and
 * Twake have had no profile to verify.
 */
const AEROCLOUD_OFFERED = new Set([
    'ftp', 'ftps', 'sftp', 'webdav', 's3', 'azure', 'backblaze', 'mega', 'filen',
    'internxt', 'kdrive', 'koofr', 'opendrive', 'filelu', 'imagekit', 'uploadcare',
    'cloudinary', 'yandexdisk', 'dropbox', 'onedrive', 'box', 'pcloud', 'zohoworkdrive',
]);

/** i18n key of the one-line sync limitation of a protocol, when it has one. */
const SYNC_CAVEAT_KEYS: Record<string, string> = {
    ftp: 'cloud.caveat.mfmt',
    ftps: 'cloud.caveat.mfmt',
    box: 'cloud.caveat.box',
    pcloud: 'cloud.caveat.pcloud',
    zohoworkdrive: 'cloud.caveat.zohoworkdrive',
    yandexdisk: 'cloud.caveat.yandexdisk',
    mega: 'cloud.caveat.mega',
    filen: 'cloud.caveat.e2e',
    internxt: 'cloud.caveat.e2e',
    filelu: 'cloud.caveat.filelu',
    backblaze: 'cloud.caveat.backblaze',
    imagekit: 'cloud.caveat.media',
    uploadcare: 'cloud.caveat.media',
    cloudinary: 'cloud.caveat.cloudinary',
    fourshared: 'cloud.caveat.fourshared',
};

export function isAeroCloudProtocol(protocol: string | undefined): boolean {
    return !!protocol && AEROCLOUD_OFFERED.has(protocol);
}

export function aeroCloudMaturity(protocol: string | undefined): AeroCloudMaturity | undefined {
    return protocol ? AEROCLOUD_MATURITY[protocol] : undefined;
}

export function useAeroCloudCaveat(): (protocol: string | undefined) => string {
    const t = useTranslation();
    return (protocol) => {
        const key = protocol ? SYNC_CAVEAT_KEYS[protocol] : undefined;
        return key ? t(key) : '';
    };
}

/** "Beta" / "Alpha" mark for a protocol; nothing for a stable one. */
export const AeroCloudMaturityBadge: React.FC<{ protocol: string | undefined; className?: string }> = ({ protocol, className = '' }) => {
    const t = useTranslation();
    const maturity = aeroCloudMaturity(protocol);
    if (maturity === 'beta') {
        return <span className={`px-1 py-0.5 text-[9px] rounded font-semibold bg-yellow-500/20 text-yellow-700 dark:text-yellow-400 ${className}`.trim()}>{t('aeroShare.experimentalTag')}</span>;
    }
    if (maturity === 'alpha') {
        return <span className={`px-1 py-0.5 text-[9px] rounded font-semibold bg-red-500/20 text-red-700 dark:text-red-400 ${className}`.trim()}>{t('cloud.maturity.alpha')}</span>;
    }
    return null;
};
