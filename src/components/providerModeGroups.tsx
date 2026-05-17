// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet: AI-assisted (see AI-TRANSPARENCY.md)

/**
 * Provider mode-group registry: groups of presets that share an
 * underlying account and differ only in the transport surface used to
 * reach it (Native API vs WebDAV bridge vs S3 bridge vs ...).
 *
 * The renderer (`ProviderModeTabs`) reads this registry and shows an
 * inline chip strip when the active preset belongs to one of the
 * groups. Adding a new group means:
 *   1. Define modes here (one entry per surface)
 *   2. Make sure each mode's `providerId` exists in `providers/registry.ts`
 *      (or omit it for "default protocol selected without a preset")
 *   3. Optional: add per-mode warnings under `activeWarnings`
 *
 * No other code change is needed; the component picks the active
 * group automatically based on the form's current providerId+protocol.
 */
import React from 'react';
import { Key, Globe, Database, Server, Cloud, Layers } from 'lucide-react';
import type { ProviderType } from '../types';

export interface ProviderMode {
    /** Registry providerId. Omit (undefined) for the "protocol only"
     *  case where the user picks the protocol directly via the
     *  ProtocolSelector and no preset is involved (e.g. Filen native
     *  API uses `protocol: 'filen'` with no providerId). */
    providerId?: string;
    /** ProviderType value to switch the form to. Spans the wider
     *  protocol set (Filen, MEGA, OAuth, ...) rather than just
     *  BaseProtocol so we can group OAuth/native-API providers too. */
    protocol: ProviderType;
    /** lucide-react icon for the chip. */
    icon: React.ReactNode;
    /** Tailwind text-color class for the active chip icon. */
    activeColor: string;
    /** Short chip label. */
    label: string;
    /** One-line description rendered below the chip strip when this
     *  mode is active. */
    description: string;
    /** Optional badge text (e.g. "BETA", "LOCAL"). */
    badge?: string;
    /** Per-mode override of the form's top-right header source. Use only
     *  when this surface genuinely has a different docs page / identity
     *  from the rest of the group. Falls back to the group header. */
    headerProviderId?: string;
    headerName?: string;
}

export interface ProviderModeGroup {
    /** Stable group identifier; used for the `key` prop and as a
     *  potential i18n root in the future. */
    id: string;
    /** "MODES" header label shown at the top of the chip strip. */
    headerLabel: string;
    /** Mode entries (ordered left-to-right). */
    modes: ProviderMode[];
    /** Optional i18n key for an extra warning block rendered under the
     *  description when a specific mode is active. Keyed by mode label. */
    activeWarnings?: Record<string, string>;
    /** Canonical source for the form's top-right header (logo + name +
     *  description + Docs link) for EVERY mode in the group. Without
     *  this, a preset-less native mode loses the registry-derived header
     *  (no Docs link, generic name) while the WebDAV mode keeps it, so
     *  the header flickered between tabs. `headerProviderId` is a
     *  registry id whose metadata represents the whole group;
     *  `headerName` optionally overrides just the displayed name (e.g.
     *  show "OpenDrive" instead of the WebDAV preset's
     *  "OpenDrive (WebDAV)"). A mode may still override per surface. */
    headerProviderId?: string;
    headerName?: string;
}

