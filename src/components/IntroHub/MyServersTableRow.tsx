// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { ArrowDownLeft, ArrowUpRight, Clock, Copy, Edit2, Folder, GripVertical, HardDrive, Loader2, Star, Trash2 } from 'lucide-react';
import { ServerProfile, ProviderType, supportsStorageQuota } from '../../types';
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
import { HealthRadial } from './HealthRadial';
import { getServerIcon, getTimeAgo, RenameInput, ServerBadges } from './ServerCard';

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
    isDraggable?: boolean;
    isDragging?: boolean;
    isDragTarget?: boolean;
    onDragStart?: (e: React.DragEvent) => void;
    onDragEnter?: (e: React.DragEvent) => void;
    onDragOver?: (e: React.DragEvent) => void;
    onDrop?: (e: React.DragEvent) => void;
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
    isDraggable,
    isDragging,
    isDragTarget,
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
}: MyServersTableRowProps) {
    const t = useTranslation();
    const isCompact = density === 'compact';
    const rowPadY = isCompact ? 'py-1' : 'py-2';
    const iconBoxSize = isCompact ? 'w-8 h-8' : 'w-10 h-10';
    const iconSize = isCompact ? 16 : 18;
    const proto = server.protocol || 'ftp';
    const quotaSupported = supportsStorageQuota(proto as ProviderType);
    const timeAgo = getTimeAgo(server.lastConnected);
    const subtitle = React.useMemo(() => getServerSubtitle(server, {
        credentialsMasked,
        showUsername: !hideUsername,
    }) || ' ', [server, credentialsMasked, hideUsername]);
    const handleMouseEnter = onHoverChange ? () => onHoverChange(server) : undefined;
    const handleMouseLeave = onHoverChange ? () => onHoverChange(null) : undefined;
    const handleRetry = onRetryHealth ? () => onRetryHealth(server) : undefined;
    const radialTitle = healthStatus
        ? t(`introHub.health.${healthStatus}`)
            + (healthLatencyMs && healthStatus !== 'pending' && healthStatus !== 'down' ? ` · ${healthLatencyMs}ms` : '')
            + (onRetryHealth ? ` · ${t('introHub.health.clickToRetry')}` : '')
        : undefined;
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
    const quotaCells = (() => {
        if (!quotaSupported) {
            return { used: '-', total: '-', pct: '-', toneText: TONE_TEXT_CLASS.unknown };
        }
        const q = server.lastQuota;
        // Fetch hasn't completed yet: show ellipsis (loading state).
        if (!q) {
            return { used: '…', total: '…', pct: '…', toneText: TONE_TEXT_CLASS.unknown };
        }
        // Provider supports quota but the response has no byte cap (e.g.
        // Cloudinary free / credit-based plans): treat like an unsupported
        // provider rather than a stuck loader.
        if (!q.total || q.total <= 0) {
            return { used: '-', total: '-', pct: '-', toneText: TONE_TEXT_CLASS.unknown };
        }
        const { tone, pct } = getStorageTone(q.used, q.total, thresholds);
        const pctText = pct === null
            ? '-'
            : pct >= 10
                ? `${Math.round(pct)}%`
                : `${Math.round(pct * 10) / 10}%`;
        return {
            used: formatBytes(q.used),
            total: formatBytes(q.total),
            pct: pctText,
            toneText: TONE_TEXT_CLASS[tone],
        };
    })();
    const quotaTitle = quotaSupported && server.lastQuota && server.lastQuota.total > 0
        ? t('introHub.storageUsedOf', {
            used: formatBytes(server.lastQuota.used),
            total: formatBytes(server.lastQuota.total),
        })
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
                        onDragStart={isDraggable ? onDragStart : undefined}
                        onDragEnd={isDraggable ? onDragEnd : undefined}
                        className={`${cellClass} text-right text-[11px] tabular-nums text-gray-400 dark:text-gray-500 ${isDraggable ? 'cursor-grab active:cursor-grabbing' : ''}`}
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
                        <button
                            onClick={(e) => { e.stopPropagation(); onConnect(server); }}
                            className={`${iconBoxSize} mx-auto shrink-0 rounded-lg bg-gray-100 dark:bg-gray-700 border border-gray-200/70 dark:border-gray-600 hover:bg-blue-100 dark:hover:bg-blue-900/30 hover:ring-2 hover:ring-blue-400/50 hover:border-blue-300 dark:hover:border-blue-500 flex items-center justify-center transition-all cursor-pointer`}
                            title={t('common.connect')}
                        >
                            {isConnecting ? <Loader2 size={iconSize} className="animate-spin text-blue-500" /> : getServerIcon(server, iconSize + 2)}
                        </button>
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
                            <div className="text-sm font-medium text-gray-900 dark:text-gray-100 truncate">{server.name}</div>
                        )}
                    </td>
                );
            case 'badges':
                return (
                    <td key="badges" className={`${cellClass} ${alignTd('badges', 'left')}`}>
                        <div className={`flex items-center ${alignFlex('badges', 'left')}`}>
                            <ServerBadges server={server} />
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
                            <HealthRadial
                                status={healthStatus || 'unknown'}
                                latencyMs={healthLatencyMs}
                                size={16}
                                title={radialTitle}
                                onRetry={handleRetry}
                            />
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
                            <button onClick={(e) => { e.stopPropagation(); onDuplicate(server); }} className="p-1 rounded-lg hover:bg-gray-200 dark:hover:bg-gray-600 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors" title={t('common.duplicate')}>
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
                                isFavorite
                                    ? 'text-yellow-400 hover:text-yellow-500'
                                    : 'text-gray-400 hover:text-yellow-400 opacity-0 group-hover:opacity-100'
                            }`}
                            title={isFavorite ? t('introHub.removeFavorite') : t('introHub.addFavorite')}
                        >
                            <Star size={12} fill={isFavorite ? 'currentColor' : 'none'} />
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
            onDragEnter={onDragEnter}
            onDragOver={onDragOver}
            onDrop={onDrop}
            onClick={handleRowClick}
            onContextMenu={(e) => onContextMenu?.(e, server)}
            onMouseEnter={handleMouseEnter}
            onMouseLeave={handleMouseLeave}
            title={selectionTitle || undefined}
            className={`group transition-colors ${onSelect ? 'cursor-pointer' : ''} ${isDragging ? 'opacity-40 bg-blue-50 dark:bg-blue-900/20' : isDragTarget ? '' : index % 2 === 1 ? 'bg-gray-50/30 dark:bg-white/[0.02]' : ''} hover:bg-gray-100/50 dark:hover:bg-white/[0.04] ${isDragTarget ? 'border-b-2 !border-b-blue-500 bg-blue-50/50 dark:bg-blue-900/15' : ''} ${selectionRingClass}`}
        >
            {orderedColumns.map(col => renderCell(col.id))}
        </tr>
    );
});
