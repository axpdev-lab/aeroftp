// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type {
    AeroFileLocalPanelId,
    PanelCapabilities,
    PanelEndpoint,
    PanelPairKind,
} from '../types/aerofile';
import type { ProviderType, ServerProfile } from '../types';

const DEFAULT_LOCAL_CAPABILITIES: PanelCapabilities = {
    canUpload: false,
    canDownload: true,
    canMove: true,
    canDelete: true,
    canServerSideCopy: false,
    canDeltaSync: true,
    canOpenTerminalHere: true,
};

const DEFAULT_REMOTE_CAPABILITIES: PanelCapabilities = {
    canUpload: true,
    canDownload: true,
    canMove: true,
    canDelete: true,
    canServerSideCopy: false,
    canDeltaSync: false,
    canOpenTerminalHere: false,
};

const DEFAULT_OVERLAY_CAPABILITIES: PanelCapabilities = {
    canUpload: true,
    canDownload: true,
    canMove: true,
    canDelete: true,
    canServerSideCopy: true,
    canDeltaSync: false,
    canOpenTerminalHere: false,
};

const SERVER_SIDE_COPY_PROTOCOLS = new Set<ProviderType>([
    's3',
    'azure',
    'github',
    'gitlab',
    'googledrive',
    'dropbox',
    'onedrive',
    'box',
    'pcloud',
    'kdrive',
    'koofr',
]);

export const normalizePanelPath = (path: string | undefined | null, fallback = '/'): string => {
    const trimmed = (path ?? '').trim();
    if (!trimmed) return fallback;
    return trimmed;
};

export const createLocalEndpoint = (
    panelId: AeroFileLocalPanelId,
    path: string,
    tabId?: string | null,
): PanelEndpoint => ({
    kind: 'local',
    panelId,
    path: normalizePanelPath(path, ''),
    tabId,
});

export const createRemoteEndpoint = (
    profile: Pick<ServerProfile, 'id' | 'name' | 'protocol' | 'providerId' | 'initialPath'>,
    path?: string,
    tabId?: string | null,
): PanelEndpoint => ({
    kind: 'remote',
    profileId: profile.id,
    profileName: profile.name,
    protocol: profile.protocol ?? 'ftp',
    providerId: profile.providerId,
    path: normalizePanelPath(path, normalizePanelPath(profile.initialPath, '/')),
    tabId,
});

export const getEndpointCapabilities = (endpoint: PanelEndpoint): PanelCapabilities => {
    if (endpoint.kind === 'local') return { ...DEFAULT_LOCAL_CAPABILITIES };
    if (endpoint.kind === 'aerovaultOverlay') return { ...DEFAULT_OVERLAY_CAPABILITIES };

    return {
        ...DEFAULT_REMOTE_CAPABILITIES,
        canServerSideCopy: SERVER_SIDE_COPY_PROTOCOLS.has(endpoint.protocol),
        canDeltaSync: endpoint.protocol === 'sftp',
    };
};

export const getEndpointLabel = (endpoint: PanelEndpoint): string => {
    if (endpoint.kind === 'local') return endpoint.path || '~';
    if (endpoint.kind === 'aerovaultOverlay') return endpoint.vaultPath;
    return endpoint.profileName || endpoint.profileId;
};

export const getEndpointKey = (endpoint: PanelEndpoint): string => {
    if (endpoint.kind === 'local') return `local:${endpoint.panelId}:${endpoint.path}`;
    if (endpoint.kind === 'aerovaultOverlay') return `overlay:${endpoint.sessionId}:${endpoint.path}`;
    return `remote:${endpoint.profileId}:${endpoint.path}`;
};

export const getPanelPairKind = (source: PanelEndpoint, destination: PanelEndpoint): PanelPairKind => {
    if (source.kind === 'aerovaultOverlay' && destination.kind === 'aerovaultOverlay') return 'overlay-overlay';
    if (source.kind === 'aerovaultOverlay' && destination.kind === 'local') return 'overlay-local';
    if (source.kind === 'local' && destination.kind === 'aerovaultOverlay') return 'local-overlay';
    if (source.kind === 'aerovaultOverlay' && destination.kind === 'remote') return 'overlay-remote';
    if (source.kind === 'remote' && destination.kind === 'aerovaultOverlay') return 'remote-overlay';
    return `${source.kind}-${destination.kind}` as PanelPairKind;
};

export const endpointsAreSame = (left: PanelEndpoint, right: PanelEndpoint): boolean =>
    getEndpointKey(left) === getEndpointKey(right);

export const __TEST_ONLY__ = {
    SERVER_SIDE_COPY_PROTOCOLS,
};
