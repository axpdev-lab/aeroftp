import * as React from 'react';
import {
    Search, X, Star, ShieldCheck, Server, Globe, Database, Cloud, Image, Code,
    HardDrive, Folder, FolderPlus, PanelLeftClose, PanelLeftOpen, ArrowLeftRight, Activity,
} from 'lucide-react';
import type { LucideIcon } from 'lucide-react';
import { useTranslation } from '../../i18n';
import { MyServersFilterBy, FILTER_CHIPS } from '../../types/catalog';
import type { ServerGroup } from '../../utils/serverGroups';

// Filters grouped into sidebar sections. The chip list grew past what a single
// wrapping row could hold once user groups (#320) landed, so the filters live in
// a vertical, collapsible sidebar that scales with any number of groups.
const QUICK_IDS: MyServersFilterBy[] = ['all', 'favorites', 'encrypted'];
const PROTOCOL_IDS: MyServersFilterBy[] = ['ftp', 's3', 'webdav', 'cloud', 'media', 'dev', 'local-bridge'];

const FILTER_ICON: Record<MyServersFilterBy, LucideIcon> = {
    all: Server,
    favorites: Star,
    encrypted: ShieldCheck,
    active: Activity,
    ftp: Globe,
    s3: Database,
    webdav: Server,
    cloud: Cloud,
    media: Image,
    dev: Code,
    'local-bridge': HardDrive,
};

const labelKeyOf = (id: MyServersFilterBy) =>
    FILTER_CHIPS.find((c) => c.id === id)?.labelKey ?? id;

interface MyServersSidebarProps {
    collapsed: boolean;
    onToggleCollapsed: () => void;
    side: 'left' | 'right';
    onToggleSide: () => void;
    searchQuery: string;
    onSearchChange: (query: string) => void;
    activeFilter: MyServersFilterBy;
    activeGroupId: string | null;
    onFilterChange: (filter: MyServersFilterBy) => void;
    chipCounts: Record<MyServersFilterBy, number>;
    groups: ServerGroup[];
    groupCounts: Record<string, number>;
    onGroupSelect: (groupId: string) => void;
    onGroupContextMenu: (e: React.MouseEvent, groupId: string) => void;
    onNewGroup: () => void;
}

