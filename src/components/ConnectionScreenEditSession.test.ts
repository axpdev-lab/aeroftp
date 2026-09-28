// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import source from './ConnectionScreen.tsx?raw';

// ConnectionScreen has no render harness here, so these read its source: the
// properties below are about which code paths exist, not about rendering.

/** The body of `const <name> = (...) => { ... };`, by brace depth. */
function bodyOf(name: string): string {
    const start = source.indexOf(`const ${name} = `);
    expect(start, `${name} is defined`).toBeGreaterThan(-1);
    const open = source.indexOf('{', source.indexOf('=>', start));
    let depth = 0;
    for (let i = open; i < source.length; i++) {
        if (source[i] === '{') depth++;
        else if (source[i] === '}' && --depth === 0) return source.slice(open, i + 1);
    }
    throw new Error(`unbalanced body for ${name}`);
}

describe('ConnectionScreen edit sessions', () => {
    it('ends every edit session through endEditSession, which resets the overlay', () => {
        // A save, a copy, a conversion, a cancel or a protocol change that
        // cleared the edit id on its own left the overlay of the profile just
        // edited (its locked binding, secrets and form choices) in the form,
        // to be recorded for the next profile.
        const clears = source.match(/setEditingProfileId\(null\)/g) ?? [];
        expect(clears).toHaveLength(1);
        expect(bodyOf('endEditSession')).toContain('setEditingProfileId(null)');
        expect(bodyOf('endEditSession')).toContain('resetOverlayForm()');
        const reset = bodyOf('resetOverlayForm');
        for (const setter of ['setAeroCryptPasswordForm', 'setAeroCryptSaltForm', 'setOverlayBindingLocked', 'setAeroCryptPassword']) {
            expect(reset, setter).toContain(`${setter}(`);
        }
    });

    it('blocks every save while exactly one rclone-crypt form is recorded', () => {
        for (const fn of ['saveToServers', 'handleSaveAsNew', 'handleConvertMode']) {
            expect(bodyOf(fn), fn).toContain('if (cryptFormsHalfRecorded) return;');
        }
        // The OAuth edit Save goes through this gate instead of saveToServers.
        expect(source).toMatch(/const oauthOverlaySaveBlocked = [^;]*\bcryptFormsHalfRecorded\b/);
        // And the buttons say so before a click.
        expect(source).toMatch(/disabled=\{saveOverride \? [^\n]*\bcryptFormsHalfRecorded\b/);
        expect(source).toContain('disabled={remotePathEscapesOverlay || cryptFormsHalfRecorded}');
    });

    it('asks before changing a recorded form on a bound profile, for both secrets', () => {
        const choices = source.match(/<CryptSecretFormChoice[\s\S]*?\/>/g) ?? [];
        expect(choices).toHaveLength(2);
        for (const choice of choices) {
            expect(choice).toContain('confirmChange={overlayFieldsLocked}');
            expect(choice).toContain('missing={cryptFormsHalfRecorded}');
        }
    });
});
