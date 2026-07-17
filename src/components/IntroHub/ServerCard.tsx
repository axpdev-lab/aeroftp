import * as React from 'react';
import { Edit2, Trash2, Copy, Loader2, Star, Heart, Clock, ShieldCheck, Lock, Check, X, ArrowUpRight, ArrowDownLeft, AlertTriangle, Users, RefreshCw, Wifi, Smartphone } from 'lucide-react';
import { ServerProfile, ProviderType, getProtocolClass, getE2EBits, profileHasQuota, resolveEffectiveQuota, effectiveManualCap, getServerCryptOverlay } from '../../types';
import type { PeerDriveState } from '../../hooks/usePeerDriveStates';
import { shortAfid } from '../../utils/aeroShare';
import { ProtocolIcon } from '../ProtocolSelector';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { getGitHubConnectionBadge, getMegaConnectionBadge, getInfiniCloudConnectionBadge } from '../../utils/providerConnectionMeta';
import { getFilenAuthVersion } from '../../utils/filenAuthVersion';
import { getServerSubtitle } from '../../utils/serverSubtitle';
import { useTranslation } from '../../i18n';
import { useCardLayout } from '../../hooks/useCardLayout';
import { useFavoriteMarker } from '../../hooks/useFavoriteMarker';
import { useIntroHubIconSize } from '../../hooks/useIntroHubIconSize';
import { formatBytes } from '../../utils/formatters';
import {
    DEFAULT_THRESHOLDS,
    getStorageTone,
    TONE_BG_CLASS,
    TONE_TEXT_CLASS,
    type StorageThresholds,
} from '../../hooks/useStorageThresholds';
import { HealthRadial } from './HealthRadial';

/** Compact storage usage bar for the detailed card layout footer. Reads from
 *  `server.lastQuota` (cached on the last successful connection). Returns
 *  null when no quota is cached: caller decides whether to render an empty
 *  slot. Many providers (S3, raw FTP/SFTP, WebDAV without quota support)
 *  never produce one, and a "- / -" placeholder is just visual noise. */
function StorageUsageBar({
    quota,
    supported,
    thresholds,
    manualTotal,
}: {
    quota: ServerProfile['lastQuota'] | undefined;
    supported: boolean;
    thresholds: StorageThresholds;
    manualTotal?: number;
}) {
    const t = useTranslation();
    // Apply the item 4a precedence (a user-set manual cap is a TRUE
    // override) here too, so the card stays consistent with the StatusBar
    // even when the cached lastQuota was persisted with total:0: e.g. a
    // scan that ran before the manual cap was set, or that was persisted
    // against a duplicate profile that lacked options.manualTotalBytes.
    // Without this the card showed "X / ∞" while the StatusBar correctly
    // showed "X / 80 GB".
    const filesSuffix = quota?.fileCount != null ? ` · ${quota.fileCount} ${t('browser.files')}` : '';
    const rawUsed = quota?.used && quota.used > 0 ? quota.used : 0;
    const rawTotal = quota?.total && quota.total > 0 ? quota.total : 0;
    const eff = resolveEffectiveQuota(rawUsed, rawTotal, manualTotal);
    const usedKnown = !!quota && quota.used > 0;

    // Nothing cached and no cap configured: faint placeholder for
    // quota-capable providers, nothing for the rest.
    if (!usedKnown && eff.total <= 0) {
        if (!supported) return null;
        const title = t('introHub.storageQuotaUnavailable');
        return (
            <div className="leading-tight opacity-60" title={title} aria-label={title}>
                <div className="flex items-center justify-between text-[10px] text-gray-400 dark:text-gray-500">
                    <span className="truncate">Quota</span>
                </div>
                <div className="h-1 mt-1 rounded-full bg-gray-200/70 dark:bg-gray-700/70 overflow-hidden" />
            </div>
        );
    }
    // A cap exists (manual or API) but `used` has not been scanned yet:
    // show "- / cap" so the slot is informative instead of blank.
    if (!usedKnown && eff.total > 0) {
        const title = `${t('introHub.storageUsedOf', { used: '-', total: formatBytes(eff.total) })}`;
        return (
            <div className="leading-tight" title={title}>
                <div className="flex items-center justify-between text-[10px] text-gray-500 dark:text-gray-400 tabular-nums">
                    <span className="truncate">- / {formatBytes(eff.total)}</span>
                    <span className="shrink-0 ml-1 tabular-nums text-gray-400">-</span>
                </div>
                <div className="h-1 mt-1 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden" />
            </div>
        );
    }
    // `used` known but no cap at all (no API total, no manual): presence
    // indicator with a filled emerald bar, "X / ∞". Hiding the slot would
    // look like a fetch failure on providers that just don't have a cap.
    if (eff.total <= 0) {
        const title = `${formatBytes(eff.used)} (no quota cap reported)${filesSuffix}`;
        return (
            <div className="leading-tight" title={title} aria-label={title}>
                <div className="flex items-center justify-between text-[10px] text-gray-500 dark:text-gray-400 tabular-nums">
                    <span className="truncate">{formatBytes(eff.used)}</span>
                    <span className="shrink-0 ml-1 tabular-nums text-gray-400">∞</span>
                </div>
                <div className="h-1 mt-1 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden">
                    <div className="h-full w-full rounded-full bg-emerald-500/60" />
                </div>
            </div>
        );
    }
    const { used, total } = eff;
    const { tone, pct } = getStorageTone(used, total, thresholds);
    const pctClamped = pct === null ? 0 : Math.max(0, Math.min(100, pct));
    const pctLabel = pct === null ? '-' : pct >= 10 ? `${Math.round(pct)}` : `${Math.round(pct * 10) / 10}`;
    // Bytes held by retained file versions (MEGAcmd), drawn as a distinct
    // purple segment so the user can see versions are a slice of the used
    // total (#270 c.17207733). Only when the provider reported it (> 0).
    const versioning = quota?.versioningBytes && quota.versioningBytes > 0 ? quota.versioningBytes : 0;
    const versioningPct = versioning > 0 && total > 0
        ? Math.max(0, Math.min(100, (versioning / total) * 100))
        : 0;
    const versioningTitle = versioning > 0
        ? `\n${t('introHub.storageVersioning', { size: formatBytes(versioning) })}`
        : '';
    return (
        <div
            className="leading-tight"
            title={`${t('introHub.storageUsedOf', { used: formatBytes(used), total: formatBytes(total) })}${filesSuffix}${versioningTitle}`}
        >
            <div className="flex items-center justify-between text-[10px] text-gray-500 dark:text-gray-400 tabular-nums">
                <span className="truncate">{formatBytes(used)} / {formatBytes(total)}</span>
                <span className={`shrink-0 ml-1 tabular-nums ${TONE_TEXT_CLASS[tone]}`}>{pctLabel}%</span>
            </div>
            <div className="h-1 mt-1 rounded-full bg-gray-200 dark:bg-gray-700 overflow-hidden">
                <div
                    className={`h-full ${TONE_BG_CLASS[tone]} transition-all`}
                    style={{ width: `${pctClamped}%` }}
                />
            </div>
            {versioning > 0 && (
                <div
                    className="h-0.5 mt-0.5 rounded-full bg-gray-200/60 dark:bg-gray-700/60 overflow-hidden"
                    title={t('introHub.storageVersioning', { size: formatBytes(versioning) })}
                    aria-label={t('introHub.storageVersioning', { size: formatBytes(versioning) })}
                >
                    <div
                        className="h-full bg-fuchsia-500/70 transition-all"
                        style={{ width: `${Math.max(versioningPct, 2)}%` }}
                    />
                </div>
            )}
        </div>
    );
}

