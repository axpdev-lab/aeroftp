// SPDX-License-Identifier: GPL-3.0-or-later
import { describe, expect, it } from 'vitest';
import { versionDownloadName } from './versionDownloadName';

const WINDOWS_REFUSED = /[<>:"/\\|?*\u0000-\u001f]|\.$/;

describe('versionDownloadName', () => {
    it('makes a MEGAcmd handle and a Nextcloud id safe in the name', () => {
        for (const [id, name] of [
            ['H:vAAAAAA2', 'report_vH_vAAAAA.pdf'],
            ['42:1700000000', 'report_v42_17000.pdf'],
        ]) {
            const offered = versionDownloadName('report.pdf', id);
            expect(offered).toBe(name);
            expect(offered).not.toMatch(WINDOWS_REFUSED);
        }
    });

    it('keeps a plain id as it is and cuts only the last extension', () => {
        expect(versionDownloadName('a.txt.bak.txt', '3.0')).toBe('a.txt.bak_v3.0.txt');
        expect(versionDownloadName('notes', 'r12')).toBe('notes_vr12');
        expect(versionDownloadName('.bashrc', 'abc')).toBe('.bashrc_vabc');
    });

    it('drops a trailing dot Windows would strip', () => {
        expect(versionDownloadName('notes', '1.')).toBe('notes_v1');
    });
});

const dialog = Object.values(
    import.meta.glob('../components/FileVersionsDialog.tsx', { query: '?raw', import: 'default', eager: true }) as Record<string, string>,
)[0] ?? '';

describe('the versions dialog', () => {
    it('offers a download under the safe name, with the id unchanged in the request', () => {
        expect(dialog).toMatch(/pickSave\(\{ defaultPath: versionDownloadName\(fileName, version\.id\) \}\)/);
        expect(dialog).toMatch(/invoke\('provider_download_version', \{\s*path: filePath,\s*versionId: version\.id,/);
        expect(dialog).not.toMatch(/version\.id\.slice\(/);
    });
});
