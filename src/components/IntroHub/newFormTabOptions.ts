// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { getProviderById, presetDefaultS3Region, resolveS3Endpoint } from '../../providers';
import { connectionProfileDefaults } from '../../providers/connectionManifest';

/**
 * The options of the form an Add Service pick opens: the protocol's manifest
 * defaults, as the protocol selector starts a form with, then the preset's
 * defaults the form preselects. Without the manifest's `proton_mode: native`
 * a Proton Drive pick opened on the CLI tab, while `profile-add` and the
 * selector create a profile on AeroFTP's own client.
 */
export function newFormTabOptions(protocol: string, providerId?: string): Record<string, unknown> {
    const provider = providerId ? getProviderById(providerId) : undefined;
    return {
        ...connectionProfileDefaults(protocol).options,
        pathStyle: provider?.defaults?.pathStyle,
        // Template presets (IBM COS, Wasabi, MEGA S4) get the region the
        // form preselects and the endpoint built from it, as on selection.
        region: presetDefaultS3Region(providerId),
        endpoint: resolveS3Endpoint(providerId, presetDefaultS3Region(providerId)) ?? undefined,
        anonymous: provider?.defaults?.anonymous,
        // Local-bridge WebDAV presets (Filen Desktop, MEGAcmd) carry their default scheme
        webdavScheme: provider?.defaults?.webdavScheme,
        // Local HTTPS bridges (Filen Desktop S3) use self-signed certs
        verifyCert: provider?.defaults?.verifyCert,
    };
}
