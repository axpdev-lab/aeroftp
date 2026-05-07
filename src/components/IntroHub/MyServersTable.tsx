// SPDX-License-Identifier: GPL-3.0-or-later
// Copyright (c) 2024-2026 axpnet -- AI-assisted (see AI-TRANSPARENCY.md)

import * as React from 'react';
import { Cloud, Database, Globe, KeyRound, Server as ServerIcon, Shield } from 'lucide-react';
import { getE2EBits, getProtocolClass, ServerProfile, type ProviderType } from '../../types';
import {
    MY_SERVERS_TABLE_COLUMNS,
    type MyServersSortableColId,
    type MyServersTableColId,
    type MyServersTableColumnsResult,
} from '../../hooks/useMyServersColumns';
import type { MyServersDensity } from '../../hooks/useMyServersDensity';
import type { StorageThresholds } from '../../hooks/useStorageThresholds';
import { TONE_TEXT_CLASS, getStorageTone } from '../../hooks/useStorageThresholds';
import { useTranslation } from '../../i18n';
import { MyServersTableHeader } from './MyServersTableHeader';
import { MyServersTableRow } from './MyServersTableRow';
import { aggregateByDedupKey } from '../../utils/storageDedup';
import { formatBytes } from '../../utils/formatters';

type HealthStatus = 'up' | 'slow' | 'down' | 'pending' | 'unknown';

interface MyServersTableProps {
    servers: ServerProfile[];
    allServers: ServerProfile[];
    columns: MyServersTableColumnsResult;
    favorites: Set<string>;
    connectingId: string | null;
    oauthConnecting: string | null;
    credentialsMasked: boolean;
    hideUsername: boolean;
    onConnect: (server: ServerProfile) => void;
    onEdit: (server: ServerProfile) => void;
    onDuplicate: (server: ServerProfile) => void;
    onDelete: (server: ServerProfile) => void;
    onToggleFavorite: (server: ServerProfile) => void;
    onContextMenu?: (e: React.MouseEvent, server: ServerProfile) => void;
    onHoverChange?: (server: ServerProfile | null) => void;
    renamingId: string | null;
    onRenameSubmit: (server: ServerProfile, newName: string) => void;
    onRenameCancel: () => void;
    canDrag: boolean;
    dragIdx: number | null;
    overIdx: number | null;
    onDragStart: (idx: number) => (e: React.DragEvent) => void;
    onDragEnter: (idx: number) => (e: React.DragEvent) => void;
    onDragOver: (idx: number) => (e: React.DragEvent) => void;
    onDrop: (idx: number) => (e: React.DragEvent) => void;
    onDragEnd: () => void;
    crossProfileSelection: string[];
    onSelect: (server: ServerProfile) => void;
    cardLayout: 'compact' | 'detailed';
    getHealthStatus: (serverId: string) => { status: HealthStatus; latencyMs?: number } | undefined;
    onRetryHealth: (server: ServerProfile) => void;
    thresholds: StorageThresholds;
    density: MyServersDensity;
}

const pctOf = (server: ServerProfile) => {
    const quota = server.lastQuota;
    if (!quota || !quota.total || quota.total <= 0) return -1;
    return (quota.used / quota.total) * 100;
};

const dateOf = (server: ServerProfile) => {
    const ts = Date.parse(server.lastConnected || '');
    return Number.isFinite(ts) ? ts : -1;
};

const PROTOCOL_ICON: Record<string, React.ReactNode> = {
    OAuth: <Cloud size={14} className="text-blue-500" />,
    API: <Database size={14} className="text-emerald-500" />,
    WebDAV: <Globe size={14} className="text-cyan-500" />,
    E2E: <Shield size={14} className="text-violet-500" />,
    FTP: <ServerIcon size={14} className="text-gray-500" />,
    FTPS: <ServerIcon size={14} className="text-sky-500" />,
    SFTP: <KeyRound size={14} className="text-indigo-500" />,
    S3: <Database size={14} className="text-orange-500" />,
    Azure: <Cloud size={14} className="text-blue-600" />,
    AeroCloud: <Cloud size={14} className="text-purple-500" />,
};

const badgeSortLabel = (server: ServerProfile) => {
    const proto = (server.protocol || 'ftp') as ProviderType;
    const protocolClass = getProtocolClass(proto);
    const e2eBits = protocolClass === 'E2E' ? getE2EBits(proto) : null;
    if (server.providerId === 'felicloud' || server.providerId === 'tabdigital') return 'API OCS';
    return [
        e2eBits ? `${protocolClass} ${e2eBits}-bit` : protocolClass,
        server.providerId || server.protocol || '',
    ].join(' ');
};

