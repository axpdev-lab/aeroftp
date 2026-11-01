// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderOptions } from '../types';

/**
 * SFTP with SCP fallback, WinSCP's default: when the server has no SFTP, the
 * same connection goes on over SCP. The saved profile stores only the
 * exception, `allowScpFallback: false` (WinSCP's "SFTP only", which the
 * WinSCP import writes too), so a profile saved before the option existed
 * keeps the default.
 */
export const scpFallbackAllowed = (options?: Pick<ProviderOptions, 'allowScpFallback'> | null): boolean =>
    options?.allowScpFallback !== false;

/** The options after the toggle: `false` when turned off, the key removed when on. */
export const withScpFallback = <T extends Pick<ProviderOptions, 'allowScpFallback'>>(options: T, allowed: boolean): T => {
    const { allowScpFallback: _previous, ...rest } = options;
    return (allowed ? rest : { ...rest, allowScpFallback: false }) as T;
};

/**
 * The `allow_scp_fallback` field of the `provider_connect` params. Sent only
 * when an SFTP profile turned the fallback off: the backend reads an absent
 * field as allowed, and no other protocol has the option.
 */
export const scpFallbackConnectParam = (
    protocol: string | undefined,
    options?: Pick<ProviderOptions, 'allowScpFallback'> | null,
): { allow_scp_fallback?: false } =>
    protocol === 'sftp' && !scpFallbackAllowed(options) ? { allow_scp_fallback: false } : {};

/**
 * The protocol a surface names for the live connection. An SFTP profile that
 * went on over SCP is named SCP, since what it can do there (the hashes the
 * server computes, resume through the shell) is what SCP can do. For display
 * only: everything that decides keeps the profile's protocol.
 */
export const connectionProtocolLabel = (protocol: string | undefined, wentOnOverScp: boolean): string | undefined =>
    protocol === 'sftp' && wentOnOverScp ? 'scp' : protocol;
