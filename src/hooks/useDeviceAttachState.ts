// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// APPENDIX-DEVICE-PROFILES Phase 3: which saved MTP profiles are physically
// attached right now. Not HTTP health — fingerprint match against live
// `list_mtp_devices` (same cadence as PlacesSidebar portable poll).

import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { guardedUnlisten } from './useTauriListener';
import type { MtpDeviceInfo } from '../types/aerofile';
import { computeAttachedProfileIds, matchLiveDevice } from '../utils/mtpFingerprint';

/**
 * Match PlacesSidebar portable poll: event + focus, 30s fallback.
 *
 * `mtp-devices-changed` is the fast path and now fires on every platform that
 * has a watcher (Windows WM_DEVICECHANGE, Linux kernel uevents), so unplug
 * flips the card red in well under a second. This interval is only the safety
 * net for platforms without one: it stays slow on purpose because each refresh
 * runs a libmtp bus scan, which must not contend with an open device session.
 */
const DEVICE_ATTACH_POLL_MS = 30_000;

export type DeviceAttachProfile = {
  id: string;
  protocol?: string;
  deviceFingerprint?: { canonical?: string } | null;
};

export interface UseDeviceAttachStateResult {
  /** Live USB rows from the last successful `list_mtp_devices`. */
  liveDevices: MtpDeviceInfo[];
  /** Saved profile ids whose fingerprint matches a live device. */
  attachedProfileIds: ReadonlySet<string>;
  /** Force a refresh (e.g. right before click-connect). */
  refresh: () => Promise<MtpDeviceInfo[]>;
  /** Match one profile against the current live list (may be stale; prefer refresh on connect). */
  findLiveDevice: (profile: DeviceAttachProfile) => MtpDeviceInfo | undefined;
}

/**
 * Poll + event-driven attach set for My Servers MTP device cards.
 * When `enabled` is false (no MTP profiles), no poll/listeners run.
 */
export function useDeviceAttachState(
  profiles: readonly DeviceAttachProfile[],
  enabled = true,
): UseDeviceAttachStateResult {
  const [liveDevices, setLiveDevices] = useState<MtpDeviceInfo[]>([]);
  const mountedRef = useRef(true);
  const profilesRef = useRef(profiles);
  profilesRef.current = profiles;

  useEffect(() => {
    mountedRef.current = true;
    return () => {
      mountedRef.current = false;
    };
  }, []);

  const refresh = useCallback(async (): Promise<MtpDeviceInfo[]> => {
    try {
      const devices = await invoke<MtpDeviceInfo[]>('list_mtp_devices');
      if (mountedRef.current) setLiveDevices(devices);
      return devices;
    } catch {
      if (mountedRef.current) setLiveDevices([]);
      return [];
    }
  }, []);

  useEffect(() => {
    if (!enabled) {
      setLiveDevices([]);
      return;
    }

    void refresh();

    const disposeListener = guardedUnlisten(
      listen<void>('mtp-devices-changed', () => {
        if (mountedRef.current) void refresh();
      }),
    );

    const intervalId = window.setInterval(() => {
      if (mountedRef.current) void refresh();
    }, DEVICE_ATTACH_POLL_MS);

    const onFocus = () => {
      if (mountedRef.current) void refresh();
    };
    window.addEventListener('focus', onFocus);

    return () => {
      window.removeEventListener('focus', onFocus);
      disposeListener();
      window.clearInterval(intervalId);
    };
  }, [enabled, refresh]);

  const attachedProfileIds = useMemo(
    () => computeAttachedProfileIds(profiles, liveDevices),
    [profiles, liveDevices],
  );

  const findLiveDevice = useCallback(
    (profile: DeviceAttachProfile) =>
      matchLiveDevice(profile.deviceFingerprint?.canonical, liveDevices),
    [liveDevices],
  );

  return { liveDevices, attachedProfileIds, refresh, findLiveDevice };
}
