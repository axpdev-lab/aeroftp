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
import { methodIcon } from './connectionMethodIcons';
import type { ProviderType } from '../types';
import { translate, type TranslationFunction } from '../i18n';

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
    /** Short chip label, and the mode's name in its group (`activeWarnings`
     *  keys, the chip key): a method name, shown as it is unless
     *  `labelText` is set. */
    label: string;
    /** The chip label in the reader's language, when it is words rather
     *  than a method name. Read it through {@link modeLabel}. */
    labelText?: (t: TranslationFunction) => string;
    /** One-line description rendered below the chip strip when this
     *  mode is active. */
    description: (t: TranslationFunction) => string;
    /** Optional badge text (e.g. "BETA", "LOCAL"). */
    badge?: string;
    /** Per-mode override of the form's top-right header source. Use only
     *  when this surface genuinely has a different docs page / identity
     *  from the rest of the group. Falls back to the group header. */
    headerProviderId?: string;
    headerName?: string;
    /** When set, this mode is backed by a local helper app bridge (Filen
     *  Desktop / MEGAcmd). The backend `bridge_status` command probes it and
     *  the UI renders a 🔴/🟠/🟢 status dot + app-aware message in place of the
     *  static "Requires …" warning (#215 follow-up). The value is the bridge
     *  `kind` understood by `bridge_status`. */
    bridgeKind?: BridgeKind;
    /** Shown in the strip but not selectable, and never resolved as the
     *  active mode: a surface that is announced but not available yet. Its
     *  `description` says why. */
    disabled?: boolean;
    /** Profile options this tab sets, and by which it is told apart from a
     *  tab of the same protocol: Proton Drive's API and CLI tabs are both
     *  `proton`, the API one is `proton_mode: 'native'`. A tab without
     *  options is the one a profile without them belongs to, so it must come
     *  after the tabs that have some. */
    options?: Record<string, string>;
    /** A tab only some builds serve: selectable once the backend answers for
     *  it (`useProtonNativeAvailable`). A saved profile of that tab still
     *  resolves to it, so its form can say the build lacks the client. */
    requires?: 'proton-native';
}

/** Local bridge helper apps probed by the backend `bridge_status` command. */
export type BridgeKind = 'filen-webdav' | 'filen-s3' | 'megacmd-webdav' | 'proton-cli';

export interface ProviderModeGroup {
    /** Stable group identifier; used for the `key` prop and as a
     *  potential i18n root in the future. */
    id: string;
    /** "MODES" header label shown at the top of the chip strip. */
    headerLabel: (t: TranslationFunction) => string;
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
    /** Optional protocol-neutral header description for the whole group.
     *  When the canonical `headerProviderId` preset carries a
     *  protocol-specific description (e.g. the OpenDrive WebDAV preset says
     *  "...via WebDAV"), that string is wrong for the other modes (#270,
     *  Quick Connect OpenDrive API showed "via WebDAV"). Set this to a
     *  neutral line; the protocol is already obvious from the MODES chips. */
    headerDescription?: (t: TranslationFunction) => string;
    /** When true, every mode in the group authenticates with the SAME
     *  credential set (e.g. Koofr / OpenDrive: the Native API and WebDAV
     *  surfaces both take the same email + app password). In that case the
     *  "Remember credentials for every protocol" opt-in is meaningless —
     *  there is a single credential set, already saved with the profile —
     *  so the checkbox is hidden (issue #215, Ehud). Leave unset for groups
     *  whose modes have structurally different credentials (Filen API key vs
     *  WebDAV user/pass, MEGA email/pass vs S4 access keys). */
    sharedCredentials?: boolean;
}