export const PROVIDER_MODE_GROUPS: ProviderModeGroup[] = [
    {
        id: 'filelu',
        headerLabel: 'FileLu Modes',
        modes: [
            {
                providerId: 'filelu',
                protocol: 'filelu',
                icon: <Key size={14} />,
                activeColor: 'text-sky-500',
                label: 'Native API',
                description:
                    'REST API with API key. Full feature set (trash, share links, password-protected files).',
            },
            {
                providerId: 'filelu-webdav',
                protocol: 'webdav',
                icon: <Globe size={14} />,
                activeColor: 'text-emerald-500',
                label: 'WebDAV',
                description:
                    'WebDAV access on port 443. Wide client compatibility, mounts as a network drive.',
            },
            {
                providerId: 'filelu-s3',
                protocol: 's3',
                icon: <Database size={14} />,
                activeColor: 'text-amber-500',
                label: 'S3',
                description:
                    'S3-compatible API on port 443. Use rclone, AWS CLI, or any S3 SDK.',
            },
            {
                providerId: 'filelu-ftp',
                protocol: 'ftp',
                icon: <Server size={14} />,
                activeColor: 'text-blue-500',
                label: 'FTP',
                description:
                    'Classic FTP on port 21. Plaintext, useful for CCTV NVRs and legacy clients.',
            },
        ],
        activeWarnings: {
            Rsync:
                'Transfer-only endpoint: browsing is not available on Rsync. Use Native API, WebDAV, S3 or FTP to navigate; switch to Rsync for high-bandwidth delta transfers.',
        },
    },
    {
        id: 'filen',
        headerLabel: 'Filen Modes',
        modes: [
            {
                // Native Filen API is selected directly via the
                // ProtocolSelector (`type: 'filen'`): no registry preset
                // is involved, so providerId is intentionally omitted.
                protocol: 'filen',
                icon: <Cloud size={14} />,
                activeColor: 'text-emerald-500',
                label: 'Native API',
                description:
                    'Zero-knowledge E2E encryption (Argon2id v3 / PBKDF2 v2). Full feature set: storage quota, trash, share-link passwords, versioning, TOTP.',
                badge: 'E2E',
            },
            {
                providerId: 'filen-desktop-webdav',
                protocol: 'webdav',
                icon: <Globe size={14} />,
                activeColor: 'text-blue-500',
                label: 'Local WebDAV',
                description:
                    'WebDAV bridge exposed by Filen Desktop on 127.0.0.1:1900. Requires Filen Desktop running and signed in.',
                badge: 'LOCAL',
            },
            {
                providerId: 'filen-desktop-s3',
                protocol: 's3',
                icon: <Layers size={14} />,
                activeColor: 'text-amber-500',
                label: 'Local S3',
                description:
                    'S3-compatible bridge exposed by Filen Desktop on 127.0.0.1:1800 via local.s3.filen.io. Path-style addressing, bucket "filen".',
                badge: 'LOCAL',
            },
        ],
        activeWarnings: {
            'Local WebDAV':
                'Requires Filen Desktop running and signed in on this machine. The bridge is local-only: nothing leaves the loopback. Use the Native API mode for E2E encryption without Filen Desktop.',
            'Local S3':
                'Requires Filen Desktop running and signed in on this machine. On first connect the bridge auto-creates a top-level folder named "filen" on your account: existing files live one level above the bridge view.',
        },
    },
    {
        id: 'opendrive',
        headerLabel: 'OpenDrive Modes',
        // WebDAV preset carries the description/Docs link/logo; show the
        // plain "OpenDrive" name so the Native API tab is not mislabeled
        // "OpenDrive (WebDAV)".
        headerProviderId: 'opendrive-webdav',
        headerName: 'OpenDrive',
        modes: [
            {
                // OpenDrive native API selected via ProtocolSelector
                // (`type: 'opendrive'`); no registry preset is involved.
                protocol: 'opendrive',
                icon: <Cloud size={14} />,
                activeColor: 'text-cyan-500',
                label: 'Native API',
                description:
                    'REST API with username + password. Full feature set: trash, storage quota, share links, recursive disk usage.',
            },
            {
                providerId: 'opendrive-webdav',
                protocol: 'webdav',
                icon: <Globe size={14} />,
                activeColor: 'text-blue-500',
                label: 'WebDAV',
                description:
                    'WebDAV access via webdav.opendrive.com on port 443. Use for legacy clients or to mount as a network drive.',
            },
        ],
    },
    {
        id: 'koofr',
        headerLabel: 'Koofr Modes',
        // Single registry entry 'koofr' (the WebDAV preset) carries the
        // canonical Koofr identity + Docs link; use it for both tabs so
        // the header stays identical (issue #213 follow-up).
        headerProviderId: 'koofr',
        modes: [
            {
                // Koofr native API selected via ProtocolSelector
                // (`type: 'koofr'`); no registry preset is involved. The
                // only registry entry id 'koofr' is the WebDAV preset, so
                // the native mode is intentionally preset-less (issue #213).
                protocol: 'koofr',
                icon: <Cloud size={14} />,
                activeColor: 'text-teal-500',
                label: 'Native API',
                description:
                    'EU-based REST API with email + app password. Full feature set: storage quota, trash (list/restore/empty), share links.',
            },
            {
                providerId: 'koofr',
                protocol: 'webdav',
                icon: <Globe size={14} />,
                activeColor: 'text-blue-500',
                label: 'WebDAV',
                description:
                    'WebDAV access via app.koofr.net/dav/Koofr on port 443. Storage quota is still read from the Koofr REST API. Use for legacy clients or to mount as a network drive.',
            },
        ],
    },
];

