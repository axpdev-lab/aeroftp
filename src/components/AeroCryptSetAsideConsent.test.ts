// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it, vi } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';

vi.mock('../i18n', () => ({ useTranslation: () => (key: string) => key }));

import { AeroCryptSetAsideConsent, setAsideConsentState } from './AeroCryptSetAsideConsent';

const render = (mode: 'atomic' | 'setsAside' | 'unsupported' | null, revocation: boolean) =>
    renderToStaticMarkup(
        createElement(AeroCryptSetAsideConsent, {
            mode,
            revocation,
            checked: false,
            onChange: () => undefined,
        }),
    );

describe('AeroCrypt set-aside consent (policy C)', () => {
    it('blocks a slot change on a set-aside server until the user consents', () => {
        expect(setAsideConsentState('setsAside', false)).toEqual({
            blocked: true,
            allowNonAtomic: false,
        });
        expect(setAsideConsentState('setsAside', true)).toEqual({
            blocked: false,
            allowNonAtomic: true,
        });
    });

    it('never sends the opt-in where no set-aside happens', () => {
        for (const mode of ['atomic', 'unsupported', null] as const) {
            for (const checked of [false, true]) {
                expect(setAsideConsentState(mode, checked)).toEqual({
                    blocked: false,
                    allowNonAtomic: false,
                });
            }
        }
    });

    it('shows the risks and a consent box only on a set-aside server', () => {
        const html = render('setsAside', false);
        expect(html).toContain('aerocryptNative.keyslotsSetAsideRisks');
        expect(html).toContain('aerocryptNative.keyslotsSetAsideConsent');
        expect(html).toContain('type="checkbox"');
        expect(html).not.toContain('aerocryptNative.keyslotsSetAsideRevocation');
        expect(render('atomic', true)).toBe('');
        expect(render(null, true)).toBe('');
    });

    it('adds the revocation warning to a remove or rotate', () => {
        expect(render('setsAside', true)).toContain('aerocryptNative.keyslotsSetAsideRevocation');
    });

    it('says the change is refused on a server that cannot replace the marker', () => {
        const html = render('unsupported', false);
        expect(html).toContain('aerocryptNative.keyslotsMarkerReplaceUnsupported');
        expect(html).not.toContain('type="checkbox"');
    });
});
