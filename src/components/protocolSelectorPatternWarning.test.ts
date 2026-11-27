// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import source from './ProtocolSelector.tsx?raw';
import { withoutComments } from '../utils/jsxTag';

// No DOM here (environment: node), so the wiring is pinned on the source, the
// same way DiscoverableTargetField.test.ts pins its stale-reply guard: the
// accountId field of an S3 preset (Cloudflare R2) carries a pattern, and the
// form must render the shared fieldPatternIssue warning for it, one place,
// with the same amber alert line ConnectionScreen uses for username/password.
describe('ProtocolSelector accountId pattern warning', () => {
    const code = withoutComments(source);

    it('computes the issue through the shared fieldPatternIssue helper', () => {
        expect(code).toContain("fieldPatternIssue(accountIdField, options.accountId ? String(options.accountId) : undefined)");
    });

    it('marks the input invalid and shows the alert with the message key', () => {
        expect(code).toContain('aria-invalid={!!accountIdPatternIssue}');
        expect(code).toContain('role="alert"');
        expect(code).toContain('{t(accountIdPatternIssue)}');
        expect(code).toContain("border-amber-500");
    });
});
