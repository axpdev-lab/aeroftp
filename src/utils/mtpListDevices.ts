// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// FE-side coalesce for `list_mtp_devices`. Backend already single-flights +
// TTL-caches detects (Known #7 amplifier); this keeps the webview from stacking
// redundant invokes when PlacesSidebar, useDeviceAttachState, and App all react
// to the same `mtp-devices-changed` burst.

import { invoke } from '@tauri-apps/api/core';
import type { MtpDeviceInfo } from '../types/aerofile';

/** Collapse a burst of hotplug events into one list call (ms). */
export const MTP_DEVICES_CHANGED_DEBOUNCE_MS = 300;

/** Shared in-flight promise: concurrent callers reuse one invoke. */
let inFlight: Promise<MtpDeviceInfo[]> | null = null;

/**
 * List attached MTP devices with FE single-flight.
 * Concurrent callers share the same promise; a new invoke starts only after
 * the previous one settles.
 */
export function listMtpDevices(): Promise<MtpDeviceInfo[]> {
  if (inFlight) return inFlight;
  inFlight = invoke<MtpDeviceInfo[]>('list_mtp_devices')
    .then((devices) => devices ?? [])
    .catch((err) => {
      // Propagate so callers can decide empty-vs-error; clear inFlight first.
      throw err;
    })
    .finally(() => {
      inFlight = null;
    });
  return inFlight;
}

/** True while a list invoke is outstanding (poll guards can skip). */
export function isMtpListInFlight(): boolean {
  return inFlight !== null;
}

/**
 * Debounce a void callback. Returns a disposer that clears the pending timer.
 * Used for `mtp-devices-changed` so N uevents → one list.
 */
export function debounceMtpDevicesChanged(
  fn: () => void,
  waitMs: number = MTP_DEVICES_CHANGED_DEBOUNCE_MS,
): { schedule: () => void; cancel: () => void } {
  let timer: ReturnType<typeof setTimeout> | null = null;
  return {
    schedule: () => {
      if (timer !== null) clearTimeout(timer);
      timer = setTimeout(() => {
        timer = null;
        fn();
      }, waitMs);
    },
    cancel: () => {
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
    },
  };
}
