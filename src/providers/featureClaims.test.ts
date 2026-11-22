// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { getAllProviders } from './registry';

// A feature flag is a promise the app keeps only through a backend and the
// gate the GUI, CLI and MCP read: `provider_supports_versions`,
// `provider_supports_thumbnails` and `supports_share_links` answer from the
// backend's `supports_*()`, and the WebDAV trash button reads the registry
// flag over the Nextcloud trash backend. On 2026-11-16 the Nextcloud family
// declared versioning the WebDAV backend did not have, TAB.DIGITAL declared a
// trash the GUI never offered, and MEGA declared thumbnails no MEGA backend
// had. This test holds every declared flag to the gate of every backend that
// can serve the preset.

const backends = import.meta.glob(
    ['../../src-tauri/src/providers/*.rs', '../../src-tauri/src/providers/filen/mod.rs'],
    { query: '?raw', import: 'default', eager: true },
) as Record<string, string>;

const rawOf = (glob: Record<string, string>): string => Object.values(glob)[0] ?? '';
const app = rawOf(import.meta.glob('../App.tsx', { query: '?raw', import: 'default', eager: true }) as Record<string, string>);
const dialog = rawOf(import.meta.glob('../components/ProvidersDialog.tsx', {
    query: '?raw',
    import: 'default',
    eager: true,
}) as Record<string, string>);

const sourceOf = (file: string): string | undefined =>
    Object.entries(backends).find(([path]) => path.endsWith(`/providers/${file}`))?.[1];

/** Whether `file` overrides the gate `fn <gate>(&self) -> bool` with anything but `false`. */
const opensGate = (file: string, gate: string): boolean => {
    const source = sourceOf(file);
    // The lookahead holds the whitespace itself: `{\s*(?!false)` would
    // backtrack into the whitespace and read a `false` on its own line as open.
    return !!source && new RegExp(`fn ${gate}\\(&self\\) -> bool \\{(?!\\s*false\\s*\\})`).test(source);
};

/**
 * The backend sources that serve a registry protocol. Every one must keep a
 * flag the preset declares: one backend keeping it would let a preset promise
 * what its default backend lacks (MEGA opens the native API by default).
 */
const BACKENDS_BY_PROTOCOL: Record<string, string[]> = {
    s3: ['s3.rs'],
    webdav: ['webdav.rs'],
    mega: ['mega.rs', 'mega_native.rs'],
    filelu: ['filelu.rs'],
    uploadcare: ['uploadcare.rs'],
    cloudinary: ['cloudinary.rs'],
    imagekit: ['imagekit.rs'],
};

/** How each flag is kept. `sync` runs on the generic transfer operations every backend has. */
const FLAG_KEPT_BY: Record<string, ((file: string) => boolean) | 'generic'> = {
    sync: 'generic',
    shareLink: file => opensGate(file, 'supports_share_links'),
    thumbnails: file => opensGate(file, 'supports_thumbnails'),
    versioning: file => opensGate(file, 'supports_versions'),
    // The trash has no trait gate: the backend method, and the GUI button that
    // reads this flag.
    trash: file => file === 'webdav.rs'
        && !!sourceOf(file)?.includes('pub async fn nextcloud_list_trash(')
        && app.includes('getProviderById(pid)?.features?.trash'),
};

/** The backend of a Help > Providers row: by name where one logo has two backends. */
const BACKEND_BY_ROW: Record<string, string> = {
    'MEGA (MEGAcmd)': 'mega.rs',
    'MEGA (API)': 'mega_native.rs',
    googledrive: 'google_drive.rs',
    dropbox: 'dropbox.rs',
    onedrive: 'onedrive.rs',
    box: 'box_provider.rs',
    zohoworkdrive: 'zoho_workdrive.rs',
    pcloud: 'pcloud.rs',
    filen: 'filen/mod.rs',
    kdrive: 'kdrive.rs',
    koofr: 'koofr.rs',
    drime: 'drime_cloud.rs',
    filelu: 'filelu.rs',
    jottacloud: 'jottacloud.rs',
    'amazon-s3': 's3.rs',
    backblaze: 'b2.rs',
    nextcloud: 'webdav.rs',
    felicloud: 'webdav.rs',
    murena: 'webdav.rs',
    tabdigital: 'webdav.rs',
};

