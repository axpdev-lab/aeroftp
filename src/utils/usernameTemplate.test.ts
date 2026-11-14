// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { connectStartFolder, expandUsernameInPath, hasUnresolvedUsernameTemplate, USERNAME_TEMPLATE_UNRESOLVED } from './usernameTemplate';
import appSource from '../App.tsx?raw';
import cases from '../shared/username-template-cases.json';
import { profileOverlayScope, resolveUsernameTemplate } from './cryptScope';
import { aeroCloudBase, profileRoot } from '../components/CloudSetupWizard';
import { resolveOverlayScope } from './overlayScope';
import type { ServerProfile } from '../types';

describe('expandUsernameInPath', () => {
    it('fills the template with the user name as written', () => {
        expect(expandUsernameInPath('/remote.php/dav/files/{username}/', 'alice@example.com'))
            .toBe('/remote.php/dav/files/alice@example.com/');
        expect(expandUsernameInPath('/files/{username}/', 'a b')).toBe('/files/a b/');
        expect(expandUsernameInPath('/plain/', '')).toBe('/plain/');
    });

    it('never yields a shorter path for a blank user name or one that could move the folder', () => {
        for (const user of ['', '   ', 'a/b', '/abs', '..', '.', 'a\\b', 'a\0b']) {
            expect(expandUsernameInPath('/files/{username}/', user)).toBe('/files/{username}/');
        }
    });

    it('runs the shared case table: untrimmed, literal split/join, never String.replace', () => {
        // The same table the backend runs (connection_manifest.rs): a refused
        // case (null) keeps the template on this side.
        for (const c of cases.cases) {
            expect(expandUsernameInPath(cases.pathTemplate, c.user), JSON.stringify(c.user))
                .toBe(c.path ?? cases.pathTemplate);
        }
    });

    it('fills the overlay folder from the shared table, as the backend overlay scope does', () => {
        // The backend runs the same table through profile_overlay_scope; a
        // trimmed name there put the encrypted folder somewhere else.
        for (const c of cases.cases) {
            const profile = {
                username: c.user,
                aeroCryptOverlay: { remoteScope: cases.pathTemplate },
            } as unknown as ServerProfile;
            expect(profileOverlayScope(profile), JSON.stringify(c.user)).toBe(c.path ?? cases.pathTemplate);
        }
    });

    it('fills the AeroCloud root from the shared table, as the backend profile_root does', () => {
        for (const c of cases.cases) {
            const filled = profileRoot({ initialPath: cases.pathTemplate, username: c.user } as unknown as ServerProfile);
            const expected = profileRoot({ initialPath: c.path ?? cases.pathTemplate, username: '' } as unknown as ServerProfile);
            expect(filled, JSON.stringify(c.user)).toBe(expected);
        }
    });

    it('no caller passes a trimmed user name to the helper', () => {
        // The helper judges blankness itself; a caller that trims moves the
        // folder away from the one the backend fills.
        const offenders: string[] = [];
        const walk = (dir: string) => {
            for (const name of readdirSync(dir)) {
                const path = join(dir, name);
                if (statSync(path).isDirectory()) { walk(path); continue; }
                if (!/\.(ts|tsx)$/.test(name) || /\.test\.tsx?$/.test(name)) continue;
                for (const m of readFileSync(path, 'utf8').matchAll(/expandUsernameInPath\(([^;]*?)\)/g)) {
                    if (/trim/.test(m[1])) offenders.push(path);
                }
            }
        };
        walk(join(process.cwd(), 'src'));
        expect(offenders).toEqual([]);
    });

    it('marks every location the user name cannot fill as unresolved, for the wizard and the form', () => {
        // The wizard (aeroCloudBase) and the connection form refuse to save or
        // enable such a location; the backend refuses to unlock or sync it.
        expect(USERNAME_TEMPLATE_UNRESOLVED).toBe(cases.unresolvedMessage);
        for (const c of cases.cases) {
            // A user literally named `{username}` fills the template with
            // itself: still unresolved, refused like a blank name.
            const unresolved = c.path === null || c.path.includes('{username}');
            const filled = expandUsernameInPath(cases.pathTemplate, c.user);
            expect(hasUnresolvedUsernameTemplate(filled), JSON.stringify(c.user)).toBe(unresolved);
            const profile = {
                username: c.user, protocol: 'webdav', initialPath: '',
                aeroCryptOverlay: { enabled: true, kind: 'aerocrypt', remoteScope: cases.pathTemplate },
            } as unknown as ServerProfile;
            const base = aeroCloudBase(profile);
            expect(hasUnresolvedUsernameTemplate(base), `${JSON.stringify(c.user)} -> ${base}`).toBe(unresolved);
        }
    });

    it('is what the crypt scope and the AeroCloud root use', () => {
        expect(resolveUsernameTemplate('/files/{username}/docs', 'alice')).toBe('/files/alice/docs');
        expect(resolveUsernameTemplate('/files/{username}/docs', '')).toBe('/files/{username}/docs');
        const profile = { initialPath: '/files/{username}/', username: '' } as unknown as ServerProfile;
        // A blank user name used to give `/files`, one level above the user's folder.
        expect(profileRoot(profile)).toBe('/files/{username}');
    });

    it('is the only place the frontend fills the template', () => {
        const placeholder = ['{', 'username', '}'].join('');
        const forbidden = [
            `split('${placeholder}')`,
            `split("${placeholder}")`,
            `replace('${placeholder}'`,
            `replace("${placeholder}"`,
            `replaceAll('${placeholder}'`,
            `replace(/\\{username\\}/`,
        ];
        const offenders: string[] = [];
        const walk = (dir: string) => {
            for (const name of readdirSync(dir)) {
                const path = join(dir, name);
                if (statSync(path).isDirectory()) { walk(path); continue; }
                if (!/\.(ts|tsx)$/.test(name) || /\.test\.tsx?$/.test(name) || name === 'usernameTemplate.ts') continue;
                readFileSync(path, 'utf8').split('\n').forEach((line, i) => {
                    if (forbidden.some(f => line.includes(f))) offenders.push(`${path}:${i + 1}`);
                });
            }
        };
        walk(join(process.cwd(), 'src'));
        expect(offenders).toEqual([]);
    });
});

