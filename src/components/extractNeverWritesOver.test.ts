// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';
import vaultStateRaw from './vault/useVaultState.ts?raw';
import cryptomatorRaw from './CryptomatorBrowser.tsx?raw';

/**
 * An extraction never writes over a file without asking.
 *
 * A `.aerozip` and a v3 vault write each entry over an existing file of the
 * same path (the crate persists a temporary file onto it). The row menu's
 * "Extract here" of both, and its "Extract to folder" into a folder named
 * after the archive that may be there already, replaced files with no
 * question; so did "Save All > Folder" of the vault panel and of the
 * Cryptomator browser, into the folder chosen. The zip/7z/tar/rar lane and
 * the Extract window already asked or extracted into a folder of their own.
 * The collision rule is unit tested in utils/extractOrchestrator.test.ts.
 */
describe('an extraction never writes over a file without asking', () => {
    const menu = (() => {
        const start = appRaw.indexOf('\n  const showLocalContextMenu = ');
        return appRaw.slice(start, appRaw.indexOf('\n  const ', start + 1));
    })();
    /** A menu entry from its label to the next entry's. */
    const after = (marker: string, length = 1600): string => {
        const start = menu.indexOf(marker);
        expect(start, marker).toBeGreaterThan(-1);
        return menu.slice(start, start + length);
    };

    it('asks before an "Extract here" writes over what the archive\'s folder holds', () => {
        const confirm = after('const confirmExtractHere = async', 900);
        // Listed afresh with its hidden entries, which the panel may not show
        // (extractHereCollisions, unit tested). A folder that cannot be listed
        // stops the extraction: its error is not caught here and taken as an
        // empty folder.
        expect(confirm).toMatch(/const collisions = await extractHereCollisions\(entryNames, rowFolder\);/);
        expect(confirm.slice(0, confirm.indexOf('setConfirmDialog('))).not.toMatch(/\.catch\(/);
        expect(confirm).toMatch(/message: t\('contextMenu\.extractOverwriteConfirm'/);
        // The general lane, the .aerozip lane and a v3 vault, each before it
        // extracts.
        expect(menu).toMatch(/const entryNames = await generalEntryNames\(\);\s*if \(entryNames === null\) \{[\s\S]*?\} else if \(!await confirmExtractHere\(entryNames\)\) return;/);
        expect(menu).toMatch(/if \(!await confirmExtractHere\(await aeroEntryNames\('aerozip', file\.path, null\)\)\) return;\s*notify\.info[^\n]*\n\s*const \{ result: count \} = await runExtractWithToast<number>\('aerovz_extract_all', \{ vaultPath: file\.path, destPath: rowFolder \}/);
        expect(menu).toMatch(/if \(kind === 'aerovault_v3' && !await confirmExtractHere\(await aeroEntryNames\(kind, file\.path, password\)\)\) return;\s*notify\.info[^\n]*\n\s*await dispatchAeroExtract\(\{ kind, archivePath: file\.path, destDir: rowFolder,/);
    });

    it('says so when the archive\'s names cannot be read, and offers a folder of its own', () => {
        // A 7z or RAR archive with encrypted names lists nothing before the
        // password: the check found nothing and the extraction went ahead.
        const names = after('const generalEntryNames = async', 1200);
        expect(names).toMatch(/\} catch \{\s*return null;\s*\}/);
        const unchecked = after('if (entryNames === null) {', 1200);
        expect(unchecked).toMatch(/message: t\('contextMenu\.extractHereUncheckedConfirm', \{ name: rowName \}\)/);
        expect(unchecked).toMatch(/secondaryLabel: t\('contextMenu\.extractToFolder'\)/);
        expect(unchecked).toMatch(/if \(choice === 'cancel'\) return;\s*if \(choice === 'folder'\) \{ await doExtract\(true\); return; \}/);
        // Asked before the password: the password dialog is drawn over the
        // confirmation (#537).
        expect(menu.indexOf('if (entryNames === null) {')).toBeLessThan(menu.indexOf('setArchivePasswordDialog({'));
    });

    it('extracts "to folder" into a folder of its own, never one that is there', () => {
        // The archive's stem, or "stem (2)" when that is taken.
        expect(menu).not.toMatch(/rowName\.replace\(\/\\\.aero(zip|vault)\$\/i, ''\)/);
        expect(menu).toMatch(/const subFolder = await resolveUniqueExtractDir\(rowFolder, rowName\);\s*notify\.info[^\n]*\n\s*const \{ result: count \} = await runExtractWithToast<number>\('aerovz_extract_all', \{ vaultPath: file\.path, destPath: subFolder \}/);
        expect(menu).toMatch(/const subFolder = await resolveUniqueExtractDir\(rowFolder, rowName\);\s*notify\.info[^\n]*\n\s*await dispatchAeroExtract\(\{ kind: await aeroVaultKind\(file\.path\), archivePath: file\.path, destDir: subFolder,/);
    });

    it('extracts a v3 vault from the row menu with the v3 command', () => {
        // Every .aerovault went to the v2 command, and a v3 vault (the
        // experimental level) failed to open there.
        expect(menu).not.toMatch(/'vault_v2_extract_all'/);
        expect([...menu.matchAll(/aeroVaultKind\(file\.path\)/g)]).toHaveLength(2);
    });

    it('names the folder it extracted into', () => {
        // "stem (2)" when "stem" was taken; the name stripped of its last
        // extension only was shown ("photos.tar" for "photos.tar.gz").
        expect(menu).not.toMatch(/rowName\.replace\(\/\\\.\[\^\.\]\+\$\/, ''\)\}\//);
        expect([...menu.matchAll(/const dest = createSubfolder \? `📁 \$\{localPathBasename\(extractOutputDir\)\}\/` : rowFolder;/g)]).toHaveLength(2);
    });

    it('saves a whole vault into a folder of its own in the folder chosen', () => {
        const extractAll = vaultStateRaw.slice(vaultStateRaw.indexOf('const handleExtractAll = async'), vaultStateRaw.indexOf('const handleSaveAll = async'));
        expect(extractAll).toMatch(/const chosen = await pickFile\(\{ directory: true \}\);/);
        expect(extractAll).toMatch(/const destPath = await resolveUniqueExtractDir\(chosen, /);
        const saveAll = cryptomatorRaw.slice(cryptomatorRaw.indexOf('const handleSaveAll = async'), cryptomatorRaw.indexOf('const handleEncrypt = async'));
        expect(saveAll).toMatch(/if \(target === 'folder'\) destPath = await resolveUniqueExtractDir\(destPath, vaultInfo\.name\);\s*const report = await invoke/);
    });
});
