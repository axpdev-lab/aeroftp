// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderOptions, ProviderType } from '../types';

/**
 * The options a saved profile keeps when it is switched between SFTP and SCP.
 * Both reach the same SSH server, so the key, its passphrase and the timeout
 * carry over, and so do the quota settings of that server (a manual total and
 * the used-space scan on connect), which the quota path reads from the saved
 * profile. So does an SFTP-only choice (`allowScpFallback: false`): SCP
 * ignores it, but dropping it would turn an SFTP-only profile switched to SCP
 * and back into one that falls back, since an absent value means allowed.
 */
export const optionsAcrossSshSwitch = (options?: ProviderOptions | null): ProviderOptions => {
    const kept: ProviderOptions = {
        private_key_path: options?.private_key_path,
        key_passphrase: options?.key_passphrase,
        timeout: options?.timeout,
        manualTotalBytes: options?.manualTotalBytes,
        autoScanUsedOnConnect: options?.autoScanUsedOnConnect,
    };
    return options?.allowScpFallback === false ? { ...kept, allowScpFallback: false } : kept;
};

/**
 * Whether a profile's registry preset still fits after a protocol switch.
 * Editing a profile whose preset speaks another protocol switches the form
 * back to the preset's protocol, so a preset that does not fit has to go with
 * the switch, or the saved profile reopens as its old protocol. No preset, or
 * one the registry does not know, fits anything, as editing leaves it alone.
 */
export const presetFitsProtocol = (presetProtocol: string | undefined, protocol: string): boolean =>
    !presetProtocol || presetProtocol === protocol;

/** The protocols a saved profile switches between when it is edited, keeping edit mode. */
export const SWITCHABLE_PROTOCOLS: ProviderType[] = ['ftp', 'ftps', 'sftp', 'scp'];

/**
 * The stored preset an edited profile keeps on save when the form has none.
 * A switch between the switchable protocols drops a preset of another
 * protocol from the form, so the save must not take it back from the stored
 * profile. Other protocols keep the stored id as it is: a native Koofr
 * profile stores `koofr`, the id of the Koofr WebDAV preset.
 */
export const storedPresetForSave = (
    storedProviderId: string | undefined,
    protocol: string,
    presetProtocolOf: (providerId: string) => string | undefined,
): string | undefined => {
    if (!storedProviderId) return undefined;
    if (!SWITCHABLE_PROTOCOLS.includes(protocol as ProviderType)) return storedProviderId;
    return presetFitsProtocol(presetProtocolOf(storedProviderId), protocol) ? storedProviderId : undefined;
};
