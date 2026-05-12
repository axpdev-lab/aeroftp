// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import type { PanelEndpoint, PanelPairKind } from '../types/aerofile';
import { endpointsAreSame, getPanelPairKind } from './panelEndpoints';

export type UnifiedTransferMode =
    | 'copy'
    | 'move'
    | 'sync'
    | 'compare'
    | 'mirror'
    | 'backup'
    | 'bisync';

export type UnifiedTransferEngine =
    | 'local-fs'
    | 'local-delta'
    | 'provider-upload'
    | 'provider-download'
    | 'aerosync'
    | 'cross-profile-transfer'
    | 'cross-profile-sync'
    | 'aerovault-overlay'
    | 'compare-only'
    | 'unsupported';

export interface UnifiedTransferPlanOptions {
    mode: UnifiedTransferMode;
    source: PanelEndpoint;
    destination: PanelEndpoint;
    entryCount?: number;
    totalBytes?: number;
    preferDelta?: boolean;
    recursive?: boolean;
}

export interface UnifiedTransferPlan {
    id: string;
    pairKind: PanelPairKind;
    mode: UnifiedTransferMode;
    source: PanelEndpoint;
    destination: PanelEndpoint;
    engine: UnifiedTransferEngine;
    canExecute: boolean;
    destructive: boolean;
    requiresPreview: boolean;
    maxParallel: number;
    entryCount: number;
    totalBytes: number;
    warnings: string[];
}

const isDestructiveMode = (mode: UnifiedTransferMode): boolean =>
    mode === 'move' || mode === 'mirror' || mode === 'bisync';

const isSyncStyleMode = (mode: UnifiedTransferMode): boolean =>
    mode === 'sync' || mode === 'mirror' || mode === 'backup' || mode === 'bisync';

const isOverlayPair = (pairKind: PanelPairKind): boolean => pairKind.includes('overlay');

export const selectUnifiedTransferEngine = (
    pairKind: PanelPairKind,
    mode: UnifiedTransferMode,
    source: PanelEndpoint,
    destination: PanelEndpoint,
    preferDelta = true,
): UnifiedTransferEngine => {
    if (mode === 'compare') return 'compare-only';
    if (isOverlayPair(pairKind)) return 'aerovault-overlay';

    if (pairKind === 'local-local') {
        return isSyncStyleMode(mode) && preferDelta ? 'local-delta' : 'local-fs';
    }

    if (pairKind === 'local-remote') {
        if (isSyncStyleMode(mode) && preferDelta && destination.kind === 'remote' && destination.protocol === 'sftp') {
            return 'aerosync';
        }
        return 'provider-upload';
    }

    if (pairKind === 'remote-local') {
        if (isSyncStyleMode(mode) && preferDelta && source.kind === 'remote' && source.protocol === 'sftp') {
            return 'aerosync';
        }
        return 'provider-download';
    }

    if (pairKind === 'remote-remote') {
        return isSyncStyleMode(mode) ? 'cross-profile-sync' : 'cross-profile-transfer';
    }

    return 'unsupported';
};

export const createUnifiedTransferPlan = (options: UnifiedTransferPlanOptions): UnifiedTransferPlan => {
    const pairKind = getPanelPairKind(options.source, options.destination);
    const engine = endpointsAreSame(options.source, options.destination)
        ? 'unsupported'
        : selectUnifiedTransferEngine(
            pairKind,
            options.mode,
            options.source,
            options.destination,
            options.preferDelta ?? true,
        );
    const destructive = isDestructiveMode(options.mode);
    const warnings: string[] = [];

    if (engine === 'unsupported') {
        warnings.push('unsupported-endpoint-pair');
    }
    if (destructive) {
        warnings.push('destructive-plan-requires-confirmation');
    }
    if (options.mode === 'mirror') {
        warnings.push('mirror-may-delete-destination-files');
    }
    if (engine === 'aerovault-overlay') {
        warnings.push('aerovault-overlay-busy-lock-required');
    }

    return {
        id: [
            'unified',
            pairKind,
            options.mode,
            Date.now().toString(36),
            Math.random().toString(36).slice(2, 8),
        ].join('-'),
        pairKind,
        mode: options.mode,
        source: options.source,
        destination: options.destination,
        engine,
        canExecute: engine !== 'unsupported',
        destructive,
        requiresPreview: destructive || isSyncStyleMode(options.mode) || options.mode === 'compare',
        maxParallel: pairKind === 'local-local' ? 4 : 1,
        entryCount: options.entryCount ?? 0,
        totalBytes: options.totalBytes ?? 0,
        warnings,
    };
};

export const __TEST_ONLY__ = {
    isDestructiveMode,
    isSyncStyleMode,
    isOverlayPair,
};
