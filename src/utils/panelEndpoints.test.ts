// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { createLocalEndpoint, createRemoteEndpoint, getEndpointCapabilities, getPanelPairKind } from './panelEndpoints';
import type { ServerProfile } from '../types';

const profile = (id: string, protocol: ServerProfile['protocol']): ServerProfile => ({
    id,
    name: id,
    host: `${id}.example.test`,
    port: 22,
    username: 'user',
    protocol,
});

describe('panel endpoint helpers', () => {
    it('classifies the four Slice B endpoint pairings', () => {
        const local = createLocalEndpoint('local', '/home/user');
        const local2 = createLocalEndpoint('local2', '/mnt/usb');
        const remote = createRemoteEndpoint(profile('prod', 'sftp'), '/var/www');
        const remote2 = createRemoteEndpoint(profile('backup', 's3'), '/bucket');

        expect(getPanelPairKind(local, local2)).toBe('local-local');
        expect(getPanelPairKind(local, remote)).toBe('local-remote');
        expect(getPanelPairKind(remote, local)).toBe('remote-local');
        expect(getPanelPairKind(remote, remote2)).toBe('remote-remote');
    });

    it('marks local and SFTP endpoints as delta-capable', () => {
        const localCaps = getEndpointCapabilities(createLocalEndpoint('local', '/tmp'));
        const sftpCaps = getEndpointCapabilities(createRemoteEndpoint(profile('sftp-prod', 'sftp'), '/'));
        const driveCaps = getEndpointCapabilities(createRemoteEndpoint(profile('drive', 'googledrive'), '/'));

        expect(localCaps.canDeltaSync).toBe(true);
        expect(sftpCaps.canDeltaSync).toBe(true);
        expect(driveCaps.canDeltaSync).toBe(false);
        expect(driveCaps.canServerSideCopy).toBe(true);
    });
});
