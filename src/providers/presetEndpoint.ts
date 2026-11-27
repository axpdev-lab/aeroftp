// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { ProviderConfig } from './types';
import type { ProviderOptions } from '../types';
import { expandUsernameInPath } from '../utils/usernameTemplate';
import { presetDefaultS3Region, resolveS3Endpoint, s3TemplateParams } from './registry';

/** The endpoint a preset connection falls back to when the user resets it. */
export interface EndpointDefault {
    /** Server URL (or S3 endpoint) of the preset default; may carry a `{username}` template. */
    server: string;
    port?: number;
}

/** The slice of connection params the endpoint helpers need. */
export interface EndpointParamsLike {
    server?: string;
    port?: number;
    username?: string;
    options?: ProviderOptions;
}

/** Trailing slashes are not a difference when comparing endpoints. */
const stripTrailingSlashes = (value: string): string => value.trim().replace(/\/+$/, '');

/**
 * The preset default endpoint for a connection, or null when there is none to
 * reset to: generic (custom) providers have no preset default, and an S3
 * preset without endpoint or endpointTemplate (e.g. Amazon S3 itself) derives
 * its endpoint from the region, not from a fixed value.
 *
 * For server-based protocols (WebDAV, Swift, ...) the default is
 * `defaults.server` / `defaults.port`, possibly a `{username}` template (Puter):
 * resetting restores the template, so the URL follows the username again.
 * For S3 presets the endpoint lives in `options.endpoint` and the default is
 * what `resolveS3Endpoint` computes for the current region, exactly the rule
 * the connect path uses.
 */
export function presetEndpointDefault(
    provider: ProviderConfig | null | undefined,
    params: EndpointParamsLike,
): EndpointDefault | null {
    if (!provider || provider.isGeneric) return null;
    if (provider.protocol === 's3') {
        const region = params.options?.region || presetDefaultS3Region(provider.id);
        const endpoint = resolveS3Endpoint(provider.id, region, s3TemplateParams(params.options));
        if (!endpoint) return null;
        return { server: endpoint, port: 443 };
    }
    const server = provider.defaults?.server;
    if (!server) return null;
    return { server, port: provider.defaults?.port };
}

/** The endpoint value currently held by the connection params. */
export function currentEndpointValue(
    provider: ProviderConfig | null | undefined,
    params: EndpointParamsLike,
): string {
    if (provider?.protocol === 's3') return params.options?.endpoint ?? '';
    return params.server ?? '';
}

/**
 * Whether the current endpoint differs from the preset default: the link
 * "Reset to default" is shown only in that case. A blank server counts as the
 * default (the form displays the default in its place), a trailing slash is
 * not a difference, and a `{username}` template also matches its expansion
 * with the current username. Server and port are both compared.
 */
export function isEndpointCustomized(
    provider: ProviderConfig | null | undefined,
    params: EndpointParamsLike,
    fallbackPort?: number,
): boolean {
    const def = presetEndpointDefault(provider, params);
    if (!def) return false;
    const current = stripTrailingSlashes(currentEndpointValue(provider, params));
    if (current !== '') {
        const defServer = stripTrailingSlashes(def.server);
        const expanded = stripTrailingSlashes(expandUsernameInPath(def.server, params.username ?? ''));
        if (current !== defServer && current !== expanded) return true;
    }
    const currentPort = params.port || fallbackPort;
    const defPort = def.port || fallbackPort;
    return currentPort !== defPort;
}
