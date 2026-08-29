// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

// Pins the two pure decisions behind the bridge picker's autodetect: which
// tools come first, and how their config path is shown. The probe loop itself
// is a Tauri round trip and is not covered here.

import { describe, it, expect } from 'vitest';
import { orderBridgeSourcesByDetection, shortenConfigPath } from './useDetectedBridgeConfigs';
import { GENERIC_BRIDGE_SOURCES } from '../components/bridge/bridgeSources';

const ids = (list: { id: string }[]) => list.map(s => s.id);

describe('orderBridgeSourcesByDetection', () => {
    it('floats detected tools to the top', () => {
        // cyberduck sits at index 10 of the curated list, dreamweaver at 11:
        // exactly the "hiding at position 11 of 15" case this exists to fix.
        const ordered = orderBridgeSourcesByDetection(GENERIC_BRIDGE_SOURCES, {
            cyberduck: '/home/me/Library/Application Support/Cyberduck/bookmark.duck',
            dreamweaver: '/home/me/sites/site.ste',
        });
        expect(ids(ordered).slice(0, 2)).toEqual(['cyberduck', 'dreamweaver']);
    });

    it('keeps the curated order inside each group', () => {
        const ordered = orderBridgeSourcesByDetection(GENERIC_BRIDGE_SOURCES, {
            filezilla: '/home/me/.config/filezilla/sitemanager.xml',
            lftp: '/home/me/.lftprc',
        });
        // Detected pair in curated order (filezilla is 3rd, lftp 10th), and the
        // untouched remainder still starts with rclone, winscp, aws.
        expect(ids(ordered).slice(0, 2)).toEqual(['filezilla', 'lftp']);
        expect(ids(ordered).slice(2, 5)).toEqual(['rclone', 'winscp', 'aws']);
    });

    it('leaves the list alone when nothing was detected, and does not mutate the input', () => {
        const before = ids(GENERIC_BRIDGE_SOURCES);
        expect(ids(orderBridgeSourcesByDetection(GENERIC_BRIDGE_SOURCES, {}))).toEqual(before);
        expect(ids(GENERIC_BRIDGE_SOURCES)).toEqual(before);
    });
});

describe('shortenConfigPath', () => {
    it('keeps the two segments that identify the tool', () => {
        expect(shortenConfigPath('/home/me/.config/filezilla/sitemanager.xml')).toBe('…/filezilla/sitemanager.xml');
    });

    it('handles Windows paths', () => {
        expect(shortenConfigPath('C:\\Users\\me\\AppData\\Roaming\\FileZilla\\sitemanager.xml')).toBe('…/FileZilla/sitemanager.xml');
    });

    it('returns short paths untouched rather than prefixing an ellipsis to nothing', () => {
        expect(shortenConfigPath('/etc/s3cfg')).toBe('/etc/s3cfg');
        expect(shortenConfigPath('.lftprc')).toBe('.lftprc');
    });
});
