// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { createElement } from 'react';
import { renderToStaticMarkup } from 'react-dom/server';
import filesystemRs from '../../src-tauri/src/filesystem.rs?raw';
import snapcraftYaml from '../../snap/snapcraft.yaml?raw';
import sidebarSource from './PlacesSidebar.tsx?raw';
import appSource from '../App.tsx?raw';
import {
    MOUNT_OBSERVE_NOT_CONNECTED,
    PartitionsNeedUdisks2,
    SNAP_CONNECT_MOUNT_OBSERVE,
    SNAP_CONNECT_REMOVABLE_MEDIA,
    SNAP_CONNECT_UDISKS2,
    UDISKS2_NOT_CONNECTED,
    VolumesUnavailable,
    needsRemovableMediaPlug,
    needsUdisks2Plug,
    readUnmountedPartitions,
    readVolumes,
    readVolumesAndPartitions,
    volumeActionError,
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

    it('tells the snap udisks2 refusal from any other failure, with the marker and a plug the snap declares', () => {
        expect(needsUdisks2Plug(`${UDISKS2_NOT_CONNECTED}: refused; connect the plug`)).toBe(true);
        expect(needsUdisks2Plug('Failed to mount /dev/sda1: device busy')).toBe(false);
        expect(needsUdisks2Plug(undefined)).toBe(false);
        expect(filesystemRs).toContain(`pub const UDISKS2_NOT_CONNECTED: &str = "${UDISKS2_NOT_CONNECTED}";`);
        expect(filesystemRs).toContain(SNAP_CONNECT_UDISKS2);
        const [, plug] = SNAP_CONNECT_UDISKS2.match(/snap connect aeroftp:([a-z0-9-]+)$/) ?? [];
        const plugs = snapcraftYaml.slice(snapcraftYaml.indexOf('    plugs:'), snapcraftYaml.indexOf('\nparts:'));
        expect(plugs).toMatch(new RegExp(`^      - ${plug}$`, 'm'));
        const hint = renderToStaticMarkup(createElement(PartitionsNeedUdisks2, { t }));
        expect(hint).toContain('sidebar.partitions_need_udisks2');
        expect(hint).toContain(SNAP_CONNECT_UDISKS2);
    });

    it('shows the partition hint, and a failed mount or eject, in the sidebar', () => {
        expect(sidebarSource).toContain('!volumesLoading && partitionsNeedUdisks2 && <PartitionsNeedUdisks2 t={t} />');
        expect(sidebarSource).toContain("title: t('sidebar.mountFailed')");
        expect(sidebarSource.match(/message: volumeActionError\(err, t\)/g)).toHaveLength(2);
        expect(sidebarSource).toContain('readVolumesAndPartitions(');
        expect(sidebarSource).toContain('setPartitionsNeedUdisks2(value.partitionsNeedPlug)');
        expect(sidebarSource).toContain('setUnmountedPartitions(value.unmounted)');
    });

    it('shows both hints when the volume read fails before UDisks2 answers, as on a fresh snap', async () => {
        let refuse: (reason: string) => void = () => {};
        const read = readVolumesAndPartitions<string, string>(
            async () => { throw `${MOUNT_OBSERVE_NOT_CONNECTED}: cannot read /proc/mounts (Permission denied)`; },
            () => new Promise((_, reject) => { refuse = reject; }),
        );
        await new Promise(resolve => setTimeout(resolve, 0));
        refuse(`${UDISKS2_NOT_CONNECTED}: refused`);
        await expect(read).resolves.toEqual({
            volumes: [], unmounted: [], unavailable: 'snap-mount-observe', partitionsNeedPlug: true,
        });
    });

    it('surfaces a refused mount location whatever the toast setting, and only that', () => {
        expect(filesystemRs).toContain(SNAP_CONNECT_REMOVABLE_MEDIA);
        expect(snapcraftYaml).toMatch(/^      - removable-media$/m);
        expect(needsRemovableMediaPlug(`cannot read /run/media/u/D (Permission denied); in the snap, connect the plug with \`${SNAP_CONNECT_REMOVABLE_MEDIA}\``)).toBe(true);
        expect(needsRemovableMediaPlug('Failed to read directory: Permission denied')).toBe(false);
        expect(appSource.match(/needsRemovableMediaPlug\(error\)/g)).toHaveLength(2);
        expect(appSource.match(/needsRemovableMediaPlug\(error\)\) \{\n\s+activityLog\.log\(/g)).toHaveLength(2);
    });

    it('keeps the partitions when only the volume read is refused, as with udisks2 alone', async () => {
        await expect(readVolumesAndPartitions<string, string>(
            async () => { throw `${MOUNT_OBSERVE_NOT_CONNECTED}: cannot read /proc/mounts (Permission denied)`; },
            async () => ['sdb1'],
        )).resolves.toEqual({ volumes: [], unmounted: ['sdb1'], unavailable: 'snap-mount-observe', partitionsNeedPlug: false });
    });

    it('keeps the volumes when only the partitions are refused', async () => {
        await expect(readVolumesAndPartitions(async () => ['/media/usb'], async () => { throw `${UDISKS2_NOT_CONNECTED}: refused`; }))
            .resolves.toEqual({ volumes: ['/media/usb'], unmounted: [], unavailable: null, partitionsNeedPlug: true });
        await expect(readVolumesAndPartitions(async () => ['/media/usb'], async () => ['sdb1']))
            .resolves.toEqual({ volumes: ['/media/usb'], unmounted: ['sdb1'], unavailable: null, partitionsNeedPlug: false });
    });

    it('says the plug only for its refusal, and keeps any other error as it is', async () => {
        const params = (key: string, values?: Record<string, string | number>) => `${key}:${values?.command}`;
        expect(volumeActionError(`${UDISKS2_NOT_CONNECTED}: refused`, params))
            .toBe(`sidebar.volume_action_needs_udisks2:${SNAP_CONNECT_UDISKS2}`);
        expect(volumeActionError('Failed to mount /dev/sda1: device busy', params)).toBe('Failed to mount /dev/sda1: device busy');
        await expect(readUnmountedPartitions(async () => ['sda1'])).resolves.toEqual({ partitions: ['sda1'], needsPlug: false });
        await expect(readUnmountedPartitions(async () => { throw `${UDISKS2_NOT_CONNECTED}: refused`; }))
            .resolves.toEqual({ partitions: [], needsPlug: true });
        await expect(readUnmountedPartitions(async () => { throw 'lsblk returned non-zero exit code'; }))
            .resolves.toEqual({ partitions: [], needsPlug: false });
    });

    it('drops the rows of an earlier read when a read fails, and says why', async () => {
        const ok = await readVolumes(async () => [['/media/usb'], ['sdb1']] as [string[], string[]]);
        expect(ok).toEqual({ volumes: ['/media/usb'], unmounted: ['sdb1'], unavailable: null });
        const refused = await readVolumes<string, string>(async () => {
            throw `${MOUNT_OBSERVE_NOT_CONNECTED}: cannot read /proc/mounts (Permission denied)`;
        });
        expect(refused).toEqual({ volumes: [], unmounted: [], unavailable: 'snap-mount-observe' });
        const failed = await readVolumes<string, string>(async () => {
            throw 'Failed to read /proc/mounts: Input/output error';
        });
        expect(failed).toEqual({ volumes: [], unmounted: [], unavailable: 'failed' });
    });

    it('is what the sidebar shows when the volume list fails', () => {
        expect(sidebarSource).toMatch(/readLatest\(volumesReadRef, \(\) => readVolumesAndPartitions\(/);
        expect(sidebarSource).toContain('setVolumesUnavailable(value.unavailable)');
        expect(sidebarSource).toMatch(/<VolumesUnavailable reason=\{volumesUnavailable\}/);
    });

    it('lets no older read of a sidebar list replace a newer one', () => {
        // Every writer of a polled list commits only the latest read.
        const lines = sidebarSource.split('\n');
        for (const setter of ['setPortableMounts(', 'setMtpAutomounterPresent(']) {
            const writers = lines.filter((l) => l.includes(setter));
            expect(writers.length).toBeGreaterThan(0);
            for (const w of writers) expect(w).toContain('latest && mountedRef.current');
        }
        expect(sidebarSource.match(/readLatest\(portableMountsReadRef,/g)).toHaveLength(2);
        expect(sidebarSource).toMatch(/readLatest\(automounterReadRef,/);
        // The volume rows: the latest read, or the eject that retires earlier reads.
        const volumeWriters = lines.filter((l) => /\bsetVolumes\(/.test(l));
        expect(volumeWriters).toHaveLength(2);
        expect(sidebarSource).toMatch(/if \(!latest \|\| !mountedRef\.current\) return;\n\s+setVolumes\(value\.volumes\);/);
        expect(sidebarSource).toMatch(/volumesReadRef\.current \+= 1;\n\s+setVolumes\(prev => prev\.filter/);
    });
});
