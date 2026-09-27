// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { formChoiceAction } from './CryptSecretFormChoice';

describe('formChoiceAction', () => {
    it('asks first on a bound profile, where the choice changes the key', () => {
        expect(formChoiceAction('clear', 'obscured', true)).toBe('confirm');
        expect(formChoiceAction(undefined, 'clear', true)).toBe('confirm');
    });

    it('applies at once on a new overlay, and ignores the current choice', () => {
        expect(formChoiceAction('clear', 'obscured', false)).toBe('apply');
        expect(formChoiceAction('obscured', 'obscured', true)).toBe('none');
    });
});
