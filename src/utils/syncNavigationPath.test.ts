// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
import { describe, expect, it, vi } from 'vitest';
import { mkdtempSync, mkdirSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { canonicalSyncSuffixEqual, isWithinSyncFolder, locationProblem, relativeSyncSegments, savedLocationProblem, resolveSyncNavigationPath as resolvePath, type SyncNavigationEntry } from './syncNavigationPath';
import locationCases from '../shared/path-location-cases.json';
import syncNavigationSource from './syncNavigationPath.ts?raw';
import overlayScopeSource from './overlayScope.ts?raw';
import liveConnectionSource from './liveConnection.ts?raw';
import wizardSource from '../components/CloudSetupWizard.tsx?raw';

const nfd = 'εσωτερικο\u0301ς';
const nfc = nfd.normalize('NFC');
const directory = (name: string) => ({ name, is_dir: true });

// Ordinary fixtures have one selected directory in each source parent.
const resolveSyncNavigationPath = (
    base: string, segments: readonly string[],
    list: (path: string) => Promise<readonly SyncNavigationEntry[]>,
    isCurrent: () => boolean = () => true,
) => resolvePath(base, segments, list, {
    base: '/source',
    listEntries: async path => [directory(segments[path.split('/').filter(Boolean).length - 1])],
}, isCurrent);

it('bounds relative suffixes by complete components and preserves literal names', () => {
    expect(relativeSyncSegments('/base-other/sub', '/base')).toBeNull();
    expect(relativeSyncSegments('/base/../escape', '/base')).toBeNull();
    // A `..` before a backslash is a parent step on a Windows panel the
    // segments are joined to: refused, as every location (`locationProblem`).
    expect(relativeSyncSegments('/base/..\\kept', '/base')).toBeNull();
    expect(relativeSyncSegments('/base/🇯🇵/dir\\name', '/base/')).toEqual(['🇯🇵', 'dir\\name']);
    expect(relativeSyncSegments('C:/sub', 'C:/')).toEqual(['sub']);
    expect(relativeSyncSegments('C:/', 'C:/')).toEqual([]);
});

it('keeps root navigation absolute without listing or rewriting it', async () => {
    const list = vi.fn();
    expect(await resolveSyncNavigationPath('C:/', [], list)).toEqual({ kind: 'found', path: 'C:/' });
    expect(await resolveSyncNavigationPath('/', [], list)).toEqual({ kind: 'found', path: '/' });
    expect(list).not.toHaveBeenCalled();
});

it('uses each destination component spelling through nested mixed-normalization parents', async () => {
    const list = vi.fn(async (path: string) => {
        if (path === '/remote') return [directory('🇯🇵')];
        if (path === '/remote/🇯🇵') return [directory(nfc)];
        if (path === `/remote/🇯🇵/${nfc}`) return [directory('cafe\u0301')];
        throw new Error(`Unexpected listing ${path}`);
    });
    expect(await resolveSyncNavigationPath('/remote', ['🇯🇵', nfd, 'café'], list)).toEqual({ kind: 'found', path: `/remote/🇯🇵/${nfc}/cafe\u0301` });
    expect(list.mock.calls.map(c => c[0])).toEqual(['/remote', '/remote/🇯🇵', `/remote/🇯🇵/${nfc}`]);
});

it('prefers an exact counterpart even when a canonical twin also exists', async () => {
    const list = async () => [directory(nfc), directory(nfd)];
    expect(await resolveSyncNavigationPath('/remote', [nfd], list)).toEqual({ kind: 'found', path: `/remote/${nfd}` });
});

it('refuses ambiguous canonical candidates and never visits their descendants', async () => {
    // Three canonically equivalent spellings of a Latin letter with two marks.
    const requested = 'a\u0323\u0301';
    const alternatives = ['ạ\u0301', 'á\u0323'];
    expect(alternatives.every(s => s.normalize('NFC') === requested.normalize('NFC'))).toBe(true);
    const list = vi.fn(async () => alternatives.map(directory));
    expect(await resolveSyncNavigationPath('/remote', [requested, 'child'], list)).toEqual({
        kind: 'conflict', path: `/remote/${requested}`, candidates: alternatives.map(s => `/remote/${s}`),
    });
    expect(list).toHaveBeenCalledTimes(1);
});

it('refuses a fallback that would reuse the exact counterpart of another source twin', async () => {
    expect((await resolvePath('/remote', [nfd], async () => [directory(nfc)], {
        base: '/source', listEntries: async () => [directory(nfd), directory(nfc)],
    })).kind).toBe('conflict');
});

it('validates the whole missing suffix before any listing or create proposal', async () => {
    const list = vi.fn(async () => []);
    expect((await resolveSyncNavigationPath('/remote', ['new', '..', 'escape'], list)).kind).toBe('conflict');
    expect(list).not.toHaveBeenCalled();
});

it('refuses exact files and canonical file/directory conflicts', async () => {
    expect((await resolveSyncNavigationPath('/remote', [nfd], async () => [{ name: nfd, is_dir: false }, directory(nfc)])).kind).toBe('conflict');
    expect((await resolveSyncNavigationPath('/remote', [nfd], async () => [{ name: nfc, is_dir: false }])).kind).toBe('conflict');
});

it('projects a genuinely missing suffix below the actual existing destination parent', async () => {
    const list = vi.fn(async (path: string) => path === '/remote' ? [directory(nfc)] : []);
    expect(await resolveSyncNavigationPath('/remote', [nfd, 'new', 'child'], list)).toEqual({ kind: 'missing', path: `/remote/${nfc}/new/child` });
    expect(list).toHaveBeenCalledTimes(2);
});

it('propagates listing errors instead of treating them as an empty directory', async () => {
    await expect(resolveSyncNavigationPath('/remote', [nfd], async () => { throw new Error('Permission denied'); })).rejects.toThrow('Permission denied');
});

it('discards cancellation before and after each listing', async () => {
    const list = vi.fn(async () => [directory(nfc)]);
    expect(await resolveSyncNavigationPath('/remote', [nfd], list, () => false)).toEqual({ kind: 'cancelled' });
    expect(list).not.toHaveBeenCalled();
    let current = true;
    expect(await resolveSyncNavigationPath('/remote', [nfd, 'child'], async () => { current = false; return [directory(nfc)]; }, () => current)).toEqual({ kind: 'cancelled' });
});

it('does not use compatibility normalization or fold a literal backslash', async () => {
    expect((await resolveSyncNavigationPath('/remote', ['Ａ'], async () => [directory('A')])).kind).toBe('missing');
    expect(await resolveSyncNavigationPath('/remote', ['dir\\name'], async () => [directory('dir\\name')])).toEqual({ kind: 'found', path: '/remote/dir\\name' });
    expect(canonicalSyncSuffixEqual(nfd, nfc)).toBe(true);
    expect(canonicalSyncSuffixEqual('dir\\name', 'dir/name')).toBe(false);
});

describe.skipIf(process.platform !== 'linux')('real destination directory inventories', () => {
    it('resolves both canonical directions without changing names or bytes', async () => {
        const root = mkdtempSync(join(tmpdir(), 'aeroftp-unicode-nav-'));
        try {
            const left = join(root, 'left');
            const right = join(root, 'right');
            mkdirSync(left); mkdirSync(right);
            mkdirSync(join(left, nfd)); mkdirSync(join(right, nfc));
            writeFileSync(join(left, nfd, 'same.txt'), 'same bytes');
            writeFileSync(join(right, nfc, 'same.txt'), 'same bytes');
            const list = async (path: string) => readdirSync(path, { withFileTypes: true }).map(e => ({ name: e.name, is_dir: e.isDirectory() }));
            expect(await resolvePath(right, [nfd], list, { base: left, listEntries: list })).toEqual({ kind: 'found', path: `${right}/${nfc}` });
            expect(await resolvePath(left, [nfc], list, { base: right, listEntries: list })).toEqual({ kind: 'found', path: `${left}/${nfd}` });
            expect(readdirSync(left)).toEqual([nfd]);
            expect(readdirSync(right)).toEqual([nfc]);
            expect(readFileSync(join(left, nfd, 'same.txt'), 'utf8')).toBe('same bytes');
            expect(readFileSync(join(right, nfc, 'same.txt'), 'utf8')).toBe('same bytes');
        } finally { rmSync(root, { recursive: true }); }
    });
});

describe('isWithinSyncFolder', () => {
    it('reads a folder and what is under it at a component boundary', () => {
        expect(isWithinSyncFolder('/docs', '/docs')).toBe(true);
        expect(isWithinSyncFolder('/docs/a', '/docs')).toBe(true);
        expect(isWithinSyncFolder('/docs/a', '/docs/')).toBe(true);
        expect(isWithinSyncFolder('/docs2', '/docs')).toBe(false);
        expect(isWithinSyncFolder('/docs2/a', '/docs')).toBe(false);
        expect(isWithinSyncFolder('C:\\Users\\u\\AeroCloud\\a', 'C:\\Users\\u\\AeroCloud')).toBe(true);
        expect(isWithinSyncFolder('C:\\Users\\u\\AeroCloud2', 'C:\\Users\\u\\AeroCloud')).toBe(false);
        expect(isWithinSyncFolder('/a', '/')).toBe(true);
        expect(isWithinSyncFolder('/a', '')).toBe(false);
        expect(isWithinSyncFolder('', '/docs')).toBe(false);
        expect(isWithinSyncFolder('\\\\nas\\share\\sync\\a', '\\\\nas\\share\\sync')).toBe(true);
    });

    it('reads a path that is not plain as under no folder', () => {
        expect(isWithinSyncFolder('/docs/../private/a.txt', '/docs')).toBe(false);
        expect(isWithinSyncFolder('/docs/./a.txt', '/docs')).toBe(false);
        expect(isWithinSyncFolder('/docs//a.txt', '/docs')).toBe(false);
        expect(isWithinSyncFolder('/private/a.txt', '/docs/../private')).toBe(false);
        expect(isWithinSyncFolder('C:\\sync\\..\\other', 'C:\\sync')).toBe(false);
    });
});

describe('locationProblem', () => {
    // The cases the backend's `location_problem` passes too
    // (`util/path_location.rs`): both sides refuse the same paths.
    it('refuses the paths of the shared table for the same reason', () => {
        expect(locationCases.cases.length).toBeGreaterThanOrEqual(20);
        for (const { path, problem } of locationCases.cases) {
            expect(locationProblem(path), JSON.stringify(path)).toBe(problem);
        }
    });

    it('refuses only a parent step or a NUL in a saved location, as the backend does', () => {
        for (const { path, savedProblem } of locationCases.cases) {
            expect(savedLocationProblem(path), JSON.stringify(path)).toBe(savedProblem);
        }
    });

    it('takes no relative steps from a path that is not plain', () => {
        expect(relativeSyncSegments('/docs/a/../../private', '/docs')).toBeNull();
        expect(relativeSyncSegments('/docs//a', '/docs')).toBeNull();
        expect(relativeSyncSegments('/private/a', '/docs/../private')).toBeNull();
        expect(relativeSyncSegments('/docs/a/', '/docs/')).toEqual(['a']);
    });
});

describe('every location site checks the path first', () => {
    // Each function of these files that strips, slices or compares a path by
    // its prefix calls `locationProblem` before it, or is listed with the
    // reason it needs no check of its own. A new one fails here until it
    // does one or the other; the backend twin is
    // `every_location_site_checks_the_path_first` (util/path_location.rs).
    const EXEMPT: Record<string, string> = {
        rootPath: 'a helper of relativeSyncSegments, which checks first',
        resolveSyncNavigationPath: 'takes the segments relativeSyncSegments checked',
        isLoginRelativeScope: 'tells the `~` form apart, strips nothing',
        loginRelativeForm: 'prefixes `~/`; every caller here checks the text first',
        normalizeLoginRelative: 'a normalizer the checked functions call; keeps `..` as written',
        hostOf: 'a host name, not a path',
        profileRoot: 'the starting folder the server reads; aeroCloudBase checks the text it comes from',
        aeroCloudRemoteFolder: 'joined only after aeroCloudFolderNotPlain refused (handleComplete)',
        CloudSetupWizard: 'the label it shows, not a location it uses',
        mayReuseForAeroCloud: 'tells an absolute folder apart, strips nothing; openAeroCloudFolder checks the folder it opens',
    };
    it('checks each path before it strips or compares it', () => {
        const checked: string[] = [];
        for (const source of [syncNavigationSource, overlayScopeSource, liveConnectionSource, wizardSource]) {
            for (const part of source.split(/\n(?=export (?:async )?function |export const |function |const [a-zA-Z]+ = \()/u)) {
                const name = /^(?:export )?(?:async )?(?:function |const )([A-Za-z0-9_]+)/u.exec(part)?.[1];
                if (!name || name === 'locationProblem') continue;
                const first = part.search(/\.startsWith\(|\.slice\(|\.replace\(\/\^/u);
                if (first < 0 || name in EXEMPT) continue;
                const check = part.indexOf('locationProblem(');
                expect(check >= 0 && check < first, `${name} strips or compares a path before it checks it`).toBe(true);
                checked.push(name);
            }
        }
        // `aeroCloudFolderNotPlain` is no site: it checks the sub-folder as
        // written and strips nothing (CloudSetupWizard.render.test.tsx).
        expect(checked).toEqual(expect.arrayContaining(['relativeSyncSegments', 'isWithinSyncFolder', 'isValidOverlayScope', 'resolveOverlayScope']));
        // The exempt names still exist: a rename cannot hide a new site.
        const all = syncNavigationSource + overlayScopeSource + liveConnectionSource + wizardSource;
        for (const name of Object.keys(EXEMPT)) expect(all, name).toMatch(new RegExp(`(function|const) ${name}\\b`, 'u'));
        // The wizard refuses before it builds the folder it saves, and the
        // AeroCloud tab before it lists the saved one.
        const complete = wizardSource.slice(wizardSource.indexOf('const handleComplete = async'));
        expect(complete.indexOf('aeroCloudFolderNotPlain(profile, subfolder)')).toBeLessThan(complete.indexOf('aeroCloudRemoteFolder('));
        const open = liveConnectionSource.slice(liveConnectionSource.indexOf('export async function openAeroCloudFolder'));
        expect(open.indexOf('savedLocationProblem(remoteFolder)')).toBeLessThan(open.indexOf('await place('));
    });
});
