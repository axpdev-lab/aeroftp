// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { readdirSync, readFileSync, statSync } from 'node:fs';
import { join } from 'node:path';
import { expandUsernameInPath } from './usernameTemplate';
import cases from '../shared/username-template-cases.json';
import { resolveUsernameTemplate } from './cryptScope';
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
