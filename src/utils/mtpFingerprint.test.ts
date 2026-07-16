// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import { describe, expect, it } from 'vitest';
import {
  canonicalizeFingerprint,
  computeAttachedProfileIds,
  deviceFingerprintFromMtpInfo,
  fingerprintEqual,
  formatUsbHex4,
  matchLiveDevice,
  mtpDeviceFingerprint,
} from './mtpFingerprint';

describe('formatUsbHex4', () => {
  it('pads and uppercases', () => {
    expect(formatUsbHex4(0x0fce)).toBe('0FCE');
    expect(formatUsbHex4(0x020d)).toBe('020D');
    expect(formatUsbHex4(18)).toBe('0012');
  });
});

describe('mtpDeviceFingerprint', () => {
  it('prefers serial over vid/pid', () => {
    expect(mtpDeviceFingerprint('QV770LUNJD', 0x0fce, 0x020d, 'Xperia')).toBe(
      'mtp:serial=QV770LUNJD',
    );
  });

  it('trims serial whitespace', () => {
    expect(mtpDeviceFingerprint('  AbC123  ')).toBe('mtp:serial=AbC123');
  });

  it('falls back to vidpid with model collapsed', () => {
    expect(mtpDeviceFingerprint(null, 0x0fce, 0x020d, '  SONY   Xperia  ')).toBe(
      'mtp:vidpid=0FCE:020D;model=SONY Xperia',
    );
  });

  it('vidpid without model when model empty', () => {
    expect(mtpDeviceFingerprint(undefined, 0x18d1, 0x4ee1, '   ')).toBe(
      'mtp:vidpid=18D1:4EE1',
    );
  });

  it('returns undefined when neither serial nor vid/pid', () => {
    expect(mtpDeviceFingerprint(null, null, null, 'phone')).toBeUndefined();
    expect(mtpDeviceFingerprint('')).toBeUndefined();
  });
});

describe('fingerprintEqual', () => {
  it('serial is case- and whitespace-insensitive', () => {
    expect(fingerprintEqual('mtp:serial=AbC123', '  mtp:serial=abc123  ')).toBe(true);
    expect(fingerprintEqual('mtp:serial=AbC123', 'mtp:serial=other')).toBe(false);
  });

  it('vidpid hex case and model whitespace', () => {
    expect(
      fingerprintEqual(
        'mtp:vidpid=0FCE:01B0;model=SONY Xperia',
        'mtp:vidpid=0fce:01b0;model=  sony   xperia  ',
      ),
    ).toBe(true);
    expect(
      fingerprintEqual(
        'mtp:vidpid=0FCE:01B0;model=SONY',
        'mtp:vidpid=0FCE:01B0;model=Other',
      ),
    ).toBe(false);
  });

  it('empty serial form never matches; unknown form is trim+lower', () => {
    expect(fingerprintEqual('', '')).toBe(false);
    expect(fingerprintEqual('mtp:serial=', 'mtp:serial=')).toBe(false);
    // Unknown forms still lower-equal (mirrors Rust fallback).
    expect(canonicalizeFingerprint('Not-A-Fp')).toBe('not-a-fp');
    expect(fingerprintEqual('Not-A-Fp', 'not-a-fp')).toBe(true);
  });
});

describe('deviceFingerprintFromMtpInfo', () => {
  it('builds structured blob from list row with serial', () => {
    const fp = deviceFingerprintFromMtpInfo({
      displayName: 'XQ-DQ54',
      serial: 'QV770LUNJD',
      vendorId: 0x0fce,
      productId: 0x020d,
      fingerprint: 'mtp:serial=QV770LUNJD',
    });
    expect(fp).toEqual({
      kind: 'mtp',
      serial: 'QV770LUNJD',
      vid: '0FCE',
      pid: '020D',
      model: 'XQ-DQ54',
      canonical: 'mtp:serial=QV770LUNJD',
    });
  });

  it('rebuilds canonical when list omits fingerprint', () => {
    const fp = deviceFingerprintFromMtpInfo({
      displayName: 'Pixel',
      serial: null,
      vendorId: 0x18d1,
      productId: 0x4ee1,
      fingerprint: null,
    });
    expect(fp?.canonical).toBe('mtp:vidpid=18D1:4EE1;model=Pixel');
    expect(fp?.kind).toBe('mtp');
  });
});

describe('matchLiveDevice / computeAttachedProfileIds', () => {
  const xperia = {
    deviceId: 'usb:001:012',
    displayName: 'XQ-DQ54',
    serial: 'QV770LUNJD',
    vendorId: 0x0fce,
    productId: 0x020d,
    fingerprint: 'mtp:serial=QV770LUNJD',
    platform: 'linux',
    storagesHint: 1,
  };
  const pixel = {
    deviceId: 'usb:001:013',
    displayName: 'Pixel',
    serial: null as string | null,
    vendorId: 0x18d1,
    productId: 0x4ee1,
    fingerprint: null as string | null,
    platform: 'linux',
    storagesHint: 1,
  };

  it('matches serial fingerprint case-insensitively', () => {
    const hit = matchLiveDevice('mtp:serial=qv770lunjd', [xperia, pixel]);
    expect(hit?.deviceId).toBe('usb:001:012');
  });

  it('matches rebuilt vid/pid when list omits fingerprint', () => {
    const hit = matchLiveDevice('mtp:vidpid=18D1:4EE1;model=Pixel', [pixel]);
    expect(hit?.deviceId).toBe('usb:001:013');
  });

  it('returns undefined when not attached', () => {
    expect(matchLiveDevice('mtp:serial=OTHER', [xperia])).toBeUndefined();
    expect(matchLiveDevice(undefined, [xperia])).toBeUndefined();
  });

  it('collects attached profile ids for mtp only', () => {
    const ids = computeAttachedProfileIds(
      [
        { id: 'p1', protocol: 'mtp', deviceFingerprint: { canonical: 'mtp:serial=QV770LUNJD' } },
        { id: 'p2', protocol: 'mtp', deviceFingerprint: { canonical: 'mtp:serial=MISSING' } },
        { id: 'p3', protocol: 'sftp', deviceFingerprint: { canonical: 'mtp:serial=QV770LUNJD' } },
        { id: 'p4', protocol: 'mtp' },
      ],
      [xperia],
    );
    expect([...ids]).toEqual(['p1']);
  });
});
