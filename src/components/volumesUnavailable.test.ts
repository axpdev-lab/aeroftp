// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import filesystemRs from '../../src-tauri/src/filesystem.rs?raw';
import snapcraftYaml from '../../snap/snapcraft.yaml?raw';
import sidebarSource from './PlacesSidebar.tsx?raw';
import {
    MOUNT_OBSERVE_NOT_CONNECTED,
    SNAP_CONNECT_MOUNT_OBSERVE,
    VolumesUnavailable,
    volumesUnavailableReason,
} from './volumesUnavailable';

const t = (key: string) => key;

describe('volumes the snap may not read', () => {
    it('reads the marker the backend puts first, and nothing else, as the snap plug', () => {
        expect(volumesUnavailableReason(
            `${MOUNT_OBSERVE_NOT_CONNECTED}: cannot read /proc/mounts (Permission denied)`,
        )).toBe('snap-mount-observe');
        expect(volumesUnavailableReason('Failed to read /proc/mounts: Permission denied')).toBe('failed');
        expect(volumesUnavailableReason(new Error('boom'))).toBe('failed');
        expect(volumesUnavailableReason(undefined)).toBe('failed');
    });

    it('shows the command that connects the plug, or says the list could not be read', () => {
        const snap = renderToStaticMarkup(createElement(VolumesUnavailable, { reason: 'snap-mount-observe', t }));
        expect(snap).toContain('sidebar.volumes_need_mount_observe');
        expect(snap).toContain(SNAP_CONNECT_MOUNT_OBSERVE);
        const failed = renderToStaticMarkup(createElement(VolumesUnavailable, { reason: 'failed', t }));
        expect(failed).toContain('sidebar.volumes_unavailable');
        expect(failed).not.toContain('snap connect');
    });

    it('uses the marker the backend sends and puts first', () => {
        expect(filesystemRs).toContain(
            `pub const MOUNT_OBSERVE_NOT_CONNECTED: &str = "${MOUNT_OBSERVE_NOT_CONNECTED}";`,
        );
        expect(filesystemRs).toMatch(/format!\(\s*"\{MOUNT_OBSERVE_NOT_CONNECTED\}:/);
        expect(filesystemRs).toContain(SNAP_CONNECT_MOUNT_OBSERVE);
    });

    it('names a plug the snap declares, under the snap name', () => {
        // `snap connect` refuses a plug the snap does not declare, so the
        // command in the message is only useful while snapcraft.yaml has it.
        const [, snapName, plug] = SNAP_CONNECT_MOUNT_OBSERVE.match(/snap connect ([a-z0-9-]+):([a-z0-9-]+)$/) ?? [];
        expect(snapcraftYaml).toMatch(new RegExp(`^name: ${snapName}$`, 'm'));
        const plugs = snapcraftYaml.slice(snapcraftYaml.indexOf('    plugs:'), snapcraftYaml.indexOf('\nparts:'));
        expect(plugs).toMatch(new RegExp(`^      - ${plug}$`, 'm'));
    });

    it('is what the sidebar shows when the volume list fails', () => {
        expect(sidebarSource).toContain('setVolumesUnavailable(volumesUnavailableReason(err))');
        expect(sidebarSource).toMatch(/<VolumesUnavailable reason=\{volumesUnavailable\}/);
    });
});
