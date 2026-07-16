// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// APPENDIX-DEVICE-PROFILES: ride the desktop's MTP mount instead of fighting it.
//
// A phone grants a single MTP session per physical connection (verified live on
// a Sony Xperia 1 V: once gvfs mounts it at plug time, no other client can open
// it, and even gvfs cannot re-open after a clean close, until the cable is
// re-plugged). So when the desktop already has the device mounted, the only
// mechanism that can work is browsing its gvfs FUSE path as a local filesystem.
// Exclusive libmtp stays the fallback for when gvfs is not in the picture.

import type { MtpDeviceInfo, VolumeInfo } from '../types/aerofile';

/** gvfs names MTP mounts `mtp:host=<vendor>_<model>_<serial>` under `/run/user/<uid>/gvfs`. */
const GVFS_MTP_MOUNT_MARKER = '/gvfs/mtp:host=';

/**
 * The gvfs FUSE mount point for `device`, or null when the desktop has not
 * mounted it (or the device reports no serial to match on).
 *
 * Matching is by serial: it is the one identifier that appears both in our
 * fingerprint and in the gvfs mount name, and it survives the bus/devnum
 * changing on every replug.
 */
export function findGvfsMtpMount(
  volumes: readonly VolumeInfo[],
  device: Pick<MtpDeviceInfo, 'serial'>,
): string | null {
  const serial = device.serial?.trim();
  if (!serial) return null;
  const hit = volumes.find(
    (v) => v.mount_point.includes(GVFS_MTP_MOUNT_MARKER) && v.mount_point.includes(serial),
  );
  return hit ? hit.mount_point : null;
}
