// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, type Dispatch, type SetStateAction, type MutableRefObject } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { AeroSyncContext, AeroSyncTab } from './types';

export interface AeroSyncState {
    initialTab: AeroSyncTab;
    context: AeroSyncContext;
}

/** App-owned context mutations, shared with the dialog integration tests. */
export function useAeroSyncContextActions(
    state: AeroSyncState | null,
    setState: Dispatch<SetStateAction<AeroSyncState | null>>,
    compareSequence: MutableRefObject<number>,
) {
    const context = state?.context;
    const dismissJournal = useCallback(async () => {
        if (!context) return;
        await invoke('delete_sync_journal_cmd', {
            localPath: context.initialSource || '',
            remotePath: context.initialDestination || '',
        }).catch(() => undefined);
        setState(previous => previous?.context === context
            ? { ...previous, context: { ...previous.context, pendingJournal: null } }
            : previous);
    }, [context, setState]);

    const invalidateComparison = useCallback(() => {
        if (!context) return;
        // A scan already in flight must not republish the pre-restore result.
        compareSequence.current += 1;
        setState(previous => {
            if (!previous || previous.context.pairKind !== context.pairKind
                || previous.context.activeProfileId !== context.activeProfileId
                || previous.context.initialSource !== context.initialSource
                || previous.context.initialDestination !== context.initialDestination) return previous;
            return { ...previous, context: { ...previous.context,
                compareResult: null, compareLoading: false, compareIdle: true,
                compareStopped: false, scanProgressId: undefined,
            } };
        });
    }, [context, setState, compareSequence]);

    return { dismissJournal, invalidateComparison };
}