describe('the twin readers of one profile', () => {
    // The Rust twins run the same cases
    // (`the_root_and_the_anchor_compose_one_profile_alike`).
    const bare = (p: string) => p.replace(/\/+$/, '');
    type Composition = {
        case: string; initialPath: string; user: string; remoteScope?: string; overlayInputs?: string[];
        root: string | null; scope: string | null;
    };
    const compositions = (cases as unknown as { compositions: Composition[] }).compositions;

    it('compose the root, the anchor and the base alike', () => {
        expect(compositions.length).toBeGreaterThanOrEqual(8);
        for (const c of compositions) {
            const profile = {
                protocol: 'webdav', initialPath: c.initialPath, username: c.user,
                aeroCryptOverlay: { enabled: true, kind: 'aerocrypt', ...(c.remoteScope ? { remoteScope: c.remoteScope } : {}) },
            } as unknown as ServerProfile;
            const root = profileRoot(profile);
            const scope = profileOverlayScope(profile);
            if (c.root === null) expect(hasUnresolvedUsernameTemplate(root), c.case).toBe(true);
            else expect(bare(root), c.case).toBe(bare(c.root));
            if (c.scope === null) {
                expect(hasUnresolvedUsernameTemplate(scope), c.case).toBe(true);
                continue;
            }
            expect(bare(scope), c.case).toBe(bare(c.scope));
            // The base AeroCloud writes in sits inside the anchor the overlay
            // encrypts, or its uploads go up in plaintext.
            const base = bare(aeroCloudBase(profile));
            const anchor = bare(scope);
            expect(base === anchor || base.startsWith(`${anchor}/`), `${c.case}: ${base} outside ${anchor}`).toBe(true);
        }
    });

    it('get the fill from the helper: the folder as written, nothing trimmed before or after', () => {
        // A folder and user names with edge spaces: each case fails if the
        // helper trims the folder or the filled result.
        const templated = compositions.filter(c => c.root !== null && c.initialPath.includes('{username}'));
        expect(templated.some(c => c.initialPath !== c.initialPath.trim())).toBe(true);
        for (const c of templated) {
            expect(bare(expandUsernameInPath(c.initialPath, c.user)), c.case).toBe(bare(c.root!));
        }
    });

    it('save every Overlays Path spelling as the scope the backend reads', () => {
        for (const c of compositions) {
            for (const input of c.overlayInputs ?? []) {
                expect(resolveOverlayScope(input, c.initialPath, 'webdav'), `${c.case}: ${input}`).toBe(c.remoteScope);
            }
        }
    });
});

