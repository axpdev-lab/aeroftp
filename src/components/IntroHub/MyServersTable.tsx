import * as React from 'react';
import { ServerProfile } from '../../types';
import type {
    MyServersColumnVisibility,
    MyServersSort,
    MyServersSortableColId,
    MyServersTableColId,
} from '../../hooks/useMyServersColumns';
import { MY_SERVERS_TABLE_COLUMNS } from '../../hooks/useMyServersColumns';
import type { MyServersDensity } from '../../hooks/useMyServersDensity';
import type { StorageThresholds } from '../../hooks/useStorageThresholds';
import { useTranslation } from '../../i18n';
import { MyServersTableHeader } from './MyServersTableHeader';
import { MyServersTableRow } from './MyServersTableRow';
import { MyServersTableFooter } from './MyServersTableFooter';

type HealthStatus = 'up' | 'slow' | 'down' | 'pending' | 'unknown';

interface MyServersTableProps {
    servers: ServerProfile[];
    allServers: ServerProfile[];
    visibility: MyServersColumnVisibility;
    sort: MyServersSort | null;
    onSort: (sort: MyServersSort | null) => void;
    onVisibleChange: (colId: MyServersTableColId, visible: boolean) => void;
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

export function MyServersTable({
    servers,
    allServers,
    visibility,
    sort,
    onSort,
    onVisibleChange,
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
    const effectiveVisibility = React.useMemo(() => {
        const next = { ...visibility, health: visibility.health && cardLayout === 'detailed' };
        if (!Object.values(next).some(Boolean)) next.name = true;
        return next;
    }, [visibility, cardLayout]);
    const visibleColumnCount = MY_SERVERS_TABLE_COLUMNS.filter(col => effectiveVisibility[col.id]).length;
    const sortLabel = sort ? t(MY_SERVERS_TABLE_COLUMNS.find(col => col.id === sort.colId)?.labelKey || '') : '';
    const dragDisabledTitle = sort
        ? t('introHub.table.clickToReturnManual', { column: sortLabel })
        : undefined;
    const sortedServers = React.useMemo(() => {
        if (!sort) return servers;
        const comparators: Record<MyServersSortableColId, (a: ServerProfile, b: ServerProfile) => number> = {
            index: () => 0,
            name: (a, b) => a.name.localeCompare(b.name, undefined, { numeric: true, sensitivity: 'base' }),
            used: (a, b) => (a.lastQuota?.used ?? -1) - (b.lastQuota?.used ?? -1),
            total: (a, b) => (a.lastQuota?.total ?? -1) - (b.lastQuota?.total ?? -1),
            pct: (a, b) => pctOf(a) - pctOf(b),
            time: (a, b) => dateOf(a) - dateOf(b),
            favorite: (a, b) => Number(favorites.has(b.id)) - Number(favorites.has(a.id)),
        };
        const direction = sort.dir === 'desc' ? -1 : 1;
        const withIndex = servers.map((server, index) => ({ server, index }));
        return withIndex
            .sort((a, b) => {
                const result = comparators[sort.colId](a.server, b.server) * direction;
                return result || a.index - b.index;
            })
            .map(item => item.server);
    }, [servers, sort, favorites]);

    return (
        <table className="w-full min-w-[1100px] table-auto border-collapse text-left">
            <MyServersTableHeader
                visibility={effectiveVisibility}
                sort={sort}
                onSort={onSort}
                onVisibleChange={onVisibleChange}
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
                            visibility={effectiveVisibility}
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
                        />
                    );
                })}
            </tbody>
            <MyServersTableFooter servers={servers} colSpan={visibleColumnCount} />
        </table>
    );
}