/**
 * Find the mode group containing the active form configuration, if any.
 *
 * Match rules:
 *   - If a mode has `providerId`, match by `providerId === activeProviderId`.
 *   - Else (preset-less mode), match by `protocol === activeProtocol`
 *     **and** there is no `activeProviderId` (so we don't claim
 *     ownership of e.g. a generic `webdav` selection when no FileLu /
 *     Filen preset is in play).
 */
export function findActiveModeGroup(
    activeProviderId: string | null | undefined,
    activeProtocol: string | null | undefined,
): ProviderModeGroup | null {
    for (const group of PROVIDER_MODE_GROUPS) {
        for (const mode of group.modes) {
            if (mode.providerId) {
                if (mode.providerId === activeProviderId) return group;
            } else {
                // Preset-less mode: match by protocol. Lenient on
                // `activeProviderId` because legacy saved profiles for
                // native protocols (e.g. Filen) sometimes persist
                // `providerId === protocol` and we want edit mode to
                // still surface the tabs.
                if (
                    mode.protocol === activeProtocol &&
                    (!activeProviderId || activeProviderId === mode.protocol)
                ) {
                    return group;
                }
            }
        }
    }
    return null;
}

/** Returns the currently active mode within a group, or null. */
export function findActiveMode(
    group: ProviderModeGroup,
    activeProviderId: string | null | undefined,
    activeProtocol: string | null | undefined,
): ProviderMode | null {
    for (const mode of group.modes) {
        if (mode.providerId) {
            if (mode.providerId === activeProviderId) return mode;
        } else {
            if (
                mode.protocol === activeProtocol &&
                (!activeProviderId || activeProviderId === mode.protocol)
            ) {
                return mode;
            }
        }
    }
    return null;
}

/**
 * Canonical header source for the form's top-right logo/name/description/
 * Docs link when the active config belongs to a mode group. Resolution:
 * active mode override -> group default. Returns null when no group is
 * active (caller keeps its existing preset/protocol fallback).
 *
 * `providerId` is a registry id to pull metadata from; `name` optionally
 * overrides only the displayed name. This keeps the header identical
 * across the group's tabs while staying overridable per surface when a
 * mode genuinely has a different docs page.
 */
export function resolveModeHeader(
    activeProviderId: string | null | undefined,
    activeProtocol: string | null | undefined,
): { providerId?: string; name?: string } | null {
    const group = findActiveModeGroup(activeProviderId, activeProtocol);
    if (!group) return null;
    const mode = findActiveMode(group, activeProviderId, activeProtocol);
    const providerId = mode?.headerProviderId ?? group.headerProviderId;
    const name = mode?.headerName ?? group.headerName;
    if (!providerId && !name) return null;
    return { providerId, name };
}
