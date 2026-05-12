// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useMemo } from 'react';
import type { LocalFile, RemoteFile, ServerProfile } from '../types';
import type {
    AeroFileLocalPanelId,
    UnifiedPanelId,
    UnifiedPanelState,
} from '../types/aerofile';
import {
    createLocalEndpoint,
    createRemoteEndpoint,
    getEndpointCapabilities,
    getPanelPairKind,
} from '../utils/panelEndpoints';

export interface LocalPanelControllerInput {
    panelId: AeroFileLocalPanelId;
    path: string;
    activeTabId?: string | null;
    files: LocalFile[];
    selected: Set<string>;
    loading?: boolean;
    error?: string | null;
}

export interface RemotePanelControllerInput {
    profile: Pick<ServerProfile, 'id' | 'name' | 'protocol' | 'providerId' | 'initialPath'> | null;
    path: string;
    activeTabId?: string | null;
    files: RemoteFile[];
    selected: Set<string>;
    loading?: boolean;
    error?: string | null;
}

export interface UseUnifiedPanelControllerOptions {
    isConnected: boolean;
    showRemotePanel: boolean;
    dualLocalPanel: boolean;
    swapPanels: boolean;
    activeFilePanel: 'remote' | 'local';
    activeLocalPanelId: AeroFileLocalPanelId;
    localPanel: LocalPanelControllerInput;
    localPanel2: LocalPanelControllerInput;
    remotePanel: RemotePanelControllerInput;
}

export interface UnifiedPanelController {
    left: UnifiedPanelState | null;
    right: UnifiedPanelState | null;
    active: UnifiedPanelState | null;
    pairKind: ReturnType<typeof getPanelPairKind> | null;
}

export const buildLocalPanelState = (
    id: UnifiedPanelId,
    input: LocalPanelControllerInput,
): UnifiedPanelState<LocalFile> => {
    const endpoint = createLocalEndpoint(input.panelId, input.path, input.activeTabId);
    return {
        id,
        endpoint,
        files: input.files,
        selected: input.selected,
        capabilities: getEndpointCapabilities(endpoint),
        loading: input.loading ?? false,
        error: input.error ?? null,
    };
};

export const buildRemotePanelState = (
    id: UnifiedPanelId,
    input: RemotePanelControllerInput,
): UnifiedPanelState<RemoteFile> | null => {
    if (!input.profile) return null;
    const endpoint = createRemoteEndpoint(input.profile, input.path, input.activeTabId);
    return {
        id,
        endpoint,
        files: input.files,
        selected: input.selected,
        capabilities: getEndpointCapabilities(endpoint),
        loading: input.loading ?? false,
        error: input.error ?? null,
    };
};

export const useUnifiedPanelController = ({
    isConnected,
    showRemotePanel,
    dualLocalPanel,
    swapPanels,
    activeFilePanel,
    activeLocalPanelId,
    localPanel,
    localPanel2,
    remotePanel,
}: UseUnifiedPanelControllerOptions): UnifiedPanelController => useMemo(() => {
    const localLeft = buildLocalPanelState('left', localPanel);

    if (!isConnected || !showRemotePanel) {
        const right = dualLocalPanel ? buildLocalPanelState('right', localPanel2) : null;
        const active = dualLocalPanel && activeLocalPanelId === 'local2' ? right : localLeft;
        return {
            left: localLeft,
            right,
            active,
            pairKind: right ? getPanelPairKind(localLeft.endpoint, right.endpoint) : null,
        };
    }

    const remote = buildRemotePanelState(swapPanels ? 'right' : 'left', remotePanel);
    const local = buildLocalPanelState(swapPanels ? 'left' : 'right', localPanel);
    const left = swapPanels ? local : remote;
    const right = swapPanels ? remote : local;
    const active = activeFilePanel === 'remote' ? remote : local;

    return {
        left,
        right,
        active,
        pairKind: left && right ? getPanelPairKind(left.endpoint, right.endpoint) : null,
    };
}, [
    isConnected,
    showRemotePanel,
    dualLocalPanel,
    swapPanels,
    activeFilePanel,
    activeLocalPanelId,
    localPanel,
    localPanel2,
    remotePanel,
]);
