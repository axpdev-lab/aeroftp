// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { AlertTriangle, ArrowDownLeft, ArrowUpRight, Clock, Copy, Edit2, Folder, GripVertical, HardDrive, Heart, Loader2, Star, Trash2 } from 'lucide-react';
import { ServerProfile, profileHasQuota, resolveEffectiveQuota, effectiveManualCap } from '../../types';
import { getServerSubtitle } from '../../utils/serverSubtitle';
import { formatBytes } from '../../utils/formatters';
import {
    DEFAULT_THRESHOLDS,
    getStorageTone,
    TONE_TEXT_CLASS,
    type StorageThresholds,
} from '../../hooks/useStorageThresholds';
import type { MyServersDensity } from '../../hooks/useMyServersDensity';
import type { MyServersTableColId } from '../../hooks/useMyServersColumns';
import type { TableColAlign, TableColumnDef } from '../../hooks/useTableColumns';
import { useTranslation } from '../../i18n';
import { useFavoriteMarker } from '../../hooks/useFavoriteMarker';
import { HealthRadial } from './HealthRadial';
import { getServerIcon, getTimeAgo, RenameInput, ServerBadges, PeerPresenceDot } from './ServerCard';
import type { PeerDriveState } from '../../hooks/usePeerDriveStates';

interface MyServersTableRowProps {
    server: ServerProfile;
    index: number;
    orderedColumns: TableColumnDef<MyServersTableColId>[];
    isConnecting: boolean;
    credentialsMasked: boolean;
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
     *  on the row body. */
    onRenameStart?: (server: ServerProfile) => void;
    isDraggable?: boolean;
    isDragging?: boolean;
    isDragTarget?: boolean;
    /** Which edge of this row the insertion line is drawn on (#453). A drop
     *  makes the dragged profile inherit this row's index, so dragging *up*
     *  inserts above the row ('top') and dragging *down* leaves it below
     *  ('bottom'). Drawing the line on a fixed edge made every upward drop
     *  look one slot off. */
    dragTargetEdge?: 'top' | 'bottom';
    /** Position of this row in the parent's `servers` array. Lets the row
     *  bind the four parent drag callbacks to its own index without
     *  forcing the parent to re-curry on every render (issue #221). */
    dragIndex?: number;
    onDragStart?: (idx: number, e: React.DragEvent) => void;
    onDragEnter?: (idx: number, e: React.DragEvent) => void;
    onDragOver?: (idx: number, e: React.DragEvent) => void;
    onDrop?: (idx: number, e: React.DragEvent) => void;
    onDragEnd?: () => void;
    dragDisabledTitle?: string;
    selectionRole?: 'source' | 'destination' | null;
    onSelect?: (server: ServerProfile) => void;
    healthStatus?: 'up' | 'slow' | 'down' | 'pending' | 'unknown';
    healthLatencyMs?: number;
    onRetryHealth?: (server: ServerProfile) => void;
    thresholds?: StorageThresholds;
    density?: MyServersDensity;
    /** Resolve effective alignment per column (user override or default). */
    resolveAlign?: (id: MyServersTableColId) => TableColAlign;
    /** True when this profile has an open session: pulses the health radial. */
    hasActiveSession?: boolean;
    /** MTP device physically attached (fingerprint match). */
    deviceAttached?: boolean;
    /** AeroShare friend rows: live drive-state for the badge chip. */
    peerState?: PeerDriveState;
}