describe('the start folder of a connect', () => {
    it('is filled, or refused before anything changes', () => {
        expect(connectStartFolder(undefined, 'alice')).toBeNull();
        expect(connectStartFolder('', 'alice')).toBeNull();
        // A folder without the placeholder is used as written.
        expect(connectStartFolder('/reports ', '')).toBe('/reports ');
        for (const c of cases.cases) {
            if (c.path === null) {
                expect(() => connectStartFolder(cases.pathTemplate, c.user), JSON.stringify(c.user)).toThrow(USERNAME_TEMPLATE_UNRESOLVED);
            } else {
                expect(connectStartFolder(cases.pathTemplate, c.user), JSON.stringify(c.user)).toBe(c.path);
            }
        }
    });

    it('is resolved by every connect branch before its first state write or request', () => {
        // App.tsx fills `{username}` through resolveUsernameTemplate only where
        // nothing connects: the storage scan root, the overlay hint, and the
        // error text of a missing folder. A connect branch that fills it any
        // other way fails here.
        const readers = [...appSource.matchAll(/resolveUsernameTemplate\(/g)].map(m => appSource.slice(Math.max(0, m.index! - 60), m.index! + 40));
        const allowed = [/const resolved = resolveUsernameTemplate\(prof\.initialPath/, /const profileInitialPath = resolveUsernameTemplate\(/, /: resolveUsernameTemplate\(landingPath/, /const badPath = resolveUsernameTemplate\(/];
        expect(readers).toHaveLength(allowed.length);
        for (const reader of readers) expect(allowed.some(a => a.test(reader)), reader).toBe(true);

        const calls = [...appSource.matchAll(/connectStartFolder\(/g)].map(m => m.index!).filter(i => !appSource.slice(i - 30, i).includes('import'));
        // Every connect branch declares its kind, in source order: a new one
        // must be added here. An OAuth branch runs after its sign-in component
        // swapped the backend, so it puts the live tab aside before any await,
        // exit or throw (else a write queued for the old tab could reach the
        // new account); the other branches have not swapped yet, so the guard
        // comes before the park.
        // `parked-by-caller`: My Servers puts the live tab aside
        // (onParkActiveSession) before its sign-in swaps the backend, pinned in
        // parkActiveSession.wiring.test.ts; the branch itself must not park again.
        const KINDS = ['oauth', 'direct', 'direct', 'parked-by-caller', 'direct', 'direct'] as const;
        expect(calls).toHaveLength(KINDS.length);
        const code = (text: string) => text.replace(/\/\/[^\n]*/g, '');
        calls.forEach((call, k) => {
            if (KINDS[k] !== 'oauth') return;
            const line = appSource.slice(0, call).split('\n').length;
            const entry = Math.max(appSource.lastIndexOf('const connectToFtpImpl = async', call), appSource.lastIndexOf('onSavedServerConnect={asConnectPhase(async', call));
            const park = appSource.indexOf('parkActiveSession(', entry);
            expect(park > entry && park < call, `line ${line}: the OAuth branch does not park before its guard`).toBe(true);
            const beforePark = code(appSource.slice(appSource.indexOf('=>', entry), park));
            for (const exit of ['await ', 'return', 'throw ', 'invoke(', 'setIsConnected(']) {
                expect(beforePark.includes(exit), `line ${line}: ${exit} before the OAuth park`).toBe(false);
            }
        });
        const branchStart = /if \(isOAuth\) \{|const logId = [^;\n]*humanLog\.logStart\('CONNECT'/g;
        const starts = [...appSource.matchAll(branchStart)].map(m => m.index!);
        for (const [k, call] of calls.entries()) {
            const line = appSource.slice(0, call).split('\n').length;
            const start = Math.max(...starts.filter(s => s < call));
            const before = appSource.slice(start, call);
            const writes = ['setIsConnected(', 'invoke(', 'runConnect(', 'setShowConnectionScreen('];
            for (const write of KINDS[k] === 'oauth' ? writes : ['parkActiveSession(', ...writes]) {
                expect(before.includes(write), `line ${line}: ${write} before the start folder`).toBe(false);
            }
            // The call is the first statement of a try, so its refusal reaches
            // that try's catch: the branch's failure path, which returns.
            const tryAt = appSource.lastIndexOf('try {', call);
            const between = appSource.slice(tryAt + 5, call).replace(/\/\/[^\n]*/g, '').replace(/(const|let)\s+\w+\s*=\s*|\w+\s*=\s*/, '').trim();
            expect(between, `line ${line}: not the first statement of its try`).toBe('');
            // The catch of that very try, found by brace depth: an inner try
            // further down has a catch of its own.
            let depth = 0;
            let close = -1;
            for (let i = tryAt + 4; i < appSource.length; i++) {
                if (appSource[i] === '{') depth++;
                else if (appSource[i] === '}' && --depth === 0) { close = i; break; }
            }
            expect(appSource.slice(close, close + 9), `line ${line}: its try has no catch`).toBe('} catch (');
            const failure = appSource.slice(close, close + 1600);
            expect(/return|reportConnectFailure\(/.test(failure), `line ${line}: the catch neither returns nor reports`).toBe(true);
        }
    });
});