/** Solid dot color per AeroShare drive-state, for the avatar presence dot.
 *  Deliberately a different anchor (avatar top-right) from the reachability
 *  health dot (icon bottom-right) so the two never read as the same signal. */
// Both connection states stay COOL (not amber): connecting/starting and an
// active pull are a pulsing azzurro, distinct from the calm green of a settled
// (live/serving) drive. The 'live' green is now durable across a remount (F3),
// so the pulse only shows while genuinely starting/syncing, never when idle.
const PEER_STATE_DOT: Record<PeerDriveState, string> = {
    starting: 'bg-sky-400 animate-pulse',
    syncing: 'bg-sky-400 animate-pulse',
    live: 'bg-emerald-500',
    serving: 'bg-emerald-500',
    // Idle (tab closed): a calm dark blue, with a slight gradient so it reads
    // as a deliberate "paused/standby" state distinct from gray (off) and
    // pulsing azzurro (syncing).
    standby: 'bg-gradient-to-br from-blue-600 to-blue-800',
    error: 'bg-red-500',
    stopped: 'bg-gray-400',
};

/** Presence-style status dot for an AeroShare friend, overlaid on the avatar's
 *  top-right (a friend's drive is "online/syncing/offline", much like a contact
 *  presence indicator). The parent must be position:relative. */
export function PeerPresenceDot({ peerState, hasActiveSession = false, className = '' }: { peerState?: PeerDriveState; hasActiveSession?: boolean; className?: string }) {
    const t = useTranslation();
    // The dot follows the REAL session state (robust, no backend-task race):
    //  - active session  -> the live sync state (green live / azzurro syncing),
    //  - no active session but the drive was brought up -> idle (dark-blue standby),
    //  - never connected this run -> gray (initial/off).
    const effective: PeerDriveState | undefined = hasActiveSession
        ? (peerState ?? 'starting')
        : (peerState && peerState !== 'stopped' ? 'standby' : peerState);
    const label = effective ? t(`aeroShare.driveState.${effective}`) : t('aeroShare.driveState.offline');
    const color = effective ? PEER_STATE_DOT[effective] : 'bg-gray-400';
    return (
        <span
            className={`absolute -top-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2 ring-white dark:ring-gray-800 pointer-events-none ${color} ${className}`}
            title={label}
            aria-label={label}
        />
    );
}

