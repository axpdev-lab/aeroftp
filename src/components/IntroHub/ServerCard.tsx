import * as React from 'react';
import { Edit2, Trash2, Copy, Loader2, Star, GripVertical, Clock, AlertTriangle, ShieldCheck, Folder, HardDrive, Check, X } from 'lucide-react';
import { ServerProfile, ProviderType, getProtocolClass } from '../../types';
import { ProtocolIcon } from '../ProtocolSelector';
import { PROVIDER_LOGOS } from '../ProviderLogos';
import { maskCredential } from '../../utils/maskCredential';
import { getGitHubConnectionBadge, getMegaConnectionBadge, getInfiniCloudConnectionBadge } from '../../utils/providerConnectionMeta';
import { useTranslation } from '../../i18n';

function ServerBadges({ server }: { server: ServerProfile }) {
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
    const protocolClass = getProtocolClass(proto as ProviderType);
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

    const badgeClass = isFtps
        ? 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300'
        : isSftp
            ? 'bg-teal-100 dark:bg-teal-900/40 text-teal-700 dark:text-teal-300'
            : isPlainFtp
                ? 'bg-amber-100 dark:bg-amber-900/40 text-amber-700 dark:text-amber-300'
                : 'bg-gray-200 dark:bg-gray-600 text-gray-600 dark:text-gray-300';

    return (
        <div className="flex items-center gap-1 flex-wrap">
            {server.providerId === 'felicloud' ? (
                <span className="text-[10px] px-1.5 py-0.5 rounded font-medium uppercase"
                      style={{ backgroundColor: '#0083ce22', color: '#0083ce' }}>
                    API OCS
                </span>
            ) : (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium uppercase ${badgeClass}`}>
                    {displayProto}
                </span>
            )}
            {showClassBadge && server.providerId !== 'felicloud' && (
                <span className={`text-[10px] px-1.5 py-0.5 rounded font-medium ${classBadgeColor[protocolClass] || 'bg-gray-100 dark:bg-gray-700 text-gray-600 dark:text-gray-400'}`}>
                    {protocolClass}
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
    viewMode: 'grid' | 'list';
    index?: number; // For zebra striping in list view
    isDraggable?: boolean;
    isDragging?: boolean;
    isDragTarget?: boolean;
    onDragStart?: (e: React.DragEvent) => void;
    onDragEnter?: (e: React.DragEvent) => void;
    onDragOver?: (e: React.DragEvent) => void;
    onDrop?: (e: React.DragEvent) => void;
    onDragEnd?: () => void;
}

function RenameInput({
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

function getServerIcon(server: ServerProfile, size = 20): React.ReactNode {
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

function getTimeAgo(dateStr?: string): string {
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
    viewMode,
    index = 0,
    isDraggable,
    isDragging,
    isDragTarget,
    onDragStart,
    onDragEnter,
    onDragOver,
    onDrop,
    onDragEnd,
}: ServerCardProps) {
    const t = useTranslation();
    const proto = server.protocol || 'ftp';
    const timeAgo = getTimeAgo(server.lastConnected);
    const handleMouseEnter = onHoverChange ? () => onHoverChange(server) : undefined;
    const handleMouseLeave = onHoverChange ? () => onHoverChange(null) : undefined;

    const subtitle = React.useMemo(() => {
        const shouldMask = credentialsMasked && server.protocol !== 'github';
        const user = shouldMask && server.username
            ? maskCredential(server.username)
            : server.username;
        const host = shouldMask && server.host
            ? maskCredential(server.host)
            : server.host;
        if (user && host) return `${user}@${host}`;
        if (host) return host;
        if (user) return user;
        return '\u00A0';
    }, [server.username, server.host, credentialsMasked, server.protocol]);

    // ===== LIST VIEW (table-like columns) =====
    if (viewMode === 'list') {
        return (
            <div
                draggable={isDraggable}
                onDragStart={onDragStart}
                onDragEnter={onDragEnter}
                onDragOver={onDragOver}
                onDrop={onDrop}
                onDragEnd={onDragEnd}
                className={`group flex items-center gap-2 px-3 py-2 border-b border-gray-100 dark:border-gray-700/50 transition-colors ${isDraggable ? 'cursor-grab active:cursor-grabbing' : ''} ${isDragging ? 'opacity-40 bg-blue-50 dark:bg-blue-900/20' : isDragTarget ? '' : index % 2 === 1 ? 'bg-gray-50/30 dark:bg-white/[0.02]' : ''} hover:bg-gray-100/50 dark:hover:bg-white/[0.04] ${isDragTarget ? 'border-b-2 !border-b-blue-500 bg-blue-50/50 dark:bg-blue-900/15' : ''}`}
                onContextMenu={(e) => onContextMenu?.(e, server)}
                onMouseEnter={handleMouseEnter}
                onMouseLeave={handleMouseLeave}
            >
                {/* Drag handle */}
                {isDraggable && (
                    <div className="text-gray-400 opacity-0 group-hover:opacity-60 shrink-0 -ml-1">
                        <GripVertical size={14} />
                    </div>
                )}

                {/* Icon = connect button (same size as grid view) */}
                <button
                    onClick={(e) => { e.stopPropagation(); onConnect(server); }}
                    className="w-10 h-10 shrink-0 rounded-lg bg-gray-100 dark:bg-gray-700 border border-gray-200/70 dark:border-gray-600 hover:bg-blue-100 dark:hover:bg-blue-900/30 hover:ring-2 hover:ring-blue-400/50 hover:border-blue-300 dark:hover:border-blue-500 flex items-center justify-center transition-all cursor-pointer"
                    title={t('common.connect')}
                >
                    {isConnecting ? <Loader2 size={18} className="animate-spin text-blue-500" /> : getServerIcon(server)}
                </button>

                {/* Col: Name */}
                <div className="flex-1 min-w-0 max-w-[200px]">
                    {isRenaming ? (
                        <RenameInput
                            initialValue={server.name}
                            onSubmit={(v) => onRenameSubmit?.(server, v)}
                            onCancel={() => onRenameCancel?.()}
                            sizeClass="text-sm"
                        />
                    ) : (
                        <div className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">{server.name}</div>
                    )}
                </div>

                {/* Col: Badge */}
                <div className="shrink-0">
                    <ServerBadges server={server} />
                </div>

                {/* Col: User/Host */}
                <div className="flex-1 min-w-0 text-xs text-gray-500 dark:text-gray-400 truncate">
                    {subtitle}
                </div>

                {/* Col: Paths (remote / local, 2 rows) */}
                {(server.initialPath || server.localInitialPath) && (
                    <div className="flex flex-col gap-0.5 min-w-0 max-w-[200px] text-right">
                        {server.initialPath && (
                            <span className="flex items-center justify-end gap-1 text-[10px] text-gray-400 dark:text-gray-500" title={server.initialPath}>
                                <Folder size={8} className="shrink-0" />
                                <span className="truncate" dir="rtl">{server.initialPath}</span>
                            </span>
                        )}
                        {server.localInitialPath && (
                            <span className="flex items-center justify-end gap-1 text-[10px] text-gray-400 dark:text-gray-500" title={server.localInitialPath}>
                                <HardDrive size={8} className="shrink-0" />
                                <span className="truncate" dir="rtl">{server.localInitialPath}</span>
                            </span>
                        )}
                    </div>
                )}

                {/* Col: Time */}
                {timeAgo && (
                    <span className="text-[11px] text-gray-400 dark:text-gray-500 tabular-nums shrink-0 text-right flex items-center gap-0.5"><Clock size={9} />{timeAgo}</span>
                )}

                {/* Actions (hover) */}
                <div className="flex items-center gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity shrink-0">
                    <button onClick={(e) => { e.stopPropagation(); onEdit(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.edit')}>
                        <Edit2 size={13} />
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); onDuplicate(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.duplicate')}>
                        <Copy size={13} />
                    </button>
                    <button onClick={(e) => { e.stopPropagation(); onDelete(server); }} className="p-1 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-500 dark:hover:text-red-400 transition-colors" title={t('common.delete')}>
                        <Trash2 size={13} />
                    </button>
                </div>

                {/* Favorite star */}
                <button
                    onClick={(e) => { e.stopPropagation(); onToggleFavorite(server); }}
                    className={`p-1 rounded-lg transition-colors shrink-0 ${
                        isFavorite
                            ? 'text-yellow-400 hover:text-yellow-500'
                            : 'text-gray-400 hover:text-yellow-400 opacity-0 group-hover:opacity-100'
                    }`}
                    title={isFavorite ? t('introHub.removeFavorite') : t('introHub.addFavorite')}
                >
                    <Star size={12} fill={isFavorite ? 'currentColor' : 'none'} />
                </button>
            </div>
        );
    }

    // ===== GRID VIEW =====
    return (
        <div
            draggable={isDraggable}
            onDragStart={onDragStart}
            onDragEnter={onDragEnter}
            onDragOver={onDragOver}
            onDrop={onDrop}
            onDragEnd={onDragEnd}
            className={`group relative bg-white dark:bg-gray-800 hover:bg-gray-50 dark:hover:bg-gray-750 border rounded-lg p-3.5 transition-colors shadow-sm dark:shadow-md ${isDraggable ? 'cursor-grab active:cursor-grabbing' : ''} ${isDragging ? 'opacity-40 scale-[0.97] shadow-lg ring-2 ring-blue-400/50 border-blue-400' : 'border-gray-100 dark:border-gray-700/50 hover:border-blue-200 dark:hover:border-blue-500/30'} ${isDragTarget ? '!border-blue-500 !border-2 bg-blue-50 dark:bg-blue-900/30 shadow-inner' : ''}`}
            onContextMenu={(e) => onContextMenu?.(e, server)}
            onMouseEnter={handleMouseEnter}
            onMouseLeave={handleMouseLeave}
        >
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
