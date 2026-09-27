// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { cryptSecretForms, hydratedSecretForms, secretFormsHalfRecorded } from './cryptSecretForm';

describe('cryptSecretForms', () => {
    it('reads the forms the binding records', () => {
        expect(cryptSecretForms({
            aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt', passwordForm: 'obscured', saltForm: 'clear' },
        })).toEqual({ password: 'obscured', salt: 'clear' });
    });

    it('shows a binding that records no form as not recorded', () => {
        expect(cryptSecretForms({
            aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt' },
        })).toEqual({ password: undefined, salt: undefined });
        expect(cryptSecretForms(null)).toEqual({ password: undefined, salt: undefined });
    });
});

describe('hydratedSecretForms', () => {
    it('starts a profile without a binding as typed for both', () => {
        expect(hydratedSecretForms(null)).toEqual({ password: 'clear', salt: 'clear' });
        expect(hydratedSecretForms({ aeroCryptOverlay: undefined })).toEqual({ password: 'clear', salt: 'clear' });
        expect(hydratedSecretForms({
            aeroCryptOverlay: { enabled: false, kind: 'rclone-crypt', passwordForm: 'obscured' },
        })).toEqual({ password: 'clear', salt: 'clear' });
    });

    it('shows what a bound overlay records, nothing included', () => {
        expect(hydratedSecretForms({
            aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt', passwordForm: 'obscured' },
        })).toEqual({ password: 'obscured', salt: undefined });
        expect(hydratedSecretForms({
            aeroCryptOverlay: { enabled: true, kind: 'rclone-crypt' },
        })).toEqual({ password: undefined, salt: undefined });
    });
});

describe('secretFormsHalfRecorded', () => {
    it('is true only when exactly one of the two forms is recorded', () => {
        expect(secretFormsHalfRecorded({ password: 'clear', salt: undefined })).toBe(true);
        expect(secretFormsHalfRecorded({ password: undefined, salt: 'obscured' })).toBe(true);
        expect(secretFormsHalfRecorded({ password: undefined, salt: undefined })).toBe(false);
        expect(secretFormsHalfRecorded({ password: 'clear', salt: 'obscured' })).toBe(false);
    });
});
