// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { useCallback, useRef, type Dispatch, type SetStateAction, type MutableRefObject } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type { AeroSyncContext, AeroSyncTab } from './types';
import { aeroSyncCompareOptions } from '../../utils/aeroSyncExcludes';
import { withoutEntries } from '../../utils/compareEndpoints';

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
                compareStopped: false, compareRefused: undefined, scanProgressId: undefined,
            } };
        });
    }, [context, setState, compareSequence]);

    /**
     * Filter the finished compare with wider settings instead of scanning
     * again (Requested 116). The backend answers with the rule the scan used.
     * Only the latest request counts, and a scan started meanwhile wins over
     * all of them. A refused pattern leaves the compare as it was and is
     * recorded with the settings it refused, for the Plan to show.
     */
    const narrowRequest = useRef(0);
    const narrowComparison = useCallback(async (userExcludes: string[], backupDir: string) => {
        const result = context?.compareResult;
        if (!result) return;
        const request = ++narrowRequest.current;
        const sequence = compareSequence.current;
        const current = () => request === narrowRequest.current && sequence === compareSequence.current;
        try {
            const leftOut = await invoke<boolean[]>('compare_paths_left_out', {
                options: aeroSyncCompareOptions(userExcludes, backupDir),
                paths: result.entries.map((entry) => {
                    const path = entry.relativePath ?? entry.name;
                    return {
                        local_path: entry.leftRelativePath ?? path,
                        remote_path: entry.rightRelativePath ?? path,
                        is_dir: !!(entry.leftIsDir || entry.rightIsDir),
                    };
                }),
            });
            if (!current()) return;
            setState(previous => previous?.context.compareResult === result
                ? { ...previous, context: { ...previous.context,
                    compareResult: withoutEntries(result, leftOut),
                    compareExcludes: userExcludes,
                    compareBackupDir: backupDir,
                    compareNarrowed: true,
                    compareNarrowRefusal: undefined,
                } }
                : previous);
        } catch (error) {
            if (!current()) return;
            setState(previous => previous?.context.compareResult === result
                ? { ...previous, context: { ...previous.context,
                    compareNarrowRefusal: { userExcludes, backupDir, message: String(error) },
                } }
                : previous);
        }
    }, [context, setState, compareSequence]);

    return { dismissJournal, invalidateComparison, narrowComparison };
}
