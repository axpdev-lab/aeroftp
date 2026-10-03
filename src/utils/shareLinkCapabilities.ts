// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * What the Share Link modal may offer for the connected provider.
 *
 * The answer comes from the backend (`provider_share_link_capabilities`),
 * which asks the provider that will build the link. The modal used to keep its
 * own per-provider table instead, and the two had drifted: Proton, B2 and
 * Immich honour expiry (and Proton and Immich a password) in
 * `create_share_link`, but the table did not list them, so the modal never
 * offered those options.
 */
export interface ShareLinkCapabilities {
    expiration: boolean;
    password: boolean;
    permissions: boolean;
    availablePermissions: string[];
    hasAdvancedOptions: boolean;
    supportsList: boolean;
    supportsRevoke: boolean;
}

/** Wire shape of `ShareLinkCapabilities` in `src-tauri/src/providers/mod.rs`. */
export interface BackendShareLinkCapabilities {
    supports_expiration: boolean;
    supports_password: boolean;
    supports_permissions: boolean;
    available_permissions: string[];
    supports_list_links: boolean;
    supports_revoke: boolean;
}

export const NO_SHARE_LINK_CAPABILITIES: ShareLinkCapabilities = {
    expiration: false,
    password: false,
    permissions: false,
    availablePermissions: [],
    hasAdvancedOptions: false,
    supportsList: false,
    supportsRevoke: false,
};

export function shareLinkCapabilitiesFromBackend(raw: BackendShareLinkCapabilities): ShareLinkCapabilities {
    const expiration = raw.supports_expiration === true;
    const password = raw.supports_password === true;
    const permissions = raw.supports_permissions === true;
    return {
        expiration,
        password,
        permissions,
        availablePermissions: Array.isArray(raw.available_permissions) ? raw.available_permissions : [],
        hasAdvancedOptions: expiration || password || permissions,
        supportsList: raw.supports_list_links === true,
        supportsRevoke: raw.supports_revoke === true,
    };
}

/**
 * Ask the connected provider. A failed query offers no options: the modal then
 * creates a plain link, and `provider_create_share_link` reports the same
 * failure (not connected, no share links) where the user can see it.
 */
export async function loadShareLinkCapabilities(
    invoke: (cmd: string) => Promise<unknown>,
): Promise<ShareLinkCapabilities> {
    try {
        const raw = (await invoke('provider_share_link_capabilities')) as BackendShareLinkCapabilities;
        return shareLinkCapabilitiesFromBackend(raw);
    } catch (err) {
        console.warn('[ShareLinkModal] provider_share_link_capabilities failed:', err);
        return NO_SHARE_LINK_CAPABILITIES;
    }
}
