import { describe, expect, it } from 'vitest';
import { stripLegacyStartFolder } from './legacyStartFolder';

describe('stripLegacyStartFolder', () => {
    const jianguoyun = 'https://dav.jianguoyun.com/dav';

    it('reads the old Jianguoyun basePath as the root', () => {
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', 'a@b.c', jianguoyun)).toBeNull();
        expect(stripLegacyStartFolder('/dav', 'jianguoyun', 'a@b.c', `${jianguoyun}/`)).toBeNull();
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', '', 'dav.jianguoyun.com/dav')).toBeNull();
    });

    it('keeps a Jianguoyun folder the user chose', () => {
        expect(stripLegacyStartFolder('/dav/projects', 'jianguoyun', 'a@b.c', jianguoyun)).toBe('/dav/projects');
        expect(stripLegacyStartFolder('/projects/', 'jianguoyun', 'a@b.c', jianguoyun)).toBe('/projects/');
    });

    it('keeps /dav when the server URL does not already end with it', () => {
        // The server root then is the origin: /dav is a real folder below it.
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', 'a@b.c', 'https://dav.jianguoyun.com')).toBe('/dav/');
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', 'a@b.c', 'https://dav.jianguoyun.com/webdav')).toBe('/dav/');
        // A /dav in the query or fragment is not the server path.
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', 'a@b.c', 'https://dav.jianguoyun.com?next=/dav')).toBe('/dav/');
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', 'a@b.c', 'https://dav.jianguoyun.com#/dav')).toBe('/dav/');
        expect(stripLegacyStartFolder('/dav/', 'jianguoyun', 'a@b.c', 'HTTPS://dav.jianguoyun.com/dav?x=1')).toBeNull();
    });

    it('strips the old basePath for its own preset only', () => {
        expect(stripLegacyStartFolder('/dav/', 'webdav', 'a@b.c', 'https://dav.example.com/dav')).toBe('/dav/');
        expect(stripLegacyStartFolder('/dav/', 'infinicloud', 'a@b.c', 'https://x.teracloud.jp/dav')).toBe('/dav/');
    });

    it('reads a hosted Nextcloud WebDAV root as the root', () => {
        for (const id of ['tabdigital', 'tabdigital-webdav', 'felicloud', 'felicloud-webdav', 'murena', 'nextcloud']) {
            expect(stripLegacyStartFolder('/remote.php/dav/files/alice/', id, 'alice', null), id).toBeNull();
        }
        // An origin-only host (no path in the server URL) reads the same.
        expect(stripLegacyStartFolder('/remote.php/dav/files/alice/', 'nextcloud', 'alice', 'https://cloud.example.test')).toBeNull();
        expect(stripLegacyStartFolder('/remote.php/dav/files/alice/Docs/', 'murena', 'alice', null))
            .toBe('/remote.php/dav/files/alice/Docs/');
        expect(stripLegacyStartFolder('/remote.php/dav/files/alice', 'webdav', 'alice', null))
            .toBe('/remote.php/dav/files/alice');
        expect(stripLegacyStartFolder('/remote.php/dav/files/alice', 'murena', '', null))
            .toBe('/remote.php/dav/files/alice');
        expect(stripLegacyStartFolder('/remote.php/dav/files/alice', 'murena', '   ', null))
            .toBe('/remote.php/dav/files/alice');
        // A padded username is not trimmed into another account's root.
        expect(stripLegacyStartFolder('/remote.php/dav/files/alice/', 'murena', ' alice ', null))
            .toBe('/remote.php/dav/files/alice/');
        expect(stripLegacyStartFolder('/remote.php/dav/files/ alice /', 'murena', ' alice ', null)).toBeNull();
    });

    it('passes an empty start folder through', () => {
        expect(stripLegacyStartFolder(undefined, 'jianguoyun', 'a', jianguoyun)).toBeNull();
        expect(stripLegacyStartFolder('', 'jianguoyun', 'a', jianguoyun)).toBe('');
    });
});