export const MyServersTableRow = React.memo(function MyServersTableRow({
    server,
    index,
    orderedColumns,
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
    dragTargetEdge = 'bottom',
    dragIndex,
    onDragStart,
    onDragEnter,
    onDragOver,
    onDrop,
    onDragEnd,
    dragDisabledTitle,
    selectionRole = null,
    onSelect,
    healthStatus,
    healthLatencyMs,
    onRetryHealth,
    thresholds = DEFAULT_THRESHOLDS,
    density = 'compact',
    resolveAlign,
    hasActiveSession = false,
    deviceAttached,
    peerState,
}: MyServersTableRowProps) {
    const t = useTranslation();
    const favoriteMarker = useFavoriteMarker();
    const isCompact = density === 'compact';
    const rowPadY = isCompact ? 'py-1' : 'py-2';
    const iconBoxSize = isCompact ? 'w-8 h-8' : 'w-10 h-10';
    const iconSize = isCompact ? 16 : 18;
    const isMtpDevice = server.protocol === 'mtp';
    const attachedTitle = deviceAttached
        ? t('introHub.deviceAttached')
        : t('introHub.deviceNotAttached');
    const quotaSupported = profileHasQuota(server);
    const timeAgo = getTimeAgo(server.lastConnected);
    const subtitle = React.useMemo(() => getServerSubtitle(server, {
        credentialsMasked,
        showUsername: !hideUsername,
    }) || ' ', [server, credentialsMasked, hideUsername]);
    const handleMouseEnter = onHoverChange ? () => onHoverChange(server) : undefined;
    const handleMouseLeave = onHoverChange ? () => onHoverChange(null) : undefined;
    const handleRetry = onRetryHealth ? () => onRetryHealth(server) : undefined;

    // Bind the parent's stable `(idx, e) => void` drag callbacks to this
    // row's `dragIndex` so the bound references survive parent re-renders.
    // This is what restores the `React.memo()` skip on rows whose own data
    // did not change (issue #221).
    const handleRowDragStart = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDragStart?.(dragIndex, e); },
        [onDragStart, dragIndex],
    );
    const handleRowDragEnter = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDragEnter?.(dragIndex, e); },
        [onDragEnter, dragIndex],
    );
    const handleRowDragOver = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDragOver?.(dragIndex, e); },
        [onDragOver, dragIndex],
    );
    const handleRowDrop = React.useCallback(
        (e: React.DragEvent) => { if (typeof dragIndex === 'number') onDrop?.(dragIndex, e); },
        [onDrop, dragIndex],
    );
    const radialTitle = isMtpDevice
        ? (hasActiveSession ? `${attachedTitle} (${t('common.goToActiveSession')})` : attachedTitle)
        : healthStatus
        ? t(`introHub.health.${healthStatus}`)
            + (healthLatencyMs && healthStatus !== 'pending' && healthStatus !== 'down' ? ` · ${healthLatencyMs}ms` : '')
            + (onRetryHealth ? ` · ${t('introHub.health.clickToRetry')}` : '')
        : undefined;
    // #180 / 4486730822: standalone connect-failure marker, mirror of ServerCard.
    const connectError = server.lastConnectionError;
    const connectErrorTitle = React.useMemo(() => {
        if (!connectError) return undefined;
        const when = getTimeAgo(connectError.timestamp);
        const head = t('introHub.connectError.failed');
        const ago = when ? t('introHub.connectError.lastFailedAt', { time: when }) : '';
        const reason = connectError.message || '';
        return [head, ago, reason].filter(Boolean).join(' · ');
    }, [connectError, t]);
    const isSource = selectionRole === 'source';
    const isDestination = selectionRole === 'destination';
    const isSelected = isSource || isDestination;
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
    const handleRowClick = onSelect ? (e: React.MouseEvent) => {
        const target = e.target as HTMLElement | null;
        if (target?.closest('button, input, a, [role="menuitem"]')) return;
        onSelect(server);
    } : undefined;
    // Resolve the effective quota with the item 4a precedence (a user-set
    // manual cap is a TRUE override) so the table stays consistent with the
    // card and the StatusBar even when the cached lastQuota was persisted
    // with total:0 (scan ran before the cap, or against a duplicate profile
    // lacking options.manualTotalBytes).
    const q = server.lastQuota;
    const manual = effectiveManualCap(server.options?.manualTotalBytes, server.protocol, server.providerId, server.host);
    const rawUsed = q?.used && q.used > 0 ? q.used : 0;
    const rawTotal = q?.total && q.total > 0 ? q.total : 0;
    const eff = resolveEffectiveQuota(rawUsed, rawTotal, manual);
    const usedKnown = !!q && q.used > 0;
    const quotaCells = (() => {
        if (!quotaSupported) {
            return { used: '-', total: '-', pct: '-', toneText: TONE_TEXT_CLASS.unknown };
        }
        if (!usedKnown) {
            // A cap exists (manual or API) but `used` has not been scanned
            // yet: show "- / cap" so it is visible once configured.
            if (eff.total > 0) {
                return { used: '-', total: formatBytes(eff.total), pct: '-', toneText: TONE_TEXT_CLASS.unknown };
            }
            // Fetch hasn't completed and no cap: ellipsis (loading state).
            if (!q) {
                return { used: '…', total: '…', pct: '…', toneText: TONE_TEXT_CLASS.unknown };
            }
            return { used: '-', total: '-', pct: '-', toneText: TONE_TEXT_CLASS.unknown };
        }
        // `used` known but no cap at all (no API total, no manual): show the
        // usage figure with "∞" instead of looking like a stuck loader.
        if (eff.total <= 0) {
            return { used: formatBytes(eff.used), total: '∞', pct: '-', toneText: TONE_TEXT_CLASS.unknown };
        }
        const { tone, pct } = getStorageTone(eff.used, eff.total, thresholds);
        const pctText = pct === null
            ? '-'
            : pct >= 10
                ? `${Math.round(pct)}%`
                : `${Math.round(pct * 10) / 10}%`;
        return {
            used: formatBytes(eff.used),
            total: formatBytes(eff.total),
            pct: pctText,
            toneText: TONE_TEXT_CLASS[tone],
        };
    })();
    const filesSuffix = q?.fileCount != null ? ` · ${q.fileCount} ${t('browser.files')}` : '';
    const quotaTitle = quotaSupported && usedKnown && eff.total > 0
        ? `${t('introHub.storageUsedOf', {
            used: formatBytes(eff.used),
            total: formatBytes(eff.total),
        })}${filesSuffix}`
        : quotaSupported && usedKnown
            ? `${formatBytes(eff.used)} ${t('statusBar.usedNoCap')}${filesSuffix}`
            : t('introHub.storageQuotaUnavailable');
    const cellClass = `px-3 ${rowPadY} align-middle border-b border-gray-100 dark:border-gray-700/50`;

    const alignTd = (id: MyServersTableColId, fallback: 'left' | 'center' | 'right'): string => {
        const a = resolveAlign?.(id) ?? fallback;
        return a === 'right' ? 'text-right' : a === 'center' ? 'text-center' : 'text-left';
    };
    const alignFlex = (id: MyServersTableColId, fallback: 'left' | 'center' | 'right'): string => {
        const a = resolveAlign?.(id) ?? fallback;
        return a === 'right' ? 'justify-end' : a === 'center' ? 'justify-center' : 'justify-start';
    };

    // Optional, default-hidden compression columns (Ehud #162): fed by the
    // aggregate written when an AeroVault op runs against this profile.
    const lc = server.lastCompression;
    const savedBytes = lc ? lc.plaintext - lc.compressed : undefined;
    const compressionCells = {
        saved: lc && savedBytes != null && savedBytes > 0 ? formatBytes(savedBytes) : '-',
        savedpct: lc ? `${lc.ratio >= 10 ? Math.round(lc.ratio) : Math.round(lc.ratio * 10) / 10}%` : '-',
    };
    const compressionTitle = lc
        ? t('introHub.table.columns.savedTitle', {
            plaintext: formatBytes(lc.plaintext),
            compressed: formatBytes(lc.compressed),
            ratio: String(Math.round(lc.ratio * 10) / 10),
        })
        : t('introHub.storageQuotaUnavailable');

    const renderCell = (id: MyServersTableColId): React.ReactNode => {
        switch (id) {
            case 'index':
                return (
                    <td
                        key="index"
                        // Drag initiates on the index <td> itself: WebKitGTK
                        // doesn't fire dragstart on <tr>, but it does on <td>
                        // and on plain divs/spans. Using the cell keeps the hit
                        // area generous (whole index column) without nesting a
                        // tiny div inside. The tr keeps drop-side handlers.
                        draggable={isDraggable}
                        onDragStart={isDraggable && onDragStart ? handleRowDragStart : undefined}
                        onDragEnd={isDraggable ? onDragEnd : undefined}
                        // `select-none`: a text selection left over from an
                        // earlier click makes the engine start a *text* drag
                        // instead of ours, so the grip silently refuses to
                        // pick the row up until the selection is cleared
                        // elsewhere (#453, "some rows won't drag anymore").
                        className={`${cellClass} select-none text-right text-[11px] tabular-nums text-gray-400 dark:text-gray-500 ${isDraggable ? 'cursor-grab active:cursor-grabbing' : ''}`}
                        title={dragDisabledTitle || (isDraggable ? t('introHub.table.dragToReorder') : undefined)}
                    >
                        <div className="flex items-center justify-end gap-1.5">
                            {isSelected && (
                                <span className={`shrink-0 flex items-center justify-center w-5 h-5 rounded-full ${
                                    isSource
                                        ? 'bg-indigo-500/15 text-indigo-600 dark:text-indigo-400 ring-1 ring-indigo-400/40'
                                        : 'bg-emerald-500/15 text-emerald-600 dark:text-emerald-400 ring-1 ring-emerald-400/40'
                                }`}>
                                    {isSource ? <ArrowUpRight size={11} strokeWidth={2.5} /> : <ArrowDownLeft size={11} strokeWidth={2.5} />}
                                </span>
                            )}
                            {isDraggable ? (
                                <GripVertical size={isCompact ? 12 : 14} className="text-gray-400 opacity-0 group-hover:opacity-70" />
                            ) : dragDisabledTitle ? (
                                <GripVertical size={isCompact ? 12 : 14} className="text-gray-300 dark:text-gray-600 cursor-not-allowed opacity-0 group-hover:opacity-70" />
                            ) : null}
                            <span>{index + 1}</span>
                        </div>
                    </td>
                );
            case 'icon':
                return (
                    <td key="icon" className={`${cellClass} text-center`}>
                        <div className="relative inline-block">
                            <button
                                onClick={(e) => { e.stopPropagation(); onConnect(server); }}
                                className={`${iconBoxSize} mx-auto shrink-0 rounded-lg flex items-center justify-center transition-all cursor-pointer ${
                                    hasActiveSession
                                        ? 'bg-emerald-50 dark:bg-emerald-900/20 border border-emerald-400/70 dark:border-emerald-500/60 ring-1 ring-emerald-400/40 hover:bg-emerald-100 dark:hover:bg-emerald-900/30 hover:ring-2 hover:ring-emerald-400/60'
                                        : 'bg-gray-100 dark:bg-gray-700 border border-gray-200/70 dark:border-gray-600 hover:bg-blue-100 dark:hover:bg-blue-900/30 hover:ring-2 hover:ring-blue-400/50 hover:border-blue-300 dark:hover:border-blue-500'
                                }`}
                                title={hasActiveSession ? t('common.goToActiveSession') : t('common.connect')}
                            >
                                {isConnecting ? <Loader2 size={iconSize} className="animate-spin text-blue-500" /> : getServerIcon(server, iconSize + 2)}
                            </button>
                            {connectError && (
                                <span
                                    className="absolute -top-1 -left-1 inline-flex items-center justify-center w-3.5 h-3.5 rounded-full bg-amber-600 dark:bg-amber-700 text-white shadow ring-2 ring-white dark:ring-gray-800 pointer-events-none"
                                    title={connectErrorTitle}
                                    aria-label={connectErrorTitle}
                                    data-testid="server-row-connect-error"
                                >
                                    <AlertTriangle size={9} strokeWidth={2.75} />
                                </span>
                            )}
                            {isMtpDevice && (
                                <span
                                    className={`absolute -bottom-0.5 -right-0.5 w-2.5 h-2.5 rounded-full ring-2 ring-white dark:ring-gray-800 pointer-events-none ${
                                        deviceAttached ? 'bg-green-500' : 'bg-red-500'
                                    } ${hasActiveSession && deviceAttached ? 'animate-pulse' : ''}`}
                                    title={radialTitle}
                                    aria-label={radialTitle}
                                    data-testid="server-row-device-attached"
                                />
                            )}
                            {/* AeroShare drive-state presence dot (matches the grid card). */}
                            {server.protocol === 'peer' && <PeerPresenceDot peerState={peerState} hasActiveSession={hasActiveSession} />}
                        </div>
                    </td>
                );
            case 'name':
                return (
                    <td key="name" className={`${cellClass} ${alignTd('name', 'left')}`}>
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
                                // bubble to the row's Cross-Profile select
                                // handler. The row body stays the select target.
                                className={`text-sm font-medium text-gray-900 dark:text-gray-100 truncate select-none ${onRenameStart ? 'cursor-text hover:text-blue-600 dark:hover:text-blue-400' : ''}`}
                                onClick={onRenameStart ? (e) => e.stopPropagation() : undefined}
                                onDoubleClick={onRenameStart ? (e) => { e.stopPropagation(); onRenameStart(server); } : undefined}
                                title={onRenameStart ? t('introHub.doubleClickToRename') : undefined}
                            >
                                {server.name}
                            </div>
                        )}
                    </td>
                );
            case 'badges':
                return (
                    <td key="badges" className={`${cellClass} ${alignTd('badges', 'left')}`}>
                        <div className={`flex items-center ${alignFlex('badges', 'left')}`}>
                            <ServerBadges server={server} peerState={peerState} />
                        </div>
                    </td>
                );
            case 'subtitle':
                return (
                    <td key="subtitle" className={`${cellClass} ${alignTd('subtitle', 'left')} text-xs text-gray-500 dark:text-gray-400 truncate`}>
                        {subtitle}
                    </td>
                );
            case 'used':
                return <td key="used" className={`${cellClass} ${alignTd('used', 'right')} text-[11px] text-gray-500 dark:text-gray-400 tabular-nums`} title={quotaTitle}>{quotaCells.used}</td>;
            case 'total':
                return <td key="total" className={`${cellClass} ${alignTd('total', 'right')} text-[11px] text-gray-400 dark:text-gray-500 tabular-nums`} title={quotaTitle}>{quotaCells.total}</td>;
            case 'pct':
                return <td key="pct" className={`${cellClass} ${alignTd('pct', 'right')} text-[11px] font-medium tabular-nums ${quotaCells.toneText}`} title={quotaTitle}>{quotaCells.pct}</td>;
            case 'saved':
                return <td key="saved" className={`${cellClass} ${alignTd('saved', 'right')} text-[11px] text-gray-500 dark:text-gray-400 tabular-nums`} title={compressionTitle}>{compressionCells.saved}</td>;
            case 'savedpct':
                return <td key="savedpct" className={`${cellClass} ${alignTd('savedpct', 'right')} text-[11px] text-gray-400 dark:text-gray-500 tabular-nums`} title={compressionTitle}>{compressionCells.savedpct}</td>;
            case 'paths':
                return (
                    <td key="paths" className={`${cellClass} ${alignTd('paths', 'right')}`}>
                        <div className={`flex flex-col gap-0.5 min-w-0 ${alignTd('paths', 'right')}`}>
                            {server.initialPath && (
                                <span className={`flex items-center ${alignFlex('paths', 'right')} gap-1 text-[10px] text-gray-400 dark:text-gray-500`} title={server.initialPath}>
                                    <Folder size={8} className="shrink-0" />
                                    <span className="truncate" dir="rtl">{server.initialPath}</span>
                                </span>
                            )}
                            {server.localInitialPath && (
                                <span className={`flex items-center ${alignFlex('paths', 'right')} gap-1 text-[10px] text-gray-400 dark:text-gray-500`} title={server.localInitialPath}>
                                    <HardDrive size={8} className="shrink-0" />
                                    <span className="truncate" dir="rtl">{server.localInitialPath}</span>
                                </span>
                            )}
                        </div>
                    </td>
                );
            case 'time':
                return (
                    <td key="time" className={`${cellClass} ${alignTd('time', 'right')} text-[11px] text-gray-400 dark:text-gray-500 tabular-nums`}>
                        {timeAgo && <span className="inline-flex items-center gap-0.5"><Clock size={9} />{timeAgo}</span>}
                    </td>
                );
            case 'health':
                return (
                    <td key="health" className={`${cellClass} ${alignTd('health', 'center')} text-gray-300 dark:text-gray-600`}>
                        <span className={`inline-flex items-center ${alignFlex('health', 'center')}`}>
                            {isMtpDevice ? (
                                <span
                                    className={`inline-block w-3.5 h-3.5 rounded-full ring-2 ring-white dark:ring-gray-800 ${
                                        deviceAttached ? 'bg-green-500' : 'bg-red-500'
                                    } ${hasActiveSession && deviceAttached ? 'animate-pulse' : ''}`}
                                    title={radialTitle}
                                    aria-label={radialTitle}
                                    data-testid="server-row-device-attached-health"
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
                        </span>
                    </td>
                );
            case 'actions':
                return (
                    <td key="actions" className={`${cellClass} ${alignTd('actions', 'right')}`}>
                        <div className={`flex items-center ${alignFlex('actions', 'right')} gap-0.5 opacity-0 group-hover:opacity-100 transition-opacity`}>
                            <button onClick={(e) => { e.stopPropagation(); onEdit(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.edit')}>
                                <Edit2 size={13} />
                            </button>
                            <button onClick={(e) => { e.stopPropagation(); onDuplicate(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.copy')}>
                                <Copy size={13} />
                            </button>
                            <button onClick={(e) => { e.stopPropagation(); onDelete(server); }} className="p-1 rounded-lg hover:bg-red-100 dark:hover:bg-red-900/30 text-gray-400 hover:text-red-500 dark:hover:text-red-400 transition-colors" title={t('common.delete')}>
                                <Trash2 size={13} />
                            </button>
                        </div>
                    </td>
                );
            case 'favorite':
                return (
                    <td key="favorite" className={`${cellClass} ${alignTd('favorite', 'center')}`}>
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
                    </td>
                );
            default:
                return null;
        }
    };

    return (
        <tr
            // NOTE: `draggable`/`onDragStart` live on the explicit grip handle
            // in the index cell (WebKitGTK doesn't reliably fire dragstart on
            // <tr>). The row keeps the drop-side handlers so users can drop
            // anywhere along the row.
            onDragEnter={onDragEnter ? handleRowDragEnter : undefined}
            onDragOver={onDragOver ? handleRowDragOver : undefined}
            onDrop={onDrop ? handleRowDrop : undefined}
            onClick={handleRowClick}
            onContextMenu={(e) => onContextMenu?.(e, server)}
            onMouseEnter={handleMouseEnter}
            onMouseLeave={handleMouseLeave}
            title={selectionTitle || undefined}
            className={`group transition-colors ${onSelect ? 'cursor-pointer' : ''} ${isDragging ? 'opacity-40 bg-blue-50 dark:bg-blue-900/20' : isDragTarget ? '' : index % 2 === 1 ? 'bg-gray-50/30 dark:bg-white/[0.02]' : ''} hover:bg-gray-100/50 dark:hover:bg-white/[0.04] ${isDragTarget ? `${dragTargetEdge === 'top' ? 'border-t-2 !border-t-blue-500' : 'border-b-2 !border-b-blue-500'} bg-blue-50/50 dark:bg-blue-900/15` : ''} ${selectionRingClass}`}
        >
            {orderedColumns.map(col => renderCell(col.id))}
        </tr>
    );
});
