// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { decisionsFor, decisionsPayload, defaultDecisions, type ProfileChange, type ProfilePreview } from './keystoreImportPreview';

const change = (id: string, kind: ProfileChange['kind'], defaultDecision: ProfileChange['defaultDecision']): ProfileChange => ({
    id, kind, defaultDecision,
    localName: kind === 'added' ? null : `${id} here`,
    backupName: kind === 'removed' ? null : `${id} backup`,
    protocol: 'sftp', host: null, fields: [], credentialsDiffer: false,
});

const preview: ProfilePreview = {
    source: 'partition',
    localSource: 'partition',
    replacesList: true,
    unchanged: 2,
    fingerprint: 'f'.repeat(64),
    changes: [change('srv_a', 'added', 'accept'), change('srv_b', 'changed', 'accept'), change('srv_c', 'removed', 'accept')],
};

describe('keystore import preview decisions', () => {
    it('offers "keep both" only for a changed profile', () => {
        expect(decisionsFor('changed')).toContain('both');
        expect(decisionsFor('added')).not.toContain('both');
        expect(decisionsFor('removed')).not.toContain('both');
    });

    it('starts every change at the import default', () => {
        expect(defaultDecisions(preview)).toEqual({ srv_a: 'accept', srv_b: 'accept', srv_c: 'accept' });
    });

    it('sends one decision per change, with a copy name only for "keep both"', () => {
        const payload = decisionsPayload(preview, { srv_a: 'reject', srv_b: 'both' }, c => `${c.backupName} (copy)`);
        expect(payload).toEqual([
            { id: 'srv_a', decision: 'reject' },
            { id: 'srv_b', decision: 'both', copyName: 'srv_b backup (copy)' },
            { id: 'srv_c', decision: 'accept' },
        ]);
    });

    it('falls back to the default for a choice that does not fit the change', () => {
        const payload = decisionsPayload(preview, { srv_a: 'both' }, () => 'unused');
        expect(payload[0]).toEqual({ id: 'srv_a', decision: 'accept' });
    });
});
