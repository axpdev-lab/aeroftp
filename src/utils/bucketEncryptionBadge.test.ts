// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { bucketEncryptionBadge } from './bucketEncryptionBadge';

describe('bucketEncryptionBadge', () => {
    it('names the type and the strength when the bucket encrypts by default', () => {
        const badge = bucketEncryptionBadge({
            state: 'on',
            mode: 'SSE-B2',
            algorithm: 'AES256',
            source: 'api',
            fetched_at: '2026-09-13T00:00:00Z',
        });
        expect(badge.label).toBe('SSE-B2 256');
        expect(badge.titleKey).toBe('introHub.bucketEncryption.onTitle');
    });

    it('names the type alone when B2 named no algorithm', () => {
        // A strength nobody told us is not a strength to display: the lock may
        // still appear, the invented "256" may not.
        const badge = bucketEncryptionBadge({
            state: 'on',
            mode: 'SSE-B2',
            source: 'api',
            fetched_at: '2026-09-13T00:00:00Z',
        });
        expect(badge.label).toBe('SSE-B2');
    });

    it('draws no lock when there is no default encryption, and no crossed lock either', () => {
        // SSE-C and client-side encryption are not reported by this field, so
        // "no default" is not a claim that the data is in the clear.
        const badge = bucketEncryptionBadge({
            state: 'off',
            source: 'api',
            fetched_at: '2026-09-13T00:00:00Z',
        });
        expect(badge.label).toBeNull();
        expect(badge.titleKey).toBe('introHub.bucketEncryption.offTitle');
    });

    it('reads an unreadable setting as unknown and never as a No', () => {
        // The frequent case: keys restricted to one bucket are the norm. This
        // must never render like the "off" case above, and the tooltip must
        // name the capability, because here the key really is the reason.
        const badge = bucketEncryptionBadge({
            state: 'unknown',
            reason: 'missing_capability',
            source: 'api',
            fetched_at: '2026-09-13T00:00:00Z',
        });
        expect(badge.label).toBeNull();
        expect(badge.titleKey).toBe('introHub.bucketEncryption.unknownKeyTitle');
        expect(badge.titleKey).not.toBe('introHub.bucketEncryption.offTitle');
    });

    it('blames the payload, not the key, when the key was allowed to look', () => {
        const badge = bucketEncryptionBadge({
            state: 'unknown',
            reason: 'unrecognised_shape',
            source: 'api',
            fetched_at: '2026-09-13T00:00:00Z',
        });
        expect(badge.titleKey).toBe('introHub.bucketEncryption.unknownShapeTitle');
    });

    it('says nothing at all when nothing was ever cached', () => {
        expect(bucketEncryptionBadge(undefined)).toEqual({ label: null, titleKey: null });
    });
});
