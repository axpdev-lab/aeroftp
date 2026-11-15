// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import source from './ConnectionScreen.tsx?raw';
import introHubSource from './IntroHub/IntroHub.tsx?raw';
import appSource from '../App.tsx?raw';
import oauthHookSource from '../hooks/useOAuth2.ts?raw';

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

    it('hands every sign-in that swapped the backend to App or reports it', () => {
        // OAuthConnect and FourSharedConnect swap the backend before their
        // onConnected runs: each goes through afterBackendSwap, whose finally
        // reports an exit that did not hand the connect over.
        const handlers = source.match(/onConnected=\{/g) ?? [];
        const wrapped = source.match(/onConnected=\{afterBackendSwap\(/g) ?? [];
        expect(handlers.length).toBeGreaterThanOrEqual(2);
        expect(wrapped).toHaveLength(handlers.length);
        expect(bodyOf('afterBackendSwap')).toContain("window.dispatchEvent(new CustomEvent('aeroftp-sign-in-abandoned', { detail: { token } }))");
        // The report names the swap it is about, taken before the handler runs.
        expect(bodyOf('afterBackendSwap')).toMatch(/const token = latestSettledBackendSwapToken\(\);\s+let handedOff = false;/);
        // Neither sign-in can start while the form would refuse its connect.
        expect(source.match(/disabled=\{remotePathEscapesOverlay \|\| overlaySaveBlocked\}\n\s+(provider=|rightColumn=)/g) ?? []).toHaveLength(2);
    });

    it('puts the live tab aside before every sign-in swaps the backend', () => {
        // The form's two swaps tell App first, in the same task (swapBackend),
        // so the transfer queue cannot start on the live tab over the new
        // account while the form decides.
        const swaps = (text: string, command: string) => [...text.matchAll(new RegExp(`invoke(<[^(]*>)?\\('${command}'`, 'g'))]
            .map(m => text.slice(text.lastIndexOf('\n', m.index!), m.index!));
        expect(swaps(source, 'fourshared_connect')).toEqual([expect.stringMatching(/swapBackend\(\(\) => $/)]);
        expect(swaps(oauthHookSource, 'oauth2_connect')).toEqual([expect.stringMatching(/swapBackend\(\(\) => $/)]);
        // App is told first, before anything is awaited, and the invoke runs
        // only in the swap's turn.
        expect(oauthHookSource).toMatch(/const token = \+\+lastBackendSwapToken;[\s\S]{0,400}?\n  tell\('start'\);\n  const previous = swapQueueTail;/);
        expect(oauthHookSource).toMatch(/ran = true;\n\s+return run\(\);/);
        // The resume of a restored transfer connects a saved OAuth or 4shared
        // profile itself: it parks first too.
        const resume = appSource.slice(appSource.indexOf('  const connectSavedProfileForResume = async ('));
        for (const [before, command] of [['let oauthResult:', "invoke('oauth2_connect'"], ["invoke<boolean>('fourshared_has_tokens'", "invoke('fourshared_connect'"]]) {
            const from = resume.indexOf(before);
            expect(from, before).toBeGreaterThan(-1);
            const untilCommand = resume.slice(from, resume.indexOf(command, from));
            expect(untilCommand, command).toContain('backendUncertainRef.current = true;\n        takeOverActiveSession();');
            // Counted as a replacement in flight until it returns, in a finally.
            expect(untilCommand, command).toMatch(/const end\w+Replacement = replacingBackend\(\);\n\s+try \{\n\s+[^\n]*$/);
        }
        // Every connect, switch and swap invoke goes through cancellableConnect,
        // which counts it the same way.
        const central = appSource.slice(appSource.indexOf('  const cancellableConnect = useCallback('));
        expect(central.slice(0, central.indexOf('  }, [beginConnectAttempt'))).toMatch(/const endReplacement = replacingBackend\(\);[\s\S]*\} finally \{[\s\S]*endReplacement\(\);\n\s+\}\n$/);
    });

    it('gates every path that writes overlay fields on the one overlay predicate', () => {
        // The predicate holds every overlay state a save must not persist,
        // the rclone-crypt form recorded for one secret only among them.
        expect(source).toMatch(/const overlaySaveBlocked = [^;]*\bcryptFormsHalfRecorded\b/);
        expect(source).toMatch(/const overlaySaveBlocked = [^;]*\boverlaysRemotePathError\b/);
        expect(source).toMatch(/const overlaySaveBlocked = [^;]*\baeroCryptConfirmMismatch\b/);
        expect(source).toMatch(/const overlaySaveBlocked = [^;]*\bdefaultSaltEntropyMismatch\b/);
        // Every call that builds the overlay fields of a profile to write sits
        // in a handler that returned on the predicate first: the nearest
        // handler start above the call must hold the check before the call.
        const definition = source.indexOf('const aeroCryptOverlayFields = ');
        const starts = [...source.matchAll(/const handle\w+ = async|const saveToServers = async|onConnected=\{(?:afterBackendSwap\()?async/g)].map(m => m.index!);
        const calls = [...source.matchAll(/aeroCryptOverlayFields\(/g)].map(m => m.index!).filter(i => i > definition + 40);
        expect(calls.length).toBeGreaterThanOrEqual(8);
        for (const call of calls) {
            const start = Math.max(...starts.filter(s => s < call));
            const line = source.slice(0, call).split('\n').length;
            expect(source.slice(start, call), `overlay fields built at line ${line}`).toMatch(/overlaySaveBlocked\) return;/);
        }
        // And every save button says so before a click: Save as new and the
        // mode conversion (both footers), and the main Save.
        expect(source.match(/disabled=\{loading \|\| remotePathEscapesOverlay \|\| overlaySaveBlocked\}/g)).toHaveLength(2);
        expect(source).toContain('<button onClick={handleSaveAsNew} disabled={remotePathEscapesOverlay || overlaySaveBlocked}');
        expect(source).toMatch(/disabled=\{saveOverride \? [^\n]*\boverlaySaveBlocked\b/);
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
        expect(save, 'a read-modify-write uses the strict read, bound to its account').toContain('await readSavedServerProfilesForWrite()');
        expect(save, 'no swallowed write').not.toMatch(/storeSavedServerProfiles\([^)]*\)\.catch\(\s*\(\)\s*=>\s*\{\s*\}\s*\)/);
        expect(save).toMatch(/if \(!prevProfile\) \{[^}]*setGitHubAlert\(/);
        // Credential rollback adds a nested catch to the write failure path.
        // Runtime rejection coverage lives in profileWriteErrors.wiring.test.
        expect(save).toContain("logger.warn('Saved profiles could not be read for the OAuth edit Save', err)");
        expect(save).toContain("logger.warn('Saved profiles could not be written for the OAuth edit Save', err)");
        expect(save).toContain('await credentials.rollback()');
        // The success activity and the editor close come only after the write.
        const write = save.indexOf('await storeSavedServerProfiles(updated, false, profilesUserId, profilesRevision)');
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

    it('tells the IntroHub tab when its edit session ends, and Cancel Editing closes the tab', () => {
        // A tab whose session had ended stayed open, and Edit on the same card
        // switched back to it: a blank Quick Connect form, or on 4.2.2 a remount
        // from the profile captured when the tab first opened (the bucket saved
        // since then showed its old value, and a Save wrote it back).
        expect(bodyOf('endEditSession')).toContain('onEditSessionEnd?.()');
        const cancels = source.match(/<button\s+onClick=\{(\w+)\}[^>]*?title=\{t\('connection\.cancelEditing'\)\}/g) ?? [];
        const handlers = [...new Set(cancels.map((c) => c.match(/onClick=\{(\w+)\}/)![1]))];
        expect(cancels.length, 'both Cancel Editing buttons are found').toBe(2);
        expect(handlers).toEqual(['handleCancelEditAndClose']);
        const cancelAndClose = bodyOf('handleCancelEditAndClose');
        expect(cancelAndClose).toContain('handleCancelEdit()');
        expect(cancelAndClose).toContain('onFormSaved?.()');

        // IntroHub reuses only a tab still editing the profile.
        expect(introHubSource).toMatch(/const existing = findOpenEditTab\(formTabs, profile\.id\);/);
        expect(introHubSource).toMatch(/onEditSessionEnd=\{\(\) => setFormTabs\(prev => markEditSessionEnded\(prev, formTab\.id\)\)\}/);
    });
});
