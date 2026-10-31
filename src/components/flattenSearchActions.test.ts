// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';
import dragRaw from '../hooks/useDragAndDrop.ts?raw';

/**
 * Local actions on the rows of a flattened search (`*`).
 *
 * A flattened search lists the entries below the panel's directory and names
 * each by its relative path ("Photos 2026/Summer"). Two halves broke there:
 *
 * - the selection was looked up in the panel's listing, which holds the
 *   directory's own entries only, so a selected nested row was skipped (Copy,
 *   Cut, F2, Batch Rename, Delete, upload, F5/F6 did nothing to it);
 * - the relative path was used as a file name, so a paste, upload, drop or
 *   rename would have created "Photos 2026/Summer" where "Summer" belongs.
 *
 * The helpers that resolve both are unit tested in utils/localSelection.test.ts
 * and the dialog's per-folder checks in BatchRenameDialog.test.ts; these pin
 * that every local action goes through them.
 */
describe('local actions on the rows of a flattened search', () => {
    /** The body of a component-level `const name = ...`, up to the next one. */
    const body = (name: string): string => {
        const start = appRaw.indexOf(`\n  const ${name} = `);
        expect(start, `${name} exists in App.tsx`).toBeGreaterThan(-1);
        const end = appRaw.indexOf('\n  const ', start + 1);
        return appRaw.slice(start, end === -1 ? undefined : end);
    };
    /** A keyboard shortcut's handler, up to the next one. */
    const shortcut = (key: string): string => {
        const start = appRaw.indexOf(`\n    '${key}': () => {`);
        expect(start, `${key} handler exists`).toBeGreaterThan(-1);
        return appRaw.slice(start, appRaw.indexOf("\n    '", start + 1));
    };
    /** A menu bar handler prop, up to the next one. */
    const menuBar = (prop: string): string => {
        const start = appRaw.indexOf(`\n          ${prop}={() => {`);
        expect(start, `${prop} exists`).toBeGreaterThan(-1);
        return appRaw.slice(start, appRaw.indexOf('\n          }}', start) + 13);
    };
    const count = (text: string, re: RegExp) => [...text.matchAll(re)].length;

    it('looks no local selection up in the listing alone', () => {
        // Every shape the listing-only lookup took; none may come back.
        const listingOnly = [
            /localFiles\.filter\(f => (selection|selectedLocalFiles)\.has\(f\.name\)\)/,
            /panel\.files\.filter\(f => panel\.selection\.has\(f\.name\)\)/,
            /panel\.files\.find\(f => f\.name === (name|n)\)/,
            /localFiles\.find\(f => f\.name === (name|fileName)/,
            /localFiles\.find\(lf => lf\.name === name\)/,
            /sortedLocalFiles\.find\(lf => lf\.name === name\)/,
            /sourceFiles\.filter\(f => names\.includes\(f\.name\)/,
            /const selectedFiles = localFiles\s*\.filter\(f => selection\.has\(f\.name\)\)/,
            // Any local listing filtered or searched by the selected names.
            /\b(localFiles2?|panel\.files|sourceState\.files|sourceFiles)\.(filter|find)\(\s*\w+ => [\w.]*\.(has|includes)\(\s*\w+\.name\)/,
        ];
        for (const re of listingOnly) expect(appRaw, String(re)).not.toMatch(re);
    });

    it('copies and cuts each file under its own name, from every way in', () => {
        // Ctrl+C, Ctrl+X, the menu bar's Copy and Cut, the row menu's Copy and Cut.
        expect(count(appRaw, /localClipboardEntries\(/g)).toBe(6);
        expect(shortcut('Ctrl+C')).toMatch(/clipboardCopy\(localClipboardEntries|const files = localClipboardEntries\(panel\.sortedFiles, panel\.files, panel\.selection\)/);
        expect(shortcut('Ctrl+X')).toMatch(/const files = localClipboardEntries\(panel\.sortedFiles, panel\.files, panel\.selection\)/);
    });

    it('renames a nested row in place, starting from its own name', () => {
        expect(shortcut('F2')).toMatch(/selectedLocalRows\(panel\.sortedFiles, panel\.files, panel\.selection\)/);
        expect(count(appRaw, /startInlineRename\(file\.path, localRowName\(file\), false\)/g)).toBe(2);
        expect(appRaw).toMatch(/renameFile\(file\.path, localRowName\(file\), false\)/);
        expect(appRaw).not.toMatch(/startInlineRename\(file\.path, file\.name, false\)/);
    });

    it('acts from the menu bar on the focused local panel', () => {
        // The menu bar used panel 1's selection whichever panel had focus; its
        // Delete then resolved those names in the focused panel, so panel 2's
        // same-named files went to the trash.
        for (const prop of ['onCut', 'onCopy', 'onPaste', 'onRename', 'onDelete']) {
            expect(menuBar(prop), prop).toMatch(/const panel = getActiveLocalState\(\);/);
        }
        expect(menuBar('onDelete')).toMatch(/deleteMultipleLocalFiles\(Array\.from\(panel\.selection\), panel\.id\)/);
        expect(menuBar('onDelete')).not.toMatch(/selectedLocalFiles/);
    });

    it('deletes exactly the rows selected, by their paths', () => {
        const del = body('deleteMultipleLocalFiles');
        expect(count(del, /selectedLocalTrees\(panel\.sortedFiles, panel\.files, new Set\(names\)\)/g)).toBe(2);
        expect(del).toMatch(/trashLocalPaths\(\s*selected\.map\(file => file\.path\)/);
    });

    it('uploads each file under its own name', () => {
        const upload = body('uploadMultipleFiles');
        expect(upload).toMatch(/const panel = getActiveLocalState\(localPanelId\);/);
        expect(upload).toMatch(/const rows = selectedLocalTrees\(panel\.sortedFiles, panel\.files, new Set\(targetNames\)\);\s*if \(refuseLocalNameClashes\(rows\)\) return;\s*let filesToUpload = rows\.map\(file => \(\{ path: file\.path, file: asLocalFileEntry\(file\) \}\)\);/);
        expect(upload).not.toMatch(/localPath: currentLocalPath|setSelectedLocalFiles\(|Array\.from\(selectedLocalFiles\)/);
        expect(body('handleLocalFileAction')).toMatch(/uploadFile\(file\.path, localRowName\(file\), false\)/);
        expect(body('uploadFile')).toMatch(/\.find\(f => f\.path === localFilePath && !f\.is_dir\)/);
    });

    it('copies and moves to the other panel under each file\'s own name', () => {
        const transfer = body('transferLocalSelectionAcrossPanels');
        expect(transfer).toMatch(/selectedLocalTrees\(sourceRows, sourceFiles, sourceSelection\)[\s\S]*?\.map\(asLocalFileEntry\)/);
    });

    it('renames a batch file by file, each in its own folder', () => {
        const start = appRaw.indexOf("label: t('batchRename.title') || 'Batch Rename',", appRaw.indexOf('\n  const showLocalContextMenu = '));
        const item = appRaw.slice(start, appRaw.indexOf('setBatchRenameDialog(', start));
        expect(item).toMatch(/selectedLocalRows\(ctxPanel\.sortedFiles, localFiles, selection\)/);
        expect(item).toMatch(/name: localRowName\(f\), path: f\.path, isDir: f\.is_dir, folder: localRowFolderKey\(f\) \|\| undefined/);
        expect(item).toMatch(/folder: localRowFolderKey\(f\), name: localRowName\(f\)/);
    });

    it('extracts a nested archive next to it, under its own name', () => {
        const menu = body('showLocalContextMenu');
        expect(menu).toMatch(/const rowFolder = localRowFolder\(file, currentLocalPath\);/);
        expect(menu).toMatch(/let extractOutputDir = rowFolder;/);
        expect(menu).toMatch(/resolveUniqueExtractDir\(rowFolder, rowName\)/);
        expect(menu).toMatch(/destPath: rowFolder \}/);
        expect(menu).toMatch(/destDir: rowFolder \}/);
        expect(menu).not.toMatch(/resolveUniqueExtractDir\(currentLocalPath/);
    });

    it('names the row\'s file where the menu shows or copies its name', () => {
        const menu = body('showLocalContextMenu');
        expect(menu).toMatch(/copyText\(localRowName\(file\)\)/);
        expect(menu).toMatch(/openInFileManager\(file\.is_dir \? file\.path : localRowFolder\(file, currentLocalPath\)\)/);
        expect(menu).toMatch(/const baseName = count === 1 \? localRowName\(file\)/);
    });

    it('drops a nested row under its own name', () => {
        expect(dragRaw).toMatch(/files: filesToDrag\.map\(f => \(isLocalPanel\(panelKey\) \? localRowName\(f\) : f\.name\)\)/);
    });

    it('acts on the focused local panel, never a hidden panel 2', () => {
        // The focus keeps naming panel 2 after it is hidden; see focusedLocalPanel.
        expect(appRaw).not.toMatch(/'local' \| 'local2' = activeLocalPanelId\)/);
        expect(appRaw).toMatch(/const getActiveLocalState = \(id: 'local' \| 'local2' = focusedLocalPanelId\)/);
        // The toolbar's Delete passed panel 1's names to the focused panel.
        const start = appRaw.indexOf('data-testid={TID.toolbarDelete}');
        const toolbarDelete = appRaw.slice(start, appRaw.indexOf('</button>', start));
        expect(toolbarDelete).toMatch(/const panel = getActiveLocalState\(\);\s*if \(panel\.selection\.size > 0\) deleteMultipleLocalFiles\(Array\.from\(panel\.selection\), panel\.id\);/);
        expect(toolbarDelete).not.toMatch(/selectedLocalFiles\b/);
        expect(appRaw).not.toMatch(/deleteMultipleLocalFiles\(Array\.from\(selectedLocalFiles\)\)/);
        expect(appRaw).toMatch(/hasSelection=\{selectedRemoteFiles\.size > 0 \|\| focusedLocalSelection\.size > 0\}/);
        // The row menu acts on the panel right-clicked.
        const menu = body('showLocalContextMenu');
        expect(menu).toMatch(/uploadMultipleFiles\(filesToUpload, localPanelId\)/);
        expect(menu).toMatch(/deleteMultipleLocalFiles\(filesToUpload, localPanelId\)/);
    });

    it('refuses files of two folders that one destination would hold under one name', () => {
        // Clipboard (Ctrl+C, Ctrl+X, menu bar and row menu Cut and Copy),
        // upload, compress; F5/F6 in the plan and in the transfer; the drag.
        expect(count(appRaw, /refuseLocalNameClashes\(/g)).toBe(8);
        expect(body('planLocalSelectionAcrossPanels')).toMatch(/localRowNameClashes\(selectedFiles\)/);
        expect(body('transferLocalSelectionAcrossPanels')).toMatch(/localRowNameClashes\(rows\)/);
        expect(body('showLocalContextMenu')).toMatch(/if \(refuseLocalNameClashes\(compressRows\)\) return;/);
        expect(dragRaw).toMatch(/isLocalPanel\(panelKey\) && refuseLocalNameClashes\?\.\(filesToDrag\)/);
        expect(appRaw).toMatch(/useDragAndDrop\(\{\s*refuseLocalNameClashes,/);
    });

    it('never drops over an existing entry, nor onto the folder a row is in', () => {
        expect(count(dragRaw, /overwrite: false/g)).toBe(2);
        expect(count(dragRaw, /if \(destPath === sourcePath\) continue;/g)).toBe(2);
    });

    it('takes a selected folder with what is below it, and renames the deepest first', () => {
        // Every action on the whole selection: delete and its confirmation,
        // F5/F6 in the plan and in the transfer, upload, Properties (row menu
        // and Alt+Enter), compress, AeroVault and Cryptomator creation and the
        // folder they go in. The clipboard takes them through
        // localClipboardEntries, the drag through outermostLocalRows.
        expect(count(appRaw, /selectedLocalTrees\(/g)).toBe(11);
        expect(body('transferLocalSelectionAcrossPanels')).toMatch(/const rows = selectedLocalTrees\(/);
        expect(body('transferLocalSelectionAcrossPanels')).not.toMatch(/mode === 'move' \?/);
        expect(body('planLocalSelectionAcrossPanels')).toMatch(/const selectedFiles = selectedLocalTrees\(/);
        expect(shortcut('Alt+Enter')).toMatch(/const entries = selectedLocalTrees\(/);
        const menu = body('showLocalContextMenu');
        expect(menu).toMatch(/const selectedEntries = \(\) => selectedLocalTrees\(/);
        expect(menu).toMatch(/const compressRows = selectedLocalTrees\(/);
        expect(count(menu, /const paths = selectedLocalTrees\(/g)).toBe(2);
        expect(menu).toMatch(/const folders = new Set\(selectedLocalTrees\(/);
        // Each row on its own: Enter, F2 (key and menu bar), Batch Rename, Tags.
        expect(count(appRaw, /selectedLocalRows\(/g)).toBe(5);
        expect(dragRaw).toMatch(/const filesToDrag = isLocalPanel\(panelKey\) \? outermostLocalRows\(selectedRows\) : selectedRows;/);
        expect(body('handleBatchRename')).toMatch(/deepestLocalRowsFirst\(\[\.\.\.renames\]/);
    });

    it('creates an archive or a vault in the folder of what it holds', () => {
        const menu = body('showLocalContextMenu');
        expect(count(menu, /outputDir: selectionFolder\(\)/g)).toBe(3);
        expect(menu).toMatch(/folderPath: file\.path, outputDir: rowFolder \}/);
        expect(menu).not.toMatch(/outputDir: currentLocalPath/);
    });

    it('rescans the flattened rows when the listing reloads', () => {
        // After a delete, rename or move the listing reloads; the flattened rows
        // used to keep the old entry on screen.
        expect(appRaw).toMatch(/\}, \[flattenSearchParse\.active, currentLocalPath, localFiles\]\);/);
        // One walk at a time: a rescan waits for the scan before it.
        expect(appRaw).toMatch(/await scan\.running\?\.catch\(\(\) => undefined\);\s*if \(cancelled\) return;/);
    });
});
