// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import {
  debounceMtpDevicesChanged,
  isMtpListInFlight,
  listMtpDevices,
  MTP_DEVICES_CHANGED_DEBOUNCE_MS,
} from './mtpListDevices';

const invokeMock = vi.fn();

vi.mock('@tauri-apps/api/core', () => ({
  invoke: (...args: unknown[]) => invokeMock(...args),
}));

describe('mtpListDevices', () => {
  beforeEach(() => {
    invokeMock.mockReset();
  });

  afterEach(() => {
    vi.useRealTimers();
  });

  it('exports a 250-400ms debounce window', () => {
    expect(MTP_DEVICES_CHANGED_DEBOUNCE_MS).toBeGreaterThanOrEqual(250);
    expect(MTP_DEVICES_CHANGED_DEBOUNCE_MS).toBeLessThanOrEqual(400);
  });

  it('single-flights concurrent listMtpDevices calls', async () => {
    let resolveInvoke!: (v: unknown) => void;
    invokeMock.mockReturnValue(
      new Promise((resolve) => {
        resolveInvoke = resolve;
      }),
    );

    const a = listMtpDevices();
    const b = listMtpDevices();
    expect(isMtpListInFlight()).toBe(true);
    expect(invokeMock).toHaveBeenCalledTimes(1);
    expect(invokeMock).toHaveBeenCalledWith('list_mtp_devices');

    resolveInvoke([{ deviceId: 'd1' }]);
    const [ra, rb] = await Promise.all([a, b]);
    expect(ra).toEqual([{ deviceId: 'd1' }]);
    expect(rb).toEqual([{ deviceId: 'd1' }]);
    expect(isMtpListInFlight()).toBe(false);

    // After settle, a new call starts a fresh invoke.
    invokeMock.mockResolvedValueOnce([]);
    await listMtpDevices();
    expect(invokeMock).toHaveBeenCalledTimes(2);
  });

  it('debounceMtpDevicesChanged collapses a burst into one call', () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const { schedule, cancel } = debounceMtpDevicesChanged(fn, 300);

    schedule();
    schedule();
    schedule();
    expect(fn).not.toHaveBeenCalled();

    vi.advanceTimersByTime(299);
    expect(fn).not.toHaveBeenCalled();
    vi.advanceTimersByTime(1);
    expect(fn).toHaveBeenCalledTimes(1);

    cancel(); // no-op after fire
  });

  it('debounceMtpDevicesChanged cancel prevents the call', () => {
    vi.useFakeTimers();
    const fn = vi.fn();
    const { schedule, cancel } = debounceMtpDevicesChanged(fn, 300);
    schedule();
    cancel();
    vi.advanceTimersByTime(500);
    expect(fn).not.toHaveBeenCalled();
  });
});