export function MyServersTable({
    servers,
    allServers,
    columns,
    favorites,
    connectingId,
    oauthConnecting,
    credentialsMasked,
    hideUsername,
    onConnect,
    onEdit,
    onDuplicate,
    onDelete,
    onToggleFavorite,
    onContextMenu,
    onHoverChange,
    renamingId,
    onRenameSubmit,
    onRenameCancel,
    canDrag,
    dragIdx,
    overIdx,
    onDragStart,
    onDragEnter,
    onDragOver,
    onDrop,
    onDragEnd,
    crossProfileSelection,
    onSelect,
    cardLayout,
    getHealthStatus,
    onRetryHealth,
    thresholds,
    density,
}: MyServersTableProps) {
    const t = useTranslation();
    const { config, orderedVisibleColumns, resolveAlign } = columns;
    const sort = config.sort;
    const sortLabel = sort
        ? t(MY_SERVERS_TABLE_COLUMNS.find(col => col.id === sort.colId)?.labelKey || '')
        : '';
    const dragDisabledTitle = sort
        ? t('introHub.table.clickToReturnManual', { column: sortLabel })
        : undefined;

    // Live width override during pointermove. Persisted on pointerup (handled
    // by useTableColumns.setWidth). We hold a ref to <colgroup> children and
    // mutate their width directly to avoid re-rendering the whole table for
    // every pixel.
    const colRefs = React.useRef<Map<MyServersTableColId, HTMLTableColElement>>(new Map());
    const handleLiveResize = React.useCallback((id: MyServersTableColId, widthPx: number) => {
        const el = colRefs.current.get(id);
        if (el) el.style.width = `${widthPx}px`;
    }, []);

    const handleReorder = React.useCallback((sourceId: MyServersTableColId, targetId: MyServersTableColId) => {
        const middle = columns.orderedAllColumns
            .filter(c => !c.pinnedStart && !c.pinnedEnd)
            .map(c => c.id);
        if (!middle.includes(sourceId) || !middle.includes(targetId)) return;
        const next = middle.filter(id => id !== sourceId);
        const idx = next.indexOf(targetId);
        if (idx < 0) return;
        next.splice(idx, 0, sourceId);
        const startIds = columns.orderedAllColumns.filter(c => c.pinnedStart).map(c => c.id);
        const endIds = columns.orderedAllColumns.filter(c => c.pinnedEnd).map(c => c.id);
        columns.setOrder([...startIds, ...next, ...endIds]);
    }, [columns]);

    const sortedServers = React.useMemo(() => {
        if (!sort) return servers;
        const comparators: Record<MyServersSortableColId, (a: ServerProfile, b: ServerProfile) => number> = {
            index: () => 0,
            name: (a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }),
            badges: (a, b) => badgeSortLabel(a).localeCompare(badgeSortLabel(b), undefined, { numeric: true, sensitivity: 'base' }),
            used: (a, b) => (a.lastQuota?.used ?? -1) - (b.lastQuota?.used ?? -1),
            total: (a, b) => (a.lastQuota?.total ?? -1) - (b.lastQuota?.total ?? -1),
            pct: (a, b) => pctOf(a) - pctOf(b),
            time: (a, b) => dateOf(a) - dateOf(b),
            favorite: (a, b) => Number(favorites.has(b.id)) - Number(favorites.has(a.id)),
        };
        const direction = sort.dir === 'desc' ? -1 : 1;
        const colId = sort.colId as MyServersSortableColId;
        const cmp = comparators[colId];
        if (!cmp) return servers;
        const withIndex = servers.map((server, index) => ({ server, index }));
        return withIndex
            .sort((a, b) => {
                const result = cmp(a.server, b.server) * direction;
                return result || a.index - b.index;
            })
            .map(item => item.server);
    }, [servers, sort, favorites]);

    const protocolRows = React.useMemo(() => {
        const aggregate = aggregateByDedupKey(servers);
        return aggregate.byProtocolClass.filter(row => row.profiles > 0);
    }, [servers]);

    const renderProtocolSummaryCell = (id: MyServersTableColId, row: (typeof protocolRows)[number], idx: number) => {
        const tone = getStorageTone(row.used, row.total, thresholds);
        const pct = row.total > 0 ? (row.used / row.total) * 100 : null;
        const pctText = pct === null ? '-' : `${pct.toFixed(1)}%`;
        // For uncapped providers (B2 native, Cloudinary) total = 0 but used > 0:
        // surface the usage figure instead of a placeholder.
        const usedText = row.total > 0 || row.used > 0 ? formatBytes(row.used) : '-';
        const totalText = row.total > 0 ? formatBytes(row.total) : '-';
        const cellClass = 'px-3 py-1.5 align-middle border-b border-gray-100 dark:border-gray-700/40 text-xs';

        switch (id) {
            case 'index':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-right text-gray-400 tabular-nums`}>P{idx + 1}</td>;
            case 'icon':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-center`}><span className="inline-flex items-center justify-center">{PROTOCOL_ICON[row.protocolClass] || <ServerIcon size={14} className="text-gray-500" />}</span></td>;
            case 'name':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-gray-800 dark:text-gray-100 font-medium`}>{row.protocolClass}</td>;
            case 'badges':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-gray-500 dark:text-gray-400`}>{t('introHub.breakdown.title')}</td>;
            case 'subtitle':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-gray-500 dark:text-gray-400`}>{t('introHub.table.footerUniqueCount', { count: row.unique })}</td>;
            case 'used':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-right tabular-nums text-gray-600 dark:text-gray-300`}>{usedText}</td>;
            case 'total':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-right tabular-nums text-gray-600 dark:text-gray-300`}>{totalText}</td>;
            case 'pct':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-right tabular-nums font-medium ${TONE_TEXT_CLASS[tone.tone]}`}>{pctText}</td>;
            case 'health':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-center text-gray-500 dark:text-gray-400 tabular-nums`}>{row.profiles}</td>;
            case 'actions':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-right text-gray-500 dark:text-gray-400 tabular-nums`}>{row.unique}</td>;
            case 'favorite':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass}`} />;
            case 'paths':
            case 'time':
                return <td key={`${id}-${row.protocolClass}`} className={`${cellClass} text-right text-gray-400 dark:text-gray-500`}>-</td>;
            default:
                return <td key={`${id}-${row.protocolClass}`} className={cellClass}>-</td>;
        }
    };

    return (
        <div data-my-servers-table>
            <table className="w-full min-w-[1100px] border-collapse text-left" style={{ tableLayout: 'fixed' }}>
                <colgroup>
                    {orderedVisibleColumns.map((col) => (
                        <col
                            key={col.id}
                            ref={(el) => {
                                if (el) colRefs.current.set(col.id, el);
                                else colRefs.current.delete(col.id);
                            }}
                            style={{ width: `${config.widths[col.id]}px` }}
                        />
                    ))}
                </colgroup>
                <MyServersTableHeader
                    columns={columns}
                    onReorder={handleReorder}
                    onLiveResize={handleLiveResize}
                />
                <tbody>
                    {sortedServers.map((server, idx) => {
                        const realIdx = allServers.findIndex(s => s.id === server.id);
                        const selectionIndex = crossProfileSelection.indexOf(server.id);
                        const selectionRole: 'source' | 'destination' | null =
                            selectionIndex === 0 ? 'source' : selectionIndex === 1 ? 'destination' : null;
                        const health = cardLayout === 'detailed' ? getHealthStatus(server.id) : undefined;
                        return (
                            <MyServersTableRow
                                key={server.id}
                                server={server}
                                index={idx}
                                orderedColumns={orderedVisibleColumns}
                                isConnecting={connectingId === server.id || oauthConnecting === server.id}
                                credentialsMasked={credentialsMasked}
                                hideUsername={hideUsername}
                                isFavorite={favorites.has(server.id)}
                                onConnect={onConnect}
                                onEdit={onEdit}
                                onDuplicate={onDuplicate}
                                onDelete={onDelete}
                                onToggleFavorite={onToggleFavorite}
                                onContextMenu={onContextMenu}
                                onHoverChange={onHoverChange}
                                isRenaming={renamingId === server.id}
                                onRenameSubmit={onRenameSubmit}
                                onRenameCancel={onRenameCancel}
                                isDraggable={canDrag}
                                isDragging={dragIdx === realIdx}
                                isDragTarget={overIdx === realIdx && dragIdx !== null && dragIdx !== realIdx}
                                onDragStart={canDrag ? onDragStart(realIdx) : undefined}
                                onDragEnter={canDrag ? onDragEnter(realIdx) : undefined}
                                onDragOver={canDrag ? onDragOver(realIdx) : undefined}
                                onDrop={canDrag ? onDrop(realIdx) : undefined}
                                onDragEnd={canDrag ? onDragEnd : undefined}
                                dragDisabledTitle={dragDisabledTitle}
                                selectionRole={selectionRole}
                                onSelect={onSelect}
                                healthStatus={health?.status}
                                healthLatencyMs={health?.latencyMs}
                                onRetryHealth={cardLayout === 'detailed' ? onRetryHealth : undefined}
                                thresholds={thresholds}
                                density={density}
                                resolveAlign={resolveAlign}
                            />
                        );
                    })}
                    {protocolRows.map((row, idx) => (
                        <tr
                            key={`protocol-${row.protocolClass}`}
                            className="bg-blue-50/50 dark:bg-blue-900/10"
                        >
                            {orderedVisibleColumns.map(col => renderProtocolSummaryCell(col.id, row, idx))}
                        </tr>
                    ))}
                </tbody>
            </table>
        </div>
    );
}
