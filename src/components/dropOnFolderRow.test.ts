// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';

/**
 * A drop on the other panel writes into the folder it was dropped on.
 *
 * The drag hook hands the drop its folder: the folder row under the pointer,
 * or the folder the panel shows when the drop lands on its empty space. The
 * drop ignored it, so files dropped on a folder row went into the folder the
 * other panel shows, and remote files dropped on panel 2 went into panel 1's
 * folder. A folder the panel does not list has to be listed for the
 * overwrite questions, and a listing that fails stops the transfer: it never
 * passes for an empty folder (#1049).
 */
describe('a drop on the other panel writes into the folder it was dropped on', () => {
    /** The body of a component-level `const name = ...`, up to the next one. */
    const body = (name: string): string => {
        const start = appRaw.indexOf(`\n  const ${name} = `);
        expect(start, `${name} exists in App.tsx`).toBeGreaterThan(-1);
        return appRaw.slice(start, appRaw.indexOf('\n  const ', start + 1));
    };
    const count = (text: string, re: RegExp) => [...text.matchAll(re)].length;
    const upload = body('uploadMultipleFiles');
    const download = body('downloadMultipleFiles');
    const drop = (() => {
        const start = appRaw.indexOf('crossPanelDropRef.current = async (');
        expect(start).toBeGreaterThan(-1);
        return appRaw.slice(start, appRaw.indexOf('\n  });', start));
    })();

    it('hands the folder dropped on to the upload and to the download', () => {
        expect(drop).toMatch(/^crossPanelDropRef\.current = async \(files, fromRemote, targetDir, sourceKey\) => \{/);
        expect(drop).toMatch(/await downloadMultipleFiles\(files\.map\(f => f\.name\), targetDir \|\| undefined\);/);
        expect(drop).toMatch(/await uploadMultipleFiles\(keys, panelId, targetDir \|\| undefined\);/);
    });

    it('uploads into that folder, a retried file too', () => {
        expect(upload).toMatch(/async \(filesOverride\?: string\[\], localPanelId: 'local' \| 'local2' = focusedLocalPanelId, remoteDir: string = currentRemotePath\) => \{/);
        // The panel's folder is the default and decides whether the panel's
        // listing will do; nothing else in the upload reads it. A retry read
        // it when it ran, after the panel had moved on.
        expect(count(upload, /\bcurrentRemotePath\b/g)).toBe(2);
        expect(upload).toMatch(/remoteDir === currentRemotePath \? undefined : await getOverwriteDestination\('upload', remoteDir\)/);
        const uploads = [...upload.matchAll(/\buploadFile\(([^;]*?)\);/g)].map(m => m[1]);
        expect(uploads).toHaveLength(5);
        for (const args of uploads) expect(args, args).toMatch(/, remoteDir$/);
        expect(upload).toMatch(/runFileBatch\('upload', pickedBatch, remoteDir\)/);
        expect(upload).toMatch(/runFileBatch\(\s*'upload',[\s\S]{0,400}?\)\),\s*remoteDir,\s*\);\s*if \(outcome\.aborted\)/);
    });

    it('downloads into that folder and reloads the panels that show it', () => {
        expect(download).toMatch(/async \(filesOverride\?: string\[\], localDir: string = currentLocalPath\) => \{/);
        expect(count(download, /\bcurrentLocalPath\b/g)).toBe(2);
        expect(download).toMatch(/localDir === currentLocalPath \? undefined : await getOverwriteDestination\('download', localDir\)/);
        expect(download).toMatch(/const frozenLocalPath = localDir;/);
        expect(download).toMatch(/await downloadFile\(item\.file\.path, item\.file\.name, localDir, /);
        expect(download).toMatch(/runFileBatch\(\s*'download',[\s\S]{0,300}?\)\),\s*localDir,\s*\);\s*if \(outcome\.aborted\) return;/);
        // Panel 2 when it shows the folder: panel 1 was reloaded whichever it was.
        expect(download).not.toMatch(/loadLocalFiles\(/);
        expect(count(download, /await refreshLocalPanelForPath\(localDir\);/g)).toBe(2);
    });

    it('asks about what the folder written to holds, not what the panel lists', () => {
        const check = body('checkFolderOverwrite');
        expect(check).toMatch(/const destination = destinationFiles \?\? \(direction === 'upload' \? remoteFiles : localFiles\);/);
        expect(check).toMatch(/const exists = destination\.some\(f => f\.name === folderName && f\.is_dir\);/);
        // Each question of both transfers, a file's and a folder's.
        for (const [name, source, questions] of [['upload', upload, 3], ['download', download, 2]] as const) {
            const asked = [...source.matchAll(/\bcheck(?:Folder)?Overwrite\(([^;]*?)\);/g)].map(m => m[1]);
            expect(asked, name).toHaveLength(questions);
            for (const args of asked) expect(args, `${name}: ${args}`).toMatch(/remainingInQueue,\s*destinationFiles,?\s*$/);
        }
    });

    it('stops when that folder cannot be listed', () => {
        // Before the queue holds a row or a writer runs, in each lane that
        // asks: the upload's selected rows and its file dialog, the download.
        for (const [name, source, failed, before, lanes] of [
            ['upload', upload, 'uploadFailed', /const (stagedUploadMode|stagedDialogMode) = /, 2],
            ['download', download, 'downloadFailed', /const frozenLocalPath = /, 1],
        ] as const) {
            const guards = [...source.matchAll(/let destinationFiles: LocalFile\[\] \| RemoteFile\[\] \| undefined;\s*try \{\s*destinationFiles = await destinationListing\(\);\s*\} catch \(error\) \{\s*notify\.error\(t\('toast\.(\w+)'\), String\(error\)\);\s*return;\s*\}\s*([^\n]*)/g)];
            expect(guards, name).toHaveLength(lanes);
            for (const guard of guards) {
                expect(guard[1]).toBe(failed);
                expect(guard[2]).toMatch(before);
            }
            expect(source, name).not.toMatch(/destinationListing\(\)\.catch\(/);
        }
    });
});
