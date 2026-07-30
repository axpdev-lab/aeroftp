// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)
//
// Which providers encrypt on the client, and at what cipher strength.
//
// Add Service has carried an "E2E 128-bit" / "E2E 256-bit" chip on these
// entries for a long time; My Servers said the same thing only in prose, in the
// subtitle under the name, so a saved MEGA profile did not look encrypted at a
// glance (discussion #347). Both now read this list, and a test holds the Add
// Service catalogue to it, so the two surfaces cannot drift apart again.
//
// This is about ZERO-KNOWLEDGE, not about transport. TLS is not on this list,
// and neither is a provider that merely encrypts at rest with its own keys: the
// question the badge answers is whether the operator can read the files.
//
// It is keyed by PROTOCOL, which is what decides the answer. MEGA's paid S4
// object storage speaks S3 and stores what it is given: a profile pointed at it
// has `protocol: 's3'` and correctly gets no badge, even though the company is
// the same one whose native protocol does encrypt client-side.

export type NativeE2eBits = 128 | 256;

export const NATIVE_E2E_BY_PROTOCOL: Readonly<Record<string, NativeE2eBits>> = Object.freeze({
    mega: 128,
    filen: 256,
    internxt: 256,
});

/** The cipher strength a protocol encrypts with client-side, or null.
 *  An own-property check rather than a plain lookup: the argument is a protocol string that
 *  can come from a stored profile, and 'constructor' would otherwise answer
 *  with something from Object.prototype. */
export const getNativeE2eBits = (protocol?: string | null): NativeE2eBits | null =>
    protocol && Object.prototype.hasOwnProperty.call(NATIVE_E2E_BY_PROTOCOL, protocol)
        ? NATIVE_E2E_BY_PROTOCOL[protocol]
        : null;

/** The Add Service chip text for a protocol with native client-side encryption. */
export const nativeE2eBadge = (bits: NativeE2eBits): string => `E2E ${bits}-bit`;
