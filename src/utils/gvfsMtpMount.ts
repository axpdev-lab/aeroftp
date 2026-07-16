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
 * True when `path` is (or is under) a gvfs MTP FUSE mount.
 *
 * Used to degrade honestly on unplug: the mount vanishes and the backend
 * reports a raw "Path does not exist", which is useless to the user.
 */
export function isGvfsMtpPath(path: string | null | undefined): boolean {
  if (!path) return false;
  return path.includes(GVFS_MTP_MOUNT_MARKER);
}

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

/**
 * True when exclusive libmtp open is certain to fail: the desktop automounts
 * MTP (gvfs) so it already spent the single session at plug time, and the
 * device is not currently mounted for us to ride.
 *
 * When there is no automounter, exclusive open is the only path and must stay
 * available. Do not treat "not mounted" alone as a failure signal.
 */
export function portableDeviceNeedsReplug(
  automounterPresent: boolean,
  gvfsMountPath: string | null | undefined,
): boolean {
  return automounterPresent && !gvfsMountPath;
}

/**
 * True when `currentPath` is the mount root or a path under it.
 *
 * Portable-devices rows use this so a gvfs-first PLACES browse lights the
 * matching phone blue even though no exclusive libmtp session id is set.
 * Child paths (e.g. .../DCIM) still count as on-device.
 */
export function isPathOnOrUnderMount(
  currentPath: string | null | undefined,
  mountPoint: string | null | undefined,
): boolean {
  if (!currentPath || !mountPoint) return false;
  if (currentPath === mountPoint) return true;
  // Avoid prefix false positives (e.g. /mnt/phone vs /mnt/phone2).
  return (
    currentPath.startsWith(mountPoint + '/')
    || currentPath.startsWith(mountPoint + '\\')
  );
}
