// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import { findGvfsMtpMount } from './gvfsMtpMount';
import type { VolumeInfo } from '../types/aerofile';

const vol = (name: string, mount_point: string): VolumeInfo => ({
  name,
  mount_point,
  volume_type: 'removable',
  total_bytes: 0,
  free_bytes: 0,
  fs_type: 'fuse',
  is_ejectable: true,
});

// Real shape observed live on the owner's station.
const XPERIA_MOUNT = '/run/user/1001/gvfs/mtp:host=Sony_XQ-DQ54_QV770LUNJD';

describe('findGvfsMtpMount', () => {
  it('finds the mount whose gvfs name carries the device serial', () => {
    const volumes = [
      vol('System', '/'),
      vol('Sony_XQ-DQ54_QV770LUNJD', XPERIA_MOUNT),
    ];
    expect(findGvfsMtpMount(volumes, { serial: 'QV770LUNJD' })).toBe(XPERIA_MOUNT);
  });

  it('returns null when the desktop has not mounted the device', () => {
    expect(findGvfsMtpMount([vol('System', '/')], { serial: 'QV770LUNJD' })).toBeNull();
  });

  it('returns null without a serial, rather than guessing a wrong phone', () => {
    const volumes = [vol('Sony', XPERIA_MOUNT)];
    expect(findGvfsMtpMount(volumes, { serial: undefined })).toBeNull();
    expect(findGvfsMtpMount(volumes, { serial: '  ' })).toBeNull();
  });

  it('does not match a same-serial path that is not a gvfs MTP mount', () => {
    const volumes = [vol('backup disk', '/media/axpdev/QV770LUNJD-backup')];
    expect(findGvfsMtpMount(volumes, { serial: 'QV770LUNJD' })).toBeNull();
  });

  it('picks the right phone when two are mounted', () => {
    const other = '/run/user/1001/gvfs/mtp:host=Samsung_Tab_R52T9';
    const volumes = [vol('Samsung', other), vol('Sony', XPERIA_MOUNT)];
    expect(findGvfsMtpMount(volumes, { serial: 'R52T9' })).toBe(other);
    expect(findGvfsMtpMount(volumes, { serial: 'QV770LUNJD' })).toBe(XPERIA_MOUNT);
  });
});
