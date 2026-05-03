import * as React from 'react';
import { Edit2, Trash2, Copy, Loader2, Star, Clock, ShieldCheck, Lock, Check, X, ArrowUpRight, ArrowDownLeft } from 'lucide-react';
import { ServerProfile, ProviderType, getProtocolClass, getE2EBits, supportsStorageQuota } from '../../types';
import { ProtocolIcon } from '../ProtocolSelector';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { getGitHubConnectionBadge, getMegaConnectionBadge, getInfiniCloudConnectionBadge } from '../../utils/providerConnectionMeta';
import { getFilenAuthVersion } from '../../utils/filenAuthVersion';
import { getServerSubtitle } from '../../utils/serverSubtitle';
import { useTranslation } from '../../i18n';
import { useCardLayout } from '../../hooks/useCardLayout';
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
 *  null when no quota is cached — caller decides whether to render an empty
 *  slot. Many providers (S3, raw FTP/SFTP, WebDAV without quota support)
 *  never produce one, and a "— / —" placeholder is just visual noise. */
function StorageUsageBar({
    quota,
    supported,
    thresholds,
}: {
    quota: ServerProfile['lastQuota'] | undefined;
    supported: boolean;
    thresholds: StorageThresholds;
}) {
    const t = useTranslation();
    if (!quota || !quota.total || quota.total <= 0) {
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
    const { used, total } = quota;
    const { tone, pct } = getStorageTone(used, total, thresholds);
    const pctClamped = pct === null ? 0 : Math.max(0, Math.min(100, pct));
    const pctLabel = pct === null ? '—' : pct >= 10 ? `${Math.round(pct)}` : `${Math.round(pct * 10) / 10}`;
    return (
        <div
            className="leading-tight"
            title={t('introHub.storageUsedOf', { used: formatBytes(used), total: formatBytes(total) })}
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
        </div>
    );
}

export function ServerBadges({ server }: { server: ServerProfile }) {
    const t = useTranslation();
    const proto = server.protocol || 'ftp';
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

    return (
        <div className="flex items-center gap-1 flex-wrap">
            {server.providerId === 'felicloud' ? (
                <span className="text-[10px] px-1.5 py-0.5 rounded font-medium uppercase"
                      style={{ backgroundColor: '#0083ce22', color: '#0083ce' }}>
                    API OCS
                </span>
            ) : showProtoBadge ? (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium uppercase ${badgeClass}`}>
                    {displayProto}
                </span>
            ) : null}
            {showClassBadge && server.providerId !== 'felicloud' && (
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
    isDraggable?: boolean;
    isDragging?: boolean;
    isDragTarget?: boolean;
    onDragStart?: (e: React.DragEvent) => void;
    onDragEnter?: (e: React.DragEvent) => void;
    onDragOver?: (e: React.DragEvent) => void;
    onDrop?: (e: React.DragEvent) => void;
    onDragEnd?: () => void;
    /** Cross-Profile Transfer selection role for this card. */
    selectionRole?: 'source' | 'destination' | null;
    /** Toggles this server in the Cross-Profile selection. Triggered by clicking the card body. */
    onSelect?: (server: ServerProfile) => void;
    /** Reachability probe state, fed by useProviderHealth in detailed layout. */
    healthStatus?: 'up' | 'slow' | 'down' | 'pending' | 'unknown';
    healthLatencyMs?: number;
    /** Click-to-recheck — re-runs the probe just for this profile. Lets the
     *  user verify a flaky tab-wide scan result without re-running the whole
     *  batch. Only wired in detailed layout. */
    onRetryHealth?: (server: ServerProfile) => void;
    /** Storage usage thresholds (warn/critical) for the % column tone. Falls
     *  back to defaults when the panel hasn't loaded settings yet. */
    thresholds?: StorageThresholds;
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
        return <img src={server.customIconUrl} className="w-6 h-6 rounded object-contain" alt="" />;
    }
    if (server.faviconUrl) {
        return <img src={server.faviconUrl} className="w-6 h-6 rounded object-contain" alt="" />;
    }
    const providerId = server.providerId;
    if (providerId && PROVIDER_LOGOS[providerId]) {
        const LogoComponent = PROVIDER_LOGOS[providerId];
        return <LogoComponent size={size} />;
    }
    const proto = server.protocol || 'ftp';
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
    isDraggable,
    isDragging,
    isDragTarget,
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
}: ServerCardProps) {
    const t = useTranslation();
    const cardLayout = useCardLayout();
    const radialTitle = healthStatus
        ? t(`introHub.health.${healthStatus}`)
            + (healthLatencyMs && healthStatus !== 'pending' && healthStatus !== 'down' ? ` · ${healthLatencyMs}ms` : '')
            + (onRetryHealth ? ` · ${t('introHub.health.clickToRetry')}` : '')
        : undefined;
    const handleRetry = onRetryHealth ? () => onRetryHealth(server) : undefined;
    const proto = server.protocol || 'ftp';
    const quotaSupported = supportsStorageQuota(proto as ProviderType);
    const timeAgo = getTimeAgo(server.lastConnected);
    const handleMouseEnter = onHoverChange ? () => onHoverChange(server) : undefined;
    const handleMouseLeave = onHoverChange ? () => onHoverChange(null) : undefined;
    // Card body click toggles cross-profile selection — but only when the click
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
        const text = getServerSubtitle(server, {
            credentialsMasked,
            showUsername: !hideUsername,
        });
        return text || '\u00A0';
    }, [server, credentialsMasked, hideUsername]);

    // ===== GRID VIEW =====
    return (
        <div
            draggable={isDraggable}
            onDragStart={onDragStart}
            onDragEnter={onDragEnter}
            onDragOver={onDragOver}
            onDrop={onDrop}
            onDragEnd={onDragEnd}
            onClick={handleCardClick}
            className={`group relative bg-white dark:bg-gray-800 hover:bg-gray-50 dark:hover:bg-gray-750 border rounded-lg p-3.5 transition-colors shadow-sm dark:shadow-md ${isDraggable ? 'cursor-grab active:cursor-grabbing' : ''} ${onSelect ? 'cursor-pointer' : ''} ${isDragging ? 'opacity-40 scale-[0.97] shadow-lg ring-2 ring-blue-400/50 border-blue-400' : 'border-gray-100 dark:border-gray-700/50 hover:border-blue-200 dark:hover:border-blue-500/30'} ${isDragTarget ? '!border-blue-500 !border-2 bg-blue-50 dark:bg-blue-900/30 shadow-inner' : ''} ${selectionRingClass}`}
            onContextMenu={(e) => onContextMenu?.(e, server)}
            onMouseEnter={handleMouseEnter}
            onMouseLeave={handleMouseLeave}
            title={selectionTitle || undefined}
        >
            {/* Cross-Profile selection badge (top-left, doesn't overlap actions on the right) */}
            {isSelected && (
                <div className={`absolute top-2 left-2 flex items-center justify-center w-5 h-5 rounded-full pointer-events-none ${
                    isSource
                        ? 'bg-indigo-500 text-white shadow ring-1 ring-indigo-400/60'
                        : 'bg-emerald-500 text-white shadow ring-1 ring-emerald-400/60'
                }`}>
                    {isSource ? <ArrowUpRight size={12} strokeWidth={2.5} /> : <ArrowDownLeft size={12} strokeWidth={2.5} />}
                </div>
            )}
            {/* Top row: clickable icon + name + badge */}
            <div className="flex items-start gap-3">
                {/* Icon = connect button */}
                <button
                    onClick={(e) => { e.stopPropagation(); onConnect(server); }}
                    disabled={isConnecting}
                    className="w-10 h-10 shrink-0 rounded-lg bg-gray-100 dark:bg-gray-700 border border-gray-200/70 dark:border-gray-600 hover:bg-blue-100 dark:hover:bg-blue-900/30 hover:ring-2 hover:ring-blue-400/50 hover:border-blue-300 dark:hover:border-blue-500 flex items-center justify-center transition-all cursor-pointer disabled:cursor-wait"
                    title={t('common.connect')}
                >
                    {isConnecting ? <Loader2 size={18} className="animate-spin text-blue-500" /> : getServerIcon(server)}
                </button>
                <div className="flex-1 min-w-0">
                    {isRenaming ? (
                        <RenameInput
                            initialValue={server.name}
                            onSubmit={(v) => onRenameSubmit?.(server, v)}
                            onCancel={() => onRenameCancel?.()}
                            sizeClass="text-sm"
                        />
                    ) : (
                        <div className="text-sm font-semibold text-gray-900 dark:text-gray-100 truncate">{server.name}</div>
                    )}
                    <div className="flex items-center gap-1.5 mt-0.5">
                        <ServerBadges server={server} />
                        {timeAgo && (
                            <span className="text-[10px] text-gray-400 dark:text-gray-500 tabular-nums flex items-center gap-0.5"><Clock size={8} />{timeAgo}</span>
                        )}
                    </div>
                </div>
            </div>

            {/* Subtitle */}
            <div className="text-xs text-gray-500 dark:text-gray-400 truncate mt-2 min-h-[1rem]">{subtitle}</div>

            {/* Footer (detailed layout): quota left (only when cached), radial
                right. The radial is rendered whenever the layout is detailed so
                the click-to-retry affordance is always available — even before
                the first scan completes. The top border anchors the section so
                cards with and without quota still feel uniform. */}
            {cardLayout === 'detailed' && (
                <div className="mt-2.5 pt-2 border-t border-gray-100 dark:border-gray-700/60 grid grid-cols-[1fr_auto] items-center gap-2 min-h-[20px]">
                    <div className="min-w-0">
                        <StorageUsageBar quota={server.lastQuota} supported={quotaSupported} thresholds={thresholds} />
                    </div>
                    <div className="shrink-0 text-gray-300 dark:text-gray-600">
                        <HealthRadial
                            status={healthStatus || 'unknown'}
                            latencyMs={healthLatencyMs}
                            size={16}
                            title={radialTitle}
                            onRetry={handleRetry}
                        />
                    </div>
                </div>
            )}

            {/* Top-right: action buttons (hover) + favorite star (rightmost) */}
            <div className="absolute top-2 right-2 flex items-center gap-0.5">
                <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity">
                    <button onClick={(e) => { e.stopPropagation(); onEdit(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.edit')}>
                        <Edit2 size={12} />
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); onDuplicate(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.duplicate')}>
                        <Copy size={12} />
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); onDelete(server); }} className="p-1 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-500 dark:hover:text-red-400 transition-colors" title={t('common.delete')}>
                        <Trash2 size={12} />
                    </button>
                </div>
                <button
                    onClick={(e) => { e.stopPropagation(); onToggleFavorite(server); }}
                    className={`p-1 rounded-lg transition-colors ${
                        isFavorite
                            ? 'text-yellow-400 hover:text-yellow-500'
                            : 'text-gray-400 hover:text-yellow-400 opacity-0 group-hover:opacity-100'
                    }`}
                    title={isFavorite ? t('introHub.removeFavorite') : t('introHub.addFavorite')}
                >
                    <Star size={12} fill={isFavorite ? 'currentColor' : 'none'} />
                </button>
            </div>
        </div>
    );
});
