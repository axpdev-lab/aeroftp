// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)
//
// Frontend mirror of Rust `mtp_device_fingerprint` / `fingerprint_equal`
// (providers/mtp/backend.rs). Used when saving an MTP device profile
// (APPENDIX-DEVICE-PROFILES Phase 2) and for attach-match in Phase 3.

import type { DeviceFingerprint } from '../types';
import type { MtpDeviceInfo } from '../types/aerofile';

/** Format a USB id as 4-digit uppercase hex (matches Rust `{:04X}`). */
export function formatUsbHex4(id: number): string {
  return (id & 0xffff).toString(16).toUpperCase().padStart(4, '0');
}

function normalizeSerial(serial: string | null | undefined): string | undefined {
  const s = serial?.trim();
  return s ? s : undefined;
}

function normalizeModel(model: string | null | undefined): string | undefined {
  if (model == null) return undefined;
  const collapsed = model.split(/\s+/).filter(Boolean).join(' ');
  return collapsed || undefined;
}

/**
 * Build a canonical fingerprint string from parts.
 * Prefer serial; fall back to vid/pid (+ optional model). Mirrors Rust
 * `mtp_device_fingerprint`.
 */
export function mtpDeviceFingerprint(
  serial?: string | null,
  vendorId?: number | null,
  productId?: number | null,
  model?: string | null,
): string | undefined {
  const s = normalizeSerial(serial);
  if (s) return `mtp:serial=${s}`;

  if (
    vendorId != null &&
    productId != null &&
    Number.isFinite(vendorId) &&
    Number.isFinite(productId)
  ) {
    const base = `mtp:vidpid=${formatUsbHex4(vendorId)}:${formatUsbHex4(productId)}`;
    const m = normalizeModel(model);
    return m ? `${base};model=${m}` : base;
  }
  return undefined;
}

/**
 * Lowercase form used only for equality checks (not storage).
 * Mirrors Rust `canonicalize_fingerprint`.
 */
export function canonicalizeFingerprint(fp: string): string {
  const t = fp.trim();
  if (t.startsWith('mtp:serial=')) {
    const serial = t.slice('mtp:serial='.length).trim();
    if (!serial) return '';
    return `mtp:serial=${serial.toLowerCase()}`;
  }
  if (t.startsWith('mtp:vidpid=')) {
    const rest = t.slice('mtp:vidpid='.length).trim();
    const semi = rest.indexOf(';');
    const ids = (semi >= 0 ? rest.slice(0, semi) : rest).trim();
    const modelPart = semi >= 0 ? rest.slice(semi + 1).trim() : undefined;
    const parts = ids.split(':');
    if (parts.length !== 2) return '';
    const vid = parts[0].trim();
    const pid = parts[1].trim();
    if (!vid || !pid) return '';
    // Parse as hex u16 like Rust; reject non-hex.
    const vidN = Number.parseInt(vid, 16);
    const pidN = Number.parseInt(pid, 16);
    if (!Number.isFinite(vidN) || !Number.isFinite(pidN)) return '';
    if (vidN < 0 || vidN > 0xffff || pidN < 0 || pidN > 0xffff) return '';
    // Storage form uses uppercase; equality form uses lowercase hex.
    let out = `mtp:vidpid=${formatUsbHex4(vidN).toLowerCase()}:${formatUsbHex4(pidN).toLowerCase()}`;
    if (modelPart) {
      let modelRaw: string | undefined;
      if (modelPart.startsWith('model=') || modelPart.startsWith('MODEL=')) {
        modelRaw = modelPart.slice('model='.length);
      }
      const m = normalizeModel(modelRaw)?.toLowerCase();
      if (m) {
        out += `;model=${m}`;
      }
    }
    return out;
  }
  // Unknown form: still allow exact trim+lower equality (matches Rust).
  return t.toLowerCase();
}

/** True when two fingerprints refer to the same device under case/ws normalize. */
export function fingerprintEqual(a: string, b: string): boolean {
  const ca = canonicalizeFingerprint(a);
  const cb = canonicalizeFingerprint(b);
  return ca.length > 0 && ca === cb;
}

/**
 * Build a `DeviceFingerprint` blob for vault storage from a list row.
 * Uses the backend-supplied `fingerprint` when present; otherwise rebuilds.
 */
export function deviceFingerprintFromMtpInfo(
  device: Pick<
    MtpDeviceInfo,
    'serial' | 'vendorId' | 'productId' | 'displayName' | 'fingerprint'
  >,
): DeviceFingerprint | undefined {
  const model = normalizeModel(device.displayName);
  const serial = normalizeSerial(device.serial);
  const vid =
    device.vendorId != null && Number.isFinite(device.vendorId)
      ? formatUsbHex4(device.vendorId)
      : undefined;
  const pid =
    device.productId != null && Number.isFinite(device.productId)
      ? formatUsbHex4(device.productId)
      : undefined;

  const canonical =
    (device.fingerprint?.trim() || undefined) ??
    mtpDeviceFingerprint(serial, device.vendorId, device.productId, model);

  if (!canonical) return undefined;

  return {
    kind: 'mtp',
    ...(serial ? { serial } : {}),
    ...(vid ? { vid } : {}),
    ...(pid ? { pid } : {}),
    ...(model ? { model } : {}),
    canonical,
  };
}

/**
 * Find the live list row that matches a saved profile fingerprint.
 * Uses `device.fingerprint` when present, else rebuilds via `deviceFingerprintFromMtpInfo`.
 * Returns undefined when the device is not attached (or fingerprint missing).
 */
export function matchLiveDevice(
  profileCanonical: string | undefined | null,
  devices: readonly MtpDeviceInfo[],
): MtpDeviceInfo | undefined {
  const want = profileCanonical?.trim();
  if (!want) return undefined;
  for (const device of devices) {
    if (device.fingerprint && fingerprintEqual(want, device.fingerprint)) {
      return device;
    }
    const rebuilt = deviceFingerprintFromMtpInfo(device);
    if (rebuilt && fingerprintEqual(want, rebuilt.canonical)) {
      return device;
    }
  }
  return undefined;
}

/**
 * Profile ids whose stored MTP fingerprint matches a currently attached device.
 * Non-mtp profiles are ignored.
 */
export function computeAttachedProfileIds(
  profiles: readonly {
    id: string;
    protocol?: string;
    deviceFingerprint?: { canonical?: string } | null;
  }[],
  devices: readonly MtpDeviceInfo[],
): Set<string> {
  const ids = new Set<string>();
  if (devices.length === 0) return ids;
  for (const p of profiles) {
    if (p.protocol !== 'mtp') continue;
    const fp = p.deviceFingerprint?.canonical;
    if (matchLiveDevice(fp, devices)) {
      ids.add(p.id);
    }
  }
  return ids;
}
