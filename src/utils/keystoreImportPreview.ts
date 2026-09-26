// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Types and pure helpers for the keystore import preview (#347). The shapes
 * mirror `keystore_profile_plan.rs`; the backend owns the diff, this file only
 * turns the user's choices into the `profileDecisions` argument of
 * `import_keystore`.
 */

export type ProfileChangeKind = 'added' | 'removed' | 'changed';
export type ProfileDecision = 'accept' | 'reject' | 'both';

export interface ProfileFieldChange {
    field: string;
    local: string | null;
    backup: string | null;
    hidden: boolean;
}

export interface ProfileChange {
    id: string;
    kind: ProfileChangeKind;
    localName: string | null;
    backupName: string | null;
    protocol: string | null;
    host: string | null;
    fields: ProfileFieldChange[];
    credentialsDiffer: boolean;
    defaultDecision: ProfileDecision;
}

export interface ProfilePreview {
    source: 'partition' | 'vault' | 'none';
    /** 'vault' when this device's partition could not be read. */
    localSource: 'partition' | 'vault' | 'none';
    replacesList: boolean;
    unchanged: number;
    changes: ProfileChange[];
    /** Sent back with the decisions; the import refuses them if it differs. */
    fingerprint: string;
}

export interface ProfileDecisionInput {
    id: string;
    decision: ProfileDecision;
    copyName?: string;
}

/** The choices that mean something for each kind of change. */
export function decisionsFor(kind: ProfileChangeKind): ProfileDecision[] {
    return kind === 'changed' ? ['accept', 'reject', 'both'] : ['accept', 'reject'];
}

/** Every change starts at what the import would do without a decision. */
export function defaultDecisions(preview: ProfilePreview): Record<string, ProfileDecision> {
    return Object.fromEntries(preview.changes.map(c => [c.id, c.defaultDecision]));
}

/**
 * The `profileDecisions` argument for `import_keystore`: one entry per change,
 * with the translated name for a "keep both" copy. A decision that does not
 * apply to its kind falls back to the default, as the backend does.
 */
export function decisionsPayload(
    preview: ProfilePreview,
    chosen: Record<string, ProfileDecision>,
    copyName: (change: ProfileChange) => string,
): ProfileDecisionInput[] {
    return preview.changes.map(change => {
        const picked = chosen[change.id];
        const decision = picked && decisionsFor(change.kind).includes(picked) ? picked : change.defaultDecision;
        return decision === 'both'
            ? { id: change.id, decision, copyName: copyName(change) }
            : { id: change.id, decision };
    });
}
