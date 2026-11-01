// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderOptions } from '../types';

/**
 * The options a saved profile keeps when it is switched between SFTP and SCP.
 * Both reach the same SSH server, so the key, its passphrase and the timeout
 * carry over. So does an SFTP-only choice (`allowScpFallback: false`): SCP
 * ignores it, but dropping it would turn an SFTP-only profile switched to SCP
 * and back into one that falls back, since an absent value means allowed.
 */
export const optionsAcrossSshSwitch = (options?: ProviderOptions | null): ProviderOptions => {
    const kept: ProviderOptions = {
        private_key_path: options?.private_key_path,
        key_passphrase: options?.key_passphrase,
        timeout: options?.timeout,
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
