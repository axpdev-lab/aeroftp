// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { isProtonNative } from './protonNative';

export type ProtonMode = 'native' | 'cli';

/** One saved path a switch moves (`to`), or keeps for lack of an equivalent (`to: null`). */
export interface ProtonPathChange {
    kind: 'initialPath' | 'aerocloud' | 'aerocloudPair' | 'syncHistory' | 'pendingSync';
    label: string;
    from: string;
    to: string | null;
}

/** What `proton_mode_switch_plan` / `_apply` answer (src-tauri/src/proton_mode_switch.rs). */
export interface ProtonSwitchPlan {
    profileId: string;
    from: ProtonMode;
    to: ProtonMode;
    changes: ProtonPathChange[];
    legacyHistories: number;
    blockers: string[];
}

export type ProtonSwitchOutcome =
    /** Not a switch: save as usual. */
    | { kind: 'none' }
    /** Switched; the profile's initial folder to save. */
    | { kind: 'switched'; initialPath: string; plan: ProtonSwitchPlan }
    /** Refused, cancelled or failed: save nothing. */
    | { kind: 'stop'; error?: string; blockers?: string[] };

export const protonModeOf = (options?: { proton_mode?: string | null } | null): ProtonMode =>
    (isProtonNative(options) ? 'native' : 'cli');

/**
 * The editor's save of a Proton Drive profile whose tab changed client
 * (Requested 79). The plan is asked and shown first; only a confirmed switch
 * writes, through `proton_mode_switch_apply`, which moves every path saved
 * for the profile and is all or nothing. The caller writes nothing before
 * this returns, and nothing at all unless it returns `switched` or `none`.
 *
 * The initial folder in the form is the stored one unless the user retyped
 * it; only then is it kept as typed, otherwise the translated one is saved.
 */
export async function runProtonModeSwitch(args: {
    invoke: <T>(cmd: string, payload: Record<string, unknown>) => Promise<T>;
    confirm: (plan: ProtonSwitchPlan) => Promise<boolean>;
    profileId: string;
    storedOptions?: { proton_mode?: string | null } | null;
    storedInitialPath?: string | null;
    formOptions?: { proton_mode?: string | null } | null;
    formInitialPath: string;
}): Promise<ProtonSwitchOutcome> {
    const from = protonModeOf(args.storedOptions);
    const to = protonModeOf(args.formOptions);
    if (from === to) return { kind: 'none' };
    let plan: ProtonSwitchPlan;
    try {
        plan = await args.invoke<ProtonSwitchPlan>('proton_mode_switch_plan', { profileId: args.profileId, mode: to });
    } catch (err) {
        return { kind: 'stop', error: String(err) };
    }
    if (plan.blockers.length > 0) return { kind: 'stop', blockers: plan.blockers };
    if (!(await args.confirm(plan))) return { kind: 'stop' };
    let applied: ProtonSwitchPlan;
    try {
        applied = await args.invoke<ProtonSwitchPlan>('proton_mode_switch_apply', { profileId: args.profileId, mode: to });
    } catch (err) {
        return { kind: 'stop', error: String(err) };
    }
    const initial = applied.changes.find((change) => change.kind === 'initialPath');
    const untouched = args.formInitialPath === (args.storedInitialPath ?? '');
    const initialPath = untouched && initial?.to != null ? initial.to : args.formInitialPath;
    return { kind: 'switched', initialPath, plan: applied };
}
