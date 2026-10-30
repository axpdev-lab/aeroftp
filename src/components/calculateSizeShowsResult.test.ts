// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';

/**
 * Calculate Size did nothing (#1441).
 *
 * The menu entry ran the scan and wrote the result into the folder size cache,
 * and the one reader of that cache is the Properties dialog. With the dialog
 * closed, which is always the case when the entry is clicked, the scan ran and
 * nothing on screen changed. Properties itself counts a folder as it opens, so
 * the entry is gone and Properties is the one way to a folder's size: on a
 * row, and on the panel's empty space and a breadcrumb segment for the
 * directory they name, which had no Properties entry.
 */
describe('a folder\'s size shows where it is asked for (#1441)', () => {
    /** The body of a component-level `const name = ...`, up to the next one. */
    const body = (name: string): string => {
        const start = appRaw.indexOf(`\n  const ${name} = `);
        expect(start, `${name} exists in App.tsx`).toBeGreaterThan(-1);
        const end = appRaw.indexOf('\n  const ', start + 1);
        return appRaw.slice(start, end === -1 ? undefined : end);
    };

    it('starts no size scan from a menu, where no dialog would show it', () => {
        for (const menu of ['directoryActionItems', 'directoryPropertiesItem', 'showLocalContextMenu', 'showLocalEmptyContextMenu', 'showLocalPathContextMenu']) {
            expect(body(menu), menu).not.toMatch(/calculate(Remote)?FolderSize\(/);
        }
    });

    it('names one entry for it in each menu, Properties', () => {
        // Two entries opening the same dialog is the shape that let one of them
        // break unnoticed.
        expect(appRaw).not.toMatch(/contextMenu\.calculateSize/);
        const properties = /label: t\('contextMenu\.properties'\)/g;
        // A row: one Properties, on the folder or on the selection it belongs to,
        // and offered on a folder as on a file.
        const row = body('showLocalContextMenu');
        expect([...row.matchAll(properties)]).toHaveLength(1);
        expect(row).not.toMatch(/directoryPropertiesItem\(/);
        const start = row.indexOf("label: t('contextMenu.properties')");
        const rowItem = row.slice(start, row.indexOf('\n      },', start));
        expect(rowItem).toMatch(/action: \(\) => openLocalProperties\(selectedEntries\(\)\)/);
        expect(rowItem).not.toMatch(/disabled/);
        // The selection is read from the rows shown: a nested row of a flattened
        // search is not in the listing.
        expect(row).toMatch(/const selectedEntries = \(\) => selectedLocalRows\(ctxPanel\.sortedFiles, localFiles, selection\)/);
        // The actions every folder menu shares carry no Properties of their own,
        // or a folder row would show two.
        const shared = body('directoryActionItems');
        expect(shared).not.toMatch(properties);
        expect(shared).not.toMatch(/directoryPropertiesItem\(/);
        // The panel's empty space and a breadcrumb segment: the directory they
        // name, which has no row there to carry the entry.
        const item = body('directoryPropertiesItem');
        expect(item).toMatch(properties);
        expect(item).not.toMatch(/disabled/);
        expect(item).toMatch(/action: \(\) => openLocalProperties\(\[\{[\s\S]*?path: dirPath,[\s\S]*?is_dir: true,/);
        expect(body('showLocalEmptyContextMenu')).toMatch(/\[\.\.\.directoryActionItems\(ctxPanel\.currentPath\), directoryPropertiesItem\(ctxPanel\.currentPath\)\]/);
        expect(body('showLocalPathContextMenu')).toMatch(/\.\.\.directoryActionItems\(dirPath\),\s*directoryPropertiesItem\(dirPath\),\s*\]\);/);
    });

    it('opens it on the tab that shows the size', () => {
        const open = body('openLocalProperties');
        // The tab is remembered from the last remote Properties (OpenDrive opens
        // it on Permissions), so a local open has to choose it.
        expect(open).toMatch(/setPropertiesInitialTab\('general'\)/);
        expect(open).toMatch(/setPropertiesDialog\(/);
        expect(open).toMatch(/setMultiPropertiesDialog\(/);
        expect(open).toMatch(/get_file_properties/);
    });

    it('opens local Properties one way, from the menus and from Alt+Enter', () => {
        const altEnter = appRaw.slice(appRaw.indexOf("'Alt+Enter': () => {"), appRaw.indexOf("'Alt+Enter': () => {") + 900);
        expect(altEnter).toMatch(/openLocalProperties\(/);
        expect(altEnter).toMatch(/selectedLocalRows\(panel\.sortedFiles, panel\.files, panel\.selection\)/);
        expect(altEnter).not.toMatch(/setPropertiesDialog\(|setMultiPropertiesDialog\(/);
        const menuItem = body('showLocalContextMenu');
        const properties = menuItem.slice(menuItem.indexOf("label: t('contextMenu.properties')"));
        expect(properties.slice(0, 300)).toMatch(/openLocalProperties\(/);
    });

    it('measures a local folder afresh each time its Properties open', () => {
        // A local scan is cheap and the folder may have changed since the last
        // one; only a remote figure is worth keeping between opens.
        const effect = appRaw.slice(appRaw.indexOf('// Start the folder size scans of the dialog that just opened'));
        expect(effect.slice(0, 1400)).toMatch(/if \(!propertiesDialog\.isRemote\) \{\s*calculateFolderSize\(p\);/);
    });

    it('follows a local scan by its own id and stops it when the dialog closes', () => {
        const scan = body('countLocalFolder');
        expect(scan).toMatch(/'local-folder-size-progress'/);
        expect(scan).toMatch(/scan_id !== scanId/);
        expect(scan).toMatch(/invoke<FolderSizeResult>\('calculate_folder_size', \{ path, scanId \}\)/);
        expect(body('cancelLocalFolderSizes')).toMatch(/invoke\('cancel_folder_size', \{ scanId \}\)/);
        const dialogs = appRaw.slice(appRaw.indexOf('{propertiesDialog && ('), appRaw.indexOf('{quickLookTarget && ('));
        expect([...dialogs.matchAll(/cancelLocalFolderSizes\(/g)]).toHaveLength(2);
    });

    it('drops a folder\'s earlier total when its recount fails', () => {
        // The dialog showed the total of the walk before as the folder's size.
        const scan = body('countLocalFolder');
        const failed = scan.slice(scan.indexOf('} catch (err) {'), scan.indexOf('} finally {'));
        expect(failed).toMatch(/if \(current\(\)\) \{\s*\/\/[^\n]*\n\s*setFolderSizeCache\(prev => \{\s*const next = new Map\(prev\);\s*next\.delete\(key\);/);
    });

    it('counts a remote folder through the one count it keeps, stopped when its dialog closes', () => {
        // A scan cancelled by the next one reported on, and returned: the dialog
        // of one folder showed the counts of another, and a closed dialog's
        // partial figure was kept and shown on the next open instead of a count.
        // The rules are in remoteFolderSizes (remoteFolderSizes.test.ts); App
        // gives it the provider's events and command, and stops it on close.
        const wiring = appRaw.slice(appRaw.indexOf('remoteSizesRef.current = remoteFolderSizes({'));
        expect(wiring.slice(0, 400)).toMatch(/listen: \(handler\) => listen<RemoteSizeProgress>\('folder-size-progress', \(event\) => handler\(event\.payload\)\)/);
        expect(wiring.slice(0, 400)).toMatch(/count: \(path, scanId\) => invoke<FolderSizeResult>\('provider_calculate_folder_size', \{ path, scanId \}\)/);
        expect(appRaw).toMatch(/const \{ count: calculateRemoteFolderSize, stop: stopRemoteFolderSize \} = remoteSizesRef\.current;/);
        expect(appRaw).not.toMatch(/'folder-size-progress',\s*\(event\) => \{/);
        const dialogs = appRaw.slice(appRaw.indexOf('{propertiesDialog && ('), appRaw.indexOf('{quickLookTarget && ('));
        expect(dialogs).toMatch(/if \(propertiesDialog\.isRemote && propertiesDialog\.is_dir\) \{\s*stopRemoteFolderSize\(\);\s*invoke\('provider_cancel_folder_size'\)/);
    });

    it('lets a second caller wait for the walk of a folder already counted', () => {
        // It returned at once: the multi-selection count went on to the next
        // folder while the first was still walked, two walks at a time.
        const calculate = body('calculateFolderSize');
        expect(calculate).toMatch(/const running = localSizeRunsRef\.current\.get\(key\);\s*if \(running\) return running;/);
        expect(calculate).toMatch(/localSizeRunsRef\.current\.set\(key, run\);/);
        expect(calculate).not.toMatch(/folderSizeCalculatingRef\.current\.has\(key\)\) return;/);
        // A cancelled walk is not waited on: a new count starts afresh.
        expect(body('cancelLocalFolderSizes')).toMatch(/localSizeRunsRef\.current\.delete\(key\);/);
    });

    it('stops a folder\'s walk only when no Properties dialog shows it any more', () => {
        // Closing or replacing one dialog stopped the walk the other still showed.
        const dialogs = appRaw.slice(appRaw.indexOf('{propertiesDialog && ('), appRaw.indexOf('{quickLookTarget && ('));
        expect(dialogs).toMatch(/const shown = new Set\(localFoldersShownBy\('multi'\)\);\s*cancelLocalFolderSizes\(\[propertiesDialog\.path\]\.filter\(path => !shown\.has\(path\)\)\);/);
        expect(dialogs).toMatch(/const shown = new Set\(localFoldersShownBy\('single'\)\);\s*cancelLocalFolderSizes\(localFoldersShownBy\('multi'\)\.filter\(path => !shown\.has\(path\)\)\);/);
        // A replaced dialog stops the folders it alone counted.
        expect(body('openLocalProperties')).toMatch(/cancelLocalFolderSizes\(localFoldersShownBy\(replacing\)\.filter\(path => !stillShown\.has\(path\)\)\);/);
    });

    it('keeps a local folder and a remote one at the same path apart', () => {
        // One cache served both sides, and every server: "/docs" sized on one
        // server was shown for "/docs" on the next.
        expect(appRaw).toMatch(/const localSizeKey = \(path: string\) => `local:\$\{path\}`/);
        expect(appRaw).toMatch(/const remoteSizeKey = /);
        // The whole connection target: bucket, container, drive, branch, port
        // (connectionTargetKey, tested in liveConnection.test.ts).
        // And the connection itself: two accounts can differ only in a
        // credential (connectionNumberOf, tested there too).
        expect(appRaw).toMatch(/const remoteSizeKey = \(path: string\) => `remote:\$\{connectionTargetKey\(liveConnectionParams\)\}#\$\{connectionNumberOf\(liveConnectionParams\)\}:\$\{path\}`;/);
        expect(appRaw).not.toMatch(/folderSizeCache\.get\(propertiesDialog\.path\)/);
    });
});