export const PROVIDER_MODE_GROUPS: ProviderModeGroup[] = [
    {
        id: 'filelu',
        headerLabel: (t) => t('provider.modes.header', { name: 'FileLu' }),
        modes: [
            {
                // Preset-less like every other group's native mode (Filen, Koofr,
                // OpenDrive, MEGA): matched by protocol so the FILELU MODES tabs also
                // appear when entering via the Native API discover preset (which sets
                // no providerId). Declaring providerId:'filelu' here forced the strict
                // providerId match, which never fired on the native entry point.
                protocol: 'filelu',
                icon: methodIcon('API', { size: 14 }),
                activeColor: 'text-sky-500',
                label: 'API',
                labelText: (t) => t('provider.modes.nativeApi'),
                description: (t) => t('provider.modes.filelu.apiDescription'),
            },
            {
                providerId: 'filelu-webdav',
                protocol: 'webdav',
                icon: methodIcon('WebDAV', { size: 14 }),
                activeColor: 'text-emerald-500',
                label: 'WebDAV',
                description: (t) => t('provider.modes.filelu.webdavDescription'),
            },
            {
                providerId: 'filelu-s3',
                protocol: 's3',
                icon: methodIcon('S3', { size: 14 }),
                activeColor: 'text-amber-500',
                // Branded "S5" by FileLu; the wire protocol stays S3. Same text
                // the Add Service badge uses (#347): the tab and the badge for
                // one method should not read differently.
                label: 'S5 (S3)',
                description: (t) => t('provider.modes.filelu.s3Description'),
            },
            {
                providerId: 'filelu-ftp',
                protocol: 'ftp',
                icon: methodIcon('FTP', { size: 14 }),
                activeColor: 'text-blue-500',
                label: 'FTP',
                description: (t) => t('provider.modes.filelu.ftpDescription'),
            },
        ],
        activeWarnings: {
            Rsync:
                'Transfer-only endpoint: browsing is not available on Rsync. Use Native API, WebDAV, S3 or FTP to navigate; switch to Rsync for high-bandwidth delta transfers.',
        },
    },
    {
        id: 'filen',
        headerLabel: (t) => t('provider.modes.header', { name: 'Filen' }),
        modes: [
            {
                // Native Filen API is selected directly via the
                // ProtocolSelector (`type: 'filen'`): no registry preset
                // is involved, so providerId is intentionally omitted.
                protocol: 'filen',
                icon: methodIcon('API', { size: 14 }),
                activeColor: 'text-emerald-500',
                label: 'API',
                labelText: (t) => t('provider.modes.nativeApi'),
                description: (t) => t('provider.modes.filen.apiDescription'),
                badge: 'E2E',
            },
            {
                providerId: 'filen-desktop-webdav',
                protocol: 'webdav',
                icon: methodIcon('WebDAV', { size: 14 }),
                activeColor: 'text-blue-500',
                label: 'WebDAV',
                description: (t) => t('provider.modes.filen.webdavDescription'),
                badge: 'LOCAL',
                bridgeKind: 'filen-webdav',
            },
            {
                providerId: 'filen-desktop-s3',
                protocol: 's3',
                icon: methodIcon('S3', { size: 14 }),
                activeColor: 'text-amber-500',
                label: 'S3',
                description: (t) => t('provider.modes.filen.s3Description'),
                badge: 'LOCAL',
                bridgeKind: 'filen-s3',
            },
        ],
        activeWarnings: {
            'WebDAV':
                'Requires Filen Desktop running and signed in on this machine. The bridge is local-only: nothing leaves the loopback. Use the Native API mode for E2E encryption without Filen Desktop.',
            'S3':
                'Requires Filen Desktop running and signed in on this machine. Each top-level folder of your Filen drive is a bucket: pick one with Fetch. Files directly in the Filen root are not reachable over S3.',
        },
    },
    {
        // Same form as Filen: the API first, then the CLI. The API tab is
        // AeroFTP's native client (ported from Proton's open-source Drive
        // SDK), served by builds with the `proton-native` feature; the CLI tab
        // drives Proton's official CLI. Both tabs use the `proton` protocol and
        // are told apart by the `proton_mode` option.
        id: 'proton',
        headerLabel: (t) => t('provider.modes.header', { name: 'Proton Drive' }),
        headerName: 'Proton Drive',
        // Neutral across the tabs: the catalog's own text names the CLI,
        // which is wrong for the API tab. Each tab says how it connects.
        headerDescription: (t) => t('provider.modes.proton.headerDescription'),
        modes: [
            {
                protocol: 'proton',
                icon: methodIcon('API', { size: 14 }),
                activeColor: 'text-purple-500',
                label: 'API',
                // Each tab also says where its paths start (Requested 79).
                description: (t) => `${t('provider.modes.proton.apiDescription')} ${t('provider.modes.proton.apiPaths')}`,
                options: { proton_mode: 'native' },
                requires: 'proton-native',
            },
            {
                protocol: 'proton',
                icon: methodIcon('CLI', { size: 14 }),
                activeColor: 'text-purple-500',
                label: 'CLI',
                description: (t) => `${t('provider.modes.proton.cliDescription')} ${t('provider.modes.proton.cliPaths')}`,
                badge: 'LOCAL',
                // Status banner: installed, signed in, or neither.
                bridgeKind: 'proton-cli',
            },
        ],
    },
    {
        id: 'opendrive',
        // Native API and WebDAV both use the same username + password, so the
        // per-protocol credential opt-in is hidden (#215).
        sharedCredentials: true,
        headerLabel: (t) => t('provider.modes.header', { name: 'OpenDrive' }),
        // WebDAV preset carries the description/Docs link/logo; show the
        // plain "OpenDrive" name so the Native API tab is not mislabeled
        // "OpenDrive (WebDAV)".
        headerProviderId: 'opendrive-webdav',
        headerName: 'OpenDrive',
        // Neutral header: the WebDAV preset's "...via WebDAV" description is
        // wrong when the Native API mode is selected (#270).
        headerDescription: (t) => t('provider.modes.opendrive.headerDescription'),
        modes: [
            {
                // OpenDrive native API selected via ProtocolSelector
                // (`type: 'opendrive'`); no registry preset is involved.
                protocol: 'opendrive',
                icon: methodIcon('API', { size: 14 }),
                activeColor: 'text-cyan-500',
                label: 'API',
                labelText: (t) => t('provider.modes.nativeApi'),
                description: (t) => t('provider.modes.opendrive.apiDescription'),
            },
            {
                providerId: 'opendrive-webdav',
                protocol: 'webdav',
                icon: methodIcon('WebDAV', { size: 14 }),
                activeColor: 'text-blue-500',
                label: 'WebDAV',
                description: (t) => t('provider.modes.opendrive.webdavDescription'),
            },
        ],
    },
    {
        id: 'koofr',
        // Native API and WebDAV both use the same email + app password, so the
        // per-protocol credential opt-in is hidden (#215).
        sharedCredentials: true,
        headerLabel: (t) => t('provider.modes.header', { name: 'Koofr' }),
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
                icon: methodIcon('API', { size: 14 }),
                activeColor: 'text-teal-500',
                label: 'API',
                labelText: (t) => t('provider.modes.nativeApi'),
                description: (t) => t('provider.modes.koofr.apiDescription'),
            },
            {
                providerId: 'koofr',
                protocol: 'webdav',
                icon: methodIcon('WebDAV', { size: 14 }),
                activeColor: 'text-blue-500',
                label: 'WebDAV',
                description: (t) => t('provider.modes.koofr.webdavDescription'),
            },
        ],
    },
    {
        // Issue #215: a single MEGA Quick Connect page that exposes the real
        // ways to reach one MEGA account: the native API, the local MEGAcmd
        // WebDAV bridge, and S4 object storage. A fourth surface Ehud asked
        // for, "Non-WebDAV MEGAcmd" (driving mega-ls/get/put directly), has no
        // backend yet and is intentionally absent until one exists.
        id: 'mega',
        headerLabel: (t) => t('provider.modes.header', { name: 'MEGA' }),
        headerProviderId: 'mega',
        headerName: 'MEGA',
        headerDescription: (t) => t('provider.modes.mega.headerDescription'),
        modes: [
            {
                // Native MEGA is selected via the ProtocolSelector (type
                // 'mega'). A fresh quick-connect carries no providerId while a
                // saved profile persists providerId 'mega', so the mode is
                // preset-less and matched by protocol: the lenient rule in
                // findActiveModeGroup covers both (activeProviderId undefined or
                // equal to the protocol).
                protocol: 'mega',
                icon: methodIcon('API', { size: 14 }),
                activeColor: 'text-red-500',
                label: 'API',
                description: (t) => t('provider.modes.mega.apiDescription'),
                badge: 'E2E',
            },
            {
                providerId: 'megacmd-webdav',
                protocol: 'webdav',
                icon: methodIcon('WebDAV', { size: 14 }),
                activeColor: 'text-blue-500',
                label: 'WebDAV (MEGAcmd)',
                description: (t) => t('provider.modes.mega.webdavDescription'),
                badge: 'LOCAL',
                bridgeKind: 'megacmd-webdav',
            },
            {
                providerId: 'mega-s4',
                protocol: 's3',
                icon: methodIcon('S3', { size: 14 }),
                activeColor: 'text-amber-500',
                // Branded "S4" by MEGA; matches the Add Service badge (#347).
                label: 'S4 (S3)',
                description: (t) => t('provider.modes.mega.s4Description'),
                badge: 'PRO',
            },
        ],
        activeWarnings: {
            'WebDAV (MEGAcmd)':
                'Requires MEGAcmd installed and logged in (run "login your-email" once in the MEGAcmd terminal). AeroFTP then runs "mega-webdav /" itself on every connect to start the bridge and read its address: no manual step. From the CLI the same profile works as "aeroftp --profile <name> ls".',
            S4:
                'MEGA S4 is a separate paid object-storage product (Pro plan). Create Access Keys and a bucket in the S4 Dashboard, then pick the matching region.',
        },
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

/** Minimal credential shape consumed by {@link resolveModeSwitchCredentials}. */
export interface ModeCredentialInput {
    username?: string;
    password?: string;
}

/**
 * Resolve which username/password to show after switching to another mode of the
 * same provider group (issue #215, security). The S3 "Secret Access Key" field
 * and the API "password" field are the SAME shared slot relabeled per protocol,
 * so blindly carrying it across modes leaked the Filen/MEGA/FileLu account
 * password into the S3 Secret Access Key on the first visit to a tab.
 *
 * Priority:
 *   1. A stash from a previous visit to the target mode (return visit) wins — the
 *      user's own typed keys come back, including an intentionally empty value.
 *   2. Else, if the group authenticates every mode with the SAME credential set
 *      (`sharedCredentials`, e.g. Koofr / OpenDrive) → carry the current values.
 *   3. Else (structurally different secrets, first visit) → blank, so an account
 *      password never bleeds into a field destined for a different server.
 */
export function resolveModeSwitchCredentials(
    group: ProviderModeGroup,
    restored: ModeCredentialInput | null | undefined,
    current: ModeCredentialInput,
): { username: string; password: string } {
    if (restored) {
        return { username: restored.username ?? '', password: restored.password ?? '' };
    }
    if (group.sharedCredentials) {
        return { username: current.username ?? '', password: current.password ?? '' };
    }
    return { username: '', password: '' };
}

/** Returns the currently active mode within a group, or null. */
export function findActiveMode(
    group: ProviderModeGroup,
    activeProviderId: string | null | undefined,
    activeProtocol: string | null | undefined,
    activeOptions?: object | null,
): ProviderMode | null {
    const options = activeOptions as Record<string, unknown> | null | undefined;
    for (const mode of group.modes) {
        // A disabled mode is never the answer.
        if (mode.disabled) continue;
        // A mode that names options is the active one only when the profile
        // carries them (Proton API and CLI are both `proton`).
        if (
            mode.options &&
            !Object.entries(mode.options).every(([key, value]) => options?.[key] === value)
        ) {
            continue;
        }
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

/** The label a mode's chip shows, in the reader's language. */
export function modeLabel(mode: ProviderMode, t: TranslationFunction): string {
    return mode.labelText ? mode.labelText(t) : mode.label;
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
    t: TranslationFunction = translate,
): { providerId?: string; name?: string; description?: string } | null {
    const group = findActiveModeGroup(activeProviderId, activeProtocol);
    if (!group) return null;
    const mode = findActiveMode(group, activeProviderId, activeProtocol);
    const providerId = mode?.headerProviderId ?? group.headerProviderId;
    const name = mode?.headerName ?? group.headerName;
    const description = group.headerDescription?.(t);
    if (!providerId && !name) return null;
    return { providerId, name, description };
}

/**
 * Every registry providerId reachable from the active mode group: the
 * canonical header preset plus each mode that carries a preset. Deduped,
 * order-stable (header first, then modes left-to-right).
 *
 * The caller resolves group-wide account links (signup / app-password
 * pages) from these ids so the "Create Account" / "Generate password"
 * buttons stay visible and identical across every tab of a group. Without
 * this, a preset-less native mode (Koofr API, OpenDrive API) carries no
 * provider and the buttons vanished when switching to it (#215). Every
 * surface in a group hits the same account, so the same signup / password
 * page is correct for all of them.
 */
export function modeGroupProviderIds(
    activeProviderId: string | null | undefined,
    activeProtocol: string | null | undefined,
): string[] {
    const group = findActiveModeGroup(activeProviderId, activeProtocol);
    if (!group) return [];
    const ids: string[] = [];
    if (group.headerProviderId) ids.push(group.headerProviderId);
    for (const mode of group.modes) {
        if (mode.providerId) ids.push(mode.providerId);
    }
    return [...new Set(ids)];
}