export function ServerBadges({ server, cryptDetailed = false, peerState }: { server: ServerProfile; cryptDetailed?: boolean; peerState?: PeerDriveState }) {
    const t = useTranslation();
    const proto = server.protocol || 'ftp';
    // AeroShare friend: a dedicated violet badge + the live drive-state chip.
    // Skips the generic protocol/class logic below (a peer is neither FTP nor
    // an "API" cloud), so it never mislabels the card.
    if (proto === 'peer') {
        // Identity badge only. The live drive-state is shown as a presence dot
        // on the avatar (PeerPresenceDot), not a second text chip here: stacking
        // a state chip on the violet AeroShare badge read as clutter (and it
        // would collide visually with the E2E/health vocabulary). `peerState`
        // is still accepted (callers render the dot themselves via PeerPresenceDot).
        return (
            <span className="text-[10px] px-1.5 py-0.5 rounded font-medium inline-flex items-center gap-0.5 bg-violet-100 dark:bg-violet-900/40 text-violet-700 dark:text-violet-300">
                <Users size={10} />
                {t('aeroShare.feature')}
            </span>
        );
    }
    // Portable MTP/WPD: never fall through to the generic "API" class badge.
    if (proto === 'mtp') {
        return (
            <span className="text-[10px] px-1.5 py-0.5 rounded font-medium inline-flex items-center gap-0.5 bg-teal-100 dark:bg-teal-900/40 text-teal-700 dark:text-teal-300">
                <Smartphone size={10} />
                MTP
            </span>
        );
    }
    // Default tlsMode matches ProtocolSelector: ftp→'explicit', ftps→'implicit'
    const tlsMode = server.options?.tlsMode || (proto === 'ftp' ? 'explicit' : proto === 'ftps' ? 'implicit' : undefined);
    // FTP with any TLS mode (except 'none') is effectively FTPS
    const displayProto = proto === 'ftp' && tlsMode && tlsMode !== 'none' ? 'ftps' : proto;
    const isFtps = displayProto === 'ftps';
    const isSftp = proto === 'sftp';
    const isPlainFtp = displayProto === 'ftp' && !isSftp;
    const hasTlsConnection = isFtps || proto === 'ftps' || isSftp;
    const certUnverified = (isFtps || proto === 'ftps') && server.options?.verifyCert === false;
    const certVerified = hasTlsConnection && !certUnverified;
    const gitHubBadge = proto === 'github' ? getGitHubConnectionBadge(server.options) : null;
    const megaBadge = proto === 'mega' ? getMegaConnectionBadge(server.options) : null;
    const infiniCloudBadge = server.providerId === 'infinicloud' ? getInfiniCloudConnectionBadge(server.options) : null;
    const filenAuthVersion = getFilenAuthVersion(server);
    const protocolClass = getProtocolClass(proto as ProviderType);
    const e2eBits = protocolClass === 'E2E' ? getE2EBits(proto as ProviderType) : null;
    const protocolClassLabel = e2eBits ? `E2E ${e2eBits}-bit` : protocolClass;
    // Skip class badge when it duplicates the brand badge (FTP/FTPS/SFTP show protocol uppercase already)
    const showClassBadge = !['FTP', 'FTPS', 'SFTP'].includes(protocolClass);
    const classBadgeColor: Record<string, string> = {
        OAuth: 'bg-indigo-100 dark:bg-indigo-900/40 text-indigo-700 dark:text-indigo-300',
        API: 'bg-sky-100 dark:bg-sky-900/40 text-sky-700 dark:text-sky-300',
        WebDAV: 'bg-purple-100 dark:bg-purple-900/40 text-purple-700 dark:text-purple-300',
        E2E: 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300',
        S3: 'bg-orange-100 dark:bg-orange-900/40 text-orange-700 dark:text-orange-300',
        Azure: 'bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300',
        AeroCloud: 'bg-cyan-100 dark:bg-cyan-900/40 text-cyan-700 dark:text-cyan-300',
        MTP: 'bg-teal-100 dark:bg-teal-900/40 text-teal-700 dark:text-teal-300',
    };

    // Only render the protocol badge when it carries dedicated color (FTP/FTPS/SFTP);
    // for everything else the colored class badge + provider icon already convey it,
    // so the gray fallback is just visual noise.
    const showProtoBadge = isFtps || isSftp || isPlainFtp;
    const badgeClass = isFtps
        ? 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300'
        : isSftp
            ? 'bg-teal-100 dark:bg-teal-900/40 text-teal-700 dark:text-teal-300'
            : 'bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-300';

    // Recognise both the bare provider id and the `-webdav` suffix variant
    // emitted by MyServersPanel's host heuristic for legacy profiles.
    // Both Felicloud and TAB.DIGITAL are Nextcloud-as-a-service providers
    // and share the same API OCS protocol class, so they use the same
    // badge tint to avoid the colour difference reading as a protocol
    // difference. The brand colour lives on the provider logo, not here.
    const pid = server.providerId || '';
    const isFelicloud = pid === 'felicloud' || pid === 'felicloud-webdav';
    const isTabdigital = pid === 'tabdigital' || pid === 'tabdigital-webdav';
    const isOcsBranded = isFelicloud || isTabdigital;
    const ocsBadgeStyle = isOcsBranded
        ? { backgroundColor: '#0083ce22', color: '#0083ce' }
        : undefined;

    // Encrypted-overlay profile: an enabled AeroCrypt (native) or rclone-crypt
    // (interop) binding REPLACES the default protocol/cert badge set with the
    // crypt identity badge (shield + brand). In detailed mode (list view) a
    // second strength badge (lock + "256-bit") follows the E2E provider badge
    // convention; in compact mode (card view) the cipher lives in the identity
    // tooltip only, so the single badge never wraps. Emerald = native, blue =
    // interop. The transport stays visible in the subtitle/host.
    const cryptKind = getServerCryptOverlay(server);
    const cryptIsRclone = cryptKind === 'rclone-crypt';
    // Both kinds use a 256-bit key, so the badge follows the E2E "256-bit"
    // convention while the exact cipher (kind-accurate) sits in the tooltip:
    // AeroCrypt = AES-256-GCM-SIV; rclone-crypt content = XSalsa20-Poly1305
    // (NaCl secretbox), filenames AES-256 (EME). Labelling rclone as "AES"
    // would be wrong, hence the cipher string is per-kind.
    const cryptCipher = cryptIsRclone ? 'XSalsa20-Poly1305' : 'AES-256-GCM-SIV';
    const cryptTint = cryptIsRclone
        ? 'bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300'
        : 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300';

    if (cryptKind) {
        const baseTitle = cryptIsRclone ? t('introHub.cryptBadge.rcloneTitle') : t('introHub.cryptBadge.aerocryptTitle');
        // Compact (card) keeps the cipher in the identity tooltip; detailed
        // (list) shows it on the dedicated strength badge instead.
        // Native AeroCrypt always speaks AECR v3 on the wire today; show a short
        // v3 chip next to the brand so pre-Tier1 / legacy vaults are not confused
        // with "unknown format" (the recovery kit path still surfaces upgrades).
        const aerocryptVersion = !cryptIsRclone ? 'v3' : null;
        const identityTitle = cryptDetailed
            ? baseTitle
            : aerocryptVersion
              ? `${cryptCipher} (${aerocryptVersion}): ${baseTitle}`
              : `${cryptCipher}: ${baseTitle}`;
        return (
            <div className="flex items-center gap-1">
                <span
                    className={`text-[10px] px-1.5 py-0.5 rounded font-medium inline-flex items-center gap-0.5 whitespace-nowrap ${cryptTint}`}
                    title={identityTitle}
                >
                    <Lock size={10} />
                    {cryptIsRclone ? t('introHub.cryptBadge.rclone') : t('introHub.cryptBadge.aerocrypt')}
                    {aerocryptVersion && (
                        <span className="opacity-80 font-semibold">{aerocryptVersion}</span>
                    )}
                </span>
                {cryptDetailed && (
                    <span
                        className={`text-[10px] px-1.5 py-0.5 rounded font-medium inline-flex items-center gap-0.5 whitespace-nowrap ${cryptTint}`}
                        title={cryptCipher}
                    >
                        <Lock size={10} />
                        256-bit
                    </span>
                )}
            </div>
        );
    }

    return (
        <div className="flex items-center gap-1 flex-wrap">
            {isOcsBranded ? (
                <span className="text-[10px] px-1.5 py-0.5 rounded font-medium uppercase"
                      style={ocsBadgeStyle}>
                    API OCS
                </span>
            ) : showProtoBadge ? (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium uppercase ${badgeClass}`}>
                    {displayProto}
                </span>
            ) : null}
            {showClassBadge && !isOcsBranded && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium inline-flex items-center gap-0.5 ${classBadgeColor[protocolClass] || 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-400'}`}>
                    {e2eBits && <Lock size={10} />}
                    {protocolClassLabel}
                </span>
            )}
            {certVerified && (
                <span className="text-[10px] px-1 py-0.5 rounded bg-green-100 dark:bg-green-900/40 text-green-600 dark:text-green-400"
                      title={t('statusBar.secureConnectionTitle', { protocol: isSftp ? 'SSH' : 'TLS' })}>
                    <ShieldCheck size={10} />
                </span>
            )}
            {certUnverified && (
                <span className="text-[10px] px-1 py-0.5 rounded bg-gray-100 dark:bg-gray-700 text-gray-400 dark:text-gray-500"
                      title={t('statusBar.insecureConnectionTitle')}>
                    <ShieldCheck size={10} />
                </span>
            )}
            {gitHubBadge && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${gitHubBadge.className}`}>
                    {gitHubBadge.label}
                </span>
            )}
            {megaBadge && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${megaBadge.className}`}>
                    {megaBadge.label}
                </span>
            )}
            {filenAuthVersion && (
                <span
                    className="text-[10px] px-1.5 py-0.5 rounded font-medium bg-blue-100 text-blue-700 dark:bg-blue-900/50 dark:text-blue-300"
                    title="Detected from Filen auth/info on successful connect"
                >
                    v{filenAuthVersion}
                </span>
            )}
            {infiniCloudBadge && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${infiniCloudBadge.className}`}>
                    {infiniCloudBadge.label}
                </span>
            )}
            {server.host === 'test.rebex.net' && (
                <span className="text-[10px] px-1.5 py-0.5 rounded font-medium bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-300">
                    DEMO
                </span>
            )}
        </div>
    );
}

interface ServerCardProps {
    server: ServerProfile;
    isConnecting: boolean;
    credentialsMasked: boolean;
    /** Hide username (left side of user@host) on the card. Toggled from MyServersToolbar. */
    hideUsername?: boolean;
    isFavorite: boolean;
    onConnect: (server: ServerProfile) => void;
    onEdit: (server: ServerProfile) => void;
    onDuplicate: (server: ServerProfile) => void;
    onDelete: (server: ServerProfile) => void;
    onToggleFavorite: (server: ServerProfile) => void;
    onContextMenu?: (e: React.MouseEvent, server: ServerProfile) => void;
    onHoverChange?: (server: ServerProfile | null) => void;
    isRenaming?: boolean;
    onRenameSubmit?: (server: ServerProfile, newName: string) => void;
    onRenameCancel?: () => void;
    /** Enters inline rename mode. Wired to a double-click on the name so the
     *  gesture never collides with the single-click Cross-Profile selection
     *  on the card body. */
    onRenameStart?: (server: ServerProfile) => void;
    isDraggable?: boolean;
    isDragging?: boolean;
    isDragTarget?: boolean;
    /** Position of this card in the parent's `servers` array. Passed as a
     *  separate prop, instead of being curried into each drag handler at
     *  the call site, so the four handler references stay stable across
     *  parent re-renders and `React.memo()` actually skips re-rendering
     *  cards whose own data did not change (issue #221). */
    dragIndex?: number;
    onDragStart?: (idx: number, e: React.DragEvent) => void;
    onDragEnter?: (idx: number, e: React.DragEvent) => void;
    onDragOver?: (idx: number, e: React.DragEvent) => void;
    onDrop?: (idx: number, e: React.DragEvent) => void;
    onDragEnd?: () => void;
    /** Cross-Profile Transfer selection role for this card. */
    selectionRole?: 'source' | 'destination' | null;
    /** Toggles this server in the Cross-Profile selection. Triggered by clicking the card body. */
    onSelect?: (server: ServerProfile) => void;
    /** Reachability probe state, fed by useProviderHealth in detailed layout. */
    healthStatus?: 'up' | 'slow' | 'down' | 'pending' | 'unknown';
    healthLatencyMs?: number;
    /** Click-to-recheck: re-runs the probe just for this profile. Lets the
     *  user verify a flaky tab-wide scan result without re-running the whole
     *  batch. Only wired in detailed layout. */
    onRetryHealth?: (server: ServerProfile) => void;
    /** Storage usage thresholds (warn/critical) for the % column tone. Falls
     *  back to defaults when the panel hasn't loaded settings yet. */
    thresholds?: StorageThresholds;
    /** True when this profile has at least one open session in the tab strip.
     *  Drives a subtle pulse on the health dot (compact) or radial (detailed)
     *  so users can tell at a glance which saved server they are already
     *  connected to. Independent from the health status itself. Issue #222. */
    hasActiveSession?: boolean;
    /** APPENDIX-DEVICE-PROFILES Phase 3: MTP device physically attached
     *  (fingerprint match). Separate from HTTP healthStatus. */
    deviceAttached?: boolean;
    /** AeroShare friend cards only: live replication/serving state for the
     *  bound drive, fed by usePeerDriveStates. Drives the badge chip. */
    peerState?: PeerDriveState;
}

export function RenameInput({
    initialValue,
    onSubmit,
    onCancel,
    sizeClass,
}: {
    initialValue: string;
    onSubmit: (value: string) => void;
    onCancel: () => void;
    sizeClass: string;
}) {
    const t = useTranslation();
    const [value, setValue] = React.useState(initialValue);
    const inputRef = React.useRef<HTMLInputElement>(null);
    React.useEffect(() => {
        inputRef.current?.focus();
        inputRef.current?.select();
    }, []);
    const submit = () => {
        const trimmed = value.trim();
        if (trimmed && trimmed !== initialValue) {
            onSubmit(trimmed);
        } else {
            onCancel();
        }
    };
    return (
        <div className="flex items-center gap-1" onClick={(e) => e.stopPropagation()}>
            <input
                ref={inputRef}
                type="text"
                value={value}
                onChange={(e) => setValue(e.target.value)}
                onKeyDown={(e) => {
                    if (e.key === 'Enter') { e.preventDefault(); submit(); }
                    if (e.key === 'Escape') { e.preventDefault(); onCancel(); }
                }}
                onBlur={submit}
                className={`flex-1 min-w-0 px-1.5 py-0.5 ${sizeClass} font-semibold bg-white dark:bg-gray-700 border border-blue-400 dark:border-blue-500 rounded focus:outline-none focus:ring-1 focus:ring-blue-500`}
            />
            <button
                onMouseDown={(e) => { e.preventDefault(); submit(); }}
                className="p-0.5 rounded text-green-600 hover:text-green-700 hover:bg-green-50 dark:hover:bg-green-900/30"
                title={t('common.confirm')}
            >
                <Check size={13} />
            </button>
            <button
                onMouseDown={(e) => { e.preventDefault(); onCancel(); }}
                className="p-0.5 rounded text-gray-400 hover:text-gray-600 hover:bg-gray-100 dark:hover:bg-gray-700"
                title={t('common.cancel')}
            >
                <X size={13} />
            </button>
        </div>
    );
}

export function getServerIcon(server: ServerProfile, size = 20): React.ReactNode {
    if (server.customIconUrl) {
        return <img src={server.customIconUrl} className="rounded object-contain" alt="" style={{ width: size, height: size }} />;
    }
    if (server.faviconUrl) {
        return <img src={server.faviconUrl} className="rounded object-contain" alt="" style={{ width: size, height: size }} />;
    }
    const providerId = server.providerId;
    if (providerId && PROVIDER_LOGOS[providerId]) {
        const LogoComponent = PROVIDER_LOGOS[providerId];
        return <LogoComponent size={size} />;
    }
    const proto = server.protocol || 'ftp';
    // AeroShare friend: no brand logo: a person glyph reads as "a friend".
    if (proto === 'peer') {
        return <Users size={size} className="text-violet-500" />;
    }
    if (PROVIDER_LOGOS[proto]) {
        const LogoComponent = PROVIDER_LOGOS[proto];
        return <LogoComponent size={size} />;
    }
    return <ProtocolIcon protocol={proto} size={size} />;
}

export function getTimeAgo(dateStr?: string): string {
    if (!dateStr) return '';
    const date = new Date(dateStr);
    const now = new Date();
    const diffMs = now.getTime() - date.getTime();
    const diffMin = Math.floor(diffMs / 60000);
    if (diffMin < 1) return 'now';
    if (diffMin < 60) return `${diffMin}m`;
    const diffH = Math.floor(diffMin / 60);
    if (diffH < 24) return `${diffH}h`;
    const diffD = Math.floor(diffH / 24);
    if (diffD < 30) return `${diffD}d`;
    return `${Math.floor(diffD / 30)}mo`;
}

export const ServerCard = React.memo(function ServerCard({
    server,
    isConnecting,
    credentialsMasked,
    hideUsername = false,
    isFavorite,
    onConnect,
    onEdit,
    onDuplicate,
    onDelete,
    onToggleFavorite,
    onContextMenu,
    onHoverChange,
    isRenaming = false,
    onRenameSubmit,
    onRenameCancel,
    onRenameStart,
    isDraggable,
    isDragging,
    isDragTarget,
    dragIndex,
    onDragStart,
    onDragEnter,
    onDragOver,
    onDrop,
    onDragEnd,
    selectionRole = null,
    onSelect,
    healthStatus,
    healthLatencyMs,
    onRetryHealth,
    thresholds = DEFAULT_THRESHOLDS,
    hasActiveSession = false,
    deviceAttached,
    peerState,
}: ServerCardProps) {
    const t = useTranslation();
    const cardLayout = useCardLayout();
    const favoriteMarker = useFavoriteMarker();
    const introHubIconSize = useIntroHubIconSize();
    const connectButtonSize = Math.max(40, Math.min(48, introHubIconSize + 16));
    const connectIconSize = Math.min(introHubIconSize, connectButtonSize - 10);
    const connectSpinnerSize = Math.max(16, Math.min(22, connectIconSize - 2));
    const isMtpDevice = server.protocol === 'mtp';
    const attachedTitle = deviceAttached
        ? t('introHub.deviceAttached')
        : t('introHub.deviceNotAttached');
    const radialTitle = isMtpDevice
        ? (hasActiveSession ? `${attachedTitle} (${t('common.goToActiveSession')})` : attachedTitle)
        : healthStatus
        ? t(`introHub.health.${healthStatus}`)
            + (healthLatencyMs && healthStatus !== 'pending' && healthStatus !== 'down' ? ` · ${healthLatencyMs}ms` : '')
            + (onRetryHealth ? ` · ${t('introHub.health.clickToRetry')}` : '')
        : undefined;
    const handleRetry = onRetryHealth ? () => onRetryHealth(server) : undefined;
    const quotaSupported = profileHasQuota(server);
    const timeAgo = getTimeAgo(server.lastConnected);
    // #180 / 4486730822: standalone connect-failure marker. Separate signal
    // from health (which is a reachability probe); never share the glyph
    // or the status enum with `useProviderHealth`.
    const connectError = server.lastConnectionError;
    const connectErrorTitle = React.useMemo(() => {
        if (!connectError) return undefined;
        const when = getTimeAgo(connectError.timestamp);
        const head = t('introHub.connectError.failed');
        const ago = when ? t('introHub.connectError.lastFailedAt', { time: when }) : '';
        const reason = connectError.message || '';
        return [head, ago, reason].filter(Boolean).join(' · ');
    }, [connectError, t]);
    const handleMouseEnter = onHoverChange ? () => onHoverChange(server) : undefined;
    const handleMouseLeave = onHoverChange ? () => onHoverChange(null) : undefined;
    // Card body click toggles cross-profile selection: but only when the click
    // didn't bubble from an interactive child (icon/button/input) which already
    // calls e.stopPropagation() in its own handler.
    const handleCardClick = onSelect ? (e: React.MouseEvent) => {
        const target = e.target as HTMLElement | null;
        if (target?.closest('button, input, a, [role="menuitem"]')) return;
        onSelect(server);
    } : undefined;
    const isSource = selectionRole === 'source';
    const isDestination = selectionRole === 'destination';
    const isSelected = isSource || isDestination;
    // Selection ring colors: indigo for source (outgoing), emerald for destination (incoming).
    const selectionRingClass = isSource
        ? 'ring-2 ring-indigo-500 dark:ring-indigo-400 border-indigo-300 dark:border-indigo-500/50'
        : isDestination
            ? 'ring-2 ring-emerald-500 dark:ring-emerald-400 border-emerald-300 dark:border-emerald-500/50'
            : '';
    const selectionTitle = isSource
        ? t('introHub.crossProfileSourceSelected')
        : isDestination
            ? t('introHub.crossProfileDestinationSelected')
            : '';

    const subtitle = React.useMemo(() => {
        // Smart subtitle: hides opaque OAuth/API tokens by default, shows
        // hostname[:port] for traditional protocols, optionally adds the
        // username when the toolbar's "show usernames" override is on.
        // Returns '' (not nbsp) so the caller can collapse the slot entirely
        // when the user toggled identifiers off, saving a row in grid view.
        return getServerSubtitle(server, {
            credentialsMasked,
            showUsername: !hideUsername,
        });
    }, [server, credentialsMasked, hideUsername]);

    // Bind the parent's stable `(idx, e) => void` drag handlers to this
    // card's `dragIndex` once per (handler, index) change instead of on
    // every parent render. This is what makes `React.memo` actually skip
    // re-rendering unchanged cards (issue #221).
    const handleCardDragStart = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDragStart?.(dragIndex, e); },
        [onDragStart, dragIndex],
    );
    const handleCardDragEnter = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDragEnter?.(dragIndex, e); },
        [onDragEnter, dragIndex],
    );
    const handleCardDragOver = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDragOver?.(dragIndex, e); },
        [onDragOver, dragIndex],
    );
    const handleCardDrop = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDrop?.(dragIndex, e); },
        [onDrop, dragIndex],
    );

    // ===== GRID VIEW =====
    return (
        <div
            data-my-server-card
            draggable={isDraggable}
            onDragStart={onDragStart ? handleCardDragStart : undefined}
            onDragEnter={onDragEnter ? handleCardDragEnter : undefined}
            onDragOver={onDragOver ? handleCardDragOver : undefined}
            onDrop={onDrop ? handleCardDrop : undefined}
            onDragEnd={onDragEnd}
            onClick={handleCardClick}
            className={`group relative bg-white dark:bg-gray-800 hover:bg-gray-50 dark:hover:bg-gray-750 border rounded-lg p-3.5 transition-colors shadow-sm dark:shadow-md ${isDraggable ? 'cursor-grab active:cursor-grabbing' : ''} ${onSelect ? 'cursor-pointer' : ''} ${isDragging ? 'opacity-40 scale-[0.97] shadow-lg ring-2 ring-blue-400/50 border-blue-400' : 'border-gray-100 dark:border-gray-700/50 hover:border-blue-200 dark:hover:border-blue-500/30'} ${isDragTarget ? '!border-blue-500 !border-2 bg-blue-50 dark:bg-blue-900/30 shadow-inner' : ''} ${selectionRingClass}`}
            onContextMenu={(e) => onContextMenu?.(e, server)}
            onMouseEnter={handleMouseEnter}
            onMouseLeave={handleMouseLeave}
            title={selectionTitle || undefined}
        >
            {/* Cross-Profile selection badge (top-left, doesn't overlap actions on the right).
                z-10 keeps it above the connect button: the button (40-48px) and
                the badge (20px) both anchor near the card's top-left corner, so
                without z-stacking the badge gets visually hidden under the
                button by DOM order. */}
            {isSelected && (
                <div className={`absolute top-2 left-2 z-10 flex items-center justify-center w-5 h-5 rounded-full pointer-events-none ${
                    isSource
                        ? 'bg-indigo-500 text-white shadow ring-1 ring-indigo-400/60'
                        : 'bg-emerald-500 text-white shadow ring-1 ring-emerald-400/60'
                }`}>
                    {isSource ? <ArrowUpRight size={12} strokeWidth={2.5} /> : <ArrowDownLeft size={12} strokeWidth={2.5} />}
                </div>
            )}
            {/* Top row: clickable icon + name + badge */}
            <div className="flex items-start gap-3">
                {/* Icon = connect button (with reachability dot overlay in compact layout) */}
                <div className="relative shrink-0">
                    <button
                        onClick={(e) => { e.stopPropagation(); onConnect(server); }}
                        disabled={isConnecting}
                        className={`rounded-lg flex items-center justify-center transition-all cursor-pointer disabled:cursor-wait ${
                            hasActiveSession
                                ? 'bg-emerald-50 dark:bg-emerald-900/20 border border-emerald-400/70 dark:border-emerald-500/60 ring-1 ring-emerald-400/40 hover:bg-emerald-100 dark:hover:bg-emerald-900/30 hover:ring-2 hover:ring-emerald-400/60'
                                : 'bg-gray-100 dark:bg-gray-700 border border-gray-200/70 dark:border-gray-600 hover:bg-blue-100 dark:hover:bg-blue-900/30 hover:ring-2 hover:ring-blue-400/50 hover:border-blue-300 dark:hover:border-blue-500'
                        }`}
                        style={{ width: connectButtonSize, height: connectButtonSize }}
                        title={hasActiveSession ? t('common.goToActiveSession') : t('common.connect')}
                    >
                        {isConnecting ? <Loader2 size={connectSpinnerSize} className="animate-spin text-blue-500" /> : getServerIcon(server, connectIconSize)}
                    </button>
                    {/* MTP attach: always show a presence dot (green = plugged in,
                        red = unplugged), same vocabulary as HTTP health up/down.
                        Separate signal from HTTP health (mtp has no health URL). */}
                    {cardLayout !== 'detailed' && isMtpDevice && (
                        <span
                            className={`absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2 ring-white dark:ring-gray-800 pointer-events-none ${
                                deviceAttached ? 'bg-green-500' : 'bg-red-500'
                            } ${hasActiveSession && deviceAttached ? 'animate-pulse' : ''}`}
                            title={radialTitle}
                            aria-label={radialTitle}
                            data-testid="server-card-device-attached"
                        />
                    )}
                    {cardLayout !== 'detailed' && !isMtpDevice && healthStatus && healthStatus !== 'unknown' && (
                        <span
                            className={`absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2 ring-white dark:ring-gray-800 pointer-events-none ${
                                healthStatus === 'up' ? 'bg-green-500'
                                : healthStatus === 'slow' ? 'bg-amber-500'
                                : healthStatus === 'down' ? 'bg-red-500'
                                : 'bg-gray-400 animate-pulse'
                            } ${hasActiveSession ? 'animate-pulse' : ''}`}
                            title={hasActiveSession ? `${radialTitle} (active session)` : radialTitle}
                            aria-label={hasActiveSession ? `${radialTitle} (active session)` : radialTitle}
                        />
                    )}
                    {/* AeroShare drive-state: presence dot on the avatar top-right.
                        Distinct from the bottom-right health dot (peer cards have no
                        reachability probe), so the two never read as the same signal. */}
                    {server.protocol === 'peer' && <PeerPresenceDot peerState={peerState} hasActiveSession={hasActiveSession} />}
                    {/* #180 / 4486730822: standalone connect-failure marker.
                        Anchored top-left so it never overlaps the bottom-right
                        health dot or the detailed-layout HealthRadial. */}
                    {connectError && (
                        <span
                            className="absolute -top-1 -left-1 inline-flex items-center justify-center w-3.5 h-3.5 rounded-full bg-amber-600 dark:bg-amber-700 text-white shadow ring-2 ring-white dark:ring-gray-800 pointer-events-none"
                            title={connectErrorTitle}
                            aria-label={connectErrorTitle}
                            data-testid="server-card-connect-error"
                        >
                            <AlertTriangle size={9} strokeWidth={2.75} />
                        </span>
                    )}
                </div>
                <div className="flex-1 min-w-0">
                    {isRenaming ? (
                        <RenameInput
                            initialValue={server.name}
                            onSubmit={(v) => onRenameSubmit?.(server, v)}
                            onCancel={() => onRenameCancel?.()}
                            sizeClass="text-sm"
                        />
                    ) : (
                        <div
                            // Double-click renames; the single clicks that
                            // compose it are swallowed here so they never
                            // bubble to the card's Cross-Profile select
                            // handler. The card body stays the select target.
                            className={`text-sm font-semibold text-gray-900 dark:text-gray-100 truncate select-none ${onRenameStart ? 'cursor-text hover:text-blue-600 dark:hover:text-blue-400' : ''}`}
                            onClick={onRenameStart ? (e) => e.stopPropagation() : undefined}
                            onDoubleClick={onRenameStart ? (e) => { e.stopPropagation(); onRenameStart(server); } : undefined}
                            title={onRenameStart ? t('introHub.doubleClickToRename') : undefined}
                        >
                            {server.name}
                        </div>
                    )}
                    <div className="flex items-center gap-1.5 mt-0.5">
                        <ServerBadges server={server} peerState={peerState} />
                        {timeAgo && (
                            <span className="text-[10px] text-gray-400 dark:text-gray-500 tabular-nums flex items-center gap-0.5"><Clock size={8} />{timeAgo}</span>
                        )}
                    </div>
                </div>
            </div>

            {/* Subtitle: rendered only when there's content to show. Hiding it
                entirely when the toolbar's @ toggle is off saves a row in the
                grid and keeps card heights uniform across providers that
                resolve to an empty identifier (S3, OAuth, opaque tokens). */}
            {subtitle && (
                <div className="text-xs text-gray-500 dark:text-gray-400 truncate mt-2 min-h-[1rem]">{subtitle}</div>
            )}

            {/* Footer (detailed layout): quota left (only when cached), radial
                right. The radial is rendered whenever the layout is detailed so
                the click-to-retry affordance is always available: even before
                the first scan completes. The top border anchors the section so
                cards with and without quota still feel uniform. */}
            {cardLayout === 'detailed' && (
                <div className="mt-2.5 pt-2 border-t border-gray-100 dark:border-gray-700/60 grid grid-cols-[1fr_auto] items-center gap-2 min-h-[20px]">
                    <div className="min-w-0">
                        <StorageUsageBar quota={server.lastQuota} supported={quotaSupported} thresholds={thresholds} manualTotal={effectiveManualCap(server.options?.manualTotalBytes, server.protocol, server.providerId, server.host)} />
                    </div>
                    <div className="shrink-0 text-gray-300 dark:text-gray-600">
                        {isMtpDevice ? (
                            <span
                                className={`inline-block w-3.5 h-3.5 rounded-full ring-2 ring-white dark:ring-gray-800 ${
                                    deviceAttached ? 'bg-green-500' : 'bg-red-500'
                                } ${hasActiveSession && deviceAttached ? 'animate-pulse' : ''}`}
                                title={radialTitle}
                                aria-label={radialTitle}
                                data-testid="server-card-device-attached-detailed"
                            />
                        ) : (
                            <HealthRadial
                                status={healthStatus || 'unknown'}
                                latencyMs={healthLatencyMs}
                                size={16}
                                title={hasActiveSession ? `${radialTitle} (active session)` : radialTitle}
                                onRetry={handleRetry}
                                pulsing={hasActiveSession}
                            />
                        )}
                    </div>
                </div>
            )}

            {/* Top-right: action buttons (hover) + favorite star (rightmost) */}
            <div className="absolute top-2 right-2 flex items-center gap-0.5">
                <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                    <button onClick={(e) => { e.stopPropagation(); onEdit(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.edit')}>
                        <Edit2 size={12} />
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); onDuplicate(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.copy')}>
                        <Copy size={12} />
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); onDelete(server); }} className="p-1 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-500 dark:hover:text-red-400 transition-colors" title={t('common.delete')}>
                        <Trash2 size={12} />
                    </button>
                </div>
                <button
                    onClick={(e) => { e.stopPropagation(); onToggleFavorite(server); }}
                    className={`p-1 rounded-lg transition-colors ${
                        favoriteMarker === 'heart'
                            ? (isFavorite
                                ? 'text-red-500 hover:text-red-600'
                                : 'text-gray-400 hover:text-red-500 opacity-0 group-hover:opacity-100')
                            : (isFavorite
                                ? 'text-yellow-400 hover:text-yellow-500'
                                : 'text-gray-400 hover:text-yellow-400 opacity-0 group-hover:opacity-100')
                    }`}
                    title={isFavorite ? t('introHub.removeFavorite') : t('introHub.addFavorite')}
                >
                    {favoriteMarker === 'heart'
                        ? <Heart size={12} fill={isFavorite ? 'currentColor' : 'none'} />
                        : <Star size={12} fill={isFavorite ? 'currentColor' : 'none'} />}
                </button>
            </div>
        </div>
    );
});
