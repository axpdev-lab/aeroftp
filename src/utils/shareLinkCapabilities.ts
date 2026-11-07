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
    /** Longest expiry the provider accepts, in seconds; null when it states none. */
    maxExpirationSecs: number | null;
}

/** Wire shape of `ShareLinkCapabilities` in `src-tauri/src/providers/mod.rs`. */
export interface BackendShareLinkCapabilities {
    supports_expiration: boolean;
    supports_password: boolean;
    supports_permissions: boolean;
    available_permissions: string[];
    supports_list_links: boolean;
    supports_revoke: boolean;
    max_expiration_secs?: number | null;
}

export const NO_SHARE_LINK_CAPABILITIES: ShareLinkCapabilities = {
    expiration: false,
    password: false,
    permissions: false,
    availablePermissions: [],
    hasAdvancedOptions: false,
    supportsList: false,
    supportsRevoke: false,
    maxExpirationSecs: null,
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
        maxExpirationSecs:
            typeof raw.max_expiration_secs === 'number' && raw.max_expiration_secs > 0
                ? raw.max_expiration_secs
                : null,
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

/**
 * The expiry choices a provider accepts: none above its stated limit, so a
 * 30-day link is never offered where the provider caps at seven days (every
 * S3 preset) and would clamp or refuse it.
 */
export function expirationChoicesWithin<T extends { value: number }>(
    presets: readonly T[],
    caps: Pick<ShareLinkCapabilities, 'maxExpirationSecs'>,
): T[] {
    const max = caps.maxExpirationSecs;
    return presets.filter(preset => max === null || preset.value <= max);
}
