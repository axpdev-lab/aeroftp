// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import appRaw from '../App.tsx?raw';
import breadcrumb from './BreadcrumbBar.tsx?raw';
import localPanel from './LocalFilePanel.tsx?raw';

/**
 * Calculate Size, Find Duplicates and Disk Usage can be run on the directory you
 * are in.
 *
 * They used to exist only on a folder *entry* in the listing, so running any of
 * them on the current directory meant leaving it, finding it again in the
 * parent, and right-clicking it there — which is what discussion #347 asked
 * about. They are now built from a path, and the same three are raised from the
 * folder entry, from the empty space of the panel, and from a segment of the
 * breadcrumb (the last of which is the current directory itself).
 */
describe('directory actions reach the current directory (#347)', () => {
    const body = (name: string): string => {
        const start = appRaw.indexOf(`const ${name} = `);
        expect(start, `${name} exists in App.tsx`).toBeGreaterThan(-1);
        return appRaw.slice(start, start + 1800);
    };

    it('builds the three actions from a path, in one place', () => {
        const items = body('directoryActionItems');
        for (const key of ['contextMenu.calculateSize', 'contextMenu.findDuplicates', 'contextMenu.diskUsage']) {
            expect(items, key).toContain(key);
        }
        // From the argument, not from a listing entry: that is what lets the same
        // list serve a folder row, the panel background and a breadcrumb segment.
        expect(items).toMatch(/calculateFolderSize\(dirPath\)/);
        expect(items).toMatch(/setDuplicateFinderPath\(dirPath\)/);
        expect(items).toMatch(/setDiskUsagePath\(dirPath\)/);
    });

    it('offers them on the empty space of the panel', () => {
        expect(body('showLocalEmptyContextMenu')).toMatch(/directoryActionItems\(ctxPanel\.currentPath\)/);
    });

    it('offers them on a breadcrumb segment', () => {
        expect(body('showLocalPathContextMenu')).toMatch(/directoryActionItems\(dirPath\)/);
    });

    it('still offers them on a folder in the listing, from the same list', () => {
        // Not a fourth copy: a copy is what lets the three menus drift apart.
        expect(appRaw).toMatch(/file\.is_dir \? directoryActionItems\(/);
        const copies = [...appRaw.matchAll(/label: t\('contextMenu\.findDuplicates'\)/g)];
        expect(copies.length, 'one definition of the Find Duplicates item').toBe(1);
    });

    it('wires the breadcrumb through to both local panels', () => {
        expect(breadcrumb).toMatch(/onSegmentContextMenu\?: \(e: React\.MouseEvent, path: string\) => void/);
        // Root segment and every other segment, so the whole bar answers.
        expect([...breadcrumb.matchAll(/onContextMenu=\{onSegmentContextMenu \?/g)]).toHaveLength(2);
        expect([...localPanel.matchAll(/onSegmentContextMenu=\{onPathContextMenu\}/g)]).toHaveLength(2);
        expect([...appRaw.matchAll(/onPathContextMenu=/g)]).toHaveLength(2);
    });

    it('leaves the remote breadcrumb alone', () => {
        // `find_duplicate_files`, `calculate_folder_size` and the disk-usage scan
        // all take a local path. Offering them on a cloud breadcrumb would be a
        // menu entry that cannot work.
        const remoteBar = appRaw.slice(appRaw.indexOf('currentPath={(rcloneCryptVaultId'));
        expect(remoteBar.slice(0, 1200)).not.toMatch(/onSegmentContextMenu/);
    });
});
