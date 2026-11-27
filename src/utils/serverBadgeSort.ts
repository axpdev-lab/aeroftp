// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { getE2EBits, getProfileProtocolClass, type ProviderType, type ServerProfile } from '../types';

/**
 * The Nextcloud-as-a-service presets that share the "API OCS" badge: the bare
 * preset id (saved from the Quick Connect form) and the `-webdav` variant
 * MyServersPanel's host heuristic gives a profile saved before the preset.
 * The card badge, the Type column sort, the search tokens and the CLI's
 * `badge_sort_label` (aeroftp_cli.rs, which holds its copy equal to this one)
 * all read this list.
 */
export const OCS_BADGE_PROVIDER_IDS: readonly string[] = [
    'felicloud',
    'felicloud-webdav',
    'tabdigital',
    'tabdigital-webdav',
    'murena',
];

export const isOcsBadgeProvider = (providerId: string | null | undefined): boolean =>
    !!providerId && OCS_BADGE_PROVIDER_IDS.includes(providerId);

/**
 * The sort key of the My Servers Type column, which follows the card badge: a
 * crypt-overlay profile (native or interop) shows the Crypt badge and sorts
 * under the Crypt family, an OCS preset under "API OCS", anything else under
 * its protocol class and provider.
 */
export const badgeSortLabel = (server: ServerProfile): string => {
    const proto = (server.protocol || 'ftp') as ProviderType;
    const protocolClass = getProfileProtocolClass(server);
    if (protocolClass !== 'Crypt' && isOcsBadgeProvider(server.providerId)) return 'API OCS';
    const e2eBits = protocolClass === 'E2E' ? getE2EBits(proto) : null;
    return [
        e2eBits ? `${protocolClass} ${e2eBits}-bit` : protocolClass,
        server.providerId || server.protocol || '',
    ].join(' ');
};
