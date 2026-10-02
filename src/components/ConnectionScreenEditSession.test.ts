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

    it('compares the OAuth edit Save against profiles loaded on mount', () => {
        // The list oauthEditHasChanges searched was filled only when an export
        // dialog opened, and IntroHub never opens it from here, so the list
        // stayed empty, the edited profile was never found and Save never
        // enabled. The list must come from a loader that runs unconditionally
        // and again whenever the saved servers change.
        const list = bodyOf('oauthEditHasChanges').match(/const ep = (\w+)\.find\(/)?.[1];
        expect(list, 'oauthEditHasChanges looks the profile up in a list').toBeTruthy();
        const setter = `set${list![0].toUpperCase()}${list!.slice(1)}(`;
        const effects = source.match(/useEffect\(\(\) => \{[\s\S]*?\n {4}\}, \[[^\]]*\]\);/g) ?? [];
        const loaders = effects.filter((e) => e.includes(setter));
        expect(loaders, `an effect calls ${setter}`).not.toHaveLength(0);
        for (const loader of loaders) {
            expect(loader, 'the loader is not gated on a flag').not.toMatch(/^\s*if \(![\w.]+\) return;/m);
            expect(loader, 'the loader reruns on serversRefreshKey').toMatch(/\[[^\]]*\bserversRefreshKey\b[^\]]*\]\);$/);
            // A rejected vault read was an unhandled rejection that left the list empty.
            expect(loader, 'the loader catches a failed read').toMatch(/\bcatch\s*\(/);
        }
        // And Save does not hinge on that read: without the list, the profile
        // the edit was opened with stands in.
        expect(bodyOf('oauthEditHasChanges')).toMatch(/\?\?\s*\(editingProfile\?\.id === editingProfileId \? editingProfile : undefined\)/);
    });

    it('reports a failed OAuth edit Save instead of closing the editor as saved', () => {
        // The OAuth edit Save only became reachable when it started enabling.
        // It swallowed a failed profile write, logged "Profile updated" and
        // closed the editor, and a profile it could not find (or a vault it
        // could not read, which the plain read answers with []) returned
        // without a word.
        const save = bodyOf('handleOAuthMetadataSave');
        expect(save, 'a read-modify-write uses the strict read').toContain('await loadSavedServerProfilesStrict()');
        expect(save, 'no swallowed write').not.toMatch(/storeSavedServerProfiles\([^)]*\)\.catch\(\s*\(\)\s*=>\s*\{\s*\}\s*\)/);
        expect(save).toMatch(/if \(!prevProfile\) \{[^}]*setGitHubAlert\(/);
        const failures = save.match(/\} catch \(\w+\) \{[^}]*setGitHubAlert\(\{[^}]*type: 'error'[^}]*\}\);\s*return;/g) ?? [];
        expect(failures, 'the read and the write each alert and stop').toHaveLength(2);
        // The success activity and the editor close come only after the write.
        const write = save.indexOf('await storeSavedServerProfiles(updated)');
        expect(write).toBeGreaterThan(-1);
        for (const after of ["'PROFILE_SAVE'", 'onFormSaved()']) {
            expect(save.indexOf(after), after).toBeGreaterThan(write);
        }
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
