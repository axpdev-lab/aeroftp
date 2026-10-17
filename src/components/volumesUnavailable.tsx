// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';

/**
 * What `list_mounted_volumes` puts first in its error when the snap may not
 * read the mount table (`MOUNT_OBSERVE_NOT_CONNECTED` in filesystem.rs).
 */
export const MOUNT_OBSERVE_NOT_CONNECTED = 'MOUNT_OBSERVE_NOT_CONNECTED';

/** Connects the plug snapd never connects on its own (snap/snapcraft.yaml). */
export const SNAP_CONNECT_MOUNT_OBSERVE = 'sudo snap connect aeroftp:mount-observe';

export type VolumesUnavailableReason = 'snap-mount-observe' | 'failed';

/** Why the volume list could not be read, from the error of `list_mounted_volumes`. */
export function volumesUnavailableReason(err: unknown): VolumesUnavailableReason {
  return String(err).startsWith(MOUNT_OBSERVE_NOT_CONNECTED) ? 'snap-mount-observe' : 'failed';
}

/** Says why the volume list is empty instead of leaving the section blank. */
export function VolumesUnavailable({
  reason,
  t,
}: {
  reason: VolumesUnavailableReason;
  t: (key: string, params?: Record<string, string | number>) => string;
}) {
  if (reason === 'snap-mount-observe') {
    return (
      <div className="px-3 py-2 text-[11px] text-gray-500 dark:text-gray-400">
        <p>{t('sidebar.volumes_need_mount_observe')}</p>
        <code className="mt-1 block select-all break-all rounded bg-gray-100 px-1.5 py-1 font-mono text-[10px] text-gray-700 dark:bg-gray-800 dark:text-gray-300">
          {SNAP_CONNECT_MOUNT_OBSERVE}
        </code>
      </div>
    );
  }
  return (
    <div className="px-3 py-2 text-[11px] text-gray-400 dark:text-gray-500">
      {t('sidebar.volumes_unavailable')}
    </div>
  );
}
