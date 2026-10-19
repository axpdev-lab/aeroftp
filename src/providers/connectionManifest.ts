// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import data from '../shared/connection-manifest.json';
import type { ProviderConfig } from './types';

export interface ConnectionProtocol {
    token: string;
    backend: string;
    aliases: string[];
    credentialKind: string;
    defaultPort: number;
    creation: 'profile' | 'managed' | 'runtime';
    frontend: boolean;
    defaultOptions: Record<string, unknown>;
    requiredOptions: string[];
    nativeProviderId?: string;
    defaultHost?: string;
    defaultUsername?: string;
    devOnly?: boolean;
}

interface ConnectionPreset {
    id: string;
    protocol: string;
    generic: boolean;
    defaults: NonNullable<ProviderConfig['defaults']>;
    firstRegion: string | null;
}

export const connectionManifest = data as unknown as {
    version: number;
    protocols: ConnectionProtocol[];
    presets: ConnectionPreset[];
    aliases: Record<string, { protocol: string; providerId?: string; options?: Record<string, unknown> }>;
    presetAliases: Record<string, string>;
};

const normalize = (value: string): string => value.trim().toLowerCase().replace(/[ _-]/g, '');

export const connectionProtocol = (token: string): ConnectionProtocol | undefined => {
    const alias = connectionManifest.aliases[token.trim().toLowerCase()];
    const key = normalize(alias?.protocol ?? token);
    return connectionManifest.protocols.find(row => normalize(row.token) === key || row.aliases.some(a => normalize(a) === key));
};

export const getConnectionPreset = (id: string): ConnectionPreset | undefined =>
    connectionManifest.presets.find(p => p.id === (connectionManifest.presetAliases[id] ?? id));

/** Defaults used by both the catalog badge and the provider picker. */
export const presetConnectionOptions = (id: string): Record<string, unknown> => {
    const preset = getConnectionPreset(id);
    const { server: _server, port: _port, basePath: _basePath, endpointTemplate: _template, ...options } = preset?.defaults ?? {};
    const protocol = preset ? connectionProtocol(preset.protocol) : undefined;
    const savedOptions = Object.fromEntries(Object.entries(options).map(([key, value]) => [key === 'tls_mode' ? 'tlsMode' : key, value]));
    return { ...protocol?.defaultOptions, ...savedOptions };
};

export const connectionProfileDefaults = (token: string, providerId?: string) => {
    const protocol = connectionProtocol(token);
    const candidate = providerId ? getConnectionPreset(providerId) : undefined;
    const preset = candidate?.protocol === protocol?.token ? candidate : undefined;
    return {
        server: preset?.defaults.server ?? protocol?.defaultHost ?? '',
        port: preset?.defaults.port ?? protocol?.defaultPort ?? 21,
        username: protocol?.defaultUsername ?? '',
        providerId: providerId ?? protocol?.nativeProviderId,
        options: { ...protocol?.defaultOptions, ...(preset ? presetConnectionOptions(preset.id) : {}) },
    };
};