const dialogRows = (): { name: string; logoId: string; versioning: boolean }[] =>
    [...dialog.matchAll(/\{ name: '([^']+)', logoId: '([^']+)'[^\n]*\n([\s\S]*?)advanced:/g)]
        .map(([, name, logoId, body]) => ({ name, logoId, versioning: body.includes("'versioning'") }));

describe('feature claims follow the backend gate', () => {
    it('reads the sources it checks', () => {
        expect(opensGate('dropbox.rs', 'supports_versions')).toBe(true);
        expect(opensGate('sftp.rs', 'supports_versions')).toBe(false);
        // A gate that answers `false` on its own line is shut.
        expect(opensGate('pcloud.rs', 'supports_versions')).toBe(false);
        expect(app.length).toBeGreaterThan(0);
        expect(dialogRows().filter(row => row.versioning).length).toBeGreaterThan(5);
    });

    it('every flag a registry preset declares is kept by each of its backends', () => {
        for (const provider of getAllProviders()) {
            for (const [flag, declared] of Object.entries(provider.features ?? {})) {
                if (!declared) continue;
                const kept = FLAG_KEPT_BY[flag];
                expect(kept, `${provider.id}: say how the flag ${flag} is kept`).toBeDefined();
                if (kept === 'generic') continue;
                const files = BACKENDS_BY_PROTOCOL[provider.protocol];
                expect(files, `${provider.id}: map protocol ${provider.protocol} to its backends`).toBeDefined();
                for (const file of files) {
                    expect(kept(file), `${provider.id} declares ${flag}, ${file} does not keep it`).toBe(true);
                }
            }
        }
    });

    // A Nextcloud with the versions app off answers 404 on /remote.php/dav/versions/:
    // measured on Felicloud and Murena 2026-11-16, so neither preset nor row claims versions.
    it.each(['felicloud', 'murena'])('%s claims no versions', (id) => {
        expect(getAllProviders().find(p => p.id === id)?.features?.versioning).toBe(false);
        expect(dialogRows().find(row => row.logoId === id)?.versioning).toBe(false);
    });

    // A declared `versioning: false` is kept only if the backend gate shuts for
    // the id: on a Nextcloud file root the WebDAV gate opens unless the id is in
    // VERSIONS_OFF_PRESETS, and the GUI file menu, the CLI and MCP read that gate.
    it('every Nextcloud preset without versions is one the WebDAV gate shuts for', () => {
        const webdav = sourceOf('webdav.rs') ?? '';
        const list = /const VERSIONS_OFF_PRESETS: &\[&str\] = &\[([^\]]*)\];/.exec(webdav)?.[1];
        expect(list, 'webdav.rs: VERSIONS_OFF_PRESETS').toBeDefined();
        const shut = new Set([...(list ?? '').matchAll(/"([^"]+)"/g)].map(([, id]) => id));
        const offPresets = getAllProviders()
            .filter(p => p.protocol === 'webdav' && p.endpoints?.webdavPath?.includes('/remote.php/dav/files/'))
            .filter(p => p.features?.versioning === false)
            .map(p => p.id);
        expect(offPresets.length).toBeGreaterThan(1);
        for (const id of offPresets) expect(shut.has(id), `${id}: declares no versions, add it to VERSIONS_OFF_PRESETS`).toBe(true);
        for (const id of shut) {
            const preset = getAllProviders().find(p => p.id === id);
            if (preset) expect(preset.features?.versioning, `${id}: shut in webdav.rs, declare versioning: false`).toBe(false);
        }
    });

    it('a Help > Providers row lists versioning only on a backend whose gate opens', () => {
        for (const row of dialogRows().filter(r => r.versioning)) {
            const file = BACKEND_BY_ROW[row.name] ?? BACKEND_BY_ROW[row.logoId];
            expect(file, `${row.name}: map the row to its backend`).toBeDefined();
            expect(opensGate(file, 'supports_versions'), `${row.name} lists versioning, ${file} has none`).toBe(true);
        }
    });
});