export function MyServersSidebar({
    collapsed,
    onToggleCollapsed,
    side,
    onToggleSide,
    searchQuery,
    onSearchChange,
    activeFilter,
    activeGroupId,
    onFilterChange,
    chipCounts,
    groups,
    groupCounts,
    onGroupSelect,
    onGroupContextMenu,
    onNewGroup,
}: MyServersSidebarProps) {
    const t = useTranslation();

    // A static filter never reads as active while a group chip is selected: the
    // two narrowings are mutually exclusive (mirrors the old toolbar behaviour).
    const filterActive = (id: MyServersFilterBy) => activeGroupId === null && activeFilter === id;

    // The "Active Sessions" filter is contextual: it only shows while at least
    // one saved server has an open session (chipCounts.active > 0). Issue #128-C.
    const showActive = (chipCounts.active ?? 0) > 0;

    const borderSide = side === 'left' ? 'border-r' : 'border-l';

    // Collapsed rail: icon-only buttons with tooltips, ~52px wide. Keeps the
    // table full-width for power users while one click brings the filters back.
    if (collapsed) {
        const railBtn = (
            key: string,
            Icon: LucideIcon,
            active: boolean,
            onClick: (e: React.MouseEvent) => void,
            title: string,
            onContextMenu?: (e: React.MouseEvent) => void,
            iconColor?: string,
        ) => (
            <button
                key={key}
                onClick={onClick}
                onContextMenu={onContextMenu}
                title={title}
                className={`flex items-center justify-center w-9 h-9 rounded-lg transition-colors ${
                    active
                        ? 'bg-blue-100 dark:bg-blue-900/40 text-blue-600 dark:text-blue-300'
                        : 'text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300'
                }`}
            >
                <Icon size={16} className={iconColor} />
            </button>
        );
        return (
            <div className={`flex flex-col items-center gap-1 py-2 px-1.5 ${borderSide} border-gray-200 dark:border-gray-700 shrink-0`}>
                <button
                    onClick={onToggleCollapsed}
                    title={t('introHub.sidebar.expand')}
                    className="flex items-center justify-center w-9 h-9 rounded-lg text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300 transition-colors mb-1"
                >
                    {side === 'left' ? <PanelLeftOpen size={16} /> : <PanelLeftClose size={16} />}
                </button>
                {showActive && railBtn(
                    'active', Activity, filterActive('active'), () => onFilterChange('active'),
                    `${t(labelKeyOf('active'))} (${chipCounts.active ?? 0})`, undefined,
                    filterActive('active') ? undefined : 'text-green-500',
                )}
                {[...QUICK_IDS, ...PROTOCOL_IDS].map((id) =>
                    railBtn(id, FILTER_ICON[id], filterActive(id), () => onFilterChange(id), `${t(labelKeyOf(id))} (${chipCounts[id] ?? 0})`),
                )}
                {groups.length > 0 && <div className="w-6 h-px bg-gray-200 dark:bg-gray-700 my-1" />}
                {groups.map((group) =>
                    railBtn(
                        group.id,
                        Folder,
                        activeGroupId === group.id,
                        () => onGroupSelect(group.id),
                        `${group.name} (${groupCounts[group.id] ?? 0})`,
                        (e) => onGroupContextMenu(e, group.id),
                        undefined,
                    ),
                )}
                {railBtn('new-group', FolderPlus, false, () => onNewGroup(), t('introHub.group.newGroup'))}
            </div>
        );
    }

    // Render helpers (plain functions, NOT inline components): defining a
    // component inside the body gives it a new identity every render, so React
    // remounts the rows on each filter change. That made the clicked row flash
    // and swallow the first click (needed two clicks to filter). Calling these
    // as functions inlines the JSX and keeps the elements stable.
    const renderSectionLabel = (text: string) => (
        <div className="px-2 pt-3 pb-1 text-[10px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500">
            {text}
        </div>
    );

    const renderFilterRow = (id: MyServersFilterBy) => {
        const Icon = FILTER_ICON[id];
        const active = filterActive(id);
        const count = chipCounts[id] ?? 0;
        return (
            <button
                key={id}
                onClick={() => onFilterChange(id)}
                className={`flex items-center gap-2.5 w-full px-2.5 py-1.5 rounded-lg text-sm transition-colors ${
                    active
                        ? 'bg-blue-100 dark:bg-blue-900/40 text-blue-700 dark:text-blue-300 font-medium'
                        : 'text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
                }`}
            >
                <Icon size={15} className="shrink-0" />
                <span className="flex-1 text-left truncate">{t(labelKeyOf(id))}</span>
                <span className={`text-[11px] tabular-nums ${active ? 'text-blue-500 dark:text-blue-300' : 'text-gray-400 dark:text-gray-500'}`}>{count}</span>
            </button>
        );
    };

    return (
        <div className={`flex flex-col w-56 shrink-0 ${borderSide} border-gray-200 dark:border-gray-700`}>
            {/* Header: title + side toggle + collapse */}
            <div className="flex items-center gap-1 px-2 pt-2 pb-1">
                <span className="flex-1 px-1 text-[10px] font-semibold uppercase tracking-wider text-gray-400 dark:text-gray-500">
                    {t('introHub.sidebar.filters')}
                </span>
                <button
                    onClick={onToggleSide}
                    title={side === 'left' ? t('introHub.sidebar.moveRight') : t('introHub.sidebar.moveLeft')}
                    className="p-1.5 rounded-lg text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
                >
                    <ArrowLeftRight size={14} />
                </button>
                <button
                    onClick={onToggleCollapsed}
                    title={t('introHub.sidebar.collapse')}
                    className="p-1.5 rounded-lg text-gray-400 dark:text-gray-500 hover:bg-gray-100 dark:hover:bg-gray-700 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
                >
                    {side === 'left' ? <PanelLeftClose size={14} /> : <PanelLeftOpen size={14} />}
                </button>
            </div>

            {/* Search */}
            <div className="relative px-2 pb-1">
                <Search size={14} className="absolute left-4 top-1/2 -translate-y-1/2 text-gray-400 pointer-events-none" />
                <input
                    type="text"
                    value={searchQuery}
                    onChange={(e) => onSearchChange(e.target.value)}
                    placeholder={t('introHub.searchServers')}
                    className="w-full h-8 pl-8 pr-7 text-sm rounded-lg bg-gray-100 dark:bg-gray-700/50 border border-gray-200 dark:border-gray-600 text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:border-blue-400 dark:focus:border-blue-500 focus:ring-1 focus:ring-blue-400/25 transition-colors"
                />
                {searchQuery && (
                    <button
                        onClick={() => onSearchChange('')}
                        className="absolute right-3.5 top-1/2 -translate-y-1/2 text-gray-400 hover:text-gray-600 dark:hover:text-gray-300"
                    >
                        <X size={13} />
                    </button>
                )}
            </div>

            {/* Scrollable filter list */}
            <div className="flex-1 overflow-y-auto px-1.5 pb-2">
                {/* Active Sessions: contextual filter, shown only while a session
                    is open. Green accent + pulsing dot tie it to the header badge. */}
                {showActive && (
                    <button
                        onClick={() => onFilterChange('active')}
                        className={`flex items-center gap-2.5 w-full mt-2 px-2.5 py-1.5 rounded-lg text-sm transition-colors ${
                            filterActive('active')
                                ? 'bg-green-100 dark:bg-green-900/40 text-green-700 dark:text-green-300 font-medium'
                                : 'text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
                        }`}
                    >
                        <span className="w-2 h-2 shrink-0 rounded-full bg-green-500 animate-pulse" />
                        <span className="flex-1 text-left truncate">{t(labelKeyOf('active'))}</span>
                        <span className={`text-[11px] tabular-nums ${filterActive('active') ? 'text-green-500 dark:text-green-300' : 'text-gray-400 dark:text-gray-500'}`}>{chipCounts.active ?? 0}</span>
                    </button>
                )}

                {renderSectionLabel(t('introHub.sidebar.quick'))}
                {QUICK_IDS.map(renderFilterRow)}

                {renderSectionLabel(t('introHub.sidebar.protocols'))}
                {PROTOCOL_IDS.map(renderFilterRow)}

                {renderSectionLabel(t('introHub.sidebar.groups'))}
                {groups.map((group) => {
                    const active = activeGroupId === group.id;
                    const count = groupCounts[group.id] ?? 0;
                    return (
                        <button
                            key={group.id}
                            onClick={() => onGroupSelect(group.id)}
                            onContextMenu={(e) => onGroupContextMenu(e, group.id)}
                            title={t('introHub.group.chipHint')}
                            className={`flex items-center gap-2.5 w-full px-2.5 py-1.5 rounded-lg text-sm transition-colors ${
                                active
                                    ? 'bg-emerald-100 dark:bg-emerald-900/40 text-emerald-700 dark:text-emerald-300 font-medium'
                                    : 'text-gray-600 dark:text-gray-300 hover:bg-gray-100 dark:hover:bg-gray-700'
                            }`}
                        >
                            <Folder size={15} className="shrink-0" style={group.color ? { color: group.color } : undefined} />
                            <span className="flex-1 text-left truncate">{group.name}</span>
                            <span className={`text-[11px] tabular-nums ${active ? 'text-emerald-500 dark:text-emerald-300' : 'text-gray-400 dark:text-gray-500'}`}>{count}</span>
                        </button>
                    );
                })}
                <button
                    onClick={onNewGroup}
                    title={t('introHub.group.newGroup')}
                    className="flex items-center gap-2.5 w-full px-2.5 py-1.5 mt-0.5 rounded-lg text-sm text-gray-400 dark:text-gray-500 hover:text-emerald-600 dark:hover:text-emerald-400 hover:bg-gray-100 dark:hover:bg-gray-700 transition-colors"
                >
                    <FolderPlus size={15} className="shrink-0" />
                    <span className="flex-1 text-left truncate">{t('introHub.group.newGroup')}</span>
                </button>
            </div>
        </div>
    );
}
