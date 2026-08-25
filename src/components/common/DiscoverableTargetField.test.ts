// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { discoveryResetKey } from './DiscoverableTargetField';
import fieldSource from './DiscoverableTargetField.tsx?raw';
import { withoutComments } from '../../utils/jsxTag';

describe('discoveryResetKey', () => {
    it('changes when any credential input changes without returning the input', () => {
        const first = discoveryResetKey('account', 'secret-one', 'https://s3.example');
        const second = discoveryResetKey('account', 'secret-two', 'https://s3.example');
        expect(first).not.toBe(second);
        expect(first).not.toContain('secret-one');
    });

    it('is stable for an unchanged form', () => {
        expect(discoveryResetKey('a', 'b')).toBe(discoveryResetKey('a', 'b'));
    });
});

describe('discovery is guarded against a stale reply', () => {
    it('compares the epoch it started with after the await', () => {
        // A discovery reply can land after the user has changed credentials.
        // Unguarded, it repopulated the list with the previous account's
        // targets and, on a single result, wrote it into the field: the form
        // then held one account's credentials pointing at another's bucket.
        // No DOM here (environment: node), so the guard is pinned on the source.
        const code = withoutComments(fieldSource);
        const start = code.indexOf('const discover = async');
        expect(start).toBeGreaterThan(-1);
        const body = code.slice(start);
        const captured = body.indexOf('const requested = epoch.current');
        const awaited = body.indexOf('await onDiscover()');
        const guarded = body.indexOf('if (requested !== epoch.current) return;');
        expect(captured).toBeGreaterThan(-1);
        expect(awaited).toBeGreaterThan(captured);
        expect(guarded, 'the epoch is never compared after the await').toBeGreaterThan(awaited);
        // The auto-select is the write that matters most, so it must sit behind
        // the guard, not before it.
        expect(body.indexOf('onChange(discovered[0].value)')).toBeGreaterThan(guarded);
    });

    it('bumps the epoch when the credentials change', () => {
        const code = withoutComments(fieldSource);
        expect(code).toContain('epoch.current++');
        // A dropped reply no longer releases the spinner, so the reset must.
        const reset = code.indexOf('epoch.current++');
        const effectEnd = code.indexOf('}, [resetKey]);', reset);
        expect(effectEnd).toBeGreaterThan(reset);
        expect(code.slice(reset, effectEnd)).toContain('setLoading(false)');
    });
});
