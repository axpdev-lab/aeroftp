// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, it, expect } from 'vitest';
import { newFormTabOptions } from './newFormTabOptions';
import { connectionProfileDefaults } from '../../providers/connectionManifest';
import { buildDiscoverCategories } from './discoverData';

describe('Add Service: the form a pick opens', () => {
    it('opens Proton Drive on the API tab, as the protocol selector and profile-add do', () => {
        // A Proton form without `proton_mode` is the CLI tab.
        expect(newFormTabOptions('proton').proton_mode).toBe('native');
        expect(newFormTabOptions('proton')).toMatchObject(connectionProfileDefaults('proton').options);
    });

    it('starts every protocol from its manifest defaults', () => {
        for (const protocol of ['ftp', 'ftps', 'mega', 'sftp', 'scp', 'webdav', 's3']) {
            expect(newFormTabOptions(protocol), protocol).toMatchObject(connectionProfileDefaults(protocol).options);
        }
    });

    it('keeps the preset defaults the form preselects', () => {
        const options = newFormTabOptions('webdav', 'megacmd-webdav');
        expect(options.webdavScheme).toBe('http');
        expect(options.anonymous).toBe(true);
    });

    it('shows the Proton Drive card as API first, like the protocol selector', () => {
        const proton = buildDiscoverCategories()
            .flatMap((category) => category.items)
            .find((item) => item.id === 'proton');
        expect(proton?.badge).toBe('API / CLI');
        expect(proton?.description).not.toMatch(/via the Proton Drive CLI/);
    });

    it('surfaces Blomp in the Cloud Storage discover category', () => {
        const blomp = buildDiscoverCategories()
            .find((category) => category.id === 'cloud-storage')
            ?.items.find((item) => item.id === 'blomp');
        expect(blomp).toBeDefined();
        expect(blomp?.protocol).toBe('swift');
        expect(blomp?.providerId).toBe('blomp');
        expect(blomp?.badge).toBe('Swift');
        expect(blomp?.searchAliases).toContain('swift');
        expect(blomp?.signupUrl).toBe('https://dashboard.blomp.com/register');
        expect(blomp?.helpUrl).toBe('https://support.blomp.com/viewforum.php?f=8');
    });
});
