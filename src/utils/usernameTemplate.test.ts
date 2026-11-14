// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { expandUsernameInPath } from './usernameTemplate';
import cases from '../shared/username-template-cases.json';
import { profileOverlayScope, resolveUsernameTemplate } from './cryptScope';
import { profileRoot } from '../components/CloudSetupWizard';
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
